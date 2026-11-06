//! Agent mode of the AI assistant: the model may run read-only queries on
//! the connection (the `run_query` tool) and answer from their results.
//!
//! A query runs only when every guard lets it through:
//! * one statement that reads (`SELECT`, `WITH … SELECT`, `VALUES`, `SHOW`,
//!   `DESCRIBE`, `EXPLAIN` without `ANALYZE`), as the SQL editor classifies it;
//! * none of the functions or clauses that act beyond reading (ending
//!   sessions, reading server files, writing files, sleeping);
//! * a session of its own, opened read-only (each driver that has the mode
//!   applies it) and made read-only again by statement for PostgreSQL /
//!   CockroachDB, MySQL / MariaDB and SQLite;
//! * at most [`MAX_ROWS`] rows, within [`TIMEOUT`].
//!
//! Statements that change data or schema go through the `execute_statement` tool
//! instead: they wait for the user's answer in the chat, and run in a session of
//! their own that commits at once. A read-only connection does not have the tool.

use crate::conn::session::SessionHandle;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use ternarygrid_ai::{ChatMessage, ChatRequest, ChatResponse, ToolCall, ToolDef, Usage};
use ternarygrid_core::{DbError, DbKind, DbResult, ExecOptions, ResultHeader, SqlSyntax, tr, trf};
use ternarygrid_sql::StatementKind;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// The tool's name.
pub const TOOL: &str = "run_query";
/// The tool for statements that change something: each waits for the user's answer.
pub const EXECUTE_TOOL: &str = "execute_statement";
/// Rows of a result the model gets.
pub const MAX_ROWS: usize = 100;
/// Time a query may take.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Time a statement the user allowed may take.
pub const EXECUTE_TIMEOUT: Duration = Duration::from_secs(300);
/// Requests of one answer (each may run queries).
pub const MAX_ROUNDS: usize = 8;
/// Characters of one value the model gets.
const MAX_VALUE_CHARS: usize = 200;

/// Functions that do more than read, refused even in a query. Matched as whole
/// names (any case, quoted or schema-qualified too, also inside string literals,
/// which some functions run as SQL): a trailing `*` matches every name starting
/// so, a trailing `(` only a call.
const DENIED: &[&str] = &[
    "pg_terminate_backend",
    "pg_cancel_backend",
    "pg_reload_conf",
    "pg_rotate_logfile*",
    "pg_switch_wal",
    "pg_switch_xlog",
    "pg_read_file",
    "pg_read_binary_file",
    "pg_ls_*",
    "pg_sleep",
    "pg_sleep_*",
    "pg_advisory_*",
    "pg_try_advisory_*",
    "lo_import",
    "lo_export",
    "dblink",
    "dblink_*",
    "set_config",
    "load_file",
    "sleep(",
    "benchmark(",
    "get_lock",
    "openrowset",
    "opendatasource",
    "openquery",
    "xp_*",
];

/// ClickHouse table functions that read files or other servers (or run programs).
const DENIED_CLICKHOUSE: &[&str] = &[
    "url(",
    "urlcluster(",
    "file(",
    "filecluster(",
    "s3(",
    "s3cluster(",
    "gcs(",
    "oss(",
    "cosn(",
    "remote(",
    "remotesecure(",
    "cluster(",
    "clusterallreplicas(",
    "hdfs(",
    "hdfscluster(",
    "mysql(",
    "postgresql(",
    "jdbc(",
    "odbc(",
    "azureblobstorage(",
    "azureblobstoragecluster(",
    "mongodb(",
    "redis(",
    "sqlite(",
    "executable(",
    "iceberg*(",
    "deltalake*(",
    "hudi*(",
    "paimon*(",
    "ytsaurus(",
    "arrowflight(",
];

/// `name` (lower case) is the denied entry `d`; `call`: it is followed by `(`.
fn denied_name(d: &str, name: &str, call: bool) -> bool {
    let (d, needs_call) = match d.strip_suffix('(') {
        Some(d) => (d, true),
        None => (d, false),
    };
    let hit = match d.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == d,
    };
    hit && (call || !needs_call)
}

/// What a statement uses that a query of the assistant may not.
#[derive(Default)]
struct Uses {
    /// A denied function.
    denied: Option<String>,
    /// The `INTO` keyword (`SELECT … INTO`, `INTO OUTFILE`).
    into: bool,
}

