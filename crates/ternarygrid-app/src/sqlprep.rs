//! Turns editor text into executable statements (FR-S2, FR-S3, FR-S8):
//! choose the statements for the requested scope, run client commands
//! (`@set`, `@include`, `@export`, `@echo`), substitute `:name` / `${name}`
//! parameters and collect the statements that need a confirmation.

use crate::exec::{ClientAction, StmtPlan};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use ternarygrid_core::dialect::SqlSyntax;
use ternarygrid_sql::{ClientCommand, Danger, ParamKind, ParamOptions, SplitOptions, StatementSpan};

/// Which part of the editor text to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The statement under the cursor (⌘↩).
    Statement(usize),
    /// The selected text, split into statements.
    Selection(Range<usize>),
    /// The whole script (⌥X).
    Script,
    /// From the statement under the cursor to the end.
    FromCursor(usize),
}

pub struct PrepConfig<'a> {
    pub syntax: &'a SqlSyntax,
    /// Redis, MongoDB and Elasticsearch requests are classified as commands.
    pub language: ternarygrid_core::QueryLanguage,
    pub split: SplitOptions,
    /// `None` disables parameter substitution.
    pub params: Option<ParamOptions>,
    /// Directory of the script, for relative `@include` paths.
    pub base_dir: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Prepared {
    pub plans: Vec<StmtPlan>,
    /// Parameters without a value (ask the user, then prepare again).
    pub missing: Vec<(ParamKind, String)>,
    /// Statements that need a confirmation: (plan index, reason).
    pub dangers: Vec<(usize, Danger)>,
    pub has_writes: bool,
    /// Problems that prevent execution (unknown command, unreadable include).
    pub errors: Vec<String>,
    /// Variables after the run's `@set` commands.
    pub variables: BTreeMap<String, String>,
}

fn spans_for(text: &str, scope: &Scope, syntax: &SqlSyntax, split: &SplitOptions) -> Vec<StatementSpan> {
    match scope {
        Scope::Statement(cursor) => ternarygrid_sql::statement_at(text, (*cursor).min(text.len()), syntax, split).into_iter().collect(),
        Scope::Selection(r) => {
            let r = r.start.min(text.len())..r.end.min(text.len());
            let Some(sub) = text.get(r.clone()) else { return Vec::new() };
            ternarygrid_sql::split_statements(sub, syntax, split)
                .into_iter()
                .map(|mut s| {
                    s.start += r.start;
                    s.end += r.start;
                    s.outer_start += r.start;
                    s.outer_end += r.start;
                    s
                })
                .collect()
        }
        Scope::Script => ternarygrid_sql::split_statements(text, syntax, split),
        Scope::FromCursor(cursor) => ternarygrid_sql::split_statements(text, syntax, split).into_iter().filter(|s| s.end >= *cursor).collect(),
    }
}

/// Prepare the statements of `scope`. `values` holds parameter values the
/// user entered (SQL text, used verbatim) and variables set earlier.
pub fn prepare(text: &str, scope: Scope, cfg: &PrepConfig, values: &BTreeMap<String, String>) -> Prepared {
    let mut out = Prepared { variables: values.clone(), ..Default::default() };
    let spans = spans_for(text, &scope, cfg.syntax, &cfg.split);
    let mut index = 0usize;
    add_spans(text, &spans, cfg, &mut out, &mut index, 0, true);
    out.missing.dedup();
    out
}

