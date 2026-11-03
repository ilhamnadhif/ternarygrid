//! SQL generation for the query builder.

use super::model::{Connector, JoinKind, QbCondition, QbJoin, QueryModel};
use std::collections::BTreeSet;
use ternarygrid_core::{DbKind, Dialect};

impl QueryModel {
    /// Renders an expression: `alias.column` references of tables in the
    /// model (and `alias.*`) are quoted with the dialect, anything else is
    /// passed through as written.
    pub fn render_expr(&self, expr: &str, dialect: &dyn Dialect) -> String {
        let e = expr.trim();
        if let Some(alias) = e.strip_suffix(".*")
            && self.table(alias).is_some()
        {
            return format!("{}.*", dialect.quote_ident(alias));
        }
        for t in &self.tables {
            if let Some(col) = e
                .strip_prefix(t.alias.as_str())
                .and_then(|r| r.strip_prefix('.'))
                && t.columns.iter().any(|c| c.name == col)
            {
                return format!(
                    "{}.{}",
                    dialect.quote_ident(&t.alias),
                    dialect.quote_ident(col)
                );
            }
        }
        e.to_string()
    }

    fn render_condition(&self, c: &QbCondition, dialect: &dyn Dialect) -> Option<String> {
        if c.expr.trim().is_empty() {
            return None;
        }
        let lhs = self.render_expr(&c.expr, dialect);
        let op = c.operator.trim().to_ascii_uppercase();
        let value = self.render_expr(&c.value, dialect);
        Some(match op.as_str() {
            "" => lhs,
            "IS NULL" | "IS NOT NULL" => format!("{lhs} {op}"),
            // incomplete (also `IN` without a list, which would render `IN ()`)
            _ if value.is_empty() => return None,
            "IN" | "NOT IN" => {
                if value.starts_with('(') {
                    format!("{lhs} {op} {value}")
                } else {
                    format!("{lhs} {op} ({value})")
                }
            }
            _ => format!("{lhs} {op} {value}"),
        })
    }

    /// Combines a WHERE / HAVING list strictly left to right, the way the
    /// list reads in the editor: `a OR b AND c` means `(a OR b) AND c`, so
    /// everything before a change of connector is parenthesized. A
    /// condition containing its own top level AND / OR (a free expression
    /// like `x = 1 OR y = 2`) is parenthesized too.
    fn render_conditions(&self, list: &[QbCondition], dialect: &dyn Dialect) -> Option<String> {
        let rendered: Vec<(Connector, String)> = list
            .iter()
            .filter_map(|c| Some((c.connector, self.render_condition(c, dialect)?)))
            .collect();
        let several = rendered.len() > 1;
        let mut out = String::new();
        let mut previous: Option<Connector> = None;
        for (i, (connector, text)) in rendered.into_iter().enumerate() {
            let text = if several && has_top_level_logic(&text) {
                format!("({text})")
            } else {
                text
            };
            if i == 0 {
                out = text;
                continue;
            }
            if previous.is_some_and(|p| p != connector) {
                out = format!("({out})");
            }
            out.push_str(&format!("\n    {} {text}", connector.sql()));
            previous = Some(connector);
        }
        if out.is_empty() { None } else { Some(out) }
    }

    fn table_ref(&self, idx: usize, dialect: &dyn Dialect) -> String {
        let Some(t) = self.tables.get(idx) else {
            return String::new();
        };
        let name = dialect.qualified(t.obj.schema.as_deref(), &t.obj.name);
        format!("{name} {}", dialect.quote_ident(&t.alias))
    }

    fn join_condition(&self, j: &QbJoin, dialect: &dyn Dialect) -> String {
        format!(
            "{}.{} = {}.{}",
            dialect.quote_ident(&j.left_alias),
            dialect.quote_ident(&j.left_column),
            dialect.quote_ident(&j.right_alias),
            dialect.quote_ident(&j.right_column)
        )
    }