/// Look through `src` token by token: words in code (and in the comments MySQL runs,
/// `/*! … */`) and the words of string literals.
fn scan(src: &str, syntax: &SqlSyntax, denied: &[&[&str]], depth: usize, out: &mut Uses) {
    use ternarygrid_sql::TokenKind as K;
    let tokens = ternarygrid_sql::tokenize(src, syntax);
    let code: Vec<&ternarygrid_sql::Token> = tokens.iter().filter(|t| !t.is_trivia()).collect();
    let deny = |name: &str, call: bool, out: &mut Uses| {
        if out.denied.is_none()
            && let Some(d) = denied.iter().flat_map(|l| l.iter()).find(|d| denied_name(d, name, call))
        {
            out.denied = Some(d.trim_end_matches(['(', '*']).to_string());
        }
    };
    let word_char = |c: char| c.is_alphanumeric() || c == '_';
    for (i, t) in code.iter().enumerate() {
        let text = t.text(src);
        match t.kind {
            K::Keyword | K::Identifier | K::QuotedIdentifier => {
                let name = ternarygrid_sql::token::unquote_ident(text).to_lowercase();
                let call = code.get(i + 1).is_some_and(|n| n.kind == K::LParen);
                deny(&name, call, out);
                out.into |= t.kind != K::QuotedIdentifier && name == "into";
            }
            K::String => {
                // Some functions run a string as SQL (`query_to_xml('SELECT pg_sleep(9)', …)`).
                let mut rest = text;
                while let Some(s) = rest.find(word_char) {
                    let after = &rest[s..];
                    let len = after.find(|c: char| !word_char(c)).unwrap_or(after.len());
                    let tail = after[len..].trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '"' | '`' | ']'));
                    deny(&after[..len].to_lowercase(), tail.starts_with('('), out);
                    rest = &after[len..];
                }
            }
            _ => {}
        }
    }
    // MySQL / MariaDB run the text of `/*! … */` and `/*M! … */`.
    if depth < 4 {
        for t in tokens.iter().filter(|t| t.kind == K::BlockComment) {
            let text = t.text(src);
            if let Some(body) = text.strip_prefix("/*!").or_else(|| text.strip_prefix("/*M!")) {
                let body = body.trim_start_matches(|c: char| c.is_ascii_digit());
                scan(body.strip_suffix("*/").unwrap_or(body), syntax, denied, depth + 1, out);
            }
        }
    }
}

/// The `run_query` tool.
pub fn tool() -> ToolDef {
    ToolDef {
        name: TOOL.into(),
        description: format!(
            "Run one read-only SQL statement (SELECT, WITH … SELECT, SHOW, DESCRIBE or EXPLAIN without ANALYZE) on the user's database \
             connection and get its result as tab separated text: at most {MAX_ROWS} rows, values cut at {MAX_VALUE_CHARS} characters. \
             Statements that change data or schema are refused."
        ),
        parameters: r#"{"type":"object","properties":{"sql":{"type":"string","description":"The SQL statement, in the database's dialect"}},"required":["sql"]}"#.into(),
    }
}

/// The `execute_statement` tool.
pub fn execute_tool() -> ToolDef {
    ToolDef {
        name: EXECUTE_TOOL.into(),
        description: "Ask the user to run one SQL statement that changes data or schema (INSERT, UPDATE, DELETE, DDL) on their connection. They see the \
                      statement and decide; if they allow it, it runs and commits at once, and you get its outcome. If they decline, you are told so."
            .into(),
        parameters: r#"{"type":"object","properties":{"sql":{"type":"string","description":"The SQL statement, in the database's dialect"}},"required":["sql"]}"#.into(),
    }
}

/// What the system prompt says about the tools; `can_execute`: with `execute_statement`.
pub fn system_note(can_execute: bool) -> String {
    let changes = if can_execute {
        format!(
            "To change data or schema, call `{EXECUTE_TOOL}` with one statement: the user is asked before it runs, and it commits at once. Do it only when the \
             user asked for the change. When they decline, do not ask again in this answer."
        )
    } else {
        "This connection is read-only: statements that change data or schema cannot run.".to_string()
    };
    format!(
        "\n\n## Running queries\nYou can run read-only SQL on the user's connection with the `{TOOL}` tool. Use it whenever the question is about \
         the data (counts, values, examples, checks, what databases, schemas or tables exist) instead of guessing, then answer from the results and \
         show the query you ran. One statement per call; at most {MAX_ROWS} rows come back, so aggregate in SQL. Never try to change data or schema \
         with it. Every schema of this database is within reach, also the ones not described above. When the user asks about another database of the \
         server that you cannot reach from here, tell them to choose it in the list at the top of this panel. {changes}"
    )
}

/// One statement to ask about, with what may be dangerous in it.
pub fn check_execute(sql: &str, syntax: &SqlSyntax) -> Result<(String, Option<&'static str>), String> {
    let stmts = ternarygrid_sql::split_and_classify(sql, syntax, &Default::default());
    let [(span, info)] = stmts.as_slice() else {
        return Err(tr("Exactly one statement per query").to_string());
    };
    Ok((sql[span.start..span.end].trim().to_string(), info.danger.map(|d| d.message())))
}

/// [`check`] for a connection's language: Redis commands, MongoDB statements
/// and Elasticsearch requests may run when they only read.
pub fn check_for(sql: &str, syntax: &SqlSyntax, kind: DbKind) -> Result<String, String> {
    let text = sql.trim();
    let Some(info) = ternarygrid_sql::classify_command(text, kind.language()) else { return check(sql, syntax, kind) };
    let stmts = ternarygrid_sql::split_statements(text, syntax, &Default::default());
    if stmts.len() != 1 {
        return Err(tr("Exactly one statement per query").to_string());
    }
    if info.is_write {
        return Err(trf("Only queries can run here ({} is not one); show the statement to the user instead", &[&info.first_keyword]));
    }
    Ok(text.to_string())
}

