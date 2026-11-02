//! Query manager (FR-S7): executed statements in `history.db` (SQLite).
//!
//! Text search uses an FTS5 trigram index (substring matching, case
//! insensitive) when the bundled SQLite supports it and every search term has
//! at least three characters; otherwise `LIKE` is used. Statements are
//! passed through [`redact_sql`] before they are stored so passwords in
//! `CREATE USER ... PASSWORD '...'` and similar statements never reach the disk.
//! The store is thread-safe (`Send + Sync`).

use parking_lot::Mutex;
use regex::Regex;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, Ordering};
use ternarygrid_core::{DbError, DbResult};

const SCHEMA_VERSION: i64 = 1;
/// Retention is enforced automatically every this many inserts.
const RETENTION_EVERY: u32 = 200;

/// Outcome of an executed statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QueryStatus {
    /// Completed successfully.
    #[default]
    Ok,
    /// Failed with an error.
    Error,
    /// Cancelled by the user.
    Cancelled,
}

impl QueryStatus {
    fn code(self) -> i64 {
        match self {
            QueryStatus::Ok => 0,
            QueryStatus::Error => 1,
            QueryStatus::Cancelled => 2,
        }
    }

    fn from_code(c: i64) -> QueryStatus {
        match c {
            1 => QueryStatus::Error,
            2 => QueryStatus::Cancelled,
            _ => QueryStatus::Ok,
        }
    }
}

/// One executed statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HistoryEntry {
    /// Row id (ignored by [`HistoryStore::add`]).
    pub id: i64,
    /// Start time, unix milliseconds.
    pub timestamp_ms: i64,
    /// Id of the connection the statement ran on.
    pub connection_id: Option<String>,
    /// Name of that connection at execution time.
    pub connection_name: Option<String>,
    /// Active database.
    pub database: Option<String>,
    /// Active schema.
    pub schema: Option<String>,
    /// Statement text (passwords redacted).
    pub sql: String,
    /// Execution time.
    pub duration_ms: Option<u64>,
    /// Rows returned or affected.
    pub rows: Option<u64>,
    /// Outcome.
    pub status: QueryStatus,
    /// Error text for failed statements.
    pub error_message: Option<String>,
}

impl HistoryEntry {
    /// An entry for `sql` stamped with the current time.
    pub fn new(sql: impl Into<String>, status: QueryStatus) -> HistoryEntry {
        HistoryEntry {
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            sql: sql.into(),
            status,
            ..Default::default()
        }
    }
}

/// Search criteria; empty fields do not filter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryQuery {
    /// Words that must all occur in the statement (case-insensitive substring match).
    pub text: Option<String>,
    /// Only statements of this connection.
    pub connection_id: Option<String>,
    /// Inclusive lower bound, unix ms.
    pub since: Option<i64>,
    /// Exclusive upper bound, unix ms.
    pub until: Option<i64>,
    /// Only statements with this outcome.
    pub status: Option<QueryStatus>,
    /// Maximum rows; 0 = 1000.
    pub limit: usize,
    /// Rows to skip (paging).
    pub offset: usize,
}

/// How much history to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetentionPolicy {
    /// Keep at most this many entries (newest win).
    pub max_entries: Option<u64>,
    /// Delete entries older than this many days.
    pub max_age_days: Option<u32>,
}

/// A keyword followed by a password value (parsed by [`value_len`]). The
/// `bare` alternatives also take an unquoted value (Oracle's
/// `IDENTIFIED BY tiger`).
static REDACT_KEYWORD: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b(?:set\s+password(?:\s+for\s+\S+)?|password|secret|(?P<bare>identified(?:\s+with\s+\S+)?\s+(?:by|as)(?:\s+(?:password|values))?))(?:\s*=\s*|\s+)"#,
    )
    .ok()
});

/// Oracle / MySQL: `IDENTIFIED BY new REPLACE old` (right after the new value).
static REDACT_REPLACE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)^\s+replace\s+").ok());

static REDACT_KV: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)\b(password|pwd)(\s*=\s*)([^\s';]+)").ok());

/// MongoDB / JSON documents: `pwd: "x"`, `"password": 'x'`.
static REDACT_JSON: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(["']?\b(?:password|passwd|pwd)["']?\s*:\s*)(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')"#,
    )
    .ok()
});

/// Redis: `AUTH [user] pass`, `HELLO 3 AUTH user pass`, `CONFIG SET requirepass|masterauth pass`.
static REDACT_REDIS: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?im)(^[ \t]*auth[ \t]+|^[ \t]*hello[ \t]+\d+[ \t]+auth[ \t]+\S+[ \t]+|\b(?:requirepass|masterauth)[ \t]+)(\S[^\r\n]*)").ok()
});