    /// FROM clause with joins: tables are attached in model order, each one
    /// through all join conditions to tables already attached; tables
    /// without a join are cross joined.
    fn render_from(&self, dialect: &dyn Dialect) -> String {
        let n = self.tables.len();
        let mut included: Vec<usize> = vec![0];
        let mut aliases: BTreeSet<&str> = self
            .tables
            .first()
            .map(|t| t.alias.as_str())
            .into_iter()
            .collect();
        let mut used = vec![false; self.joins.len()];
        let mut out = format!("FROM {}", self.table_ref(0, dialect));
        while included.len() < n {
            let connected = (0..n).filter(|i| !included.contains(i)).find(|&i| {
                let a = self.tables[i].alias.as_str();
                self.joins.iter().any(|j| {
                    (j.left_alias == a && aliases.contains(j.right_alias.as_str()))
                        || (j.right_alias == a && aliases.contains(j.left_alias.as_str()))
                })
            });
            match connected {
                Some(i) => {
                    let a = self.tables[i].alias.clone();
                    let mut kind: Option<JoinKind> = None;
                    let mut conds = Vec::new();
                    for (ji, j) in self.joins.iter().enumerate() {
                        if used[ji] {
                            continue;
                        }
                        let new_is_right =
                            j.right_alias == a && aliases.contains(j.left_alias.as_str());
                        let new_is_left =
                            j.left_alias == a && aliases.contains(j.right_alias.as_str());
                        if new_is_right || new_is_left {
                            used[ji] = true;
                            conds.push(self.join_condition(j, dialect));
                            if kind.is_none() {
                                kind = Some(if new_is_right {
                                    j.kind
                                } else {
                                    j.kind.flipped()
                                });
                            }
                        }
                    }
                    let kind = kind.unwrap_or_default();
                    // A loaded model can carry a FULL JOIN although the UI
                    // does not offer it for MySQL / MariaDB: keep it (so the
                    // server rejects the statement instead of silently
                    // returning other rows) and say why.
                    let note = match (kind, dialect.kind()) {
                        (JoinKind::Full, DbKind::MySql) => {
                            " /* FULL JOIN is not supported by MySQL: choose another join type */"
                        }
                        (JoinKind::Full, DbKind::MariaDb) => {
                            " /* FULL JOIN is not supported by MariaDB: choose another join type */"
                        }
                        _ => "",
                    };
                    out.push_str(&format!(
                        "\n    {}{note} {} ON {}",
                        kind.sql(),
                        self.table_ref(i, dialect),
                        conds.join(" AND ")
                    ));
                    included.push(i);
                    if let Some(t) = self.tables.get(i) {
                        aliases.insert(t.alias.as_str());
                    }
                }
                None => {
                    let Some(i) = (0..n).find(|i| !included.contains(i)) else {
                        break;
                    };
                    out.push_str(&format!("\n    CROSS JOIN {}", self.table_ref(i, dialect)));
                    included.push(i);
                    if let Some(t) = self.tables.get(i) {
                        aliases.insert(t.alias.as_str());
                    }
                }
            }
        }
        out
    }