/// `Ok(statement)` when `sql` may run, else why not.
pub fn check(sql: &str, syntax: &SqlSyntax, kind: DbKind) -> Result<String, String> {
    let stmts = ternarygrid_sql::split_and_classify(sql, syntax, &Default::default());
    let [(span, info)] = stmts.as_slice() else {
        return Err(tr("Exactly one statement per query").to_string());
    };
    let stmt = sql[span.start..span.end].trim().to_string();
    let reads = matches!(info.kind, StatementKind::Select | StatementKind::Show | StatementKind::Describe | StatementKind::Explain);
    let lower = stmt.to_lowercase();
    let analyze = info.kind == StatementKind::Explain && lower.split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == "analyze" || w == "analyse");
    if !reads || info.is_write || analyze {
        return Err(trf("Only queries can run here ({} is not one); show the statement to the user instead", &[&info.kind.label()]));
    }
    let lists: &[&[&str]] = if kind == DbKind::ClickHouse { &[DENIED, DENIED_CLICKHOUSE] } else { &[DENIED] };
    let mut uses = Uses::default();
    scan(&stmt, syntax, lists, 0, &mut uses);
    if uses.into {
        return Err(tr("SELECT … INTO writes; only queries can run here").to_string());
    }
    if let Some(d) = uses.denied {
        return Err(trf("{} does more than read; it cannot run here", &[&d]));
    }
    Ok(stmt)
}

/// Opens the agent's session (awaited at its first query).
pub type Opener = Pin<Box<dyn Future<Output = DbResult<SessionHandle>> + Send>>;

/// What the agent needs to run queries during one answer.
pub struct Agent {
    pub kind: DbKind,
    pub syntax: SqlSyntax,
    /// The schema the assistant is set to (made the session's current one).
    pub schema: Option<String>,
    opener: Option<Opener>,
    session: Option<SessionHandle>,
    /// Opens the session of the statements the user allows (`None`: a read-only connection).
    writer: Option<Opener>,
    write_session: Option<SessionHandle>,
}

impl Agent {
    pub fn new(kind: DbKind, syntax: SqlSyntax, schema: Option<String>, opener: Opener) -> Agent {
        Agent { kind, syntax, schema, opener: Some(opener), session: None, writer: None, write_session: None }
    }

    /// With the `execute_statement` tool, its session opened by `opener`.
    pub fn with_writer(mut self, opener: Opener) -> Agent {
        self.writer = Some(opener);
        self
    }

    fn can_execute(&self) -> bool {
        self.writer.is_some() || self.write_session.is_some()
    }

    /// The session of allowed statements, opened at the first: each commits at once.
    async fn write_session(&mut self) -> DbResult<SessionHandle> {
        if let Some(s) = &self.write_session {
            return Ok(s.clone());
        }
        let opener = self.writer.take().ok_or_else(|| DbError::connection(tr("The assistant's session could not be opened")))?;
        let s = opener.await?;
        s.lock().await.set_transaction_mode(ternarygrid_core::TransactionMode::Auto).await?;
        if let Some(schema) = &self.schema {
            let _ = s.lock().await.set_current_schema(schema).await;
        }
        self.write_session = Some(s.clone());
        Ok(s)
    }

    /// Run a statement the user allowed.
    async fn execute(&mut self, stmt: String) -> ToolStep {
        let session = match self.write_session().await {
            Ok(s) => s,
            Err(e) => return ToolStep::failed(stmt, e.full_text()),
        };
        let options = ExecOptions { timeout: Some(EXECUTE_TIMEOUT), ..Default::default() };
        let ex = match session.execute(&stmt, &[], &options).await {
            Ok(ex) => ex,
            Err(e) => return ToolStep::failed(stmt, e.full_text()),
        };
        match ex.header {
            // RETURNING and the like: committed, with rows to show.
            Some(ResultHeader::Rows(cols)) => match read_rows(&session, ex.token, &cols).await {
                Ok((cols, rows, more)) => ToolStep { kind: StepKind::Statement, ..ToolStep::rows(stmt, cols, rows, more) },
                Err(e) => ToolStep::failed(stmt, e.full_text()),
            },
            Some(ResultHeader::Update { rows_affected, tag, .. }) => ToolStep::executed(stmt, rows_affected, tag),
            _ => ToolStep::executed(stmt, None, None),
        }
    }

    /// The session, opened at the first query and made read-only where the
    /// database can.
    pub async fn session(&mut self) -> DbResult<SessionHandle> {
        if let Some(s) = &self.session {
            return Ok(s.clone());
        }
        let opener = self.opener.take().ok_or_else(|| DbError::connection(tr("The assistant's session could not be opened")))?;
        let s = opener.await?;
        // Each statement its own transaction, so that the read-only default applies to it.
        s.lock().await.set_transaction_mode(ternarygrid_core::TransactionMode::Auto).await?;
        let read_only = match self.kind {
            DbKind::Postgres | DbKind::CockroachDb => Some("SET default_transaction_read_only = on"),
            DbKind::MySql | DbKind::MariaDb => Some("SET SESSION TRANSACTION READ ONLY"),
            DbKind::Sqlite => Some("PRAGMA query_only = ON"),
            _ => None,
        };
        if let Some(q) = read_only {
            s.execute_update(q, &[]).await?;
        }
        if let Some(schema) = &self.schema {
            let _ = s.lock().await.set_current_schema(schema).await;
        }
        self.session = Some(s.clone());
        Ok(s)
    }