/// Length of the string literal at the start of `s`: `'…'` (with `''`),
/// `N'…'`, PostgreSQL `E'…'` (backslash escapes), `"…"` or `$tag$…$tag$`
/// (any lines). An unterminated literal runs to the end of `s`.
fn literal_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let (start, backslash) = match b {
        [b'E' | b'e', b'\'', ..] => (1, true),
        [b'N' | b'n', b'\'', ..] => (1, false),
        [b'\'' | b'"', ..] => (0, false),
        [b'$', ..] => {
            let tag_end = 1 + b[1..]
                .iter()
                .position(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))?;
            if b[tag_end] != b'$' || b.get(1).is_some_and(u8::is_ascii_digit) {
                return None; // `$1` is a parameter
            }
            let tag = &s[..=tag_end];
            return Some(match s[tag.len()..].find(tag) {
                Some(i) => 2 * tag.len() + i,
                None => s.len(),
            });
        }
        _ => return None,
    };
    let quote = b[start];
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' if backslash => i += 2,
            c if c == quote => {
                if b.get(i + 1) == Some(&quote) {
                    i += 2;
                } else {
                    return Some(i + 1);
                }
            }
            _ => i += 1,
        }
    }
    Some(s.len())
}

/// Length of the password value at the start of `s` and whether it was
/// unquoted: a literal ([`literal_len`]), MySQL's `PASSWORD('…')`, or, when
/// `bare` is allowed, an unquoted word.
fn value_len(s: &str, bare: bool) -> Option<(usize, bool)> {
    if let Some(n) = literal_len(s) {
        return Some((n, false));
    }
    if s.len() >= 8 && s.as_bytes()[..8].eq_ignore_ascii_case(b"password") {
        let rest = &s[8..];
        let open = rest.len() - rest.trim_start().len();
        if rest[open..].starts_with('(') {
            let inner = &rest[open + 1..];
            let pad = inner.len() - inner.trim_start().len();
            let lit = literal_len(&inner[pad..])?;
            let after = &inner[pad + lit..];
            let close = after.len() - after.trim_start().len();
            let end = 8 + open + 1 + pad + lit + close;
            let closed = after[close..].starts_with(')');
            return Some((if closed { end + 1 } else { end }, false));
        }
    }
    if !bare {
        return None;
    }
    let n = s
        .find(|c: char| c.is_whitespace() || matches!(c, ';' | ',' | '(' | ')' | '\'' | '"'))
        .unwrap_or(s.len());
    // MySQL `IDENTIFIED BY RANDOM PASSWORD` names no password.
    (n > 0 && !s[..n].eq_ignore_ascii_case("random")).then_some((n, true))
}

/// Masks the values after the password keywords; `None` when there is none.
fn redact_keyword_values(sql: &str) -> Option<String> {
    let re = REDACT_KEYWORD.as_ref()?;
    let mask = |out: &mut String, bare: bool| out.push_str(if bare { "***" } else { "'***'" });
    let mut out = String::new();
    let (mut copied, mut pos) = (0, 0);
    while let Some(caps) = re.captures_at(sql, pos) {
        let m = caps.get(0)?;
        let bare = caps.name("bare").is_some();
        let Some((len, unquoted)) = value_len(&sql[m.end()..], bare) else {
            pos = m.end();
            continue;
        };
        out.push_str(&sql[copied..m.end()]);
        mask(&mut out, unquoted);
        let mut end = m.end() + len;
        if bare
            && let Some(r) = REDACT_REPLACE.as_ref().and_then(|r| r.find(&sql[end..]))
            && let Some((old, unquoted)) = value_len(&sql[end + r.end()..], true)
        {
            out.push_str(&sql[end..end + r.end()]);
            mask(&mut out, unquoted);
            end += r.end() + old;
        }
        copied = end;
        pos = end;
    }
    if copied == 0 {
        return None;
    }
    out.push_str(&sql[copied..]);
    Some(out)
}