    /// GROUP BY expressions actually used: the explicit list plus every
    /// non aggregated output item when the select list has aggregates.
    pub fn effective_group_by(&self, dialect: &dyn Dialect) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |e: String| {
            if !e.is_empty() && !out.contains(&e) {
                out.push(e);
            }
        };
        for g in &self.group_by {
            push(self.render_expr(g, dialect));
        }
        let has_aggregates = self
            .select
            .iter()
            .any(|s| s.visible && s.aggregate.is_some());
        if has_aggregates {
            for s in self
                .select
                .iter()
                .filter(|s| s.visible && s.aggregate.is_none())
            {
                push(self.render_expr(&s.expr, dialect));
            }
        }
        out
    }

    /// Generates the SELECT statement for `dialect` (empty when the model
    /// has no tables).
    pub fn to_sql(&self, dialect: &dyn Dialect) -> String {
        if self.tables.is_empty() {
            return String::new();
        }
        let items: Vec<String> = self
            .select
            .iter()
            .filter(|s| s.visible && (!s.expr.trim().is_empty() || s.aggregate.is_some()))
            .map(|s| {
                let e = self.render_expr(&s.expr, dialect);
                let e = match s.aggregate {
                    Some(a) => a.apply(&e),
                    None => e,
                };
                if s.alias.trim().is_empty() {
                    e
                } else {
                    format!("{e} AS {}", dialect.quote_ident(s.alias.trim()))
                }
            })
            .collect();
        let kind = dialect.kind();
        let mut sql = String::from("SELECT");
        if self.distinct {
            sql.push_str(" DISTINCT");
        }
        if let (DbKind::SqlServer, Some(n)) = (kind, self.limit) {
            sql.push_str(&format!(" TOP {n}"));
        }
        match items.as_slice() {
            [] => sql.push_str(" *"),
            [one] => {
                sql.push(' ');
                sql.push_str(one);
            }
            many => {
                sql.push_str("\n    ");
                sql.push_str(&many.join(",\n    "));
            }
        }
        sql.push('\n');
        sql.push_str(&self.render_from(dialect));
        if let Some(w) = self.render_conditions(&self.conditions, dialect) {
            sql.push_str("\nWHERE ");
            sql.push_str(&w);
        }
        let group = self.effective_group_by(dialect);
        if !group.is_empty() {
            sql.push_str("\nGROUP BY ");
            sql.push_str(&group.join(", "));
        }
        if let Some(h) = self.render_conditions(&self.having, dialect) {
            sql.push_str("\nHAVING ");
            sql.push_str(&h);
        }
        let order: Vec<String> = self
            .order_by
            .iter()
            .filter(|(e, _)| !e.trim().is_empty())
            .map(|(e, desc)| {
                format!(
                    "{}{}",
                    self.render_expr(e, dialect),
                    if *desc { " DESC" } else { "" }
                )
            })
            .collect();
        if !order.is_empty() {
            sql.push_str("\nORDER BY ");
            sql.push_str(&order.join(", "));
        }
        if let Some(n) = self.limit {
            match kind {
                DbKind::SqlServer => {}
                DbKind::Generic | DbKind::Oracle => return dialect.limit_query(&sql, n, 0),
                DbKind::Postgres
                | DbKind::CockroachDb
                | DbKind::MySql
                | DbKind::MariaDb
                | DbKind::Sqlite
                | DbKind::ClickHouse
                | DbKind::Cassandra => sql.push_str(&format!("\nLIMIT {n}")),
                // The visual builder is not offered for these.
                DbKind::Redis | DbKind::MongoDb | DbKind::Elasticsearch | DbKind::Neo4j => {}
            }
        }
        sql
    }
}

