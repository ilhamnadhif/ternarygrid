//! [`OracleSession`]: one thin-client connection. `oracledb` is a blocking
//! client whose `Connection` is `Send + Sync` (it locks internally), so every
//! call runs on a blocking thread with a shared `Arc<Connection>`.

use crate::catalog;
use crate::convert::{self, Read};
use crate::dialect::{OracleDialect, qualified, quote};
use crate::error::OraResultExt;
use async_trait::async_trait;
use oracledb::{Connection, ToDbValue};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use ternarygrid_core::admin::{KillMode, SessionList, TabularData};
use ternarygrid_core::config::SslMode;
use ternarygrid_core::ddl::{
    RoutineDef, SchemaSnapshot, SequenceDef, SnapshotOptions, TableDef, ViewDef,
};
use ternarygrid_core::dialect::Dialect;
use ternarygrid_core::driver::{ServerInfo, parse_version_number};
use ternarygrid_core::meta::*;
use ternarygrid_core::plan::{PlanNode, QueryPlan};
use ternarygrid_core::value::ColumnMeta;
use ternarygrid_core::{
    CancelHandle, ConnectParams, DbError, DbKind, DbResult, ErrorKind, ExecOptions, QueryCursor,
    ResultHeader, Row, ServerMessage, Session, TransactionMode, Value, trf,
};

/// Rows the server sends per round trip.
const FETCH_ARRAY: u32 = 500;

/// Reject characters that could break out of a TNS connect descriptor value.
fn validate_tns_value(field: &str, value: &str) -> DbResult<()> {
    if value
        .chars()
        .any(|c| matches!(c, '(' | ')' | '=' | '\r' | '\n' | '\0'))
    {
        return Err(DbError::config(format!(
            "Invalid Oracle {field}: contains forbidden TNS descriptor characters"
        )));
    }
    Ok(())
}

/// Connect descriptor for `params`: a service name (the database field) or a
/// `sid` property, over TCP or TCPS, or the `connect_descriptor` property as is.
pub(crate) fn descriptor(params: &ConnectParams) -> DbResult<String> {
    if let Some(d) = params
        .property("connect_descriptor")
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        return Ok(d.to_string());
    }
    validate_tns_value("host", &params.host)?;
    // TCPS is a listener of its own: "prefer" cannot fall back, so it means TCP.
    let tls = matches!(
        params.ssl.mode,
        SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull
    );
    let protocol = if tls { "TCPS" } else { "TCP" };
    let target = match params
        .property("sid")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(sid) => {
            validate_tns_value("SID", sid)?;
            format!("(SID={sid})")
        }
        None => {
            let service = params
                .database
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .unwrap_or("FREEPDB1");
            validate_tns_value("service name", service)?;
            format!("(SERVICE_NAME={service})")
        }
    };
    let mut security = String::new();
    if tls {
        let verify = if params.ssl.mode == SslMode::VerifyFull {
            "TRUE"
        } else {
            "FALSE"
        };
        security = format!("(SECURITY=(SSL_SERVER_DN_MATCH={verify}))");
    }
    Ok(format!(
        "(DESCRIPTION=(ADDRESS=(PROTOCOL={protocol})(HOST={})(PORT={})){security}(CONNECT_DATA={target}))",
        params.host, params.port
    ))
}

/// Token used for conservative Oracle read-only statement classification.
#[derive(Debug, PartialEq, Eq)]
enum OraTok {
    Word(String),
    Quoted(String),
    Punct(char),
    Literal,
}

/// Tokenize Oracle SQL, skipping comments, single-quoted strings, and Oracle
/// `q'...'` / `nq'...'` alternative quote literals.
fn oracle_tokens(sql: &str) -> Vec<OraTok> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && chars.get(i + 1) == Some(&'-') {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if let Some(delim_idx) = if (c == 'n' || c == 'N')
            && matches!(chars.get(i + 1), Some('q' | 'Q'))
            && chars.get(i + 2) == Some(&'\'')
            && i + 3 < chars.len()
        {
            Some(i + 3)
        } else if (c == 'q' || c == 'Q') && chars.get(i + 1) == Some(&'\'') && i + 2 < chars.len() {
            Some(i + 2)
        } else {
            None
        } {
            let open = chars[delim_idx];
            let close = match open {
                '[' => ']',
                '{' => '}',
                '(' => ')',
                '<' => '>',
                other => other,
            };
            i = delim_idx + 1;
            while i < chars.len() {
                if chars[i] == close && chars.get(i + 1) == Some(&'\'') {
                    i += 2;
                    break;
                }
                i += 1;
            }
            out.push(OraTok::Literal);
        } else if c == '\'' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    i += 1;
                    if chars.get(i) == Some(&'\'') {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            out.push(OraTok::Literal);
        } else if c == '"' {
            i += 1;
            let mut s = String::new();
            while i < chars.len() {
                if chars[i] == '"' {
                    i += 1;
                    if chars.get(i) == Some(&'"') {
                        s.push('"');
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    s.push(chars[i]);
                    i += 1;
                }
            }
            out.push(OraTok::Quoted(s));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '$' | '#'))
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            out.push(OraTok::Word(word.to_ascii_uppercase()));
        } else {
            out.push(OraTok::Punct(c));
            i += 1;
        }
    }
    out
}

/// Conservative check for statements allowed on a read-only Oracle connection.
fn is_read_only_sql(sql: &str) -> bool {
    let toks = oracle_tokens(sql);
    let mut start = 0;
    while matches!(toks.get(start), Some(OraTok::Punct('('))) {
        start += 1;
    }
    let Some(first) = toks.get(start) else {
        return true;
    };
    let rest = &toks[start..];
    let has_nextval = rest.windows(2).any(|w| {
        matches!(
            (&w[0], &w[1]),
            (OraTok::Punct('.'), OraTok::Word(n)) if n == "NEXTVAL"
        ) || matches!(
            (&w[0], &w[1]),
            (OraTok::Punct('.'), OraTok::Quoted(n)) if n == "NEXTVAL"
        )
    });
    let has_for_update = rest.windows(2).any(|w| {
        matches!(
            (&w[0], &w[1]),
            (OraTok::Word(a), OraTok::Word(b)) if a == "FOR" && b == "UPDATE"
        )
    });
    match first {
        OraTok::Word(w) if w == "SELECT" => !has_for_update && !has_nextval,
        OraTok::Word(w) if w == "WITH" => {
            if has_for_update || has_nextval {
                return false;
            }
            // Reject inline PL/SQL declarations (`WITH FUNCTION`/`WITH PROCEDURE`) and DML/DDL keywords.
            let forbidden = [
                "FUNCTION",
                "PROCEDURE",
                "INSERT",
                "UPDATE",
                "DELETE",
                "MERGE",
                "CREATE",
                "ALTER",
                "DROP",
                "TRUNCATE",
                "GRANT",
                "REVOKE",
                "BEGIN",
                "DECLARE",
                "CALL",
                "EXEC",
                "EXECUTE",
                "LOCK",
            ];
            !rest
                .iter()
                .skip(1)
                .any(|t| matches!(t, OraTok::Word(word) if forbidden.contains(&word.as_str())))
        }
        OraTok::Word(w) if w == "COMMIT" || w == "ROLLBACK" => true,
        OraTok::Word(w) if w == "SET" => {
            let words: Vec<&str> = rest
                .iter()
                .filter_map(|t| match t {
                    OraTok::Word(w) => Some(w.as_str()),
                    _ => None,
                })
                .collect();
            words == ["SET", "TRANSACTION", "READ", "ONLY"]
        }
        _ => false,
    }
}