/// Masks passwords in SQL text: `PASSWORD 'x'`, `PASSWORD = 'x'` (also
/// `ENCRYPTED PASSWORD`, `E'…'`, `$$…$$` and `$tag$…$tag$` values),
/// `IDENTIFIED BY 'x'` / `IDENTIFIED BY tiger` (and `REPLACE old`),
/// `IDENTIFIED WITH plugin BY 'x'`, `SET PASSWORD = PASSWORD('x')`,
/// `password=x` inside connection strings, `pwd: "x"` in MongoDB documents
/// and Redis `AUTH` / `requirepass` become `'***'` / `***`.
pub fn redact_sql(sql: &str) -> Cow<'_, str> {
    let lower = sql.to_ascii_lowercase();
    if !(lower.contains("password")
        || lower.contains("identified")
        || lower.contains("pwd")
        || lower.contains("passwd")
        || lower.contains("secret")
        || lower.contains("auth")
        || lower.contains("requirepass"))
    {
        return Cow::Borrowed(sql);
    }
    let mut out: Cow<'_, str> = Cow::Borrowed(sql);
    if let Some(masked) = redact_keyword_values(&out) {
        out = Cow::Owned(masked);
    }
    if let Some(re) = REDACT_KV.as_ref()
        && re.is_match(&out)
    {
        out = Cow::Owned(re.replace_all(&out, "$1$2***").into_owned());
    }
    if let Some(re) = REDACT_JSON.as_ref()
        && re.is_match(&out)
    {
        out = Cow::Owned(re.replace_all(&out, "$1\"***\"").into_owned());
    }
    if let Some(re) = REDACT_REDIS.as_ref()
        && re.is_match(&out)
    {
        out = Cow::Owned(re.replace_all(&out, "$1***").into_owned());
    }
    out
}

fn sql_err(e: rusqlite::Error) -> DbError {
    DbError::io(format!("Query history database error: {e}"))
}

/// The file is damaged or not an SQLite database (as opposed to locked,
/// unreadable or on a full disk).
fn is_corrupt(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
    )
}

/// Thread-safe query history store.
pub struct HistoryStore {
    conn: Mutex<Connection>,
    fts: bool,
    redact: bool,
    retention: Mutex<Option<RetentionPolicy>>,
    inserts: AtomicU32,
}

impl std::fmt::Debug for HistoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryStore")
            .field("fts", &self.fts)
            .field("redact", &self.redact)
            .finish()
    }
}