/// True when `sql` contains a logical operator (`AND`, `OR`, `XOR`, `||`,
/// `&&`) outside parentheses, string literals and quoted names, i.e. when
/// it must be parenthesized before being combined with other conditions.
fn has_top_level_logic(sql: &str) -> bool {
    let b = sql.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' | b'"' | b'`' => quote = Some(c),
            b'[' => quote = Some(b']'),
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b'|' | b'&' if depth == 0 && b.get(i + 1) == Some(&c) => return true,
            _ if depth == 0 && c.is_ascii_alphabetic() => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80)
                {
                    i += 1;
                }
                let word_start = start == 0
                    || !(b[start - 1].is_ascii_alphanumeric()
                        || b[start - 1] == b'_'
                        || b[start - 1] == b'.'
                        || b[start - 1] >= 0x80);
                let word = &sql[start..i];
                if word_start
                    && (word.eq_ignore_ascii_case("and")
                        || word.eq_ignore_ascii_case("or")
                        || word.eq_ignore_ascii_case("xor"))
                {
                    return true;
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::model::tests::{customers, order_items, orders};
    use super::super::model::*;
    use ternarygrid_core::ddl::DdlGenerator;
    use ternarygrid_core::dialect::{GenericDialect, SqlSyntax};
    use ternarygrid_core::{DbKind, Dialect};

    /// Dialect wrapper reporting another product.
    pub(crate) struct KindDialect(DbKind, GenericDialect, SqlSyntax);
    impl Dialect for KindDialect {
        fn kind(&self) -> DbKind {
            self.0
        }
        fn syntax(&self) -> &SqlSyntax {
            &self.2
        }
        fn ddl(&self) -> &dyn DdlGenerator {
            self.1.ddl()
        }
    }
    pub(crate) fn dialect(kind: DbKind) -> KindDialect {
        let mut syntax = SqlSyntax::ansi();
        if matches!(kind, DbKind::MySql | DbKind::MariaDb) {
            syntax.identifier_quotes = &[('`', '`')];
        }
        KindDialect(kind, GenericDialect::new(), syntax)
    }

    fn model() -> QueryModel {
        let mut m = QueryModel::default();
        m.add_table(&orders());
        m.add_table(&customers());
        m
    }

    #[test]
    fn empty_and_star() {
        let d = dialect(DbKind::Postgres);
        assert_eq!(QueryModel::default().to_sql(&d), "");
        let m = model();
        assert_eq!(
            m.to_sql(&d),
            "SELECT *\nFROM shop.orders o\n    INNER JOIN shop.customers c ON o.customer_id = c.id"
        );
    }

    #[test]
    fn full_query_with_quoting() {
        let d = dialect(DbKind::Postgres);
        let mut m = model();
        m.set_column_selected("o", "order", true); // reserved word -> quoted
        m.set_column_selected("c", "name", true);
        m.select[1].alias = "customer name".into();
        m.conditions.push(QbCondition::new("o.total", ">", "100"));
        m.conditions.push(QbCondition {
            connector: Connector::Or,
            ..QbCondition::new("c.region", "IN", "'EU', 'US'")
        });
        m.conditions
            .push(QbCondition::new("c.name", "IS NOT NULL", "ignored"));
        m.conditions.push(QbCondition::new("o.total", "=", "")); // incomplete: skipped
        m.order_by.push(("o.total".into(), true));
        m.order_by.push(("c.name".into(), false));
        m.distinct = true;
        m.limit = Some(50);
        let sql = m.to_sql(&d);
        assert_eq!(
            sql,
            "SELECT DISTINCT\n    o.\"order\",\n    c.name AS \"customer name\"\nFROM shop.orders o\n    INNER JOIN shop.customers c ON o.customer_id = c.id\nWHERE (o.total > 100\n    OR c.region IN ('EU', 'US'))\n    AND c.name IS NOT NULL\nORDER BY o.total DESC, c.name\nLIMIT 50"
        );
    }

    fn where_clause(m: &QueryModel) -> String {
        let sql = m.to_sql(&dialect(DbKind::Postgres));
        sql.split_once("\nWHERE ")
            .map(|(_, w)| w.to_string())
            .unwrap_or_default()
    }

    fn cond(connector: Connector, expr: &str, op: &str, value: &str) -> QbCondition {
        QbCondition {
            connector,
            ..QbCondition::new(expr, op, value)
        }
    }

    #[test]
    fn conditions_combine_left_to_right() {
        let mut m = model();
        // a OR b AND c reads (a OR b) AND c
        m.conditions = vec![
            cond(Connector::And, "o.total", ">", "1"),
            cond(Connector::Or, "o.total", "<", "0"),
            cond(Connector::And, "c.name", "IS NOT NULL", ""),
        ];
        assert_eq!(
            where_clause(&m),
            "(o.total > 1\n    OR o.total < 0)\n    AND c.name IS NOT NULL"
        );
        // a AND b OR c reads (a AND b) OR c
        m.conditions[1].connector = Connector::And;
        m.conditions[2].connector = Connector::Or;
        assert_eq!(
            where_clause(&m),
            "(o.total > 1\n    AND o.total < 0)\n    OR c.name IS NOT NULL"
        );
        // a OR b AND c OR d: every change of connector closes a group
        m.conditions[1].connector = Connector::Or;
        m.conditions[2].connector = Connector::And;
        m.conditions
            .push(cond(Connector::Or, "c.region", "=", "'EU'"));
        assert_eq!(
            where_clause(&m),
            "((o.total > 1\n    OR o.total < 0)\n    AND c.name IS NOT NULL)\n    OR c.region = 'EU'"
        );
        // the same connector throughout needs no parentheses
        for c in &mut m.conditions {
            c.connector = Connector::And;
        }
        assert_eq!(
            where_clause(&m),
            "o.total > 1\n    AND o.total < 0\n    AND c.name IS NOT NULL\n    AND c.region = 'EU'"
        );
        // the connector of a skipped (incomplete) first condition is ignored
        m.conditions = vec![
            cond(Connector::And, "o.total", "=", ""),
            cond(Connector::Or, "o.total", ">", "1"),
            cond(Connector::And, "c.region", "=", "'EU'"),
        ];
        assert_eq!(where_clause(&m), "o.total > 1\n    AND c.region = 'EU'");
    }

    #[test]
    fn conditions_with_their_own_or_are_parenthesized() {
        let mut m = model();
        m.conditions = vec![
            cond(Connector::And, "c.region = 'EU' OR c.region = 'US'", "", ""),
            cond(Connector::And, "o.total", ">", "100"),
        ];
        assert_eq!(
            where_clause(&m),
            "(c.region = 'EU' OR c.region = 'US')\n    AND o.total > 100"
        );
        // alone it needs no parentheses
        m.conditions.truncate(1);
        assert_eq!(where_clause(&m), "c.region = 'EU' OR c.region = 'US'");
        // AND / OR inside literals, quoted names, parentheses or words do not count
        m.conditions = vec![
            cond(Connector::And, "c.name", "=", "'tom and jerry'"),
            cond(Connector::Or, "c.region", "IN", "('a', 'b') "),
            cond(Connector::Or, "coalesce(c.name, 'x') or_else", "", ""),
            cond(Connector::Or, "\"brand or\" = 1", "", ""),
        ];
        assert_eq!(
            where_clause(&m),
            "c.name = 'tom and jerry'\n    OR c.region IN ('a', 'b')\n    OR coalesce(c.name, 'x') or_else\n    OR \"brand or\" = 1"
        );
        assert!(super::has_top_level_logic("a || b"));
        assert!(super::has_top_level_logic("a XOR b"));
        assert!(super::has_top_level_logic("x BETWEEN 1 AND 2"));
        assert!(!super::has_top_level_logic("o.and_flag = 1"));
    }

    #[test]
    fn in_without_a_value_is_skipped() {
        let mut m = model();
        m.conditions = vec![
            QbCondition::new("c.region", "IN", ""),
            QbCondition::new("c.region", "NOT IN", "  "),
        ];
        let sql = m.to_sql(&dialect(DbKind::Postgres));
        assert!(!sql.contains("IN ()"), "{sql}");
        assert!(!sql.contains("WHERE"), "{sql}");
        m.conditions[0].value = "'EU'".into();
        assert!(
            m.to_sql(&dialect(DbKind::Postgres))
                .ends_with("WHERE c.region IN ('EU')")
        );
    }

    #[test]
    fn full_join_is_flagged_for_mysql_and_mariadb() {
        let mut m = model();
        m.joins[0].kind = JoinKind::Full;
        let pg = m.to_sql(&dialect(DbKind::Postgres));
        assert!(
            pg.contains("\n    FULL JOIN shop.customers c ON o.customer_id = c.id"),
            "{pg}"
        );
        for (kind, name) in [(DbKind::MySql, "MySQL"), (DbKind::MariaDb, "MariaDB")] {
            let sql = m.to_sql(&dialect(kind));
            assert!(
                sql.contains(&format!(
                    "\n    FULL JOIN /* FULL JOIN is not supported by {name}: choose another join type */ shop.customers c ON"
                )),
                "{sql}"
            );
        }
        m.joins[0].kind = JoinKind::Left;
        assert!(!m.to_sql(&dialect(DbKind::MySql)).contains("/*"));
    }

    #[test]
    fn group_by_is_derived_from_aggregates() {
        let d = dialect(DbKind::Postgres);
        let mut m = model();
        m.set_column_selected("c", "region", true);
        m.select.push(QbSelectItem {
            aggregate: Some(Aggregate::Sum),
            alias: "revenue".into(),
            ..QbSelectItem::new("o.total")
        });
        m.select.push(QbSelectItem {
            aggregate: Some(Aggregate::Count),
            ..QbSelectItem::new("")
        });
        m.select.push(QbSelectItem {
            aggregate: Some(Aggregate::CountDistinct),
            ..QbSelectItem::new("o.customer_id")
        });
        m.having.push(QbCondition::new("SUM(o.total)", ">", "1000"));
        let sql = m.to_sql(&d);
        assert!(sql.contains("SUM(o.total) AS revenue"), "{sql}");
        assert!(sql.contains("COUNT(*)"), "{sql}");
        assert!(sql.contains("COUNT(DISTINCT o.customer_id)"), "{sql}");
        assert!(
            sql.contains("\nGROUP BY c.region\nHAVING SUM(o.total) > 1000"),
            "{sql}"
        );
        // explicit group by is kept and merged without duplicates
        m.group_by.push("c.region".into());
        m.group_by.push("c.name".into());
        assert_eq!(
            m.effective_group_by(&d),
            vec!["c.region".to_string(), "c.name".to_string()]
        );
        // hidden items are not output and not grouped
        m.select[0].visible = false;
        assert!(!m.to_sql(&d).contains("    c.region,"));
    }

    #[test]
    fn join_kinds_orientation_and_cross_join() {
        let d = dialect(DbKind::Postgres);
        let mut m = QueryModel::default();
        m.add_table(&customers());
        m.add_table(&orders());
        m.joins[0].kind = JoinKind::Left; // o LEFT JOIN c
        let sql = m.to_sql(&d);
        // customers come first, so the join is written from the other side
        assert!(
            sql.contains("RIGHT JOIN shop.orders o ON o.customer_id = c.id"),
            "{sql}"
        );
        m.add_table(&order_items());
        let sql = m.to_sql(&d);
        assert!(
            sql.contains("INNER JOIN shop.order_items oi ON oi.order_id = o.id"),
            "{sql}"
        );
        let mut unrelated = ternarygrid_core::ddl::TableDef::new(None, "regions");
        unrelated
            .columns
            .push(ternarygrid_core::ddl::ColumnDef::new("id", "int"));
        m.add_table(&unrelated);
        assert!(m.to_sql(&d).contains("CROSS JOIN regions r"));
        // multi column join conditions are combined
        m.add_join(QbJoin {
            left_alias: "oi".into(),
            left_column: "qty".into(),
            right_alias: "o".into(),
            right_column: "total".into(),
            kind: JoinKind::Inner,
        });
        assert!(
            m.to_sql(&d)
                .contains("ON oi.order_id = o.id AND oi.qty = o.total")
        );
    }

    #[test]
    fn limit_per_dialect() {
        let mut m = model();
        m.limit = Some(10);
        assert!(m.to_sql(&dialect(DbKind::MySql)).ends_with("\nLIMIT 10"));
        assert!(
            m.to_sql(&dialect(DbKind::MySql))
                .contains("FROM shop.orders o")
        );
        let ss = m.to_sql(&dialect(DbKind::SqlServer));
        assert!(ss.starts_with("SELECT TOP 10 *"), "{ss}");
        assert!(!ss.contains("LIMIT"));
        let generic = m.to_sql(&dialect(DbKind::Generic));
        assert!(generic.starts_with("SELECT * FROM (SELECT *"), "{generic}");
        // identifiers needing quotes use the dialect's quote characters
        m.set_column_selected("o", "order", true);
        assert!(m.to_sql(&dialect(DbKind::MySql)).contains("o.`order`"));
    }
}