/// Statement text as the server wants it: SQL without a trailing `;`, a
/// PL/SQL block with its final `;` (statement splitters remove it).
pub(crate) fn normalize(sql: &str) -> String {
    let mut s = sql.trim();
    // A trailing `/` line (SQL*Plus) ends a block.
    if let Some(rest) = s.strip_suffix('/')
        && rest.ends_with(['\n', '\r'])
    {
        s = rest.trim_end();
    }
    if is_plsql(s) {
        let mut s = s.to_string();
        if !s.ends_with(';') {
            s.push(';');
        }
        s
    } else {
        s.trim_end_matches(';').trim_end().to_string()
    }
}

/// The first words of a statement, upper case, comments skipped.
fn leading_words(sql: &str, n: usize) -> Vec<String> {
    let mut words = Vec::new();
    let mut rest = sql;
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("--") {
            rest = r.split_once('\n').map(|(_, r)| r).unwrap_or("");
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map(|(_, r)| r).unwrap_or("");
        } else {
            break;
        }
    }
    for w in rest
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
    {
        words.push(w.to_ascii_uppercase());
        if words.len() == n {
            break;
        }
    }
    words
}

/// PL/SQL: anonymous blocks and stored code (`CREATE [OR REPLACE]
/// [EDITIONABLE] PROCEDURE | FUNCTION | PACKAGE | TRIGGER | TYPE ...`).
pub(crate) fn is_plsql(sql: &str) -> bool {
    let w = leading_words(sql, 6);
    match w.first().map(String::as_str) {
        Some("BEGIN" | "DECLARE") => true,
        Some("CREATE") => w
            .iter()
            .skip(1)
            .find(|w| {
                !matches!(
                    w.as_str(),
                    "OR" | "REPLACE" | "EDITIONABLE" | "NONEDITIONABLE" | "EDITIONING"
                )
            })
            .is_some_and(|w| {
                matches!(
                    w.as_str(),
                    "PROCEDURE" | "FUNCTION" | "PACKAGE" | "TRIGGER" | "TYPE" | "LIBRARY"
                )
            }),
        _ => false,
    }
}

/// Stored code compiled by `sql`: (type, name) for reading its errors.
fn compiled_object(sql: &str) -> Option<(String, String)> {
    let w = leading_words(sql, 8);
    if w.first().map(String::as_str) != Some("CREATE") {
        return None;
    }
    let mut it = w.iter().skip(1).skip_while(|w| {
        matches!(
            w.as_str(),
            "OR" | "REPLACE" | "EDITIONABLE" | "NONEDITIONABLE"
        )
    });
    let mut ty = it.next()?.clone();
    let mut next = it.next()?.clone();
    if next == "BODY" {
        ty = format!("{ty} BODY");
        next = it.next()?.clone();
    }
    // `schema.name`: the words were split on the dot.
    let name = it
        .next()
        .filter(|_| sql.to_ascii_uppercase().contains(&format!("{next}.")))
        .cloned()
        .unwrap_or(next);
    Some((ty, name))
}

/// `sql` ends the transaction: COMMIT, ROLLBACK and DDL, which commits implicitly.
/// `ROLLBACK [WORK] TO [SAVEPOINT] x` keeps it open, and so do `ALTER SESSION` and
/// `ALTER SYSTEM` (not DDL: they commit nothing).
fn ends_transaction(sql: &str) -> bool {
    let w = leading_words(sql, 3);
    match w.first().map(String::as_str) {
        Some("COMMIT") => true,
        Some("ROLLBACK") => !w.iter().skip(1).any(|w| w == "TO"),
        Some("ALTER") => !matches!(w.get(1).map(String::as_str), Some("SESSION" | "SYSTEM")),
        Some("CREATE" | "DROP" | "TRUNCATE" | "GRANT" | "REVOKE" | "RENAME" | "COMMENT") => true,
        _ => false,
    }
}

/// Savepoint set before `EXPLAIN PLAN`, whose PLAN_TABLE rows join the open transaction.
const EXPLAIN_SAVEPOINT: &str = "ternarygrid_explain";

/// Statements undoing what `EXPLAIN PLAN` wrote: back to [`EXPLAIN_SAVEPOINT`], which
/// keeps the user's transaction and its pending changes as they were; when no
/// transaction was open before, the one `EXPLAIN PLAN` started ends with a rollback.
/// Never a COMMIT: that would commit the user's pending work.
fn explain_cleanup(transaction_before: bool) -> Vec<String> {
    let mut out = vec![format!("ROLLBACK TO SAVEPOINT {EXPLAIN_SAVEPOINT}")];
    if !transaction_before {
        out.push("ROLLBACK".into());
    }
    out
}

/// A transaction is open on the server (also one the session's flag does not know of:
/// `SELECT ... FOR UPDATE`, `LOCK TABLE`). Assumed open when it cannot be told.
fn transaction_open(conn: &Connection) -> bool {
    match catalog::query(
        conn,
        "SELECT DBMS_TRANSACTION.LOCAL_TRANSACTION_ID FROM dual",
        &[],
    ) {
        Ok(rows) => rows
            .first()
            .and_then(|r| r.first())
            .is_some_and(|v| !v.is_null()),
        Err(_) => true,
    }
}

/// Command tag of a statement without rows: `INSERT`, `CREATE TABLE` ...
fn tag(sql: &str) -> String {
    let w = leading_words(sql, 2);
    match w.first().map(String::as_str) {
        Some("CREATE" | "ALTER" | "DROP" | "TRUNCATE") => w.join(" "),
        Some(first) => first.to_string(),
        None => String::new(),
    }
}

struct Cancel {
    params: ConnectParams,
    /// `sid,serial#` of the session.
    target: String,
    /// Its RAC instance (`SYS_CONTEXT('USERENV', 'INSTANCE')`): the helper connection may land
    /// on another one, where `sid,serial#` names no session (or another session).
    instance: Option<String>,
    requested: AtomicBool,
}

/// `sid,serial#` of a session id as listed (`sid, serial#[, @inst]`), for comparing.
fn sid_serial(id: &str) -> String {
    id.split(',')
        .take(2)
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(",")
}

#[async_trait]
impl CancelHandle for Cancel {
    /// `ALTER SYSTEM CANCEL SQL` from a second connection (needs the ALTER
    /// SYSTEM privilege); an open cursor stops fetching in any case.
    async fn cancel(&self) -> DbResult<()> {
        self.requested.store(true, Ordering::SeqCst);
        let params = self.params.clone();
        let target = self.target.clone();
        let instance = self.instance.clone();
        tokio::task::spawn_blocking(move || {
            let conn = open(&params)?;
            let mut r = match &instance {
                Some(i) => conn
                    .execute(&format!("ALTER SYSTEM CANCEL SQL '{target}, @{i}'"), &[])
                    .db(),
                None => conn
                    .execute(&format!("ALTER SYSTEM CANCEL SQL '{target}'"), &[])
                    .db(),
            };
            // A server that does not take the instance (before 18c): without it.
            if instance.is_some()
                && r.as_ref()
                    .is_err_and(|e| e.code.as_deref() != Some("ORA-01031"))
            {
                r = conn
                    .execute(&format!("ALTER SYSTEM CANCEL SQL '{target}'"), &[])
                    .db();
            }
            // Logs the helper connection off.
            drop(conn);
            r.map(|_| ()).map_err(|e| {
                if e.code.as_deref() == Some("ORA-01031") {
                    DbError::unsupported("Cancelling a statement needs the ALTER SYSTEM privilege")
                } else {
                    e
                }
            })
        })
        .await
        .map_err(|e| DbError::internal(e.to_string()))?
    }
}

