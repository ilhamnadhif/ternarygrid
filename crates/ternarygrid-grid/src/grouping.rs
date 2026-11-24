//! Grouping of fetched rows (the Grouping panel, FR-D5).

use crate::expr::RowValues;
use crate::view::{ClientFilter, FilterOp, ViewContext, sort_cmp};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use ternarygrid_core::{SharedRowStore, Value};

const CHUNK_ROWS: usize = 4096;

/// What to group by.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSpec {
    /// Model columns forming the group key.
    pub columns: Vec<usize>,
    /// Optional numeric columns aggregated per group (SUM / AVG / MIN / MAX).
    #[serde(default)]
    pub measures: Vec<usize>,
}

/// Aggregated statistics for one measure column in a group.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MeasureStats {
    /// Sum over finite numeric values.
    pub sum: Option<f64>,
    /// Average over finite numeric values.
    pub avg: Option<f64>,
    /// Minimum over finite numeric values.
    pub min: Option<f64>,
    /// Maximum over finite numeric values.
    pub max: Option<f64>,
}

/// One group.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupRow {
    /// Key values (one per [`GroupSpec::columns`]).
    pub keys: Vec<Value>,
    /// Number of rows.
    pub count: usize,
    /// Aggregated measures (one per [`GroupSpec::measures`], in the same order).
    pub measures: Vec<MeasureStats>,
}

/// Column by which grouped rows are sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GroupSortCol {
    /// Group key column (index into [`GroupSpec::columns`]).
    Key(usize),
    /// Row count.
    Count,
    /// Sum of a measure (index into [`GroupSpec::measures`]).
    Sum(usize),
    /// Average of a measure (index into [`GroupSpec::measures`]).
    Avg(usize),
}

/// Sort column and direction of a grouped result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GroupSort {
    /// Which column to sort by.
    pub col: GroupSortCol,
    /// True for descending (`DESC`), false for ascending (`ASC`).
    pub descending: bool,
}

impl Default for GroupSort {
    fn default() -> Self {
        GroupSort { col: GroupSortCol::Count, descending: true }
    }
}

/// Result of grouping.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroupResult {
    /// Groups, sorted (largest count first by default).
    pub groups: Vec<GroupRow>,
    /// Rows examined.
    pub rows: usize,
    /// More distinct groups existed than `max_groups`; the rest were merged away.
    pub truncated: bool,
}

fn cmp_opt_f64(a: Option<f64>, b: Option<f64>, descending: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let ord = x.total_cmp(&y);
            if descending { ord.reverse() } else { ord }
        }
    }
}

fn cmp_keys(a: &[Value], b: &[Value]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b) {
        let o = sort_cmp(x, y);
        if o != std::cmp::Ordering::Equal {
            return o;
        }
    }
    a.len().cmp(&b.len())
}

/// Sort `groups` in place by `sort` (NULL / missing values always sort last,
/// with deterministic tie-breaking by Count DESC then key values ASC).
pub fn sort_groups(groups: &mut [GroupRow], sort: GroupSort) {
    use std::cmp::Ordering;
    groups.sort_by(|a, b| {
        let primary = match sort.col {
            GroupSortCol::Key(i) => {
                let va = a.keys.get(i).unwrap_or(&Value::Null);
                let vb = b.keys.get(i).unwrap_or(&Value::Null);
                match (va.is_null(), vb.is_null()) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) => {
                        let o = sort_cmp(va, vb);
                        if sort.descending { o.reverse() } else { o }
                    }
                }
            }
            GroupSortCol::Count => {
                if sort.descending {
                    b.count.cmp(&a.count)
                } else {
                    a.count.cmp(&b.count)
                }
            }
            GroupSortCol::Sum(i) => cmp_opt_f64(a.measures.get(i).and_then(|m| m.sum), b.measures.get(i).and_then(|m| m.sum), sort.descending),
            GroupSortCol::Avg(i) => cmp_opt_f64(a.measures.get(i).and_then(|m| m.avg), b.measures.get(i).and_then(|m| m.avg), sort.descending),
        };
        primary.then_with(|| b.count.cmp(&a.count)).then_with(|| cmp_keys(&a.keys, &b.keys))
    });
}

struct Ext<'a> {
    base: &'a [Value],
    base_len: usize,
    virt: &'a [Value],
}

impl RowValues for Ext<'_> {
    fn value(&self, col: usize) -> Option<&Value> {
        if col < self.base_len { self.base.get(col) } else { self.virt.get(col - self.base_len) }
    }
}

struct MeasureAcc {
    n: usize,
    sum: f64,
    min: f64,
    max: f64,
}

impl Default for MeasureAcc {
    fn default() -> Self {
        MeasureAcc { n: 0, sum: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY }
    }
}

impl MeasureAcc {
    fn push(&mut self, v: f64) {
        self.n += 1;
        self.sum += v;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
    }

    fn finish(self) -> MeasureStats {
        if self.n > 0 { MeasureStats { sum: Some(self.sum), avg: Some(self.sum / self.n as f64), min: Some(self.min), max: Some(self.max) } } else { MeasureStats::default() }
    }
}