    /// Run the tool call `call`.
    pub async fn answer(&mut self, call: &ToolCall) -> ToolStep {
        let sql = call_sql(call);
        if call.name != TOOL {
            return ToolStep::refused(sql, trf("There is no tool {}", &[&call.name]));
        }
        let stmt = match check_for(&sql, &self.syntax, self.kind) {
            Ok(s) => s,
            Err(why) => return ToolStep::refused(sql, why),
        };
        let session = match self.session().await {
            Ok(s) => s,
            Err(e) => return ToolStep::failed(stmt, e.full_text()),
        };
        match run(&session, &stmt).await {
            Ok((cols, rows, more)) => ToolStep::rows(stmt, cols, rows, more),
            Err(e) => ToolStep::failed(stmt, e.full_text()),
        }
    }
}

/// Run `stmt`: the column names, at most [`MAX_ROWS`] rows as text, and
/// whether there were more.
async fn run(session: &SessionHandle, stmt: &str) -> DbResult<(Vec<String>, Vec<Vec<String>>, bool)> {
    let ex = session.execute(stmt, &[], &ExecOptions { timeout: Some(TIMEOUT), ..Default::default() }).await?;
    let Some(ResultHeader::Rows(cols)) = ex.header else { return Ok((Vec::new(), Vec::new(), false)) };
    read_rows(session, ex.token, &cols).await
}

/// The rows of a result, see [`run`].
async fn read_rows(session: &SessionHandle, token: u64, cols: &[ternarygrid_core::ColumnMeta]) -> DbResult<(Vec<String>, Vec<Vec<String>>, bool)> {
    let out = session.fetch(token, MAX_ROWS + 1).await;
    session.close_cursor_token(token).await;
    let out = out?;
    let more = out.rows.len() > MAX_ROWS;
    let rows = out
        .rows
        .into_iter()
        .take(MAX_ROWS)
        .map(|r| {
            r.iter()
                .map(|v| {
                    let t = if v.is_null() { "NULL".to_string() } else { v.to_text().replace(['\t', '\n', '\r'], " ") };
                    if t.chars().count() > MAX_VALUE_CHARS { t.chars().take(MAX_VALUE_CHARS).collect::<String>() + "…" } else { t }
                })
                .collect()
        })
        .collect();
    Ok((cols.iter().map(|c| c.name.clone()).collect(), rows, more))
}

/// What a [`ToolStep`] was.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StepKind {
    /// A read-only query.
    #[default]
    Query,
    /// A statement the user allowed.
    Statement,
    /// A statement the user did not allow.
    Declined,
}