fn add_spans(text: &str, spans: &[StatementSpan], cfg: &PrepConfig, out: &mut Prepared, index: &mut usize, depth: usize, editor_ranges: bool) {
    for span in spans {
        let stmt = span.text(text);
        if stmt.trim().is_empty() {
            continue;
        }
        let range = if editor_ranges { span.start..span.end } else { 0..0 };
        if let Some(cmd) = ternarygrid_sql::parse_client_command(stmt) {
            match cmd {
                Err(e) => out.errors.push(e),
                Ok(ClientCommand::Set { name, value }) => {
                    let value = substitute(&value, cfg, &out.variables, &mut Vec::new());
                    out.variables.insert(name.clone(), value.clone());
                    push(out, index, stmt, String::new(), range, false, Some(ClientAction::Set { name, value }));
                }
                Ok(ClientCommand::Echo(t)) => {
                    let t = substitute(&t, cfg, &out.variables, &mut Vec::new());
                    push(out, index, stmt, String::new(), range, false, Some(ClientAction::Echo(t)));
                }
                Ok(ClientCommand::Export { options }) => push(out, index, stmt, String::new(), range, false, Some(ClientAction::Export(options))),
                Ok(ClientCommand::Include { path }) => {
                    if depth >= 8 {
                        out.errors.push(format!("@include nested too deeply: {path}"));
                        continue;
                    }
                    let p = Path::new(&path);
                    let full = if p.is_absolute() { p.to_path_buf() } else { cfg.base_dir.clone().unwrap_or_default().join(p) };
                    match std::fs::read_to_string(&full) {
                        Ok(inner) => {
                            let inner_spans = ternarygrid_sql::split_statements(&inner, cfg.syntax, &cfg.split);
                            let sub = PrepConfig {
                                syntax: cfg.syntax,
                                language: cfg.language,
                                split: cfg.split.clone(),
                                params: cfg.params,
                                base_dir: full.parent().map(Path::to_path_buf),
                            };
                            add_spans(&inner, &inner_spans, &sub, out, index, depth + 1, false);
                        }
                        Err(e) => out.errors.push(format!("@include {}: {e}", full.display())),
                    }
                }
                Ok(ClientCommand::Other { name, .. }) => out.errors.push(format!("Unknown client command @{name}")),
            }
            continue;
        }
        let mut missing = Vec::new();
        let sql = substitute(stmt, cfg, &out.variables, &mut missing);
        for m in missing {
            if !out.missing.contains(&m) {
                out.missing.push(m);
            }
        }
        let (is_write, danger) = classify_substituted(stmt, &sql, cfg);
        if let Some(d) = danger {
            out.dangers.push((*index, d));
        }
        out.has_writes |= is_write;
        push(out, index, stmt, sql, range, is_write, None);
    }
}

/// Whether the statement writes and why it needs a confirmation. Parameter values
/// are SQL text: what is sent (`sql`) is classified too, so a value cannot turn a
/// read into a write (`${x}` = `1; DROP TABLE t`) past the read-only check and the
/// confirmations.
fn classify_substituted(original: &str, sql: &str, cfg: &PrepConfig) -> (bool, Option<Danger>) {
    let classify = |s: &str| ternarygrid_sql::classify_command(s, cfg.language).unwrap_or_else(|| ternarygrid_sql::classify(s, cfg.syntax));
    let info = classify(original);
    let (mut is_write, mut danger) = (info.is_write, info.danger);
    if sql != original {
        let parts: Vec<(bool, Option<Danger>)> = if cfg.language.is_sql() {
            // A value may add statements: each one sent counts.
            ternarygrid_sql::split_and_classify(sql, cfg.syntax, &cfg.split).into_iter().map(|(_, i)| (i.is_write, i.danger)).collect()
        } else {
            Vec::new()
        };
        let parts = if parts.is_empty() {
            let i = classify(sql);
            vec![(i.is_write, i.danger)]
        } else {
            parts
        };
        for (w, d) in parts {
            is_write |= w;
            danger = danger.or(d);
        }
    }
    (is_write, danger)
}

fn push(out: &mut Prepared, index: &mut usize, original: &str, sql: String, range: Range<usize>, is_write: bool, client: Option<ClientAction>) {
    out.plans.push(StmtPlan { index: *index, sql, original: original.to_string(), range, is_write, client });
    *index += 1;
}