struct Acc {
    keys: Vec<Value>,
    count: usize,
    measures: Vec<MeasureAcc>,
}

fn key_part(v: &Value) -> (u8, String) {
    match v {
        Value::Null => (0, String::new()),
        // Equal numbers group together regardless of representation (3, 3.0,
        // 3.00), as exact decimal text: big integers stay apart.
        Value::Int(i) => (1, i.to_string()),
        Value::UInt(u) => (1, u.to_string()),
        Value::Float(f) => (1, if *f == 0.0 { "0".into() } else { format!("{f}") }),
        Value::Decimal(d) => match plain_decimal(d) {
            Some(t) => (1, t),
            None => match v.as_f64() {
                Some(f) => (1, format!("{f}")),
                None => (2, d.to_string()),
            },
        },
        other => (2, other.to_text().into_owned()),
    }
}

/// `d` without a plus sign, leading zeros or trailing fraction zeros
/// (`+001.50` → `1.5`, `-0.0` → `0`); `None` when it is not a plain number.
fn plain_decimal(d: &str) -> Option<String> {
    let (neg, body) = match d.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, d.strip_prefix('+').unwrap_or(d)),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    if int.is_empty() && frac.is_empty() || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let int = int.trim_start_matches('0');
    let frac = frac.trim_end_matches('0');
    let mut out = String::new();
    if neg && !(int.is_empty() && frac.is_empty()) {
        out.push('-');
    }
    out.push_str(if int.is_empty() { "0" } else { int });
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    Some(out)
}

/// Group the rows `rows` of `store` (all stored rows when `None`).
/// Returns `None` when cancelled.
pub fn compute_groups(store: &SharedRowStore, rows: Option<&[u32]>, spec: &GroupSpec, ctx: &ViewContext, max_groups: usize, cancel: &AtomicBool) -> Option<GroupResult> {
    let total = match rows {
        Some(r) => r.len(),
        None => store.lock().len(),
    };
    let mut map: HashMap<Vec<(u8, String)>, Acc> = HashMap::new();
    let mut truncated = false;
    let mut virt = vec![Value::Null; ctx.virtual_columns.len()];
    let mut needed: Vec<usize> = spec.columns.clone();
    needed.extend(spec.measures.iter().copied());
    let virt_needed: Vec<usize> = needed.iter().filter(|c| **c >= ctx.base_columns && **c - ctx.base_columns < ctx.virtual_columns.len()).map(|c| c - ctx.base_columns).collect();
    let measure_count = spec.measures.len();
    let mut scanned = 0usize;
    let mut process = |row: &[Value], idx: u32, map: &mut HashMap<Vec<(u8, String)>, Acc>| {
        if ctx.removed.contains(idx) {
            return;
        }
        for &v in &virt_needed {
            virt[v] = ctx.virtual_columns[v].eval(row).unwrap_or(Value::Null);
        }
        let ext = Ext { base: row, base_len: ctx.base_columns, virt: &virt };
        let key: Vec<(u8, String)> = spec.columns.iter().map(|&c| key_part(ext.value(c).unwrap_or(&Value::Null))).collect();
        scanned += 1;
        if !map.contains_key(&key) {
            if map.len() >= max_groups {
                truncated = true;
                return;
            }
            let keys = spec.columns.iter().map(|&c| ext.value(c).cloned().unwrap_or(Value::Null)).collect();
            let measures = (0..measure_count).map(|_| MeasureAcc::default()).collect();
            map.insert(key.clone(), Acc { keys, count: 0, measures });
        }
        if let Some(acc) = map.get_mut(&key) {
            acc.count += 1;
            for (slot, &mcol) in acc.measures.iter_mut().zip(&spec.measures) {
                if let Some(m) = ext.value(mcol).and_then(Value::as_f64).filter(|f| f.is_finite()) {
                    slot.push(m);
                }
            }
        }
    };
    let mut start = 0usize;
    while start < total {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let end = (start + CHUNK_ROWS).min(total);
        let mut guard = store.lock();
        match rows {
            None => guard.for_each_in(start, end, |i, r| process(r, i as u32, &mut map)),
            Some(list) => {
                for &idx in &list[start..end] {
                    if let Some(r) = guard.get(idx as usize) {
                        process(r, idx, &mut map);
                    }
                }
            }
        }
        drop(guard);
        start = end;
    }
    let mut groups: Vec<GroupRow> =
        map.into_values().map(|a| GroupRow { keys: a.keys, count: a.count, measures: a.measures.into_iter().map(MeasureAcc::finish).collect() }).collect();
    sort_groups(&mut groups, GroupSort::default());
    Some(GroupResult { groups, rows: scanned, truncated })
}