/// One query the assistant ran (or wanted to): shown in the chat, its
/// `for_model` text sent back as the tool's result.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolStep {
    #[serde(default)]
    pub kind: StepKind,
    pub sql: String,
    /// Ran and returned this.
    pub ok: bool,
    /// "42 rows", or why it did not run.
    pub summary: String,
    /// The result as the model gets it.
    pub for_model: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl ToolStep {
    fn refused(sql: String, why: String) -> ToolStep {
        ToolStep { kind: StepKind::Query, sql, ok: false, summary: why.clone(), for_model: format!("Not run: {why}"), columns: Vec::new(), rows: Vec::new() }
    }

    fn failed(sql: String, error: String) -> ToolStep {
        ToolStep { kind: StepKind::Query, sql, ok: false, summary: error.clone(), for_model: format!("Error: {error}"), columns: Vec::new(), rows: Vec::new() }
    }

    fn rows(sql: String, columns: Vec<String>, rows: Vec<Vec<String>>, more: bool) -> ToolStep {
        let mut text = columns.join("\t");
        for r in &rows {
            text.push('\n');
            text.push_str(&r.join("\t"));
        }
        let summary = if more { trf("first {} rows", &[&rows.len()]) } else { ternarygrid_core::trn(rows.len(), "1 row", "{} rows") };
        text.push_str(&if more { format!("\n({} rows shown; there are more)", rows.len()) } else { format!("\n({} rows)", rows.len()) });
        ToolStep { kind: StepKind::Query, sql, ok: true, summary, for_model: text, columns, rows }
    }

    /// The user did not allow the statement.
    fn declined(sql: String) -> ToolStep {
        let for_model = "Not run: the user declined this statement. Do not ask again in this answer.".to_string();
        ToolStep { kind: StepKind::Declined, sql, ok: false, summary: tr("You did not allow it").to_string(), for_model, columns: Vec::new(), rows: Vec::new() }
    }

    /// Done: the rows it changed and the server's tag ("INSERT 0 3").
    fn executed(sql: String, rows: Option<u64>, tag: Option<String>) -> ToolStep {
        let summary = match rows {
            Some(n) => ternarygrid_core::trn(n as usize, "1 row changed", "{} rows changed"),
            None => tag.clone().unwrap_or_else(|| tr("done").to_string()),
        };
        let for_model = format!("Done and committed: {}", tag.unwrap_or_else(|| summary.clone()));
        ToolStep { kind: StepKind::Statement, sql, ok: true, summary, for_model, columns: Vec::new(), rows: Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg() -> SqlSyntax {
        crate::driver_registry().dialect(DbKind::Postgres).syntax().clone()
    }

    #[test]
    fn only_queries_run() {
        let s = pg();
        let check = |sql: &str, s: &SqlSyntax| check(sql, s, DbKind::Postgres);
        assert_eq!(check("SELECT count(*) FROM charges;", &s).unwrap(), "SELECT count(*) FROM charges");
        assert!(check("WITH x AS (SELECT 1) SELECT * FROM x", &s).is_ok());
        assert!(check("EXPLAIN SELECT 1", &s).is_ok());
        assert!(check("SHOW search_path", &s).is_ok());
        for refused in [
            "DELETE FROM charges",
            "UPDATE charges SET paid = true",
            "DROP TABLE charges",
            "SELECT 1; DELETE FROM charges",
            "EXPLAIN ANALYZE DELETE FROM charges",
            "EXPLAIN (ANALYZE) SELECT 1",
            "SELECT * INTO copy FROM charges",
            "SELECT pg_terminate_backend(123)",
            "SELECT pg_read_file('/etc/passwd')",
            "COPY charges TO '/tmp/x'",
            "CALL cleanup()",
            "",
        ] {
            assert!(check(refused, &s).is_err(), "{refused}");
        }
    }

    #[test]
    fn denied_names_are_whole_words() {
        let s = pg();
        let check = |sql: &str| check(sql, &s, DbKind::Postgres);
        // Names that merely contain a denied one, and words in string literals.
        for ok in [
            "SELECT exp_date, max_xp_level FROM cards",
            "SELECT 'it turned into a pumpkin' AS story",
            "SELECT * FROM notes WHERE body LIKE '%into%'",
            "SELECT sleep_minutes, pg_sleeper FROM habits",
            "SELECT sleep FROM habits",
            "SELECT 'expected' -- into\n",
        ] {
            assert!(check(ok).is_ok(), "{ok}: {:?}", check(ok));
        }
        for refused in [
            "SELECT pg_sleep(1)",
            "SELECT PG_SLEEP_FOR('1 s')",
            "SELECT pg_catalog.pg_read_file('/etc/passwd')",
            "SELECT \"pg_read_file\"('/etc/passwd')",
            "SELECT pg_ls_waldir()",
            "SELECT pg_try_advisory_lock(1)",
            "SELECT * FROM dblink_get_connections()",
            "SELECT sleep (5)",
            "SELECT sleep/**/(5)",
            "SELECT 1 FROM t /*!INTO OUTFILE '/tmp/x' */ LIMIT 1",
            "SELECT 1 /*!50000 , sleep(5) */ FROM t",
            "SELECT 1 /*M!100000 , benchmark(9, 1) */ FROM t",
            "SELECT query_to_xml('select pg_sleep(100)', true, false, '')",
            "SELECT * INTO copy FROM charges",
            "SELECT a FROM t INTO OUTFILE '/tmp/x'",
        ] {
            assert!(check(refused).is_err(), "{refused}");
        }
        let mysql = crate::driver_registry().dialect(DbKind::MySql).syntax().clone();
        assert!(super::check("SELECT * FROM master..xp_dirtree", &mysql, DbKind::MySql).is_err());
        assert!(super::check("SELECT exp_date FROM t WHERE note = 'turned into'", &mysql, DbKind::MySql).is_ok());
    }

    #[test]
    fn clickhouse_table_functions_are_refused() {
        let s = crate::driver_registry().dialect(DbKind::ClickHouse).syntax().clone();
        let check = |sql: &str| check(sql, &s, DbKind::ClickHouse);
        for refused in [
            "SELECT * FROM url('http://169.254.169.254/latest/meta-data', CSV)",
            "SELECT * FROM file('/etc/passwd', LineAsString)",
            "SELECT * FROM s3('https://bucket.s3.amazonaws.com/x.csv')",
            "SELECT * FROM remote('other:9000', system.users)",
            "SELECT * FROM remoteSecure('other:9440', db.t)",
            "SELECT * FROM mysql('h:3306', 'db', 't', 'u', 'p')",
            "SELECT * FROM postgresql('h:5432', 'db', 't', 'u', 'p')",
            "SELECT * FROM hdfs('hdfs://h:9000/x', 'TSV')",
            "SELECT * FROM jdbc('jdbc:x', 't')",
            "SELECT * FROM odbc('DSN=x', 't')",
            "SELECT * FROM azureBlobStorage('x', 'c', 'b')",
            "SELECT * FROM executable('script.sh', TSV, 'x String')",
            "SELECT * FROM icebergS3('https://x')",
            "SELECT * FROM `url`('http://x', CSV)",
        ] {
            assert!(check(refused).is_err(), "{refused}");
        }
        // Columns and tables of these names are not calls.
        for ok in ["SELECT url, file, remote, URLHierarchy(url) FROM visits", "SELECT count() FROM system.tables WHERE engine = 'MySQL'"] {
            assert!(check(ok).is_ok(), "{ok}: {:?}", check(ok));
        }
        // Other databases have no such table functions.
        assert!(super::check("SELECT url(x) FROM t", &pg(), DbKind::Postgres).is_ok());
    }

    async fn sqlite(path: std::path::PathBuf) -> DbResult<SessionHandle> {
        use ternarygrid_core::Driver;
        let mut cfg = ternarygrid_core::ConnectionConfig::new(DbKind::Sqlite, "t");
        cfg.file_path = Some(path.display().to_string());
        let s = ternarygrid_driver_sqlite::SqliteDriver::new().connect(&ternarygrid_core::ConnectParams::from_config(&cfg, None)).await?;
        Ok(SessionHandle::new(s, "t", None))
    }

    fn reply(text: &str, calls: &[&str]) -> ChatResponse {
        let tool_calls =
            calls.iter().enumerate().map(|(i, sql)| ToolCall { id: format!("c{i}"), name: TOOL.into(), arguments: serde_json::json!({ "sql": sql }).to_string() }).collect();
        ChatResponse { text: text.into(), tool_calls, usage: Usage { input_tokens: 10, output_tokens: 1, ..Default::default() }, ..Default::default() }
    }

    #[tokio::test]
    async fn the_model_queries_a_read_only_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let setup = sqlite(path.clone()).await.unwrap();
        setup.execute_update("CREATE TABLE charges (id INTEGER, paid INTEGER)", &[]).await.unwrap();
        setup.execute_update("INSERT INTO charges VALUES (1, 1), (2, 0), (3, 1)", &[]).await.unwrap();
        let syntax = crate::driver_registry().dialect(DbKind::Sqlite).syntax().clone();
        let mut agent = Agent::new(DbKind::Sqlite, syntax, None, Box::pin(sqlite(path)));
        // The model tries a write, then counts; the third request answers.
        let mut replies =
            vec![reply("", &["DELETE FROM charges"]), reply("Counting.", &["SELECT count(*) AS n FROM charges WHERE paid = 1"]), reply("2 are paid.", &[])].into_iter();
        let mut sent = Vec::new();
        let mut events = Vec::new();
        let r = converse(
            ChatRequest::new("system", vec![ChatMessage::user("How many are paid?")]),
            &mut agent,
            &CancellationToken::new(),
            |req| {
                sent.push(req);
                let r = replies.next().unwrap();
                Box::pin(async move { Ok(r) })
            },
            |e| events.push(e),
        )
        .await
        .unwrap();
        assert_eq!(r.text, "Counting.\n\n2 are paid.");
        assert_eq!(r.usage.input_tokens, 30);
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0].tools[0].name, TOOL);
        assert!(sent[0].system.contains(TOOL));
        // The refusal and the result went back to the model.
        let last = &sent[2].messages;
        assert!(last[2].content.starts_with("Not run:"), "{:?}", last[2]);
        assert_eq!(last[4].content, "n\n2\n(1 rows)");
        let steps: Vec<_> = events.iter().filter_map(|e| if let AgentEvent::Step(s) = e { Some(s) } else { None }).collect();
        assert_eq!((steps[0].ok, steps[1].ok), (false, true));
        assert_eq!(steps[1].rows, vec![vec!["2".to_string()]]);
        // Its session cannot write even what the check would let through.
        let session = agent.session().await.unwrap();
        assert!(session.execute_update("INSERT INTO charges VALUES (4, 0)", &[]).await.is_err());
    }

    fn ask_to_run(sql: &str) -> ChatResponse {
        let arguments = serde_json::json!({ "sql": sql }).to_string();
        ChatResponse { tool_calls: vec![ToolCall { id: "x".into(), name: EXECUTE_TOOL.into(), arguments }], ..Default::default() }
    }

    /// A table of 3 rows, and what the model's `first` reply leads to when each question is
    /// answered by `answer`: the requests, the steps, and the rows left.
    async fn with_a_question(first: ChatResponse, writer: bool, answer: impl Fn(Approval)) -> (Vec<ChatRequest>, Vec<ToolStep>, Vec<(String, Option<&'static str>)>, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let setup = sqlite(path.clone()).await.unwrap();
        setup.execute_update("CREATE TABLE charges (id INTEGER, paid INTEGER)", &[]).await.unwrap();
        setup.execute_update("INSERT INTO charges VALUES (1, 1), (2, 0), (3, 1)", &[]).await.unwrap();
        let syntax = crate::driver_registry().dialect(DbKind::Sqlite).syntax().clone();
        let mut agent = Agent::new(DbKind::Sqlite, syntax, None, Box::pin(sqlite(path.clone())));
        if writer {
            agent = agent.with_writer(Box::pin(sqlite(path)));
        }
        let mut replies = vec![first, reply("Done.", &[])].into_iter();
        let (mut sent, mut steps, mut asked) = (Vec::new(), Vec::new(), Vec::new());
        converse(
            ChatRequest::new("system", vec![ChatMessage::user("Delete the unpaid charge")]),
            &mut agent,
            &CancellationToken::new(),
            |req| {
                sent.push(req);
                let r = replies.next().unwrap();
                Box::pin(async move { Ok(r) })
            },
            |e| match e {
                AgentEvent::Step(s) => steps.push(s),
                AgentEvent::Approval(a) => {
                    asked.push((a.sql.clone(), a.danger));
                    answer(a);
                }
                _ => {}
            },
        )
        .await
        .unwrap();
        let left = run(&setup, "SELECT count(*) FROM charges").await.unwrap().1[0][0].clone();
        (sent, steps, asked, left)
    }

    #[tokio::test]
    async fn a_statement_runs_when_the_user_allows_it() {
        let (sent, steps, asked, left) = with_a_question(ask_to_run("DELETE FROM charges WHERE paid = 0"), true, |a| a.answer(true)).await;
        assert_eq!(asked, vec![("DELETE FROM charges WHERE paid = 0".to_string(), None)]);
        assert_eq!(left, "2");
        assert_eq!((steps[0].kind, steps[0].ok), (StepKind::Statement, true));
        assert_eq!(steps[0].summary, "1 row changed");
        assert!(sent[1].messages.last().unwrap().content.starts_with("Done and committed"), "{:?}", sent[1].messages.last());
        assert!(sent[0].tools.iter().any(|t| t.name == EXECUTE_TOOL));
    }

    #[tokio::test]
    async fn a_statement_stays_unrun_when_the_user_declines_or_never_answers() {
        for allow in [Some(false), None] {
            let (sent, steps, _, left) = with_a_question(ask_to_run("DELETE FROM charges WHERE paid = 0"), true, |a| match allow {
                Some(allow) => a.answer(allow),
                None => drop(a),
            })
            .await;
            assert_eq!(left, "3", "{allow:?}");
            assert_eq!(steps[0].kind, StepKind::Declined);
            assert!(sent[1].messages.last().unwrap().content.starts_with("Not run: the user declined"));
        }
    }

    #[tokio::test]
    async fn a_dangerous_statement_says_why_when_asking() {
        let (_, _, asked, _) = with_a_question(ask_to_run("DELETE FROM charges"), true, |a| a.answer(false)).await;
        assert_eq!(asked[0].1, Some("DELETE without WHERE removes every row"));
    }

    #[tokio::test]
    async fn only_one_statement_is_asked_about() {
        let (_, steps, asked, left) = with_a_question(ask_to_run("DELETE FROM charges; DROP TABLE charges"), true, |a| a.answer(true)).await;
        assert!(asked.is_empty(), "{asked:?}");
        assert_eq!(left, "3");
        assert!(!steps[0].ok);
    }

    #[tokio::test]
    async fn a_read_only_connection_cannot_be_asked() {
        let (sent, steps, asked, left) = with_a_question(ask_to_run("DELETE FROM charges"), false, |a| a.answer(true)).await;
        assert!(sent[0].tools.iter().all(|t| t.name != EXECUTE_TOOL));
        assert!(sent[0].system.contains("read-only"));
        assert!(asked.is_empty());
        assert_eq!(left, "3");
        assert!(!steps[0].ok);
    }

    #[tokio::test]
    async fn stopping_while_asking_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let setup = sqlite(path.clone()).await.unwrap();
        setup.execute_update("CREATE TABLE charges (id INTEGER)", &[]).await.unwrap();
        setup.execute_update("INSERT INTO charges VALUES (1)", &[]).await.unwrap();
        let syntax = crate::driver_registry().dialect(DbKind::Sqlite).syntax().clone();
        let mut agent = Agent::new(DbKind::Sqlite, syntax, None, Box::pin(sqlite(path.clone()))).with_writer(Box::pin(sqlite(path)));
        let cancel = CancellationToken::new();
        let (stop, mut kept) = (cancel.clone(), None);
        let mut replies = vec![ask_to_run("DELETE FROM charges")].into_iter();
        let r = converse(
            ChatRequest::new("system", vec![ChatMessage::user("Delete them")]),
            &mut agent,
            &cancel,
            |_| {
                let r = replies.next().unwrap();
                Box::pin(async move { Ok(r) })
            },
            |e| {
                if let AgentEvent::Approval(a) = e {
                    // Still waiting for an answer when Stop is pressed.
                    kept = Some(a);
                    stop.cancel();
                }
            },
        )
        .await;
        assert_eq!(r.unwrap_err().kind, ternarygrid_core::ErrorKind::Cancelled);
        assert_eq!(run(&setup, "SELECT count(*) FROM charges").await.unwrap().1[0][0], "1");
        drop(kept);
    }

    #[test]
    fn results_for_the_model() {
        let step = ToolStep::rows("SELECT 1".into(), vec!["n".into()], vec![vec!["1".into()], vec!["2".into()]], false);
        assert_eq!(step.for_model, "n\n1\n2\n(2 rows)");
        assert!(step.ok);
        let more = ToolStep::rows("SELECT 1".into(), vec!["n".into()], vec![vec!["1".into()]], true);
        assert!(more.for_model.ends_with("(1 rows shown; there are more)"));
    }
}