fn open(params: &ConnectParams) -> DbResult<Connection> {
    let user = params.user.as_deref().unwrap_or("");
    let password = params.password.as_ref().map(|p| p.expose()).unwrap_or("");
    let mut cfg = oracledb::Config::default()
        .set_credentials(user, password)
        .set_connect_string(&descriptor(params)?)
        .db()?
        .set_transport_connect_timeout(Some(params.connect_timeout))
        .set_program(&params.application_name)
        .db()?;
    let tls = matches!(
        params.ssl.mode,
        SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull
    );
    let set = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
    let wallet = params.property("wallet_location").filter(|d| !d.is_empty());
    // The driver takes certificates from a wallet only (a CA file or client certificate was
    // ignored without a word): a CA file becomes a wallet of its own for the connect.
    let mut ca_wallet = None;
    if let Some(dir) = wallet {
        cfg = cfg.set_wallet_location(dir);
        if let Some(pw) = params.property("wallet_password") {
            cfg = cfg.set_wallet_password(pw);
        }
        if tls && set(&params.ssl.ca_cert) {
            tracing::warn!(
                "Oracle: the wallet's certificates are trusted, the CA file is not used"
            );
        }
    } else if tls {
        if set(&params.ssl.client_cert) || set(&params.ssl.client_key) {
            return Err(DbError::config(
                "Oracle takes a client certificate from a wallet: set the wallet_location property to the wallet's directory (ewallet.pem) instead",
            ));
        }
        if let Some(ca) = params
            .ssl
            .ca_cert
            .as_deref()
            .filter(|c| !c.trim().is_empty())
        {
            let pem = std::fs::read_to_string(ca.trim()).map_err(|e| {
                DbError::config(format!("Cannot read the CA certificate {ca}: {e}"))
            })?;
            let dir = tempfile::Builder::new()
                .prefix("ternarygrid-oracle-ca")
                .tempdir()
                .map_err(|e| DbError::internal(e.to_string()))?;
            std::fs::write(dir.path().join("ewallet.pem"), pem)
                .map_err(|e| DbError::internal(e.to_string()))?;
            cfg = cfg.set_wallet_location(dir.path().display().to_string());
            ca_wallet = Some(dir);
        }
    }
    let mode = match params
        .property("role")
        .map(|r| r.trim().to_ascii_uppercase())
    {
        Some(r) if r == "SYSDBA" => oracledb::AUTH_MODE_SYSDBA,
        Some(r) if r == "SYSOPER" => oracledb::AUTH_MODE_SYSOPER,
        Some(r) if r == "SYSDG" => oracledb::AUTH_MODE_SYSDGD,
        Some(r) if r == "SYSBACKUP" => oracledb::AUTH_MODE_SYSBKP,
        Some(r) if r == "SYSKM" => oracledb::AUTH_MODE_SYSKMT,
        _ => oracledb::AUTH_MODE_DEFAULT,
    };
    cfg = cfg.set_auth_mode(mode);
    let r = oracledb::connect(cfg).db();
    drop(ca_wallet);
    r.map_err(|e| if tls { tls_hint(e, params) } else { e })
}

/// The driver checks the server certificate and its host name in every TLS mode (it has no
/// unverified TLS), against the host it connects to: say so when a certificate is refused.
fn tls_hint(e: DbError, params: &ConnectParams) -> DbError {
    let text = e.full_text();
    if text.contains("not valid for name") || text.contains("NotValidForName") {
        if params
            .tls_server_name
            .as_deref()
            .is_some_and(|n| n != params.host)
        {
            return e.with_hint("Oracle checks the certificate against the address it connects to: through an SSH tunnel or a proxy that is the local end, not the server's name. Connect to the server directly for TCPS, or use TCP inside the tunnel.");
        }
        return e.with_hint("Oracle checks the server certificate's host name in every TLS mode: connect by a name the certificate holds.");
    }
    if text.contains("UnknownIssuer") || text.contains("invalid peer certificate") {
        return e.with_hint("Oracle verifies the server certificate in every TLS mode: give the CA that issued it (CA certificate file, or a wallet in wallet_location).");
    }
    e
}

pub struct OracleSession {
    /// Taken (and logged off) by [`Session::close`].
    conn: Option<Arc<Connection>>,
    info: ServerInfo,
    dialect: Arc<OracleDialect>,
    mode: TransactionMode,
    in_tx: bool,
    closed: Arc<AtomicBool>,
    cancel: Arc<Cancel>,
    service: Option<String>,
    /// The session zone's offset (seconds east of UTC), read at connect: TIMESTAMP WITH
    /// LOCAL TIME ZONE values arrive as its wall clock.
    ltz_offset: i32,
    /// Only statements that read run on this session.
    read_only: bool,
}

impl OracleSession {
    pub async fn connect(params: &ConnectParams) -> DbResult<Self> {
        let p = params.clone();
        let (conn, info, service, ltz_offset, instance) = tokio::task::spawn_blocking(move || -> DbResult<_> {
            let conn = open(&p)?;
            let version = conn.version().db()?;
            let sid = conn.session_id().db()?;
            let serial = conn.serial_num().db()?;
            // ISO dates for implicit conversions (text typed into the grid, literals).
            conn.execute(
                "ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY-MM-DD HH24:MI:SS' NLS_TIMESTAMP_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF' \
                 NLS_TIMESTAMP_TZ_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF TZH:TZM' NLS_NUMERIC_CHARACTERS = '.,'",
                &[],
            )
            .db()?;
            if let Some(tz) = p.property("time_zone").filter(|t| !t.is_empty() && *t != "local" && *t != "utc") {
                // Unknown region names fail: the server's zone is kept.
                let _ = conn.execute(&format!("ALTER SESSION SET TIME_ZONE = '{}'", tz.replace('\'', "")), &[]);
            }
            let _ = conn.execute("BEGIN DBMS_OUTPUT.ENABLE(NULL); END;", &[]);
            let v = version.to_string();
            let info = ServerInfo { product: "Oracle Database".into(), version_number: parse_version_number(&v), version: v, session_id: Some(format!("{sid},{serial}")) };
            let service = conn.service_name().ok().map(str::to_string).filter(|s| !s.is_empty());
            let instance = conn
                .query_row("SELECT SYS_CONTEXT('USERENV', 'INSTANCE') FROM DUAL", &[])
                .ok()
                .and_then(|r| r.get::<Option<String>>(0).ok().flatten())
                .filter(|i| !i.is_empty() && i.chars().all(|c| c.is_ascii_digit()));
            let ltz_offset = conn
                .query_row("SELECT TZ_OFFSET(SESSIONTIMEZONE) FROM DUAL", &[])
                .ok()
                .and_then(|r| r.get::<Option<String>>(0).ok().flatten())
                .and_then(|t| convert::offset_seconds(&t))
                .unwrap_or_else(|| chrono::Local::now().offset().local_minus_utc());
            Ok((conn, info, service, ltz_offset, instance))
        })
        .await
        .map_err(|e| DbError::internal(e.to_string()))??;
        let cancel = Arc::new(Cancel {
            params: params.clone(),
            target: info.session_id.clone().unwrap_or_default(),
            instance,
            requested: AtomicBool::new(false),
        });
        Ok(OracleSession {
            conn: Some(Arc::new(conn)),
            info,
            dialect: Arc::new(OracleDialect::new()),
            mode: if params.auto_commit {
                TransactionMode::Auto
            } else {
                TransactionMode::Manual
            },
            in_tx: false,
            closed: Arc::new(AtomicBool::new(false)),
            cancel,
            service,
            ltz_offset,
            read_only: params.read_only,
        })
    }