/// Client filter selecting the rows of one group.
pub fn group_filter(spec: &GroupSpec, group: &GroupRow, column_names: &[String]) -> Option<ClientFilter> {
    let mut filter: Option<ClientFilter> = None;
    for (&c, v) in spec.columns.iter().zip(&group.keys) {
        let name = column_names.get(c)?;
        let op = if v.is_null() { FilterOp::IsNull } else { FilterOp::Eq };
        let cond = ClientFilter::condition(c, name, op, v);
        filter = Some(match filter {
            None => cond,
            Some(f) => f.and(cond),
        });
    }
    filter
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternarygrid_core::RowStore;

    #[test]
    fn number_keys_are_exact() {
        let k = |v: Value| key_part(&v);
        assert_ne!(k(Value::Int(9_007_199_254_740_993)), k(Value::Int(9_007_199_254_740_992)));
        assert_eq!(k(Value::Int(3)), k(Value::Float(3.0)));
        assert_eq!(k(Value::Int(3)), k(Value::decimal("3.00")));
        assert_eq!(k(Value::Float(0.1)), k(Value::decimal("+0.10")));
        assert_eq!(k(Value::Int(0)), k(Value::decimal("-0.0")));
        assert_eq!(k(Value::UInt(u64::MAX)), k(Value::decimal(u64::MAX.to_string())));
    }

    fn store() -> SharedRowStore {
        let s = RowStore::shared(1 << 24);
        {
            let mut g = s.lock();
            for i in 0..1000i64 {
                let cat = match i % 3 {
                    0 => Value::text("a"),
                    1 => Value::text("b"),
                    _ => Value::Null,
                };
                g.push(vec![cat, Value::Int(i % 2), Value::Float(i as f64)]);
            }
        }
        s
    }

    #[test]
    fn groups_with_multiple_measures_and_sorting() {
        let s = store();
        let ctx = ViewContext { base_columns: 3, ..Default::default() };
        let spec = GroupSpec { columns: vec![0], measures: vec![1, 2] };
        let mut r = compute_groups(&s, None, &spec, &ctx, 1000, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.rows, 1000);
        assert_eq!(r.groups.len(), 3);
        assert_eq!(r.groups[0].count, 334);
        assert_eq!(r.groups[0].keys, vec![Value::text("a")]);
        let total: usize = r.groups.iter().map(|g| g.count).sum();
        assert_eq!(total, 1000);
        let a = &r.groups[0];
        assert_eq!(a.measures.len(), 2);
        assert_eq!(a.measures[0].min, Some(0.0));
        assert_eq!(a.measures[0].max, Some(1.0));
        assert_eq!(a.measures[1].min, Some(0.0));
        assert_eq!(a.measures[1].max, Some(999.0));
        assert!((a.measures[1].avg.unwrap() - 499.5).abs() < 1e-9);

        // Sort by Key ASC puts NULL last ("a", "b", NULL).
        sort_groups(&mut r.groups, GroupSort { col: GroupSortCol::Key(0), descending: false });
        assert_eq!(r.groups[0].keys, vec![Value::text("a")]);
        assert_eq!(r.groups[1].keys, vec![Value::text("b")]);
        assert_eq!(r.groups[2].keys, vec![Value::Null]);

        // Sort by Key DESC still puts NULL last ("b", "a", NULL).
        sort_groups(&mut r.groups, GroupSort { col: GroupSortCol::Key(0), descending: true });
        assert_eq!(r.groups[0].keys, vec![Value::text("b")]);
        assert_eq!(r.groups[1].keys, vec![Value::text("a")]);
        assert_eq!(r.groups[2].keys, vec![Value::Null]);

        // Sort by Sum(1) ASC and DESC.
        sort_groups(&mut r.groups, GroupSort { col: GroupSortCol::Sum(1), descending: false });
        assert!(r.groups[0].measures[1].sum.unwrap() <= r.groups[1].measures[1].sum.unwrap());
        assert!(r.groups[1].measures[1].sum.unwrap() <= r.groups[2].measures[1].sum.unwrap());
        sort_groups(&mut r.groups, GroupSort { col: GroupSortCol::Avg(1), descending: true });
        assert!(r.groups[0].measures[1].avg.unwrap() >= r.groups[1].measures[1].avg.unwrap());

        // Two key columns, no measures, subset of rows, group limit.
        let spec = GroupSpec { columns: vec![0, 1], measures: vec![] };
        let rows: Vec<u32> = (0..100).collect();
        let r = compute_groups(&s, Some(&rows), &spec, &ctx, 4, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.groups.len(), 4);
        assert!(r.truncated);
        assert!(r.groups.iter().all(|g| g.measures.is_empty()));
        assert!(compute_groups(&s, None, &spec, &ctx, 10, &AtomicBool::new(true)).is_none());
    }

    #[test]
    fn filter_for_group() {
        let spec = GroupSpec { columns: vec![0, 1], measures: vec![] };
        let g = GroupRow { keys: vec![Value::Null, Value::Int(1)], count: 1, measures: vec![] };
        let names = vec!["cat".to_string(), "flag".to_string(), "v".to_string()];
        let f = group_filter(&spec, &g, &names).unwrap();
        assert_eq!(f.text(), "(cat IS NULL) AND (flag = 1)");
        assert!(f.matches(&vec![Value::Null, Value::Int(1), Value::Null]));
        assert!(!f.matches(&vec![Value::text("a"), Value::Int(1), Value::Null]));
    }
}