/// Answer `req`, running the queries the model asks for, in at most
/// [`MAX_ROUNDS`] requests. The text of every request goes to `on_text`
/// (with a blank line between requests), each query to `on_step`.
pub async fn converse<'a, S, F>(mut req: ChatRequest, agent: &mut Agent, cancel: &CancellationToken, mut stream: S, mut on_event: F) -> DbResult<ChatResponse>
where
    S: FnMut(ChatRequest) -> Pin<Box<dyn Future<Output = DbResult<ChatResponse>> + Send + 'a>>,
    F: FnMut(AgentEvent),
{
    let can_execute = agent.can_execute();
    req.tools = if can_execute { vec![tool(), execute_tool()] } else { vec![tool()] };
    req.system.push_str(&system_note(can_execute));
    let mut text = String::new();
    let mut usage = Usage::default();
    for round in 1..=MAX_ROUNDS {
        let mut r = stream(req.clone()).await?;
        usage.input_tokens += r.usage.input_tokens;
        usage.output_tokens += r.usage.output_tokens;
        usage.cache_read_input_tokens += r.usage.cache_read_input_tokens;
        usage.cache_creation_input_tokens += r.usage.cache_creation_input_tokens;
        text.push_str(&r.text);
        if r.tool_calls.is_empty() || round == MAX_ROUNDS {
            if !r.tool_calls.is_empty() {
                let note = format!("\n\n{}", trf("(Stopped after {} rounds of queries.)", &[&MAX_ROUNDS]));
                on_event(AgentEvent::Text(note.clone()));
                text.push_str(&note);
            }
            r.text = text;
            r.usage = usage;
            r.tool_calls.clear();
            return Ok(r);
        }
        if !text.trim().is_empty() && !text.ends_with("\n\n") {
            let sep = if text.ends_with('\n') { "\n" } else { "\n\n" };
            on_event(AgentEvent::Text(sep.to_string()));
            text.push_str(sep);
        }
        req.messages.push(ChatMessage::assistant_calls(r.text, r.tool_calls.clone(), Some(r.reasoning)));
        for call in &r.tool_calls {
            let step = if call.name == EXECUTE_TOOL && can_execute {
                match ask_and_execute(agent, call, cancel, &mut on_event).await {
                    Some(step) => step,
                    None => return Err(DbError::cancelled()),
                }
            } else {
                on_event(AgentEvent::Running(call_sql(call)));
                tokio::select! {
                    s = agent.answer(call) => s,
                    _ = cancel.cancelled() => return Err(DbError::cancelled()),
                }
            };
            req.messages.push(ChatMessage::tool_result(call.id.clone(), step.for_model.clone()));
            on_event(AgentEvent::Step(step));
        }
    }
    unreachable!("the last round returns")
}