    /// Run `f` with the connection on a blocking thread.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> DbResult<T> + Send + 'static,
    ) -> DbResult<T> {
        let Some(conn) = self
            .conn
            .clone()
            .filter(|_| !self.closed.load(Ordering::SeqCst))
        else {
            return Err(DbError::connection("The connection is closed"));
        };
        let closed = self.closed.clone();
        let r = tokio::task::spawn_blocking(move || f(&conn))
            .await
            .map_err(|e| DbError::internal(e.to_string()))?;
        if let Err(e) = &r
            && e.kind == ErrorKind::Connection
        {
            closed.store(true, Ordering::SeqCst);
        }
        r
    }
}

/// Lines written with DBMS_OUTPUT by the last block.
fn dbms_output(conn: &Connection) -> Vec<ServerMessage> {
    let mut out = Vec::new();
    for _ in 0..10_000 {
        let Ok(mut r) = conn.execute(
            "BEGIN DBMS_OUTPUT.GET_LINE(:1, :2); END;",
            &[&oracledb::DB_TYPE_VARCHAR, &oracledb::DB_TYPE_NUMBER],
        ) else {
            break;
        };
        let row = r.out_bind_data();
        let status: i64 = row.get::<Option<i64>>(1).ok().flatten().unwrap_or(1);
        if status != 0 {
            break;
        }
        let line: String = row
            .get::<Option<String>>(0)
            .ok()
            .flatten()
            .unwrap_or_default();
        out.push(ServerMessage::new("OUTPUT", line));
    }
    out
}

/// Compilation errors of stored code just created.
fn compile_errors(conn: &Connection, sql: &str) -> Vec<ServerMessage> {
    let Some((ty, name)) = compiled_object(sql) else {
        return Vec::new();
    };
    let Ok(rows) = catalog::query(
        conn,
        "SELECT line, position, text, attribute FROM user_errors WHERE name = :1 AND type = :2 ORDER BY sequence",
        &[&name, &ty],
    ) else {
        return Vec::new();
    };
    rows.iter()
        .map(|r| {
            let at = format!(
                "{}/{}",
                catalog::text(&r[0]).unwrap_or_default(),
                catalog::text(&r[1]).unwrap_or_default()
            );
            let severity = catalog::text(&r[3]).unwrap_or_else(|| "ERROR".into());
            let mut m = ServerMessage::new(
                severity,
                format!("{at} {}", catalog::text(&r[2]).unwrap_or_default().trim()),
            );
            m.detail = Some(format!("{ty} {name}"));
            m
        })
        .collect()
}

/// What `execute` produced.
enum Outcome {
    Rows {
        columns: Vec<ColumnMeta>,
        reads: Vec<Read>,
        cursor: Box<oracledb::Cursor>,
        /// Whether `run` started a fresh `SET TRANSACTION READ ONLY` transaction.
        started_ro_tx: bool,
    },
    Done {
        rows: u64,
        tag: String,
        messages: Vec<ServerMessage>,
        dml: bool,
        plsql: bool,
    },
}

/// Strategy for establishing `SET TRANSACTION READ ONLY` before executing a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadOnlyTxPrep {
    /// Session is not read-only.
    None,
    /// Auto-commit read-only: clear any lingering tx, set read-only (propagating errors),
    /// and roll back on early error or when the cursor completes/closes/drops.
    AutoRollbackThenSet,
    /// Manual read-only without an active tx: set read-only (propagating all errors).
    ManualStart,
    /// Manual read-only with an active tx: ensure read-only, tolerating `ORA-01453`.
    ManualContinue,
}

fn read_only_tx_prep(read_only: bool, auto_commit: bool, in_tx: bool) -> ReadOnlyTxPrep {
    if !read_only {
        ReadOnlyTxPrep::None
    } else if auto_commit {
        ReadOnlyTxPrep::AutoRollbackThenSet
    } else if !in_tx {
        ReadOnlyTxPrep::ManualStart
    } else {
        ReadOnlyTxPrep::ManualContinue
    }
}

fn is_ora_01453(err: &DbError) -> bool {
    err.code.as_deref() == Some("ORA-01453")
}

fn should_rollback_before_explain(read_only: bool, in_tx: bool) -> bool {
    read_only && !in_tx
}