impl HistoryStore {
    /// Opens (creating if needed) the history database at `path`. The file is
    /// created with mode 0600 inside a 0700 directory.
    pub fn open(path: &Path) -> DbResult<HistoryStore> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            crate::paths::ensure_private_dir(dir)?;
        }
        if !path.exists() {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            opts.open(path)
                .map_err(|e| DbError::io(format!("Cannot create {}: {e}", path.display())))?;
        }
        Self::open_sqlite(path).map_err(sql_err)
    }

    fn open_sqlite(path: &Path) -> rusqlite::Result<HistoryStore> {
        Self::init_sqlite(Connection::open(path)?)
    }

    /// Like [`HistoryStore::open`], but a damaged database file is moved
    /// aside (`history.db.corrupt-<timestamp>`) and a new one created, so a
    /// broken history never prevents TernaryGrid from starting. Returns the store
    /// and a warning when recovery happened.
    pub fn open_or_recover(path: &Path) -> DbResult<(HistoryStore, Option<String>)> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            crate::paths::ensure_private_dir(dir)?;
        }
        if !path.exists() {
            return Self::open(path).map(|s| (s, None));
        }
        // Only a damaged file is moved aside: a lock held past the busy timeout
        // or a permission problem is reported, the history is kept.
        match Self::open_sqlite(path) {
            Ok(s) => Ok((s, None)),
            Err(e) if !is_corrupt(&e) => Err(sql_err(e)),
            Err(first) => {
                let first = sql_err(first);
                let backup = crate::paths::backup_corrupt_file(path)?;
                for suffix in ["-wal", "-shm"] {
                    let side = PathBuf::from(format!("{}{suffix}", path.display()));
                    let _ = std::fs::remove_file(side);
                }
                let store = Self::open(path)?;
                Ok((
                    store,
                    Some(format!(
                        "The query history could not be opened ({}); it was moved to {} and a new one was started",
                        first.message,
                        backup.display()
                    )),
                ))
            }
        }
    }

    /// In-memory store (tests, private mode).
    pub fn open_in_memory() -> DbResult<HistoryStore> {
        Self::init(Connection::open_in_memory().map_err(sql_err)?)
    }

    fn init(conn: Connection) -> DbResult<HistoryStore> {
        Self::init_sqlite(conn).map_err(sql_err)
    }

    fn init_sqlite(conn: Connection) -> rusqlite::Result<HistoryStore> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL is not available for in-memory databases; ignore the result.
        let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0));
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS history (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts INTEGER NOT NULL,
                 connection_id TEXT,
                 connection_name TEXT,
                 database_name TEXT,
                 schema_name TEXT,
                 sql TEXT NOT NULL,
                 duration_ms INTEGER,
                 rows INTEGER,
                 status INTEGER NOT NULL DEFAULT 0,
                 error_message TEXT
             );
             CREATE INDEX IF NOT EXISTS history_ts ON history(ts);
             CREATE INDEX IF NOT EXISTS history_conn_ts ON history(connection_id, ts);",
        )?;
        let fts = conn
            .execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(sql, content='history', content_rowid='id', tokenize='trigram');
                 CREATE TRIGGER IF NOT EXISTS history_fts_ai AFTER INSERT ON history BEGIN
                     INSERT INTO history_fts(rowid, sql) VALUES (new.id, new.sql);
                 END;
                 CREATE TRIGGER IF NOT EXISTS history_fts_ad AFTER DELETE ON history BEGIN
                     INSERT INTO history_fts(history_fts, rowid, sql) VALUES ('delete', old.id, old.sql);
                 END;
                 CREATE TRIGGER IF NOT EXISTS history_fts_au AFTER UPDATE ON history BEGIN
                     INSERT INTO history_fts(history_fts, rowid, sql) VALUES ('delete', old.id, old.sql);
                     INSERT INTO history_fts(rowid, sql) VALUES (new.id, new.sql);
                 END;",
            )
            .is_ok();
        if !fts {
            tracing::info!("SQLite FTS5 trigram index unavailable; query history search uses LIKE");
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(HistoryStore {
            conn: Mutex::new(conn),
            fts,
            redact: true,
            retention: Mutex::new(None),
            inserts: AtomicU32::new(0),
        })
    }

    /// True when full-text search (FTS5 trigram) is in use.
    pub fn uses_fts(&self) -> bool {
        self.fts
    }

    /// Enables or disables password redaction before storing (default on).
    pub fn set_redaction(&mut self, enabled: bool) {
        self.redact = enabled;
    }

    /// Sets the retention policy and applies it now. It is re-applied
    /// automatically every few hundred inserts.
    pub fn set_retention(&self, policy: Option<RetentionPolicy>) -> DbResult<usize> {
        *self.retention.lock() = policy;
        match policy {
            Some(p) => self.apply_retention(&p),
            None => Ok(0),
        }
    }

    /// Records a statement; returns its id.
    pub fn add(&self, entry: &HistoryEntry) -> DbResult<i64> {
        let (sql, error) = if self.redact {
            (
                redact_sql(&entry.sql),
                entry.error_message.as_deref().map(redact_sql),
            )
        } else {
            (
                Cow::Borrowed(entry.sql.as_str()),
                entry.error_message.as_deref().map(Cow::Borrowed),
            )
        };
        let to_i64 = |v: Option<u64>| v.map(|n| i64::try_from(n).unwrap_or(i64::MAX));
        let id = {
            let conn = self.conn.lock();
            conn.execute(
                "INSERT INTO history (ts, connection_id, connection_name, database_name, schema_name, sql, duration_ms, rows, status, error_message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    entry.timestamp_ms,
                    entry.connection_id,
                    entry.connection_name,
                    entry.database,
                    entry.schema,
                    sql.as_ref(),
                    to_i64(entry.duration_ms),
                    to_i64(entry.rows),
                    entry.status.code(),
                    error.as_deref(),
                ],
            )
            .map_err(sql_err)?;
            conn.last_insert_rowid()
        };
        if self.inserts.fetch_add(1, Ordering::Relaxed) % RETENTION_EVERY == RETENTION_EVERY - 1 {
            let policy = *self.retention.lock();
            if let Some(p) = policy
                && let Err(e) = self.apply_retention(&p)
            {
                tracing::warn!("query history retention failed: {e}");
            }
        }
        Ok(id)
    }

    /// One entry by id.
    pub fn get(&self, id: i64) -> DbResult<Option<HistoryEntry>> {
        let conn = self.conn.lock();
        conn.query_row(
            &format!("{SELECT_COLUMNS} WHERE h.id = ?1"),
            [id],
            row_to_entry,
        )
        .optional()
        .map_err(sql_err)
    }

    fn where_clause(&self, q: &HistoryQuery) -> (String, Vec<SqlValue>) {
        let mut conds: Vec<String> = Vec::new();
        let mut params: Vec<SqlValue> = Vec::new();
        if let Some(text) = q.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            let terms = split_terms(text);
            if self.fts && !terms.is_empty() && terms.iter().all(|t| t.chars().count() >= 3) {
                let query = terms
                    .iter()
                    .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                conds.push(
                    "h.id IN (SELECT rowid FROM history_fts WHERE history_fts MATCH ?)".into(),
                );
                params.push(SqlValue::Text(query));
            } else {
                for t in terms {
                    conds.push("h.sql LIKE ? ESCAPE '\\'".into());
                    params.push(SqlValue::Text(format!("%{}%", escape_like(&t))));
                }
            }
        }
        if let Some(c) = &q.connection_id {
            conds.push("h.connection_id = ?".into());
            params.push(SqlValue::Text(c.clone()));
        }
        if let Some(since) = q.since {
            conds.push("h.ts >= ?".into());
            params.push(SqlValue::Integer(since));
        }
        if let Some(until) = q.until {
            conds.push("h.ts < ?".into());
            params.push(SqlValue::Integer(until));
        }
        if let Some(s) = q.status {
            conds.push("h.status = ?".into());
            params.push(SqlValue::Integer(s.code()));
        }
        let clause = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };
        (clause, params)
    }

    /// Entries matching `q`, newest first.
    pub fn search(&self, q: &HistoryQuery) -> DbResult<Vec<HistoryEntry>> {
        let (clause, mut params) = self.where_clause(q);
        let limit = if q.limit == 0 { 1000 } else { q.limit };
        params.push(SqlValue::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
        params.push(SqlValue::Integer(
            i64::try_from(q.offset).unwrap_or(i64::MAX),
        ));
        let sql =
            format!("{SELECT_COLUMNS}{clause} ORDER BY h.ts DESC, h.id DESC LIMIT ? OFFSET ?");
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
        let rows = stmt
            .query_map(params_from_iter(params), row_to_entry)
            .map_err(sql_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
    }

    /// Number of entries matching `q` (limit / offset ignored).
    pub fn count(&self, q: &HistoryQuery) -> DbResult<u64> {
        let (clause, params) = self.where_clause(q);
        let conn = self.conn.lock();
        let n: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM history h{clause}"),
                params_from_iter(params),
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Total number of entries.
    pub fn total(&self) -> DbResult<u64> {
        self.count(&HistoryQuery::default())
    }

    /// Deletes entries by id; returns how many were deleted.
    pub fn delete(&self, ids: &[i64]) -> DbResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let conn = self.conn.lock();
        conn.execute(
            &format!("DELETE FROM history WHERE id IN ({placeholders})"),
            params_from_iter(ids.iter()),
        )
        .map_err(sql_err)
    }

    /// Deletes everything older than `before` (unix ms), or everything when `None`.
    pub fn clear(&self, before: Option<i64>) -> DbResult<usize> {
        let conn = self.conn.lock();
        let n = match before {
            Some(ts) => conn.execute("DELETE FROM history WHERE ts < ?1", [ts]),
            None => conn.execute("DELETE FROM history", []),
        }
        .map_err(sql_err)?;
        Ok(n)
    }

    /// Applies a retention policy now; returns the number of deleted entries.
    pub fn apply_retention(&self, policy: &RetentionPolicy) -> DbResult<usize> {
        let mut deleted = 0;
        if let Some(days) = policy.max_age_days {
            let cutoff = chrono::Utc::now().timestamp_millis() - i64::from(days) * 86_400_000;
            deleted += self.clear(Some(cutoff))?;
        }
        if let Some(max) = policy.max_entries {
            let max = i64::try_from(max).unwrap_or(i64::MAX);
            let conn = self.conn.lock();
            deleted += conn
                .execute(
                    "DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY ts DESC, id DESC LIMIT ?1)",
                    [max],
                )
                .map_err(sql_err)?;
        }
        Ok(deleted)
    }
}

const SELECT_COLUMNS: &str = "SELECT h.id, h.ts, h.connection_id, h.connection_name, h.database_name, h.schema_name, h.sql, h.duration_ms, h.rows, h.status, h.error_message FROM history h";

fn row_to_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let to_u64 = |v: Option<i64>| v.map(|n| u64::try_from(n).unwrap_or(0));
    Ok(HistoryEntry {
        id: r.get(0)?,
        timestamp_ms: r.get(1)?,
        connection_id: r.get(2)?,
        connection_name: r.get(3)?,
        database: r.get(4)?,
        schema: r.get(5)?,
        sql: r.get(6)?,
        duration_ms: to_u64(r.get(7)?),
        rows: to_u64(r.get(8)?),
        status: QueryStatus::from_code(r.get(9)?),
        error_message: r.get(10)?,
    })
}