/// Replace parameters with their values; unknown ones are collected in `missing`
/// and left in place.
fn substitute(stmt: &str, cfg: &PrepConfig, vars: &BTreeMap<String, String>, missing: &mut Vec<(ParamKind, String)>) -> String {
    let Some(opts) = cfg.params else { return stmt.to_string() };
    let refs = ternarygrid_sql::find_parameters(stmt, cfg.syntax, &opts);
    if refs.is_empty() {
        return stmt.to_string();
    }
    match ternarygrid_sql::substitute(stmt, cfg.syntax, &opts, &|r| vars.get(&r.name).cloned()) {
        Ok(s) => s,
        Err(unresolved) => {
            for r in unresolved {
                let k = (r.kind, r.name.clone());
                if !missing.contains(&k) {
                    missing.push(k);
                }
            }
            stmt.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(syntax: &SqlSyntax) -> PrepConfig<'_> {
        PrepConfig { syntax, language: ternarygrid_core::QueryLanguage::Sql, split: SplitOptions::default(), params: Some(ParamOptions::default()), base_dir: None }
    }

    #[test]
    fn statement_under_cursor_and_script() {
        let syntax = SqlSyntax::ansi();
        let text = "select 1;\nselect 2;\n\nupdate t set a = 1;";
        let p = prepare(text, Scope::Statement(12), &cfg(&syntax), &BTreeMap::new());
        assert_eq!(p.plans.len(), 1);
        assert_eq!(p.plans[0].sql, "select 2");
        let p = prepare(text, Scope::Script, &cfg(&syntax), &BTreeMap::new());
        assert_eq!(p.plans.len(), 3);
        assert!(p.has_writes);
        assert_eq!(p.dangers, vec![(2, Danger::UpdateWithoutWhere)]);
    }

    #[test]
    fn variables_and_parameters() {
        let syntax = SqlSyntax::ansi();
        let text = "@set lim = 10\nselect * from t where id = :id limit ${lim}";
        let p = prepare(text, Scope::Script, &cfg(&syntax), &BTreeMap::new());
        assert_eq!(p.missing, vec![(ParamKind::Named, "id".to_string())]);
        let mut v = BTreeMap::new();
        v.insert("id".to_string(), "42".to_string());
        let p = prepare(text, Scope::Script, &cfg(&syntax), &v);
        assert!(p.missing.is_empty());
        assert!(matches!(p.plans[0].client, Some(ClientAction::Set { .. })));
        assert_eq!(p.plans[1].sql, "select * from t where id = 42 limit 10");
    }

    #[test]
    fn parameters_are_classified_after_substitution() {
        let syntax = SqlSyntax::ansi();
        let text = "select * from t where id = ${x}";
        // A value is SQL text: it can add a write to a read.
        let mut v = BTreeMap::new();
        v.insert("x".to_string(), "1; DELETE FROM t".to_string());
        let p = prepare(text, Scope::Script, &cfg(&syntax), &v);
        assert!(p.missing.is_empty());
        assert!(p.has_writes && p.plans[0].is_write);
        assert_eq!(p.dangers, vec![(0, Danger::DeleteWithoutWhere)]);
        let p = prepare("select :p", Scope::Script, &cfg(&syntax), &BTreeMap::from([("p".to_string(), "1; update t set a = 2".to_string())]));
        assert!(p.has_writes);
        assert_eq!(p.dangers, vec![(0, Danger::UpdateWithoutWhere)]);
        // A plain value keeps the read a read.
        v.insert("x".to_string(), "42".to_string());
        let p = prepare(text, Scope::Script, &cfg(&syntax), &v);
        assert!(!p.has_writes && p.dangers.is_empty());
        assert_eq!(p.plans[0].sql, "select * from t where id = 42");
        // Without a value it is asked for (and nothing is classified as a write).
        let p = prepare(text, Scope::Script, &cfg(&syntax), &BTreeMap::new());
        assert_eq!(p.missing, vec![(ParamKind::Template, "x".to_string())]);
        assert!(!p.has_writes);
    }

    #[test]
    fn include_reads_relative_files() {
        let dir = std::env::temp_dir().join(format!("ternarygrid-prep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("inc.sql"), "select 'inc';\nselect 'inc2';").unwrap();
        let syntax = SqlSyntax::ansi();
        let mut c = cfg(&syntax);
        c.base_dir = Some(dir.clone());
        let p = prepare("@include inc.sql\nselect 3;", Scope::Script, &c, &BTreeMap::new());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let sqls: Vec<&str> = p.plans.iter().map(|p| p.sql.as_str()).collect();
        assert_eq!(sqls, vec!["select 'inc'", "select 'inc2'", "select 3"]);
    }
}