fn run(
    conn: &Connection,
    sql: &str,
    params: &[Value],
    opts: &ExecOptions,
    auto_commit: bool,
    read_only: bool,
    in_tx: bool,
) -> DbResult<Outcome> {
    let binds: Vec<Box<dyn ToDbValue + Send + Sync>> = params.iter().map(convert::bind).collect();
    let refs: Vec<&dyn ToDbValue> = binds.iter().map(|b| b.as_ref() as &dyn ToDbValue).collect();
    if let Some(t) = opts.timeout {
        let _ = conn.set_call_timeout(Some(t));
    }
    let mut started_ro_tx = false;
    let result = (|| -> DbResult<Outcome> {
        let mut stmt = conn
            .statement(sql)
            .db()?
            .fetch_array_size(FETCH_ARRAY)
            .build()
            .db()?;
        if stmt.is_query() {
            match read_only_tx_prep(read_only, auto_commit, in_tx) {
                ReadOnlyTxPrep::None => {}
                ReadOnlyTxPrep::AutoRollbackThenSet => {
                    let _ = conn.rollback();
                    conn.execute("SET TRANSACTION READ ONLY", &[]).db()?;
                    started_ro_tx = true;
                }
                ReadOnlyTxPrep::ManualStart => {
                    conn.execute("SET TRANSACTION READ ONLY", &[]).db()?;
                    started_ro_tx = true;
                }
                ReadOnlyTxPrep::ManualContinue => {
                    match conn.execute("SET TRANSACTION READ ONLY", &[]).db() {
                        Ok(_) => started_ro_tx = true,
                        Err(e) if is_ora_01453(&e) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            let cursor = stmt.query(&refs).db()?;
            let (columns, reads): (Vec<ColumnMeta>, Vec<Read>) =
                cursor.columns().iter().map(convert::column).unzip();
            return Ok(Outcome::Rows {
                columns,
                reads,
                cursor: Box::new(cursor),
                started_ro_tx,
            });
        }
        let (dml, plsql, ddl) = (stmt.is_dml(), stmt.is_plsql(), stmt.is_ddl());
        if read_only && (dml || plsql || ddl || stmt.is_dml_returning()) {
            return Err(DbError::read_only(
                "The connection is read-only: statement changes data",
            ));
        }
        let result = stmt.execute(&refs).db()?;
        let rows = result.rows_affected();
        // LOCK TABLE is none of these to oracledb: in auto mode its lock was held until some
        // later DML committed, blocking the other sessions.
        let lock = leading_words(sql, 1).first().is_some_and(|w| w == "LOCK");
        if auto_commit && (dml || plsql || lock) {
            conn.commit().db()?;
        }
        let mut messages = Vec::new();
        if plsql && !sql.trim_start().to_ascii_uppercase().starts_with("CREATE") {
            messages.extend(dbms_output(conn));
        }
        if ddl || plsql {
            if let Ok(Some(w)) = conn.last_warning() {
                messages.push(ServerMessage::new(
                    "WARNING",
                    w.lines().next().unwrap_or(&w).to_string(),
                ));
            }
            if messages.iter().any(|m| m.severity == "WARNING") {
                messages.extend(compile_errors(conn, sql));
            }
        }
        Ok(Outcome::Done {
            rows,
            tag: tag(sql),
            messages,
            dml,
            plsql,
        })
    })();
    if opts.timeout.is_some() {
        let _ = conn.set_call_timeout(None);
    }
    if result.is_err() && started_ro_tx {
        let _ = conn.rollback();
    }
    result
}

/// Transaction completion owed once an [`OracleCursor`] finishes, closes, or drops.
enum EndTx {
    Commit(Arc<Connection>),
    Rollback(Arc<Connection>),
}

impl EndTx {
    fn execute(&self) {
        match self {
            Self::Commit(conn) => {
                if let Err(e) = conn.commit() {
                    tracing::warn!("committing after SELECT … FOR UPDATE: {e}");
                }
            }
            Self::Rollback(conn) => {
                if let Err(e) = conn.rollback() {
                    tracing::warn!("rolling back read-only transaction: {e}");
                }
            }
        }
    }

    async fn finish_async(self) {
        let _ = tokio::task::spawn_blocking(move || self.execute()).await;
    }

    fn finish(&mut self) {
        let action = match self {
            Self::Commit(conn) => Self::Commit(conn.clone()),
            Self::Rollback(conn) => Self::Rollback(conn.clone()),
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn_blocking(move || action.execute());
            }
            Err(_) => {
                std::thread::spawn(move || action.execute());
            }
        }
    }
}

struct OracleCursor {
    header: Option<ResultHeader>,
    cursor: Option<Box<oracledb::Cursor>>,
    reads: Arc<Vec<Read>>,
    messages: Vec<ServerMessage>,
    cancel: Arc<Cancel>,
    done: bool,
    /// The session's flag: a fetch that loses the connection sets it too.
    closed: Arc<AtomicBool>,
    /// Transaction ended once the rows are read, or the cursor closes/drops
    /// (`SELECT … FOR UPDATE` commits; read-only queries in auto mode roll back).
    end_tx: Option<EndTx>,
}

impl OracleCursor {
    async fn finish_tx_async(&mut self) {
        if let Some(end_tx) = self.end_tx.take() {
            end_tx.finish_async().await;
        }
    }

    fn finish_tx(&mut self) {
        if let Some(mut end_tx) = self.end_tx.take() {
            end_tx.finish();
        }
    }
}

impl Drop for OracleCursor {
    fn drop(&mut self) {
        self.cursor = None;
        self.finish_tx();
    }
}

#[async_trait]
impl QueryCursor for OracleCursor {
    async fn next_result(&mut self) -> DbResult<Option<ResultHeader>> {
        Ok(self.header.take())
    }

    async fn fetch(&mut self, max_rows: usize) -> DbResult<Vec<Row>> {
        if self.done {
            return Ok(Vec::new());
        }
        let Some(mut cursor) = self.cursor.take() else {
            return Ok(Vec::new());
        };
        let reads = self.reads.clone();
        let cancel = self.cancel.clone();
        let fetched = tokio::task::spawn_blocking(move || -> DbResult<_> {
            let mut rows = Vec::with_capacity(max_rows.min(4096));
            let mut done = false;
            while rows.len() < max_rows.max(1) {
                if cancel.requested.load(Ordering::SeqCst) {
                    return Err(DbError::cancelled());
                }
                match cursor.next() {
                    Some(r) => rows.push(Row::from(convert::row(&r.db()?, &reads)?)),
                    None => {
                        done = true;
                        break;
                    }
                }
            }
            Ok((cursor, rows, done))
        })
        .await
        .map_err(|e| DbError::internal(e.to_string()))?;
        let (cursor, rows, done) = match fetched {
            Ok(v) => v,
            Err(e) => {
                if e.kind == ErrorKind::Connection {
                    self.closed.store(true, Ordering::SeqCst);
                }
                self.done = true;
                self.finish_tx_async().await;
                return Err(e);
            }
        };
        self.done = done;
        if !done {
            self.cursor = Some(cursor);
        } else {
            drop(cursor);
            self.finish_tx_async().await;
        }
        Ok(rows)
    }

    async fn close(&mut self) -> DbResult<()> {
        self.cursor = None;
        self.done = true;
        self.finish_tx_async().await;
        Ok(())
    }

    fn take_messages(&mut self) -> Vec<ServerMessage> {
        std::mem::take(&mut self.messages)
    }
}

#[async_trait]
impl Session for OracleSession {
    fn kind(&self) -> DbKind {
        DbKind::Oracle
    }

    fn dialect(&self) -> Arc<dyn Dialect> {
        self.dialect.clone()
    }

    fn server_info(&self) -> &ServerInfo {
        &self.info
    }

    fn cancel_handle(&self) -> Option<Arc<dyn CancelHandle>> {
        Some(self.cancel.clone())
    }

    async fn ping(&mut self) -> DbResult<()> {
        self.blocking(|c| c.ping().db()).await.map_err(|e| {
            if e.kind == ErrorKind::Connection {
                e
            } else {
                DbError::connection(e.message)
            }
        })
    }

    async fn close(&mut self) -> DbResult<()> {
        self.closed.store(true, Ordering::SeqCst);
        // Dropping the last reference logs off (a cursor keeps none). Off the async
        // threads, and not waited for long: a dead network may not answer the logoff.
        if let Some(conn) = self.conn.take() {
            let logoff = tokio::task::spawn_blocking(move || drop(conn));
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), logoff).await;
        }
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    async fn execute(
        &mut self,
        sql: &str,
        params: &[Value],
        opts: &ExecOptions,
    ) -> DbResult<Box<dyn QueryCursor>> {
        let sql = normalize(sql);
        if sql.is_empty() {
            return Ok(Box::new(OracleCursor {
                header: None,
                cursor: None,
                reads: Arc::new(Vec::new()),
                messages: Vec::new(),
                cancel: self.cancel.clone(),
                done: true,
                closed: self.closed.clone(),
                end_tx: None,
            }));
        }
        let words = leading_words(&sql, 1);
        let first = words.first().map(String::as_str).unwrap_or("");
        if self.read_only && !is_read_only_sql(&sql) {
            let label = if first.is_empty() { "statement" } else { first };
            return Err(DbError::read_only(trf(
                "The connection is read-only: {} changes data",
                &[&label],
            )));
        }
        self.cancel.requested.store(false, Ordering::SeqCst);
        let ends_tx = ends_transaction(&sql);
        let params = params.to_vec();
        let opts = opts.clone();
        let auto = self.mode == TransactionMode::Auto;
        let read_only = self.read_only;
        let in_tx = self.in_tx;
        let text = sql.clone();
        let outcome = match self
            .blocking(move |c| run(c, &text, &params, &opts, auto, read_only, in_tx))
            .await
        {
            Ok(o) => o,
            Err(e) => {
                // Oracle commits the open transaction before a DDL statement runs, also when
                // the statement then fails (ORA-00955): nothing is left to roll back.
                if ends_tx {
                    self.in_tx = false;
                }
                return Err(e);
            }
        };
        let cancel = self.cancel.clone();
        match outcome {
            Outcome::Rows {
                columns,
                mut reads,
                cursor,
                started_ro_tx,
            } => {
                convert::in_session_zone(&mut reads, self.ltz_offset);
                // Locks held until the end of the transaction (`… FOR UPDATE`, also after WITH):
                // in auto mode committed once the rows are read (a commit before would end the
                // fetch, ORA-01002), not left to some later DML.
                let for_update = matches!(first, "SELECT" | "WITH")
                    && sql.to_ascii_uppercase().contains("FOR UPDATE");
                if (read_only && !auto && started_ro_tx) || (for_update && !auto) {
                    self.in_tx = true;
                }
                let end_tx = if read_only && auto {
                    self.conn.clone().map(EndTx::Rollback)
                } else if for_update && auto {
                    self.conn.clone().map(EndTx::Commit)
                } else {
                    None
                };
                Ok(Box::new(OracleCursor {
                    header: Some(ResultHeader::Rows(columns)),
                    cursor: Some(cursor),
                    reads: Arc::new(reads),
                    messages: Vec::new(),
                    cancel,
                    done: false,
                    closed: self.closed.clone(),
                    end_tx,
                }))
            }
            Outcome::Done {
                rows,
                tag,
                messages,
                dml,
                plsql,
            } => {
                if ends_tx {
                    self.in_tx = false;
                } else if !auto
                    && (dml
                        || plsql
                        || first == "LOCK"
                        || (first == "SET"
                            && leading_words(&sql, 2).get(1).map(String::as_str)
                                == Some("TRANSACTION")))
                {
                    // LOCK TABLE holds its lock until the end of the transaction, and
                    // SET TRANSACTION begins a transaction.
                    self.in_tx = true;
                }
                let rows_affected = if dml { Some(rows) } else { None };
                Ok(Box::new(OracleCursor {
                    header: Some(ResultHeader::Update {
                        rows_affected,
                        last_insert_id: None,
                        tag: Some(tag),
                    }),
                    cursor: None,
                    reads: Arc::new(Vec::new()),
                    messages,
                    cancel,
                    done: true,
                    closed: self.closed.clone(),
                    end_tx: None,
                }))
            }
        }
    }

    fn transaction_mode(&self) -> TransactionMode {
        self.mode
    }

    async fn set_transaction_mode(&mut self, mode: TransactionMode) -> DbResult<()> {
        if mode == TransactionMode::Auto && self.in_tx {
            self.commit().await?;
        }
        self.mode = mode;
        Ok(())
    }

    fn in_transaction(&self) -> bool {
        self.in_tx
    }

    async fn commit(&mut self) -> DbResult<()> {
        self.blocking(|c| c.commit().db()).await?;
        self.in_tx = false;
        Ok(())
    }

    async fn rollback(&mut self) -> DbResult<()> {
        self.blocking(|c| c.rollback().db()).await?;
        self.in_tx = false;
        Ok(())
    }

    async fn current_database(&mut self) -> DbResult<Option<String>> {
        Ok(self.service.clone())
    }

    async fn current_schema(&mut self) -> DbResult<Option<String>> {
        self.blocking(catalog::current_schema).await
    }

    async fn set_current_schema(&mut self, schema: &str) -> DbResult<()> {
        let sql = format!("ALTER SESSION SET CURRENT_SCHEMA = {}", quote(schema));
        self.blocking(move |c| c.execute(&sql, &[]).db().map(|_| ()))
            .await
    }

    async fn list_databases(&mut self) -> DbResult<Vec<DatabaseInfo>> {
        Ok(vec![DatabaseInfo {
            name: self.service.clone().unwrap_or_else(|| "ORACLE".into()),
            is_current: true,
            ..Default::default()
        }])
    }

    async fn list_schemas(&mut self) -> DbResult<Vec<SchemaInfo>> {
        self.blocking(catalog::list_schemas).await
    }

    async fn list_objects(
        &mut self,
        schema: &SchemaRef,
        kind: ObjectKind,
    ) -> DbResult<Vec<ObjectInfo>> {
        let schema = match schema.schema.clone() {
            Some(s) => s,
            None => self
                .blocking(catalog::current_schema)
                .await?
                .unwrap_or_default(),
        };
        self.blocking(move |c| catalog::list_objects(c, &schema, kind))
            .await
    }

    async fn list_children(
        &mut self,
        parent: &ObjectRef,
        kind: ObjectKind,
    ) -> DbResult<Vec<ObjectInfo>> {
        let parent = parent.clone();
        self.blocking(move |c| catalog::list_children(c, &parent, kind))
            .await
    }

    async fn table_definition(&mut self, table: &ObjectRef) -> DbResult<TableDef> {
        let table = table.clone();
        self.blocking(move |c| catalog::table_definition(c, &table))
            .await
    }

    async fn view_definition(&mut self, view: &ObjectRef) -> DbResult<ViewDef> {
        let view = view.clone();
        self.blocking(move |c| catalog::view_definition(c, &view))
            .await
    }

    async fn routine_definition(&mut self, routine: &ObjectRef) -> DbResult<RoutineDef> {
        let routine = routine.clone();
        self.blocking(move |c| catalog::routine_definition(c, &routine))
            .await
    }

    async fn sequence_definition(&mut self, sequence: &ObjectRef) -> DbResult<SequenceDef> {
        let sequence = sequence.clone();
        self.blocking(move |c| catalog::sequence_definition(c, &sequence))
            .await
    }

    /// DBMS_METADATA when allowed (it needs SELECT_CATALOG_ROLE for other
    /// users' objects), else DDL generated from the dictionary.
    async fn object_ddl(&mut self, obj: &ObjectRef) -> DbResult<String> {
        let o = obj.clone();
        if let Ok(ddl) = self.blocking(move |c| catalog::metadata_ddl(c, &o)).await
            && !ddl.trim().is_empty()
        {
            return Ok(ddl);
        }
        let dialect = self.dialect.clone();
        let ddl = dialect.ddl();
        Ok(match obj.kind {
            ObjectKind::Table => {
                let t = self.table_definition(obj).await?;
                ddl.create_table(&t, true)
                    .iter()
                    .map(|s| format!("{s};"))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
            ObjectKind::View | ObjectKind::MaterializedView => {
                let v = self.view_definition(obj).await?;
                ddl.create_view(&v, true).join(";\n") + ";"
            }
            ObjectKind::Sequence => {
                let s = self.sequence_definition(obj).await?;
                format!("{};", ddl.create_sequence(&s))
            }
            ObjectKind::Function | ObjectKind::Procedure => {
                self.routine_definition(obj).await?.definition
            }
            _ => return Err(DbError::unsupported("DDL of this object")),
        })
    }

    async fn search_metadata(&mut self, search: &MetadataSearch) -> DbResult<Vec<ObjectInfo>> {
        let search = search.clone();
        self.blocking(move |c| catalog::search(c, &search)).await
    }

    async fn schema_snapshot(
        &mut self,
        schema: &SchemaRef,
        opts: &SnapshotOptions,
    ) -> DbResult<SchemaSnapshot> {
        let name = match schema.schema.clone() {
            Some(s) => s,
            None => self
                .blocking(catalog::current_schema)
                .await?
                .unwrap_or_default(),
        };
        let o = *opts;
        let n = name.clone();
        let (tables, views, sequences) = self
            .blocking(move |c| {
                let tables = catalog::table_definitions(c, &n, None, &o)?;
                let mut views = Vec::new();
                if o.views {
                    for v in catalog::list_objects(c, &n, ObjectKind::View)? {
                        if o.view_definitions || o.columns {
                            views.push(catalog::view_definition(c, &v.obj)?);
                        } else {
                            views.push(ViewDef {
                                schema: Some(n.clone()),
                                name: v.obj.name,
                                ..Default::default()
                            });
                        }
                    }
                }
                let mut sequences = Vec::new();
                if o.sequences {
                    for s in catalog::list_objects(c, &n, ObjectKind::Sequence)? {
                        sequences.push(catalog::sequence_definition(c, &s.obj)?);
                    }
                }
                Ok((tables, views, sequences))
            })
            .await?;
        Ok(SchemaSnapshot {
            kind: Some(DbKind::Oracle),
            database: self.service.clone(),
            schema: Some(name),
            tables,
            views,
            sequences,
            routines: Vec::new(),
            types: Vec::new(),
        })
    }

    /// `EXPLAIN PLAN` into PLAN_TABLE, read back as a tree and as DBMS_XPLAN text.
    async fn explain(&mut self, sql: &str, analyze: bool) -> DbResult<QueryPlan> {
        if analyze {
            return Err(DbError::unsupported("EXPLAIN ANALYZE"));
        }
        let sql = normalize(sql);
        let id = format!(
            "TERNARYGRID{}",
            std::process::id() as u64 * 1000
                + (std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_millis())
                    .unwrap_or(0) as u64)
        );
        let clear_ro_tx = should_rollback_before_explain(self.read_only, self.in_tx);
        self.blocking(move |c| {
            // Clear any lingering read-only transaction (when no manual tx is open)
            // so EXPLAIN PLAN does not hit ORA-01456 when writing to PLAN_TABLE.
            if clear_ro_tx {
                let _ = c.rollback();
            }
            // EXPLAIN PLAN writes PLAN_TABLE rows in the open transaction: they are undone
            // to a savepoint, so the user's transaction is neither committed nor ended.
            let transaction_before = transaction_open(c);
            c.execute(&format!("SAVEPOINT {EXPLAIN_SAVEPOINT}"), &[]).db()?;
            let plan = (|| -> DbResult<QueryPlan> {
                c.execute(&format!("EXPLAIN PLAN SET STATEMENT_ID = '{id}' FOR {sql}"), &[]).db()?;
                let rows = catalog::query(
                    c,
                    "SELECT id, parent_id, operation, options, object_owner, object_name, object_alias, cost, cardinality, bytes, access_predicates, filter_predicates \
                     FROM plan_table WHERE statement_id = :1 ORDER BY id",
                    &[&id.as_str()],
                )?;
                let text = catalog::query(c, "SELECT plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY('PLAN_TABLE', :1, 'TYPICAL'))", &[&id.as_str()])
                    .map(|r| r.iter().filter_map(|r| r.first().and_then(catalog::text)).collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                Ok(QueryPlan { roots: plan_tree(&rows), text, raw: None, planning_ms: None, execution_ms: None, analyzed: false })
            })();
            for stmt in explain_cleanup(transaction_before) {
                let _ = c.execute(&stmt, &[]);
            }
            plan
        })
        .await
    }

    async fn list_sessions(&mut self) -> DbResult<SessionList> {
        self.blocking(|c| {
            let rows = catalog::query(
                c,
                "SELECT s.sid || ',' || s.serial# id, s.username, s.status, s.osuser, s.machine, s.program, s.logon_time, s.event, s.blocking_session, q.sql_text \
                 FROM v$session s LEFT JOIN v$sql q ON q.sql_id = s.sql_id AND q.child_number = s.sql_child_number \
                 WHERE s.type = 'USER' ORDER BY s.sid",
                &[],
            )?;
            let mut data = TabularData::new(["ID", "User", "Status", "OS user", "Machine", "Program", "Logon", "Wait event", "Blocked by", "Query"].map(String::from).to_vec());
            data.rows = rows;
            Ok(SessionList { data, id_column: 0, query_column: Some(9), state_column: Some(2), blocked_by_column: Some(8) })
        })
        .await
    }

    async fn kill_session(&mut self, id: &str, mode: KillMode) -> DbResult<()> {
        let id = id.replace('\'', "");
        if mode == KillMode::Terminate
            && self
                .info
                .session_id
                .as_deref()
                .is_some_and(|own| sid_serial(own) == sid_serial(&id))
        {
            return Err(DbError::config("Refusing to kill the current session"));
        }
        let sql = match mode {
            KillMode::CancelQuery => format!("ALTER SYSTEM CANCEL SQL '{id}'"),
            KillMode::Terminate => format!("ALTER SYSTEM KILL SESSION '{id}' IMMEDIATE"),
        };
        self.blocking(move |c| c.execute(&sql, &[]).db().map(|_| ()))
            .await
    }
}

/// PLAN_TABLE rows (id, parent_id, ...) as a tree.
fn plan_tree(rows: &[Vec<Value>]) -> Vec<PlanNode> {
    let num = |v: &Value| match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Decimal(s) => s.parse().ok(),
        _ => None,
    };
    let mut nodes: Vec<(i64, Option<i64>, PlanNode)> = rows
        .iter()
        .map(|r| {
            let t = |i: usize| r.get(i).and_then(catalog::text);
            let mut n = PlanNode::new(match t(3) {
                Some(o) => format!("{} {}", t(2).unwrap_or_default(), o),
                None => t(2).unwrap_or_default(),
            });
            n.relation = match (t(4), t(5)) {
                (Some(o), Some(name)) => Some(qualified(Some(&o), &name)),
                (None, Some(name)) => Some(name),
                _ => None,
            };
            if n.node_type.contains("INDEX") {
                n.index = n.relation.take();
            }
            n.alias = t(6);
            n.total_cost = r.get(7).and_then(num);
            n.plan_rows = r.get(8).and_then(num);
            n.plan_width = r.get(9).and_then(num);
            n.condition = t(10).or(t(11));
            if let Some(f) = t(11) {
                n.properties.push(("Filter".into(), f));
            }
            if let Some(a) = t(10) {
                n.properties.push(("Access".into(), a));
            }
            let id = r.first().and_then(num).unwrap_or(0.0) as i64;
            let parent = r.get(1).and_then(num).map(|p| p as i64);
            (id, parent, n)
        })
        .collect();
    // Attach children bottom-up (ids are in depth-first order).
    let mut roots = Vec::new();
    while let Some((id, parent, node)) = nodes.pop() {
        let _ = id;
        match parent.and_then(|p| nodes.iter_mut().find(|(i, _, _)| *i == p)) {
            Some((_, _, p)) => p.children.insert(0, node),
            None => roots.insert(0, node),
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternarygrid_core::config::ConnectionConfig;

    #[test]
    fn statements_are_sent_as_oracle_wants_them() {
        assert_eq!(normalize("SELECT 1 FROM dual;"), "SELECT 1 FROM dual");
        assert_eq!(normalize("BEGIN NULL; END"), "BEGIN NULL; END;");
        assert_eq!(
            normalize("create or replace procedure p as begin null; end;\n/"),
            "create or replace procedure p as begin null; end;"
        );
        assert_eq!(
            normalize("CREATE TABLE t (a NUMBER);"),
            "CREATE TABLE t (a NUMBER)"
        );
        assert!(is_plsql("-- hi\nDECLARE x NUMBER; BEGIN NULL; END;"));
        assert!(is_plsql(
            "CREATE OR REPLACE EDITIONABLE PACKAGE BODY p AS END;"
        ));
        assert!(!is_plsql("CREATE TABLE t (a NUMBER)"));
        assert_eq!(
            compiled_object("CREATE OR REPLACE PACKAGE BODY hr.pkg AS END;"),
            Some(("PACKAGE BODY".into(), "PKG".into()))
        );
        assert_eq!(
            compiled_object("create function f return number is begin return 1; end;"),
            Some(("FUNCTION".into(), "F".into()))
        );
        assert_eq!(tag("create table t (a number)"), "CREATE TABLE");
        assert_eq!(tag("insert into t values (1)"), "INSERT");
    }

    #[test]
    fn statements_ending_the_transaction() {
        assert!(ends_transaction("COMMIT"));
        assert!(ends_transaction("commit work"));
        assert!(ends_transaction("ROLLBACK"));
        assert!(ends_transaction("rollback work"));
        assert!(ends_transaction("CREATE TABLE t (a NUMBER)"));
        assert!(ends_transaction("ALTER TABLE t ADD (b NUMBER)"));
        assert!(ends_transaction("TRUNCATE TABLE t"));
        // The transaction stays open.
        assert!(!ends_transaction("ROLLBACK TO SAVEPOINT ternarygrid_save"));
        assert!(!ends_transaction("rollback to s1"));
        assert!(!ends_transaction("ROLLBACK WORK TO SAVEPOINT s1"));
        assert!(!ends_transaction("SAVEPOINT s1"));
        assert!(!ends_transaction(
            "ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY-MM-DD'"
        ));
        assert!(!ends_transaction("ALTER SYSTEM CANCEL SQL '1,2'"));
        assert!(!ends_transaction("INSERT INTO t VALUES (1)"));
    }

    /// The current session is found in a listed id with or without its instance.
    #[test]
    fn session_ids_compare_by_sid_and_serial() {
        assert_eq!(sid_serial("12,345"), "12,345");
        assert_eq!(sid_serial("12, 345, @2"), "12,345");
        assert_ne!(sid_serial("12,346"), sid_serial("12,345"));
    }

    #[test]
    fn explain_never_commits() {
        // A transaction of the user's: only what EXPLAIN PLAN wrote is undone.
        assert_eq!(
            explain_cleanup(true),
            vec!["ROLLBACK TO SAVEPOINT ternarygrid_explain"]
        );
        // None before: the one EXPLAIN PLAN started is rolled back as well.
        assert_eq!(
            explain_cleanup(false),
            vec!["ROLLBACK TO SAVEPOINT ternarygrid_explain", "ROLLBACK"]
        );
        for before in [true, false] {
            assert!(
                explain_cleanup(before)
                    .iter()
                    .all(|s| !s.to_ascii_uppercase().contains("COMMIT"))
            );
        }
    }

    #[test]
    fn descriptors() {
        let mut cfg = ConnectionConfig::new(DbKind::Oracle, "x");
        cfg.host = "db".into();
        cfg.database = Some("ORCLPDB1".into());
        let p = ConnectParams::from_config(&cfg, None);
        assert_eq!(
            descriptor(&p).unwrap(),
            "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=db)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=ORCLPDB1)))"
        );
        cfg.properties.insert("sid".into(), "ORCL".into());
        cfg.ssl.mode = SslMode::VerifyFull;
        let p = ConnectParams::from_config(&cfg, None);
        assert_eq!(
            descriptor(&p).unwrap(),
            "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCPS)(HOST=db)(PORT=1521))(SECURITY=(SSL_SERVER_DN_MATCH=TRUE))(CONNECT_DATA=(SID=ORCL)))"
        );
    }

    #[test]
    fn tns_descriptor_injection_is_rejected() {
        for bad in [
            "db)(ADDRESS=(HOST=evil)",
            "db)",
            "db(",
            "db=evil",
            "db\r\n(HOST=evil)",
            "db\0evil",
        ] {
            let mut cfg = ConnectionConfig::new(DbKind::Oracle, "x");
            cfg.host = bad.into();
            cfg.database = Some("ORCLPDB1".into());
            let e = descriptor(&ConnectParams::from_config(&cfg, None)).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Config, "host {bad:?}");

            let mut cfg = ConnectionConfig::new(DbKind::Oracle, "x");
            cfg.host = "db".into();
            cfg.database = Some(bad.into());
            let e = descriptor(&ConnectParams::from_config(&cfg, None)).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Config, "database {bad:?}");

            let mut cfg = ConnectionConfig::new(DbKind::Oracle, "x");
            cfg.host = "db".into();
            cfg.properties.insert("sid".into(), bad.into());
            let e = descriptor(&ConnectParams::from_config(&cfg, None)).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Config, "sid {bad:?}");
        }
    }

    #[test]
    fn read_only_sql_classification() {
        for r in [
            "SELECT 1 FROM dual",
            "  -- comment\n((SELECT * FROM users WHERE note = 'FOR UPDATE'))",
            "SELECT q'[FOR UPDATE; DELETE FROM t]' FROM dual",
            "SELECT nq'[FOR UPDATE]' FROM dual",
            "SELECT NQ'{my_seq.NEXTVAL}' FROM dual",
            "SELECT nextval_note FROM t",
            "WITH c AS (SELECT 1 AS a FROM dual) SELECT * FROM c",
            "SET TRANSACTION READ ONLY",
            "COMMIT",
            "ROLLBACK",
        ] {
            assert!(is_read_only_sql(r), "expected read: {r}");
        }
        for w in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "MERGE INTO t USING s ON (t.id = s.id) WHEN MATCHED THEN UPDATE SET t.a = s.a",
            "CREATE TABLE t (a NUMBER)",
            "DROP TABLE t",
            "TRUNCATE TABLE t",
            "ALTER TABLE t ADD (b NUMBER)",
            "BEGIN DELETE FROM t; COMMIT; END;",
            "DECLARE x NUMBER; BEGIN NULL; END;",
            "CALL my_proc()",
            "LOCK TABLE t IN EXCLUSIVE MODE",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t FOR UPDATE NOWAIT",
            "SELECT my_seq.NEXTVAL FROM dual",
            "SELECT my_seq.\"NEXTVAL\" FROM dual",
            "SELECT nq'[']' FROM t FOR UPDATE -- '",
            "SELECT NQ'[']', my_seq.NEXTVAL FROM dual -- '",
            "WITH FUNCTION f RETURN NUMBER IS PRAGMA AUTONOMOUS_TRANSACTION; BEGIN DELETE FROM t; COMMIT; RETURN 1; END; SELECT f() FROM dual",
            "SET TRANSACTION READ WRITE",
            "/* SELECT 1 */ DELETE FROM t",
            "-- SELECT 1\nDROP TABLE t",
        ] {
            assert!(!is_read_only_sql(w), "expected write: {w}");
        }
    }

    #[test]
    fn read_only_transaction_lifecycle_decisions() {
        assert_eq!(read_only_tx_prep(false, true, false), ReadOnlyTxPrep::None);
        assert_eq!(read_only_tx_prep(false, false, true), ReadOnlyTxPrep::None);
        assert_eq!(
            read_only_tx_prep(true, true, false),
            ReadOnlyTxPrep::AutoRollbackThenSet
        );
        assert_eq!(
            read_only_tx_prep(true, true, true),
            ReadOnlyTxPrep::AutoRollbackThenSet
        );
        assert_eq!(
            read_only_tx_prep(true, false, false),
            ReadOnlyTxPrep::ManualStart
        );
        assert_eq!(
            read_only_tx_prep(true, false, true),
            ReadOnlyTxPrep::ManualContinue
        );

        assert!(is_ora_01453(
            &DbError::query("SET TRANSACTION must be first statement of transaction")
                .with_code("ORA-01453")
        ));
        assert!(!is_ora_01453(
            &DbError::query("cannot perform DML in READ ONLY transaction").with_code("ORA-01456")
        ));
        assert!(!is_ora_01453(&DbError::query("other")));

        assert!(should_rollback_before_explain(true, false));
        assert!(!should_rollback_before_explain(true, true));
        assert!(!should_rollback_before_explain(false, false));
        assert!(!should_rollback_before_explain(false, true));
    }

    #[test]
    fn plan_rows_become_a_tree() {
        let row = |id: i64, parent: Option<i64>, op: &str| {
            vec![
                Value::Int(id),
                parent.map_or(Value::Null, Value::Int),
                Value::Text(op.into()),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Int(3),
                Value::Int(10),
                Value::Null,
                Value::Null,
                Value::Null,
            ]
        };
        let roots = plan_tree(&[
            row(0, None, "SELECT STATEMENT"),
            row(1, Some(0), "HASH JOIN"),
            row(2, Some(1), "TABLE ACCESS"),
            row(3, Some(1), "INDEX"),
        ]);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].children[0].node_type, "HASH JOIN");
        assert_eq!(roots[0].children[0].children.len(), 2);
        assert_eq!(roots[0].children[0].children[0].node_type, "TABLE ACCESS");
    }
}