/// A statement waiting for the user's answer: nothing runs before [`Approval::answer`], and
/// dropping it (a new question, the panel closed) declines it.
#[derive(Debug)]
pub struct Approval {
    pub sql: String,
    /// What may be dangerous in it (English; through `tr()` for display).
    pub danger: Option<&'static str>,
    reply: oneshot::Sender<bool>,
}

impl Approval {
    /// The user's answer: run it or not.
    pub fn answer(self, allow: bool) {
        let _ = self.reply.send(allow);
    }
}

/// `None`: stopped while waiting for the user.
async fn ask_and_execute<F: FnMut(AgentEvent)>(agent: &mut Agent, call: &ToolCall, cancel: &CancellationToken, on_event: &mut F) -> Option<ToolStep> {
    let sql = call_sql(call);
    let (stmt, danger) = match check_execute(&sql, &agent.syntax) {
        Ok(checked) => checked,
        Err(why) => return Some(ToolStep::refused(sql, why)),
    };
    let (reply, answer) = oneshot::channel();
    on_event(AgentEvent::Approval(Approval { sql: stmt.clone(), danger, reply }));
    let allowed = tokio::select! {
        a = answer => a.unwrap_or(false),
        _ = cancel.cancelled() => return None,
    };
    if !allowed {
        return Some(ToolStep::declined(stmt));
    }
    on_event(AgentEvent::Running(stmt.clone()));
    // Not cancelled from here: the user allowed it, and it may have started.
    Some(agent.execute(stmt).await)
}

/// Progress of [`converse`] besides the streamed text.
#[derive(Debug)]
pub enum AgentEvent {
    /// Text added between requests.
    Text(String),
    /// A query started.
    Running(String),
    /// A query finished (or was refused).
    Step(ToolStep),
    /// A statement that needs the user's answer.
    Approval(Approval),
}

fn call_sql(call: &ToolCall) -> String {
    serde_json::from_str::<serde_json::Value>(&call.arguments).ok().and_then(|v| v["sql"].as_str().map(str::to_string)).unwrap_or_default()
}