/// Splits search text into terms; `"quoted phrases"` stay together.
fn split_terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                if quoted && !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                quoted = !quoted;
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn escape_like(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    for c in t.chars() {
        if matches!(c, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sql: &str, conn: &str, ts: i64, status: QueryStatus) -> HistoryEntry {
        HistoryEntry {
            timestamp_ms: ts,
            connection_id: Some(conn.into()),
            connection_name: Some(format!("{conn} name")),
            database: Some("db".into()),
            schema: Some("public".into()),
            sql: sql.into(),
            duration_ms: Some(12),
            rows: Some(3),
            status,
            ..Default::default()
        }
    }

    fn seeded() -> HistoryStore {
        let h = HistoryStore::open_in_memory().unwrap();
        h.add(&entry(
            "SELECT * FROM users WHERE id = 1",
            "c1",
            1_000,
            QueryStatus::Ok,
        ))
        .unwrap();
        h.add(&entry(
            "select name from Customers",
            "c1",
            2_000,
            QueryStatus::Ok,
        ))
        .unwrap();
        h.add(&entry(
            "UPDATE users SET name = 'x_y' WHERE id = 2",
            "c2",
            3_000,
            QueryStatus::Error,
        ))
        .unwrap();
        h.add(&entry(
            "DELETE FROM orders",
            "c2",
            4_000,
            QueryStatus::Cancelled,
        ))
        .unwrap();
        h
    }

    #[test]
    fn add_get_search_count() {
        let h = seeded();
        assert!(h.uses_fts(), "bundled SQLite has FTS5 trigram");
        assert_eq!(h.total().unwrap(), 4);
        let all = h.search(&HistoryQuery::default()).unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].sql, "DELETE FROM orders", "newest first");
        let e = h.get(all[3].id).unwrap().unwrap();
        assert_eq!(e.connection_name.as_deref(), Some("c1 name"));
        assert_eq!(
            (e.duration_ms, e.rows, e.status),
            (Some(12), Some(3), QueryStatus::Ok)
        );
        assert!(h.get(9999).unwrap().is_none());

        let q = |text: &str| HistoryQuery {
            text: Some(text.into()),
            ..Default::default()
        };
        // FTS (terms >= 3 chars), case-insensitive substring.
        assert_eq!(h.search(&q("USERS")).unwrap().len(), 2);
        assert_eq!(h.search(&q("ustom")).unwrap().len(), 1);
        assert_eq!(h.search(&q("users where")).unwrap().len(), 2);
        assert_eq!(h.search(&q("\"users set\"")).unwrap().len(), 1);
        // Short terms fall back to LIKE; LIKE wildcards are escaped.
        assert_eq!(h.search(&q("id")).unwrap().len(), 2);
        assert_eq!(h.search(&q("x_y")).unwrap().len(), 1);
        assert_eq!(h.search(&q("_")).unwrap().len(), 1);
        assert_eq!(h.search(&q("%")).unwrap().len(), 0);

        let by_conn = HistoryQuery {
            connection_id: Some("c2".into()),
            ..Default::default()
        };
        assert_eq!(h.count(&by_conn).unwrap(), 2);
        let by_status = HistoryQuery {
            status: Some(QueryStatus::Error),
            ..Default::default()
        };
        assert_eq!(h.search(&by_status).unwrap()[0].status, QueryStatus::Error);
        let range = HistoryQuery {
            since: Some(2_000),
            until: Some(4_000),
            ..Default::default()
        };
        assert_eq!(h.count(&range).unwrap(), 2);
        let page = HistoryQuery {
            limit: 2,
            offset: 1,
            ..Default::default()
        };
        let p = h.search(&page).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].timestamp_ms, 3_000);
    }

    #[test]
    fn delete_clear_and_retention() {
        let h = seeded();
        let ids: Vec<i64> = h
            .search(&HistoryQuery::default())
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(h.delete(&ids[..1]).unwrap(), 1);
        assert_eq!(h.delete(&[]).unwrap(), 0);
        // FTS index follows deletions.
        assert_eq!(
            h.search(&HistoryQuery {
                text: Some("orders".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
            0
        );
        assert_eq!(h.clear(Some(2_000)).unwrap(), 1);
        assert_eq!(h.total().unwrap(), 2);
        assert_eq!(
            h.apply_retention(&RetentionPolicy {
                max_entries: Some(1),
                max_age_days: None
            })
            .unwrap(),
            1
        );
        assert_eq!(h.total().unwrap(), 1);
        // Entries are from 1970: an age limit removes them.
        assert_eq!(
            h.set_retention(Some(RetentionPolicy {
                max_entries: None,
                max_age_days: Some(30)
            }))
            .unwrap(),
            1
        );
        assert_eq!(h.total().unwrap(), 0);
        h.add(&HistoryEntry::new("select 1", QueryStatus::Ok))
            .unwrap();
        assert_eq!(h.total().unwrap(), 1, "recent entries are kept");
        assert_eq!(h.clear(None).unwrap(), 1);
    }

    #[test]
    fn automatic_retention() {
        let h = HistoryStore::open_in_memory().unwrap();
        h.set_retention(Some(RetentionPolicy {
            max_entries: Some(50),
            max_age_days: None,
        }))
        .unwrap();
        for i in 0..(RETENTION_EVERY as usize + 10) {
            h.add(&HistoryEntry::new(format!("select {i}"), QueryStatus::Ok))
                .unwrap();
        }
        let total = h.total().unwrap();
        assert!(total <= 60, "{total}");
    }

    #[test]
    fn passwords_are_redacted() {
        let cases = [
            (
                "CREATE USER bob WITH PASSWORD 'hunter2'",
                "CREATE USER bob WITH PASSWORD '***'",
            ),
            (
                "alter role x encrypted password 'a''b'",
                "alter role x encrypted password '***'",
            ),
            (
                "CREATE USER 'u'@'%' IDENTIFIED BY 'hunter2'",
                "CREATE USER 'u'@'%' IDENTIFIED BY '***'",
            ),
            (
                "ALTER USER u IDENTIFIED WITH mysql_native_password BY 'hunter2'",
                "ALTER USER u IDENTIFIED WITH mysql_native_password BY '***'",
            ),
            (
                "SET PASSWORD FOR u = PASSWORD('hunter2')",
                "SET PASSWORD FOR u = '***'",
            ),
            (
                "CREATE LOGIN l WITH PASSWORD = N'hunter2'",
                "CREATE LOGIN l WITH PASSWORD = '***'",
            ),
            (
                "select dblink_connect('host=h password=hunter2 user=u')",
                "select dblink_connect('host=h password=*** user=u')",
            ),
            (
                "CREATE USER 'u'@'%' IDENTIFIED BY \"s3cret\"",
                "CREATE USER 'u'@'%' IDENTIFIED BY '***'",
            ),
            ("select 1", "select 1"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_sql(input), expected, "{input}");
        }
        // Forms that used to slip through.
        let more = [
            // Oracle / MySQL unquoted, VALUES hashes and REPLACE of the old password.
            (
                "CREATE USER scott IDENTIFIED BY tiger DEFAULT TABLESPACE users",
                "CREATE USER scott IDENTIFIED BY *** DEFAULT TABLESPACE users",
            ),
            (
                "alter user scott identified by tiger;",
                "alter user scott identified by ***;",
            ),
            (
                "ALTER USER scott IDENTIFIED BY lion REPLACE tiger",
                "ALTER USER scott IDENTIFIED BY *** REPLACE ***",
            ),
            (
                "ALTER USER u IDENTIFIED BY 'new1' REPLACE 'old1'",
                "ALTER USER u IDENTIFIED BY '***' REPLACE '***'",
            ),
            (
                "CREATE USER x IDENTIFIED BY VALUES 'S:AB12'",
                "CREATE USER x IDENTIFIED BY VALUES '***'",
            ),
            (
                "CREATE DATABASE LINK l CONNECT TO u IDENTIFIED BY pw1 USING 'tns'",
                "CREATE DATABASE LINK l CONNECT TO u IDENTIFIED BY *** USING 'tns'",
            ),
            (
                "CREATE USER u IDENTIFIED BY RANDOM PASSWORD",
                "CREATE USER u IDENTIFIED BY RANDOM PASSWORD",
            ),
            // PostgreSQL E'' strings, dollar quoting (also multi-line), roles.
            (
                r"ALTER ROLE r PASSWORD E'it\'s ''x'' secret'",
                "ALTER ROLE r PASSWORD '***'",
            ),
            (
                "ALTER ROLE r PASSWORD $$hun\nter2$$ LOGIN",
                "ALTER ROLE r PASSWORD '***' LOGIN",
            ),
            (
                "CREATE ROLE r LOGIN PASSWORD $pw$hunter2$pw$ VALID UNTIL 'infinity'",
                "CREATE ROLE r LOGIN PASSWORD '***' VALID UNTIL 'infinity'",
            ),
            (
                "CREATE ROLE r WITH LOGIN ENCRYPTED PASSWORD 'SCRAM-SHA-256$4096:abc'",
                "CREATE ROLE r WITH LOGIN ENCRYPTED PASSWORD '***'",
            ),
            (
                "create role r encrypted password E'a\\\\b' createdb",
                "create role r encrypted password '***' createdb",
            ),
            (
                "ALTER ROLE r PASSWORD 'unterminated",
                "ALTER ROLE r PASSWORD '***'",
            ),
            // Not passwords.
            ("ALTER ROLE r PASSWORD NULL", "ALTER ROLE r PASSWORD NULL"),
            (
                "ALTER USER u PASSWORD EXPIRE",
                "ALTER USER u PASSWORD EXPIRE",
            ),
            (
                "SELECT password FROM users WHERE id = $1",
                "SELECT password FROM users WHERE id = $1",
            ),
            // MongoDB and Redis consoles.
            (
                r#"db.createUser({user: "u", pwd: "hunter2", roles: []})"#,
                r#"db.createUser({user: "u", pwd: "***", roles: []})"#,
            ),
            (r#"{"password": 'a\'b'}"#, r#"{"password": "***"}"#),
            ("AUTH default hunter2", "AUTH ***"),
            (
                "CONFIG SET requirepass hunter2",
                "CONFIG SET requirepass ***",
            ),
            ("HELLO 3 AUTH default hunter2", "HELLO 3 AUTH default ***"),
            ("SELECT author FROM books", "SELECT author FROM books"),
        ];
        for (input, expected) in more {
            assert_eq!(redact_sql(input), expected, "{input}");
        }
        let h = HistoryStore::open_in_memory().unwrap();
        let mut e = HistoryEntry::new(
            "CREATE USER bob WITH PASSWORD 'hunter2'",
            QueryStatus::Error,
        );
        e.error_message = Some("near PASSWORD 'hunter2': syntax error".into());
        let id = h.add(&e).unwrap();
        let back = h.get(id).unwrap().unwrap();
        assert!(!back.sql.contains("hunter2"));
        assert!(!back.error_message.unwrap().contains("hunter2"));
    }

    #[test]
    fn file_store_is_private_and_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join("history.db");
        {
            let h = HistoryStore::open(&path).unwrap();
            h.add(&HistoryEntry::new("select 42", QueryStatus::Ok))
                .unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let h = HistoryStore::open(&path).unwrap();
        assert_eq!(h.total().unwrap(), 1);
        assert_eq!(
            h.search(&HistoryQuery {
                text: Some("select 42".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn corrupt_database_is_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        std::fs::write(
            &path,
            b"this is not an sqlite database, just garbage bytes......",
        )
        .unwrap();
        assert!(HistoryStore::open(&path).is_err());
        let (h, warning) = HistoryStore::open_or_recover(&path).unwrap();
        assert!(warning.unwrap().contains("moved to"));
        h.add(&HistoryEntry::new("select 1", QueryStatus::Ok))
            .unwrap();
        assert_eq!(h.total().unwrap(), 1);
        let backups = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("history.db.corrupt-")
            })
            .count();
        assert_eq!(backups, 1);
        // A healthy file opens without recovery.
        drop(h);
        let (_, warning) = HistoryStore::open_or_recover(&path).unwrap();
        assert!(warning.is_none());
    }

    #[test]
    fn only_a_damaged_database_is_moved_aside() {
        // Something that cannot be opened but is not a damaged database (here a
        // directory; a lock held past the busy timeout behaves the same): an
        // error, and the history stays where it is.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), b"x").unwrap();
        assert!(HistoryStore::open_or_recover(&path).is_err());
        assert!(path.join("keep").exists());
        let moved = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
        assert!(!moved, "not treated as corruption");
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        assert!(!is_corrupt(&busy));
        let notadb = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
            None,
        );
        assert!(is_corrupt(&notadb));
    }

    #[test]
    fn concurrent_inserts() {
        let h = std::sync::Arc::new(HistoryStore::open_in_memory().unwrap());
        let threads: Vec<_> = (0..4)
            .map(|t| {
                let h = h.clone();
                std::thread::spawn(move || {
                    for i in 0..50 {
                        h.add(&HistoryEntry::new(
                            format!("select {t}, {i}"),
                            QueryStatus::Ok,
                        ))
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(h.total().unwrap(), 200);
    }
}
