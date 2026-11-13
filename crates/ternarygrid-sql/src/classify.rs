//! Statement classification: what a statement does, whether it writes,
//! whether it returns rows, whether it is dangerous and what it targets.
//!
//! This drives read-only enforcement (FR-C6: every statement with
//! [`StatementInfo::is_write`] is rejected on read-only connections) and the
//! confirmations for dangerous statements on production connections, so the
//! rules are conservative: when in doubt a statement is a write.

use crate::split::{SplitOptions, StatementSpan, split_tokens};
use crate::token::{Token, TokenKind, is_word, tokenize, unquote_ident};
use serde::{Deserialize, Serialize};
use ternarygrid_core::SqlSyntax;

/// Transaction control statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TxCommand {
    /// `BEGIN`, `START TRANSACTION`
    Begin,
    /// `COMMIT`, `END` (PostgreSQL)
    Commit,
    /// `ROLLBACK [TO SAVEPOINT x]`, `ABORT`
    Rollback,
    /// `SAVEPOINT x`
    Savepoint,
    /// `RELEASE [SAVEPOINT] x`
    Release,
    /// `SET TRANSACTION ...`, `SET SESSION CHARACTERISTICS AS TRANSACTION ...`
    SetTransaction,
}

/// What a statement does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StatementKind {
    /// Queries: `SELECT`, `WITH ... SELECT`, `VALUES`, `TABLE x`.
    Select,
    /// `INSERT`, `REPLACE INTO`, `UPSERT`.
    Insert,
    /// `UPDATE`
    Update,
    /// `DELETE`
    Delete,
    /// `MERGE`
    Merge,
    /// `CREATE ...`
    Create,
    /// `ALTER ...`
    Alter,
    /// `DROP ...`
    Drop,
    /// `TRUNCATE`
    Truncate,
    /// `RENAME` (MySQL)
    Rename,
    /// `COMMENT ON ...`
    Comment,
    /// `GRANT`
    Grant,
    /// `REVOKE`
    Revoke,
    /// Transaction control.
    Transaction(TxCommand),
    /// Session settings: `SET`, `RESET`.
    Set,
    /// `SHOW ...`
    Show,
    /// `USE db`
    Use,
    /// `CALL`, `EXEC`, `EXECUTE`, `DO`.
    Call,
    /// `EXPLAIN [ANALYZE] ...`
    Explain,
    /// `DESCRIBE` / `DESC`
    Describe,
    /// `COPY`, `LOAD DATA`
    Copy,
    /// `VACUUM`, `ANALYZE`, `REINDEX`, `OPTIMIZE`, `CHECK`, `REPAIR`, `CLUSTER` ...
    Maintenance,
    /// SQLite `PRAGMA`
    Pragma,
    /// `LOCK` / `UNLOCK`
    Lock,
    /// Client side command (`@set`, `@include` ...), handled by the application.
    ClientCommand,
    /// Anything else (treated as a write).
    Other,
}

impl StatementKind {
    /// Short English label (route through `tr()` for display).
    pub fn label(self) -> &'static str {
        match self {
            StatementKind::Select => "Query",
            StatementKind::Insert => "Insert",
            StatementKind::Update => "Update",
            StatementKind::Delete => "Delete",
            StatementKind::Merge => "Merge",
            StatementKind::Create => "Create",
            StatementKind::Alter => "Alter",
            StatementKind::Drop => "Drop",
            StatementKind::Truncate => "Truncate",
            StatementKind::Rename => "Rename",
            StatementKind::Comment => "Comment",
            StatementKind::Grant => "Grant",
            StatementKind::Revoke => "Revoke",
            StatementKind::Transaction(_) => "Transaction",
            StatementKind::Set => "Set",
            StatementKind::Show => "Show",
            StatementKind::Use => "Use",
            StatementKind::Call => "Call",
            StatementKind::Explain => "Explain",
            StatementKind::Describe => "Describe",
            StatementKind::Copy => "Copy",
            StatementKind::Maintenance => "Maintenance",
            StatementKind::Pragma => "Pragma",
            StatementKind::Lock => "Lock",
            StatementKind::ClientCommand => "Client command",
            StatementKind::Other => "Statement",
        }
    }
}

/// Reasons a statement needs confirmation on production connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Danger {
    /// `DELETE` without a top-level `WHERE`.
    DeleteWithoutWhere,
    /// `UPDATE` without a top-level `WHERE`.
    UpdateWithoutWhere,
    /// Any `DROP`.
    Drop,
    /// `TRUNCATE`.
    Truncate,
    /// `ALTER ... DROP` (columns, constraints, partitions ...).
    AlterDrop,
}

impl Danger {
    /// English description (route through `tr()` for display).
    pub fn message(self) -> &'static str {
        match self {
            Danger::DeleteWithoutWhere => "DELETE without WHERE removes every row",
            Danger::UpdateWithoutWhere => "UPDATE without WHERE changes every row",
            Danger::Drop => "DROP permanently removes the object",
            Danger::Truncate => "TRUNCATE removes every row",
            Danger::AlterDrop => "ALTER ... DROP removes part of the object",
        }
    }
}

/// Possibly qualified object name, unquoted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct QualifiedName {
    /// Database / catalog of three-part names (`db.schema.table`).
    pub catalog: Option<String>,
    /// Schema part, if written.
    pub schema: Option<String>,
    /// Object name.
    pub name: String,
}

/// Classification result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementInfo {
    /// What the statement does.
    pub kind: StatementKind,
    /// First keyword, upper case (`SELECT`, `WITH`, `@SET` ...); empty for
    /// statements that do not start with a word.
    pub first_keyword: String,
    /// The statement probably produces a result set.
    pub returns_rows: bool,
    /// The statement may modify data, schema, server state or files.
    pub is_write: bool,
    /// Why the statement needs a confirmation, if it does.
    pub danger: Option<Danger>,
    /// Main object: the table written to, the object created/altered/dropped,
    /// the first table read by a query, the procedure called.
    pub target: Option<QualifiedName>,
}

impl StatementInfo {
    fn new(kind: StatementKind, first_keyword: String) -> StatementInfo {
        StatementInfo {
            kind,
            first_keyword,
            returns_rows: false,
            is_write: false,
            danger: None,
            target: None,
        }
    }

    fn rows(mut self) -> Self {
        self.returns_rows = true;
        self
    }

    fn write(mut self) -> Self {
        self.is_write = true;
        self
    }

    /// Transaction control (`BEGIN`, `COMMIT`, `ROLLBACK`, ...).
    pub fn is_transaction_control(&self) -> bool {
        matches!(self.kind, StatementKind::Transaction(_))
    }
}

/// What a Redis command, MongoDB shell statement or Elasticsearch request
/// does (`None` for SQL and CQL: use [`classify`]).
pub fn classify_command(
    stmt: &str,
    language: ternarygrid_core::QueryLanguage,
) -> Option<StatementInfo> {
    use ternarygrid_core::QueryLanguage as L;
    let text = stmt.trim();
    let first: String = text
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '(')
        .collect::<String>()
        .to_ascii_uppercase();
    let mut info = StatementInfo::new(StatementKind::Other, first.clone());
    match language {
        L::Redis => {
            let read = matches!(
                first.as_str(),
                "GET"
                    | "MGET"
                    | "GETRANGE"
                    | "STRLEN"
                    | "EXISTS"
                    | "TYPE"
                    | "TTL"
                    | "PTTL"
                    | "EXPIRETIME"
                    | "KEYS"
                    | "SCAN"
                    | "HGET"
                    | "HMGET"
                    | "HGETALL"
                    | "HKEYS"
                    | "HVALS"
                    | "HLEN"
                    | "HEXISTS"
                    | "HSCAN"
                    | "HSTRLEN"
                    | "HRANDFIELD"
                    | "LRANGE"
                    | "LINDEX"
                    | "LLEN"
                    | "LPOS"
                    | "SMEMBERS"
                    | "SISMEMBER"
                    | "SMISMEMBER"
                    | "SCARD"
                    | "SSCAN"
                    | "SRANDMEMBER"
                    | "SINTER"
                    | "SUNION"
                    | "SDIFF"
                    | "SINTERCARD"
                    | "ZRANGE"
                    | "ZREVRANGE"
                    | "ZRANGEBYSCORE"
                    | "ZREVRANGEBYSCORE"
                    | "ZRANGEBYLEX"
                    | "ZSCORE"
                    | "ZMSCORE"
                    | "ZRANK"
                    | "ZREVRANK"
                    | "ZCARD"
                    | "ZCOUNT"
                    | "ZLEXCOUNT"
                    | "ZSCAN"
                    | "ZRANDMEMBER"
                    | "XRANGE"
                    | "XREVRANGE"
                    | "XLEN"
                    | "XINFO"
                    | "XREAD"
                    | "XPENDING"
                    | "PFCOUNT"
                    | "BITCOUNT"
                    | "BITPOS"
                    | "GETBIT"
                    | "GEOPOS"
                    | "GEODIST"
                    | "GEOSEARCH"
                    | "JSON.GET"
                    | "JSON.TYPE"
                    | "INFO"
                    | "PING"
                    | "ECHO"
                    | "TIME"
                    | "DBSIZE"
                    | "LASTSAVE"
                    | "RANDOMKEY"
                    | "OBJECT"
                    | "MEMORY"
                    | "SLOWLOG"
                    | "LATENCY"
                    | "COMMAND"
                    | "SELECT"
                    | "DUMP"
                    | "PUBSUB"
                    | "TOUCH"
            ) || first == "CLIENT" && text.to_ascii_uppercase().contains("LIST")
                || first == "CONFIG"
                    && text.to_ascii_uppercase().split_whitespace().nth(1) == Some("GET");
            info.is_write = !read;
            info.returns_rows = read;
            info.kind = if read {
                StatementKind::Select
            } else {
                StatementKind::Other
            };
            if matches!(
                first.as_str(),
                "FLUSHALL" | "FLUSHDB" | "SWAPDB" | "SHUTDOWN"
            ) {
                info.danger = Some(Danger::Truncate);
            } else if matches!(first.as_str(), "DEL" | "UNLINK") {
                info.danger = Some(Danger::Drop);
            }
        }
        L::Mongo => {
            let t = js_tokens(text);
            let calls = js_calls(&t);
            // Only methods known to read are reads: anything else (`insert`, `save`,
            // `findAndModify`, `mapReduce`, `createView`, `createUser`, …) writes.
            let unknown_call = calls.iter().any(|c| {
                let known = if c.member {
                    MONGO_READ_METHODS
                } else {
                    MONGO_GLOBAL_CALLS
                };
                !known.contains(&c.name.as_str())
            });
            // An aggregation `$out` / `$merge` stage writes its result.
            let write_stage = t.iter().any(
                |x| matches!(x, JsTok::Ident(s) | JsTok::Str(s) if s == "$out" || s == "$merge"),
            );
            let use_db = matches!(t.first(), Some(JsTok::Ident(w)) if w.eq_ignore_ascii_case("use"))
                && t.len() > 1;
            info.is_write = unknown_call || write_stage || use_db;
            info.returns_rows = !info.is_write;
            info.kind = if info.is_write {
                StatementKind::Other
            } else {
                StatementKind::Select
            };
            let called = |m: &str| calls.iter().any(|c| c.member && c.name == m);
            let empty_filter = |m: &str| {
                calls.iter().any(|c| {
                    c.member && c.name == m && {
                        let a = c.open + 1;
                        t.get(a) == Some(&JsTok::Punct(')'))
                            || (t.get(a) == Some(&JsTok::Punct('{'))
                                && t.get(a + 1) == Some(&JsTok::Punct('}'))
                                && matches!(t.get(a + 2), Some(JsTok::Punct(')' | ','))))
                    }
                })
            };
            if called("drop") || called("dropDatabase") {
                info.danger = Some(Danger::Drop);
            } else if empty_filter("deleteMany") || empty_filter("remove") {
                info.danger = Some(Danger::DeleteWithoutWhere);
            } else if empty_filter("updateMany") {
                info.danger = Some(Danger::UpdateWithoutWhere);
            }
        }
        L::Search => {
            let method = first.as_str();
            // Like the driver: `METHOD target`, where `target` is not SQL's FROM.
            let next = text.split_whitespace().nth(1).unwrap_or("");
            let rest = matches!(method, "GET" | "POST" | "PUT" | "DELETE" | "HEAD")
                && !next.is_empty()
                && !next.eq_ignore_ascii_case("FROM");
            if !rest {
                return None;
            }
            // The driver takes the path from the request line (`/` when it has none).
            let (line, body) = text.split_once('\n').unwrap_or((text, ""));
            let mut words = line.split_whitespace().skip(1);
            let target = words.next().unwrap_or("/");
            let path = target.split(['?', '#']).next().unwrap_or("");
            let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            // The endpoint is the first `_` segment (index names cannot start with `_`):
            // `/product_catalog/_doc` is `_doc`, `/logs/_search/scroll` is `_search`.
            let api = segments
                .iter()
                .copied()
                .find(|s| s.starts_with('_') && !matches!(*s, "_plugins" | "_opendistro"))
                .unwrap_or("");
            // Encoded or relative paths may reach another endpoint.
            let plain = !path.contains('%') && !segments.iter().any(|s| matches!(*s, "." | ".."));
            let reads = [
                "_search",
                "_msearch",
                "_count",
                "_sql",
                "_ppl",
                "_query",
                "_eql",
                "_explain",
                "_field_caps",
                "_validate",
                "_mget",
                "_termvectors",
                "_mtermvectors",
                "_analyze",
                "_rank_eval",
                "_search_shards",
                "_async_search",
                "_knn_search",
                "_terms_enum",
                "_render",
            ];
            // GET requests that still change the index.
            let side_effects = ["_refresh", "_flush", "_forcemerge", "_cache"];
            // OpenSearch SQL / PPL can delete documents.
            let dml = matches!(api, "_sql" | "_ppl") && {
                let request: String = words
                    .chain(body.split_whitespace())
                    .collect::<Vec<_>>()
                    .join(" ");
                request
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .any(|w| {
                        [
                            "DELETE", "INSERT", "UPDATE", "REPLACE", "UPSERT", "MERGE", "CREATE",
                            "ALTER", "DROP", "TRUNCATE",
                        ]
                        .iter()
                        .any(|k| w.eq_ignore_ascii_case(k))
                    })
            };
            info.is_write = dml
                || match method {
                    "GET" | "HEAD" => segments.iter().any(|s| side_effects.contains(s)),
                    "POST" => !(plain && reads.contains(&api)),
                    _ => true,
                };
            info.returns_rows = !info.is_write;
            info.kind = if info.is_write {
                StatementKind::Other
            } else {
                StatementKind::Select
            };
            if method == "DELETE" {
                info.danger = Some(Danger::Drop);
            } else if segments.contains(&"_delete_by_query") {
                info.danger = Some(Danger::DeleteWithoutWhere);
            }
        }
        _ => return None,
    }
    Some(info)
}

/// MongoDB collection, cursor and database methods that only read.
const MONGO_READ_METHODS: &[&str] = &[
    "find",
    "findOne",
    "aggregate",
    "count",
    "countDocuments",
    "estimatedDocumentCount",
    "distinct",
    "getIndexes",
    "getIndexKeys",
    "getIndexSpecs",
    "stats",
    "dataSize",
    "storageSize",
    "totalSize",
    "totalIndexSize",
    "isCapped",
    "latencyStats",
    "sort",
    "limit",
    "skip",
    "projection",
    "pretty",
    "toArray",
    "batchSize",
    "maxTimeMS",
    "allowDiskUse",
    "comment",
    "hint",
    "collation",
    "min",
    "max",
    "readConcern",
    "readPref",
    "returnKey",
    "showRecordId",
    "size",
    "itcount",
    "explain",
    "hasNext",
    "next",
    "getCollection",
    "getSiblingDB",
    "getCollectionNames",
    "getCollectionInfos",
    "getName",
    "serverStatus",
    "version",
    "hostInfo",
    "getUsers",
    "getUser",
    "getRoles",
    "getRole",
    "currentOp",
    "getProfilingStatus",
    "getProfilingLevel",
    "hello",
    "isMaster",
];

/// Calls without a receiver that only build values (`ObjectId("…")`, `new Date()`),
/// and keywords followed by `(`.
const MONGO_GLOBAL_CALLS: &[&str] = &[
    "ObjectId",
    "ISODate",
    "Date",
    "NumberLong",
    "Long",
    "NumberInt",
    "Int32",
    "NumberDecimal",
    "Decimal128",
    "Double",
    "UUID",
    "BinData",
    "Timestamp",
    "RegExp",
    "MinKey",
    "MaxKey",
    "print",
    "printjson",
    "tojson",
    "function",
    "if",
    "for",
    "while",
    "switch",
    "return",
    "typeof",
    "catch",
];

/// Token of a MongoDB shell statement.
#[derive(Debug, Clone, PartialEq, Eq)]
enum JsTok {
    /// Name or number (`db`, `$out`, `5`).
    Ident(String),
    /// String (unescaped like the driver does) or regular expression literal.
    Str(String),
    Punct(char),
}

/// Tokens of a shell statement: white space and comments dropped, strings
/// unescaped, read the way the TernaryGrid MongoDB driver reads them.
fn js_tokens(src: &str) -> Vec<JsTok> {
    let c: Vec<char> = src.chars().collect();
    let mut out: Vec<JsTok> = Vec::new();
    let mut i = 0;
    let ident_char = |ch: char| {
        ch.is_ascii_alphanumeric()
            || ch == '_'
            || ch == '$'
            || !(ch.is_ascii() || ch.is_whitespace())
    };
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if ch == '"' || ch == '\'' {
            let mut s = String::new();
            i += 1;
            while i < c.len() && c[i] != ch {
                if c[i] == '\\' {
                    i += 1;
                    match c.get(i) {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some('b') => s.push('\u{8}'),
                        Some('f') => s.push('\u{c}'),
                        Some('0') => s.push('\0'),
                        Some('u') => {
                            let hex: String = c.iter().skip(i + 1).take(4).collect();
                            if let Some(u) =
                                u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                            {
                                s.push(u);
                                i += 4;
                            }
                        }
                        Some(&e) => s.push(e),
                        None => {}
                    }
                } else {
                    s.push(c[i]);
                }
                i += 1;
            }
            i += 1;
            out.push(JsTok::Str(s));
        } else if ch == '/'
            && !matches!(
                out.last(),
                Some(JsTok::Ident(_) | JsTok::Str(_) | JsTok::Punct(')' | ']'))
            )
        {
            // Regular expression literal (where a value is expected).
            let mut s = String::new();
            let mut class = false;
            i += 1;
            while i < c.len() {
                match c[i] {
                    '\\' => {
                        s.push('\\');
                        i += 1;
                    }
                    '[' => class = true,
                    ']' => class = false,
                    '/' if !class => break,
                    _ => {}
                }
                if let Some(&x) = c.get(i) {
                    s.push(x);
                }
                i += 1;
            }
            i += 1;
            while i < c.len() && c[i].is_ascii_alphabetic() {
                i += 1;
            }
            out.push(JsTok::Str(s));
        } else if ident_char(ch) {
            let start = i;
            while i < c.len() && ident_char(c[i]) {
                i += 1;
            }
            out.push(JsTok::Ident(c[start..i].iter().collect()));
        } else {
            out.push(JsTok::Punct(ch));
            i += 1;
        }
    }
    out
}

/// A call in a shell statement.
struct JsCall {
    /// Function name; empty when it is computed (`f()()`, `x[k]()`).
    name: String,
    /// Called on an object (`.name(`, `["name"](`).
    member: bool,
    /// Index of the `(` (or `` ` `` of a tagged template).
    open: usize,
}

/// Every call in `t`: `.name(`, `.name?.(`, `["name"](`, `name(` …
fn js_calls(t: &[JsTok]) -> Vec<JsCall> {
    let p = |k: usize, ch: char| t.get(k) == Some(&JsTok::Punct(ch));
    let mut calls = Vec::new();
    for k in 0..t.len() {
        let open = if p(k + 1, '(') || p(k + 1, '`') {
            k + 1
        } else if p(k + 1, '?') && p(k + 2, '.') && p(k + 3, '(') {
            k + 3
        } else {
            continue;
        };
        let (name, member) = match &t[k] {
            JsTok::Ident(n) => (n.clone(), k > 0 && p(k - 1, '.')),
            JsTok::Punct(']') => match (k >= 2 && p(k - 2, '['), &t[k.saturating_sub(1)]) {
                (true, JsTok::Str(s)) => (s.clone(), true),
                _ => (String::new(), true),
            },
            JsTok::Punct(')') => (String::new(), false),
            _ => continue,
        };
        calls.push(JsCall { name, member, open });
    }
    calls
}

/// Classify one statement.
pub fn classify(stmt: &str, syntax: &SqlSyntax) -> StatementInfo {
    let tokens = tokenize(stmt, syntax);
    classify_tokens(stmt, &tokens, syntax)
}

/// Split a script and classify every statement (tokenizes once).
pub fn split_and_classify(
    src: &str,
    syntax: &SqlSyntax,
    opts: &SplitOptions,
) -> Vec<(StatementSpan, StatementInfo)> {
    let split = split_tokens(src, syntax, opts);
    split
        .statements
        .iter()
        .map(|s| {
            let toks = split.tokens.get(s.tokens.clone()).unwrap_or(&[]);
            (s.span, classify_tokens(src, toks, syntax))
        })
        .collect()
}

/// Classify the statement made of `tokens` (offsets into `src`).
pub(crate) fn classify_tokens(src: &str, tokens: &[Token], syntax: &SqlSyntax) -> StatementInfo {
    let sig: Vec<Token> = tokens.iter().filter(|t| !t.is_trivia()).copied().collect();
    // MySQL runs the text of `/*! … */` comments as part of the statement.
    let expanded = expand_exec_comments(src, tokens);
    if sig.is_empty() {
        return match expanded {
            Some(text) => classify_text(&text, syntax),
            None => StatementInfo::new(StatementKind::Other, String::new()),
        };
    }
    let mut info = classify_sig(src, &sig, syntax);
    // MySQL runs `/*! … */` comments: `SELECT * FROM t /*!INTO OUTFILE '/tmp/x'*/` writes.
    if syntax.delimiter_command && !info.is_write {
        for c in tokens.iter().filter(|t| is_exec_comment(t, src)) {
            let inner = exec_comment_body(c.text(src));
            let toks = tokenize(inner, syntax);
            let sig: Vec<Token> = toks.iter().filter(|t| !t.is_trivia()).copied().collect();
            let words: Vec<String> = sig
                .iter()
                .map(|t| t.text(inner).to_ascii_uppercase())
                .collect();
            let into_file = words
                .windows(2)
                .any(|w| w[0] == "INTO" && (w[1] == "OUTFILE" || w[1] == "DUMPFILE"));
            // A fragment (`, 2`, `PARTITION BY …`) is no statement: only a writing statement word counts.
            let writes = words.first().is_some_and(|w| {
                LATER_WRITE_STARTS.contains(&w.as_str())
                    || ["REPLACE", "RENAME", "LOAD"].contains(&w.as_str())
            });
            if into_file || writes {
                info.is_write = true;
                break;
            }
        }
        // Clauses spread over several comments and the text around them
        // (`SELECT * FROM t /*!INTO*/ /*!OUTFILE '/tmp/x'*/`).
        if !info.is_write
            && let Some(text) = &expanded
        {
            let e = classify_text(text, syntax);
            info.is_write = e.is_write;
            info.danger = info.danger.or(e.danger);
        }
    }
    info
}

/// `/*! … */` (MySQL) or `/*M! … */` (MariaDB) executable comment.
fn is_exec_comment(t: &Token, src: &str) -> bool {
    t.kind == TokenKind::BlockComment && {
        let text = t.text(src);
        text.starts_with("/*!") || text.starts_with("/*M!")
    }
}

/// The statement as MySQL runs it: executable comments replaced by their
/// text; `None` without such comments.
fn expand_exec_comments(src: &str, tokens: &[Token]) -> Option<String> {
    if !tokens.iter().any(|t| is_exec_comment(t, src)) {
        return None;
    }
    let mut text = String::new();
    for t in tokens {
        if is_exec_comment(t, src) {
            text.push(' ');
            text.push_str(exec_comment_body(t.text(src)));
            text.push(' ');
        } else {
            text.push_str(t.text(src));
        }
    }
    Some(text)
}

/// Classify `text` (an expanded statement).
fn classify_text(text: &str, syntax: &SqlSyntax) -> StatementInfo {
    let toks = tokenize(text, syntax);
    let sig: Vec<Token> = toks.iter().filter(|t| !t.is_trivia()).copied().collect();
    let mut info = classify_sig(text, &sig, syntax);
    // An executable comment left inside one is not looked into.
    if toks.iter().any(|t| is_exec_comment(t, text)) {
        info.is_write = true;
    }
    info
}

/// Classify the significant tokens `sig`. Statements separated by `;` at the
/// top level (`SELECT 1; DELETE FROM t` given to [`classify`] as one, a MySQL
/// custom delimiter) all count: any write makes the whole a write.
fn classify_sig(src: &str, sig: &[Token], syntax: &SqlSyntax) -> StatementInfo {
    let mut pieces = Vec::new();
    let (mut depth, mut from) = (0i32, 0usize);
    for (k, t) in sig.iter().enumerate() {
        match t.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => depth -= 1,
            TokenKind::Semicolon if depth <= 0 => {
                if k > from {
                    pieces.push(from..k);
                }
                from = k + 1;
            }
            _ => {}
        }
    }
    if from < sig.len() {
        pieces.push(from..sig.len());
    }
    let Some(first) = pieces.first() else {
        return classify_run(src, sig, syntax);
    };
    let mut info = classify_run(src, &sig[first.clone()], syntax);
    // Procedure bodies and blocks hold statements of their own: their dangers are not the statement's.
    let holds_body = info.is_write
        && matches!(
            info.kind,
            StatementKind::Create | StatementKind::Alter | StatementKind::Other
        );
    for r in &pieces[1..] {
        let piece = classify_run(src, &sig[r.clone()], syntax);
        info.is_write |= piece.is_write;
        if !holds_body {
            info.danger = info.danger.or(piece.danger);
        }
    }
    info
}

/// Classify one statement (or a T-SQL run of statements without `;`).
fn classify_run(src: &str, sig: &[Token], syntax: &SqlSyntax) -> StatementInfo {
    let mut info = Sig { src, t: sig }.classify(0, 0);
    // T-SQL runs statements one after another without `;` (`SELECT 1 DELETE FROM t`,
    // `UPDATE t SET a = 1 DELETE FROM u WHERE id = 5`): one span may hold several.
    // Each is classified on its own tokens, so a later WHERE does not count for an
    // earlier UPDATE.
    if syntax.batch_separator.is_some() {
        let (mut from, mut kind, mut is_write) = (0, info.kind, info.is_write);
        let mut several = false;
        let mut danger = None;
        loop {
            let next = next_batch_statement(src, sig, from, kind, is_write);
            if next.is_none() && !several {
                break;
            }
            let piece = Sig {
                src,
                t: &sig[from..next.unwrap_or(sig.len())],
            }
            .classify(0, 0);
            info.is_write |= piece.is_write;
            danger = danger.or(piece.danger);
            several = true;
            let Some(next) = next else { break };
            // What the next statement is shows in its first words.
            let head = Sig {
                src,
                t: &sig[next..(next + 16).min(sig.len())],
            }
            .classify(0, 0);
            (from, kind, is_write) = (next, head.kind, head.is_write);
        }
        if several {
            info.danger = danger;
        }
    }
    info
}

/// Statements that may follow another one in a T-SQL batch without `;`.
/// Start of the T-SQL statement after the one at `from` (of `kind`), in a
/// run without `;`. Statements that hold others (MERGE's actions, CREATE's
/// body, GRANT's privileges) are not cut.
fn next_batch_statement(
    src: &str,
    sig: &[Token],
    from: usize,
    kind: StatementKind,
    is_write: bool,
) -> Option<usize> {
    use StatementKind as K;
    let cut = !is_write
        || matches!(
            kind,
            K::Insert | K::Update | K::Delete | K::Drop | K::Truncate | K::Call
        );
    if !cut {
        return None;
    }
    let mut depth = 0i32;
    for (k, t) in sig.iter().enumerate().skip(from + 1) {
        match t.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => depth -= 1,
            _ if depth == 0 => {
                let w = t.text(src);
                let is = |s: &str| w.eq_ignore_ascii_case(s);
                let write = LATER_WRITE_STARTS.iter().any(|s| is(s))
                    // `a INNER MERGE JOIN b` is a join hint, no MERGE statement.
                    && !(is("MERGE") && sig.get(k + 1).is_some_and(|n| n.text(src).eq_ignore_ascii_case("JOIN")))
                    // `INSERT … EXEC p` fills the insert.
                    && !(kind == K::Insert && (is("EXEC") || is("EXECUTE")));
                // `DISABLE TRIGGER trg ON t`
                let trigger = (is("ENABLE") || is("DISABLE"))
                    && sig
                        .get(k + 1)
                        .is_some_and(|n| n.text(src).eq_ignore_ascii_case("TRIGGER"));
                // After UPDATE / DELETE a SELECT is the next statement (`INSERT … SELECT` fills the insert).
                let read = is("SELECT") && matches!(kind, K::Update | K::Delete);
                if write || trigger || read {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

const LATER_WRITE_STARTS: &[&str] = &[
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "DROP",
    "ALTER",
    "CREATE",
    "TRUNCATE",
    "EXEC",
    "EXECUTE",
    "GRANT",
    "REVOKE",
    "DENY",
    "BACKUP",
    "RESTORE",
    "DBCC",
    "BULK",
    "RECONFIGURE",
    "KILL",
    "SHUTDOWN",
    "CHECKPOINT",
    "WRITETEXT",
    "UPDATETEXT",
    "SETUSER",
];

/// Settings that make a session read-only: turning one off or resetting it
/// lifts the read-only mode of a read-only connection.
const READ_ONLY_SETTINGS: &[&str] = &[
    "default_transaction_read_only",
    "transaction_read_only",
    "tx_read_only",
    "read_only",
    "super_read_only",
    // ClickHouse
    "readonly",
];

/// `name` (possibly quoted or qualified: `@@session.transaction_read_only`) is
/// one of [`READ_ONLY_SETTINGS`].
fn is_read_only_setting(name: &str) -> bool {
    let name = name.trim_matches(['`', '"']).to_ascii_lowercase();
    let name = name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .trim_start_matches('@');
    READ_ONLY_SETTINGS.contains(&name)
}

/// Body of a MySQL executable comment: `/*!40101 SET x */` → `SET x`
/// (MariaDB: `/*M!100100 SET x */`).
fn exec_comment_body(text: &str) -> &str {
    let inner = text
        .strip_prefix("/*!")
        .or_else(|| text.strip_prefix("/*M!"))
        .unwrap_or(text);
    let inner = inner.strip_suffix("*/").unwrap_or(inner);
    inner
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim()
}

const MAX_DEPTH: u32 = 8;

const QUERY_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "foreign_key_check",
    "integrity_check",
    "quick_check",
    "database_list",
    "collation_list",
    "function_list",
    "module_list",
    "pragma_list",
    "compile_options",
    "page_count",
    "freelist_count",
    "data_version",
];

struct Sig<'a> {
    src: &'a str,
    t: &'a [Token],
}

impl<'a> Sig<'a> {
    fn sub(&self, from: usize, to: usize) -> Sig<'a> {
        let to = to.min(self.t.len());
        let from = from.min(to);
        Sig {
            src: self.src,
            t: &self.t[from..to],
        }
    }

    fn kind(&self, i: usize) -> Option<TokenKind> {
        self.t.get(i).map(|t| t.kind)
    }

    fn w(&self, i: usize, word: &str) -> bool {
        self.t.get(i).is_some_and(|t| is_word(t, self.src, word))
    }

    fn any(&self, i: usize, words: &[&str]) -> bool {
        words.iter().any(|w| self.w(i, w))
    }

    fn text(&self, i: usize) -> &'a str {
        self.t.get(i).map(|t| t.text(self.src)).unwrap_or("")
    }

    fn upper(&self, i: usize) -> String {
        match self.kind(i) {
            Some(TokenKind::Keyword | TokenKind::Identifier) => self.text(i).to_ascii_uppercase(),
            _ => String::new(),
        }
    }

    /// Index of the `)` matching the `(` at `i`.
    fn close_paren(&self, i: usize) -> usize {
        let mut depth = 0i32;
        for (j, t) in self.t.iter().enumerate().skip(i) {
            match t.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth <= 0 {
                        return j;
                    }
                }
                _ => {}
            }
        }
        self.t.len()
    }

    /// First index >= `from` at parenthesis depth 0 satisfying `pred`.
    fn find_top(&self, from: usize, pred: impl Fn(usize) -> bool) -> Option<usize> {
        let mut depth = 0i32;
        for (j, t) in self.t.iter().enumerate().skip(from) {
            match t.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => depth -= 1,
                _ if depth <= 0 && pred(j) => return Some(j),
                _ => {}
            }
        }
        None
    }

    fn has_top_word(&self, from: usize, word: &str) -> bool {
        self.find_top(from, |j| self.w(j, word)).is_some()
    }

    fn ident_text(&self, i: usize) -> Option<String> {
        let t = self.t.get(i)?;
        match t.kind {
            TokenKind::Identifier | TokenKind::Keyword => Some(t.text(self.src).to_string()),
            TokenKind::QuotedIdentifier => Some(unquote_ident(t.text(self.src)).into_owned()),
            _ => None,
        }
    }

    /// Dotted name starting at `i`.
    fn name_at(&self, i: usize) -> Option<QualifiedName> {
        let mut parts = vec![self.ident_text(i)?];
        let mut j = i + 1;
        while self.kind(j) == Some(TokenKind::Dot) && parts.len() < 4 {
            match self.ident_text(j + 1) {
                Some(p) => parts.push(p),
                None => break,
            }
            j += 2;
        }
        let name = parts.pop()?;
        let schema = parts.pop();
        let catalog = parts.pop();
        Some(QualifiedName {
            catalog,
            schema,
            name,
        })
    }

    /// Skip words in `skip` starting at `i`.
    fn skip_words(&self, mut i: usize, skip: &[&str]) -> usize {
        while self.any(i, skip) {
            i += 1;
        }
        i
    }

    fn skip_if_exists(&self, mut i: usize) -> usize {
        if self.w(i, "IF") {
            i += 1;
            if self.w(i, "NOT") {
                i += 1;
            }
            if self.w(i, "EXISTS") {
                i += 1;
            }
        }
        i
    }

    fn top_from_target(&self, from: usize) -> Option<QualifiedName> {
        let f = self.find_top(from, |j| self.w(j, "FROM"))?;
        let i = self.skip_words(f + 1, &["ONLY", "LATERAL"]);
        if self.kind(i) == Some(TokenKind::LParen) {
            return None;
        }
        self.name_at(i)
    }

    fn returning(&self, from: usize) -> bool {
        self.find_top(from, |j| self.w(j, "RETURNING") || self.w(j, "OUTPUT"))
            .is_some()
    }

    fn classify(&self, start: usize, depth: u32) -> StatementInfo {
        let mut p = start;
        while self.kind(p) == Some(TokenKind::LParen) {
            p += 1;
        }
        let first = self.upper(p);
        if self.kind(p) == Some(TokenKind::ClientCommand) {
            // Without client command splitting SQL may follow on the next lines
            // (`@set x = 1` / `DELETE FROM t`): that is what the statement does.
            let mut q = p + 1;
            while self.kind(q) == Some(TokenKind::ClientCommand) {
                q += 1;
            }
            if q < self.t.len() {
                return self.classify(q, depth + 1);
            }
            let text = self.text(p);
            let name: String = text.chars().take_while(|c| !c.is_whitespace()).collect();
            return StatementInfo::new(StatementKind::ClientCommand, name.to_uppercase());
        }
        if depth > MAX_DEPTH {
            return StatementInfo::new(StatementKind::Other, first).write();
        }
        use StatementKind as K;
        let info = StatementInfo::new;
        match first.as_str() {
            "SELECT" => self.select(p, first),
            "WITH" => self.with(p, depth),
            "VALUES" | "SUMMARIZE" => {
                let mut i = info(K::Select, first).rows();
                i.is_write = self.lifts_read_only(p + 1);
                i
            }
            "TABLE" => {
                let mut i = info(K::Select, first).rows();
                i.target = self.name_at(p + 1);
                i
            }
            "INSERT" | "REPLACE" | "UPSERT" => {
                let mut i = info(K::Insert, first).write();
                i.returns_rows = self.returning(p);
                let into = (p + 1..(p + 6).min(self.t.len())).find(|&j| self.w(j, "INTO"));
                i.target = match into {
                    Some(j) => self.name_at(j + 1),
                    None => self.name_at(self.skip_words(
                        p + 1,
                        &["LOW_PRIORITY", "DELAYED", "HIGH_PRIORITY", "IGNORE"],
                    )),
                };
                i
            }
            "UPDATE" => {
                let mut i = info(K::Update, first).write();
                if !self.has_top_word(p + 1, "WHERE") {
                    i.danger = Some(Danger::UpdateWithoutWhere);
                }
                i.returns_rows = self.returning(p);
                let mut j = self.skip_words(p + 1, &["ONLY", "LOW_PRIORITY", "IGNORE"]);
                if self.w(j, "OR") {
                    j += 2;
                }
                i.target = self.name_at(j);
                i
            }
            "DELETE" => {
                let mut i = info(K::Delete, first).write();
                if !self.has_top_word(p + 1, "WHERE") {
                    i.danger = Some(Danger::DeleteWithoutWhere);
                }
                i.returns_rows = self.returning(p);
                i.target = self.top_from_target(p + 1).or_else(|| self.name_at(p + 1));
                i
            }
            "MERGE" => {
                let mut i = info(K::Merge, first).write();
                i.returns_rows = self.returning(p);
                let j = if self.w(p + 1, "INTO") { p + 2 } else { p + 1 };
                i.target = self.name_at(j);
                i
            }
            "CREATE" => {
                let mut i = info(K::Create, first).write();
                i.target = self.object_name_after_kind(p + 1);
                i
            }
            "ALTER" => {
                let mut i = info(K::Alter, first).write();
                i.target = self.object_name_after_kind(p + 1);
                let drop = self.find_top(p + 1, |j| {
                    self.w(j, "DROP")
                        && !self.any(j + 1, &["DEFAULT", "NOT", "IDENTITY", "EXPRESSION"])
                });
                if drop.is_some() {
                    i.danger = Some(Danger::AlterDrop);
                }
                i
            }
            "DROP" => {
                let mut i = info(K::Drop, first).write();
                i.danger = Some(Danger::Drop);
                i.target = self.object_name_after_kind(p + 1);
                i
            }
            "TRUNCATE" => {
                let mut i = info(K::Truncate, first).write();
                i.danger = Some(Danger::Truncate);
                i.target = self.name_at(self.skip_words(p + 1, &["TABLE", "ONLY"]));
                i
            }
            "RENAME" => {
                let mut i = info(K::Rename, first).write();
                i.target = self.name_at(self.skip_words(p + 1, &["TABLE", "USER"]));
                i
            }
            "COMMENT" => info(K::Comment, first).write(),
            "GRANT" => info(K::Grant, first).write(),
            "REVOKE" => info(K::Revoke, first).write(),
            "BEGIN" => {
                let next = p + 1;
                let tx = next >= self.t.len()
                    || self.any(
                        next,
                        &[
                            "TRANSACTION",
                            "TRAN",
                            "WORK",
                            "DEFERRED",
                            "IMMEDIATE",
                            "EXCLUSIVE",
                            "ISOLATION",
                            "READ",
                            "DISTRIBUTED",
                        ],
                    );
                if tx {
                    let mut i = info(K::Transaction(TxCommand::Begin), first);
                    i.is_write = self.read_write(next);
                    i
                } else {
                    info(K::Other, first).write()
                }
            }
            "START" => {
                if self.w(p + 1, "TRANSACTION") {
                    let mut i = info(K::Transaction(TxCommand::Begin), first);
                    i.is_write = self.read_write(p + 2);
                    i
                } else {
                    info(K::Other, first).write()
                }
            }
            "COMMIT" | "END" => info(K::Transaction(TxCommand::Commit), first),
            "ROLLBACK" | "ABORT" => info(K::Transaction(TxCommand::Rollback), first),
            "SAVEPOINT" => info(K::Transaction(TxCommand::Savepoint), first),
            "RELEASE" => info(K::Transaction(TxCommand::Release), first),
            "SET" => self.set(p, first),
            "RESET" => {
                let mut i = info(K::Set, first);
                // `RESET ALL` and `RESET default_transaction_read_only` lift the read-only
                // session of a read-only connection.
                i.is_write = self.any(
                    p + 1,
                    &[
                        "MASTER", "SLAVE", "REPLICA", "QUERY", "BINARY", "PERSIST", "ALL",
                    ],
                ) || is_read_only_setting(self.text(p + 1));
                i
            }
            "SHOW" => info(K::Show, first).rows(),
            "DESCRIBE" | "DESC" => {
                let mut i = info(K::Describe, first).rows();
                i.target = self.name_at(p + 1);
                i
            }
            "EXPLAIN" => self.explain(p, first, depth),
            "USE" => info(K::Use, first),
            "CALL" | "EXEC" | "EXECUTE" => {
                let mut i = info(K::Call, first).write().rows();
                i.target = self.name_at(p + 1);
                i
            }
            "DO" => info(K::Call, first).write(),
            "COPY" => {
                let mut i = info(K::Copy, first);
                let to_stdout = self
                    .find_top(p + 1, |j| self.w(j, "TO"))
                    .is_some_and(|j| self.any(j + 1, &["STDOUT"]));
                i.is_write = !to_stdout;
                if self.kind(p + 1) != Some(TokenKind::LParen) {
                    i.target = self.name_at(p + 1);
                } else {
                    // `COPY (DELETE FROM t RETURNING *) TO STDOUT` runs the statement.
                    let inner = self
                        .sub(p + 2, self.close_paren(p + 1))
                        .classify(0, depth + 1);
                    i.is_write |= inner.is_write;
                    i.danger = inner.danger;
                }
                i
            }
            "LOAD" | "IMPORT" => info(K::Copy, first).write(),
            "VACUUM" | "ANALYZE" | "ANALYSE" | "REINDEX" | "CLUSTER" | "CHECKPOINT" | "REFRESH"
            | "FLUSH" => info(K::Maintenance, first).write(),
            "OPTIMIZE" | "CHECK" | "REPAIR" | "CHECKSUM" => {
                info(K::Maintenance, first).write().rows()
            }
            "PRAGMA" => self.pragma(p, first),
            "LOCK" => {
                let mut i = info(K::Lock, first).write();
                i.target = self.name_at(self.skip_words(p + 1, &["TABLE", "TABLES"]));
                i
            }
            "UNLOCK" => info(K::Lock, first),
            "PREPARE" => match self.find_top(p + 1, |j| self.w(j, "AS")) {
                Some(a) if !self.w(p + 1, "TRANSACTION") => {
                    let inner = self.classify(a + 1, depth + 1);
                    let mut i = info(K::Other, first);
                    i.is_write = inner.is_write;
                    i
                }
                _ => info(K::Other, first).write(),
            },
            "DECLARE" => {
                if self.kind(p + 1) == Some(TokenKind::Variable) {
                    return info(K::Other, first);
                }
                match self.find_top(p + 1, |j| self.w(j, "FOR")) {
                    Some(f) if self.has_top_word(p + 1, "CURSOR") => {
                        let inner = self.classify(f + 1, depth + 1);
                        let mut i = info(K::Other, first);
                        i.is_write = inner.is_write;
                        i
                    }
                    _ => info(K::Other, first).write(),
                }
            }
            "FETCH" | "MOVE" => info(K::Other, first).rows(),
            // `DISCARD ALL` runs `RESET ALL`.
            "DISCARD" => {
                let mut i = info(K::Other, first);
                i.is_write = !self.any(p + 1, &["PLANS", "SEQUENCES", "TEMP", "TEMPORARY"]);
                i
            }
            // `WAITFOR (RECEIVE … FROM queue)` takes messages off a queue.
            "WAITFOR" => {
                let mut i = info(K::Other, first);
                i.is_write = self.kind(p + 1) == Some(TokenKind::LParen);
                i
            }
            "CLOSE" | "DEALLOCATE" | "LISTEN" | "UNLISTEN" | "PRINT" | "RAISERROR" | "THROW" => {
                info(K::Other, first)
            }
            _ => info(K::Other, first).write(),
        }
    }

    fn select(&self, p: usize, first: String) -> StatementInfo {
        let mut i = StatementInfo::new(StatementKind::Select, first).rows();
        if let Some(into) = self.find_top(p + 1, |j| self.w(j, "INTO")) {
            i.returns_rows = false;
            // `INTO @v` (MySQL / T-SQL variables) only reads; `${name}` and `:name` are
            // replaced by any text before the statement runs.
            let to_variable = self.kind(into + 1) == Some(TokenKind::Variable)
                && !self.text(into + 1).starts_with("${");
            if !to_variable {
                // PostgreSQL / T-SQL `SELECT ... INTO new_table`, MySQL `INTO OUTFILE`.
                i.is_write = true;
                if !self.any(into + 1, &["OUTFILE", "DUMPFILE"]) {
                    i.target = self.name_at(into + 1);
                }
            }
        }
        // Row locking clauses are rejected in read-only transactions.
        let locking = self.find_top(p + 1, |j| {
            (self.w(j, "FOR")
                && (self.any(j + 1, &["UPDATE", "SHARE"]) || (self.any(j + 1, &["NO", "KEY"]))))
                || (self.w(j, "LOCK") && self.w(j + 1, "IN"))
        });
        if locking.is_some() || self.lifts_read_only(p + 1) {
            i.is_write = true;
        }
        if i.target.is_none() {
            i.target = self.top_from_target(p + 1);
        }
        i
    }

    fn with(&self, p: usize, depth: u32) -> StatementInfo {
        let mut j = p + 1;
        if self.w(j, "RECURSIVE") {
            j += 1;
        }
        let mut cte_write = false;
        let mut cte_danger = None;
        // name [(cols)] AS [NOT] [MATERIALIZED] ( body ) [, ...]
        loop {
            if !matches!(
                self.kind(j),
                Some(TokenKind::Identifier | TokenKind::Keyword | TokenKind::QuotedIdentifier)
            ) {
                break;
            }
            j += 1;
            if self.kind(j) == Some(TokenKind::LParen) {
                j = self.close_paren(j) + 1;
            }
            if !self.w(j, "AS") {
                break;
            }
            j += 1;
            j = self.skip_words(j, &["NOT", "MATERIALIZED"]);
            if self.kind(j) != Some(TokenKind::LParen) {
                break;
            }
            let close = self.close_paren(j);
            let body = self.sub(j + 1, close).classify(0, depth + 1);
            cte_write |= body.is_write;
            if cte_danger.is_none() {
                cte_danger = body.danger;
            }
            j = close + 1;
            if self.kind(j) == Some(TokenKind::Comma) {
                j += 1;
                continue;
            }
            break;
        }
        let mut main = if j < self.t.len() {
            self.classify(j, depth + 1)
        } else {
            StatementInfo::new(StatementKind::Select, String::new()).rows()
        };
        if main.kind == StatementKind::Other && main.first_keyword.is_empty() {
            main.kind = StatementKind::Select;
        }
        main.first_keyword = "WITH".into();
        main.is_write |= cte_write;
        if main.danger.is_none() {
            main.danger = cte_danger;
        }
        main
    }

    fn set(&self, p: usize, first: String) -> StatementInfo {
        use StatementKind as K;
        let n = p + 1;
        let tx = self.w(n, "TRANSACTION")
            || (self.any(n, &["SESSION", "LOCAL", "GLOBAL"])
                && (self.w(n + 1, "TRANSACTION") || self.w(n + 1, "CHARACTERISTICS")));
        if tx {
            let mut i = StatementInfo::new(K::Transaction(TxCommand::SetTransaction), first);
            // READ WRITE lifts the read-only session of a read-only connection.
            i.is_write = self.w(n, "GLOBAL") || self.read_write(n);
            return i;
        }
        let global = self.any(n, &["GLOBAL", "PERSIST", "PERSIST_ONLY", "PASSWORD"])
            || self.text(n).to_ascii_lowercase().starts_with("@@global")
            || self.text(n).to_ascii_lowercase().starts_with("@@persist");
        // `SET default_transaction_read_only = off`, `SET SESSION transaction_read_only = 0`:
        // turning a read-only setting off (a value that is not plainly "on" counts as off).
        let read_only_setting = (n..self.t.len()).any(|k| {
            if !is_read_only_setting(self.text(k)) {
                return false;
            }
            let v = if self.text(k + 1) == "=" || self.w(k + 1, "TO") {
                k + 2
            } else {
                k + 1
            };
            let value = self.text(v).trim_matches(['\'', '"']).to_ascii_lowercase();
            !matches!(value.as_str(), "on" | "1" | "true" | "yes")
        });
        let mut i = StatementInfo::new(K::Set, first);
        i.is_write = global || read_only_setting;
        i
    }

    /// `READ WRITE` from token `from` on.
    fn read_write(&self, from: usize) -> bool {
        (from..self.t.len()).any(|k| self.w(k, "READ") && self.w(k + 1, "WRITE"))
    }

    /// A `set_config('default_transaction_read_only', …)` call from token `from` on
    /// (or one whose setting is not a plain string): it lifts the read-only session
    /// of a read-only connection like `SET` does.
    fn lifts_read_only(&self, from: usize) -> bool {
        (from..self.t.len()).any(|k| {
            if self.kind(k + 1) != Some(TokenKind::LParen)
                || !self
                    .ident_text(k)
                    .is_some_and(|n| n.eq_ignore_ascii_case("set_config"))
            {
                return false;
            }
            let name = self.text(k + 2);
            let plain = self.kind(k + 2) == Some(TokenKind::String)
                && self.kind(k + 3) == Some(TokenKind::Comma)
                && name.len() >= 2
                && name.starts_with('\'')
                && name.ends_with('\'');
            !plain || is_read_only_setting(&name[1..name.len() - 1])
        })
    }

    fn explain(&self, p: usize, first: String, depth: u32) -> StatementInfo {
        let mut j = p + 1;
        let mut analyze = false;
        if self.kind(j) == Some(TokenKind::LParen) {
            let close = self.close_paren(j);
            for k in j + 1..close {
                if self.any(k, &["ANALYZE", "ANALYSE"])
                    && !self.any(k + 1, &["FALSE", "OFF", "0"])
                    && self.text(k + 1) != "0"
                {
                    analyze = true;
                }
            }
            j = close + 1;
        }
        const OPTS: &[&str] = &[
            "ANALYZE",
            "ANALYSE",
            "VERBOSE",
            "COSTS",
            "BUFFERS",
            "FORMAT",
            "QUERY",
            "PLAN",
            "EXTENDED",
            "PARTITIONS",
            "FOR",
            "JSON",
            "TEXT",
            "XML",
            "YAML",
            "TREE",
            "TRADITIONAL",
            "PIPELINE",
            "AST",
            "SYNTAX",
            "ESTIMATE",
        ];
        for _ in 0..12 {
            if self.any(j, &["ANALYZE", "ANALYSE"]) {
                analyze = true;
                j += 1;
            } else if self.any(j, OPTS) || matches!(self.kind(j), Some(TokenKind::Operator)) {
                j += 1;
            } else {
                break;
            }
        }
        let mut i = StatementInfo::new(StatementKind::Explain, first).rows();
        if j >= self.t.len() {
            return i;
        }
        let inner = self.classify(j, depth + 1);
        if inner.kind == StatementKind::Other
            && !inner.first_keyword.is_empty()
            && self.kind(j + 1).is_none()
        {
            // MySQL `EXPLAIN tbl` describes a table.
            i.target = self.name_at(j);
            return i;
        }
        i.target = inner.target.clone();
        if analyze {
            i.is_write = inner.is_write;
            i.danger = inner.danger;
        }
        i
    }

    fn pragma(&self, p: usize, first: String) -> StatementInfo {
        let mut i = StatementInfo::new(StatementKind::Pragma, first);
        let assigns = self
            .find_top(p + 1, |j| {
                self.kind(j) == Some(TokenKind::Operator) && self.text(j) == "="
            })
            .is_some();
        // pragma name may be schema-qualified: main.table_info(x)
        let mut n = p + 1;
        if self.kind(n + 1) == Some(TokenKind::Dot) {
            n += 2;
        }
        let name = self.text(n).to_ascii_lowercase();
        let has_arg = self.kind(n + 1) == Some(TokenKind::LParen);
        // Without an argument these still change the database file.
        let writes = ["optimize", "incremental_vacuum", "wal_checkpoint"].contains(&name.as_str());
        if assigns || writes || (has_arg && !QUERY_PRAGMAS.contains(&name.as_str())) {
            i.is_write = true;
        } else {
            i.returns_rows = true;
        }
        i
    }

    /// `CREATE/ALTER/DROP [modifiers] KIND [IF [NOT] EXISTS] [ONLY] name`.
    fn object_name_after_kind(&self, from: usize) -> Option<QualifiedName> {
        const KINDS: &[&str] = &[
            "TABLE",
            "VIEW",
            "INDEX",
            "SEQUENCE",
            "SCHEMA",
            "DATABASE",
            "FUNCTION",
            "PROCEDURE",
            "PROC",
            "TRIGGER",
            "TYPE",
            "DOMAIN",
            "EXTENSION",
            "ROLE",
            "USER",
            "EVENT",
            "RULE",
            "POLICY",
            "TABLESPACE",
            "SERVER",
            "PUBLICATION",
            "SUBSCRIPTION",
            "COLLATION",
            "AGGREGATE",
            "OPERATOR",
            "SYNONYM",
            "PACKAGE",
            "MACRO",
        ];
        let kind = (from..(from + 10).min(self.t.len())).find(|&j| self.any(j, KINDS))?;
        let mut j = self.skip_if_exists(kind + 1);
        j = self.skip_words(j, &["CONCURRENTLY", "ONLY"]);
        j = self.skip_if_exists(j);
        if self.w(j, "ON") {
            return None;
        }
        self.name_at(j)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use StatementKind as K;

    fn c(sql: &str) -> StatementInfo {
        classify(sql, &pg())
    }
    fn cm(sql: &str) -> StatementInfo {
        classify(sql, &mysql())
    }

    #[test]
    fn later_tsql_statements_and_read_write() {
        let t = |sql: &str| classify(sql, &tsql());
        for sql in [
            "SELECT 1\nDELETE FROM t",
            "SET NOCOUNT ON\nUPDATE t SET a = 1",
            "DECLARE @x INT\nDROP TABLE t",
            "PRINT 'x'\nTRUNCATE TABLE t",
        ] {
            assert!(t(sql).is_write, "{sql}");
        }
        assert!(!t("SELECT a FROM t WHERE b IN (SELECT b FROM u)").is_write);
        assert!(!t("SELECT * FROM a INNER MERGE JOIN b ON a.id = b.id").is_write);
        // Each statement of a run is checked on its own.
        assert_eq!(
            t("UPDATE orders SET status = 'x'\nDELETE FROM log WHERE id = 5").danger,
            Some(Danger::UpdateWithoutWhere)
        );
        assert_eq!(
            t("UPDATE t SET a = 1 WHERE id = 2\nDELETE FROM log").danger,
            Some(Danger::DeleteWithoutWhere)
        );
        assert_eq!(
            t("SELECT 1\nUPDATE t SET a = 1 WHERE id = 2\nDELETE FROM log").danger,
            Some(Danger::DeleteWithoutWhere)
        );
        assert_eq!(
            t("UPDATE t SET a = 1 WHERE id = 2\nSELECT * FROM t").danger,
            None
        );
        assert_eq!(
            t("UPDATE t WITH (ROWLOCK) SET a = 1 WHERE id = 2").danger,
            None
        );
        assert_eq!(t("INSERT INTO t EXEC dbo.p").danger, None);
        assert_eq!(t("INSERT INTO t SELECT a FROM u WHERE b = 1").danger, None);
        assert_eq!(t("MERGE t USING s ON t.id = s.id WHEN MATCHED THEN DELETE WHEN NOT MATCHED THEN INSERT (id) VALUES (s.id);").danger, None);
        assert!(c("SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE").is_write);
        assert!(c("SET default_transaction_read_only = off").is_write);
        assert!(cm("SET SESSION transaction_read_only = 0").is_write);
        assert!(!c("SET default_transaction_read_only = on").is_write);
        assert!(!c("SET my_app.read_only_mode = 'x'").is_write);
        assert!(c("BEGIN READ WRITE").is_write);
        assert!(c("START TRANSACTION READ WRITE").is_write);
        assert!(cm("SET SESSION TRANSACTION READ WRITE").is_write);
        assert!(
            !c("BEGIN").is_write && !c("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE").is_write
        );
        assert!(cm("SELECT * FROM users /*!INTO OUTFILE '/tmp/x.csv'*/").is_write);
        assert!(!cm("SELECT 1 /*!50000 , 2 */").is_write);
    }

    #[test]
    fn queries() {
        let i = c("SELECT * FROM public.users WHERE id = 1");
        assert_eq!(i.kind, K::Select);
        assert!(i.returns_rows && !i.is_write && i.danger.is_none());
        assert_eq!(i.first_keyword, "SELECT");
        assert_eq!(
            i.target,
            Some(QualifiedName {
                catalog: None,
                schema: Some("public".into()),
                name: "users".into()
            })
        );
        let i = c("-- comment\n/* x */ ((SELECT 1)) UNION (SELECT 2)");
        assert_eq!(
            (i.kind, i.returns_rows, i.is_write),
            (K::Select, true, false)
        );
        assert_eq!(c("VALUES (1), (2)").kind, K::Select);
        let i = c("TABLE users");
        assert_eq!((i.kind, i.returns_rows), (K::Select, true));
        assert_eq!(i.target.map(|t| t.name).as_deref(), Some("users"));
        let i = c("WITH x AS (SELECT 1) SELECT * FROM x");
        assert_eq!(
            (i.kind, i.returns_rows, i.is_write, i.first_keyword.as_str()),
            (K::Select, true, false, "WITH")
        );
        let i =
            c("WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t) SELECT * FROM t");
        assert!(!i.is_write && i.returns_rows);
        for q in [
            "SHOW search_path",
            "DESCRIBE users",
            "DESC users",
            "EXPLAIN SELECT 1",
            "EXPLAIN DELETE FROM t",
        ] {
            let i = cm(q);
            assert!(i.returns_rows && !i.is_write, "{q}: {i:?}");
        }
    }

    #[test]
    fn select_into_and_locking() {
        let i = c("SELECT * INTO new_table FROM t");
        assert!(i.is_write && !i.returns_rows);
        assert_eq!(i.target.map(|t| t.name).as_deref(), Some("new_table"));
        let i = cm("SELECT a INTO @v FROM t LIMIT 1");
        assert!(!i.is_write && !i.returns_rows);
        assert!(cm("SELECT * FROM t INTO OUTFILE '/tmp/x'").is_write);
        assert!(c("SELECT * FROM t FOR UPDATE").is_write);
        assert!(c("SELECT * FROM t FOR NO KEY UPDATE SKIP LOCKED").is_write);
        // INTO inside a subquery does not count
        assert!(!c("SELECT (SELECT 1) FROM t").is_write);
    }

    #[test]
    fn writes_and_cte_writes() {
        let i = c("INSERT INTO s.t (a) VALUES (1) RETURNING id");
        assert_eq!(
            (i.kind, i.is_write, i.returns_rows),
            (K::Insert, true, true)
        );
        assert_eq!(
            i.target,
            Some(QualifiedName {
                catalog: None,
                schema: Some("s".into()),
                name: "t".into()
            })
        );
        let i = cm("REPLACE INTO t VALUES (1)");
        assert_eq!((i.kind, i.is_write), (K::Insert, true));
        let i = c("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d");
        assert!(i.is_write && i.returns_rows);
        assert_eq!(i.danger, Some(Danger::DeleteWithoutWhere));
        let i = c("WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x");
        assert_eq!(
            (i.kind, i.is_write, i.first_keyword.as_str()),
            (K::Insert, true, "WITH")
        );
        let i = c("WITH x AS (SELECT id FROM a) UPDATE t SET v = 1 FROM x WHERE t.id = x.id");
        assert_eq!((i.kind, i.is_write, i.danger), (K::Update, true, None));
        let i = c("MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET v = s.v");
        assert_eq!((i.kind, i.is_write), (K::Merge, true));
        assert_eq!(i.target.map(|t| t.name).as_deref(), Some("t"));
    }

    #[test]
    fn dangers() {
        assert_eq!(c("DELETE FROM t").danger, Some(Danger::DeleteWithoutWhere));
        assert_eq!(c("DELETE FROM t WHERE id = 1").danger, None);
        assert_eq!(
            c("delete from t where id in (select id from u)").danger,
            None
        );
        assert_eq!(
            c("UPDATE t SET a = 1").danger,
            Some(Danger::UpdateWithoutWhere)
        );
        assert_eq!(
            c("UPDATE t SET a = (SELECT b FROM u WHERE u.id = t.id)").danger,
            Some(Danger::UpdateWithoutWhere)
        );
        assert_eq!(c("UPDATE t SET a = 1 WHERE id = 2").danger, None);
        assert_eq!(
            cm("DELETE FROM t LIMIT 10").danger,
            Some(Danger::DeleteWithoutWhere)
        );
        assert_eq!(c("DROP TABLE IF EXISTS a, b").danger, Some(Danger::Drop));
        assert_eq!(
            c("DROP TABLE IF EXISTS a, b")
                .target
                .map(|t| t.name)
                .as_deref(),
            Some("a")
        );
        assert_eq!(c("TRUNCATE TABLE t").danger, Some(Danger::Truncate));
        assert_eq!(c("TRUNCATE t").target.map(|t| t.name).as_deref(), Some("t"));
        assert_eq!(
            c("ALTER TABLE t DROP COLUMN x").danger,
            Some(Danger::AlterDrop)
        );
        assert_eq!(c("ALTER TABLE t ALTER COLUMN x DROP NOT NULL").danger, None);
        assert_eq!(c("ALTER TABLE t ALTER COLUMN x DROP DEFAULT").danger, None);
        assert_eq!(c("ALTER TABLE t ADD COLUMN y int").danger, None);
        assert_eq!(
            c("EXPLAIN ANALYZE DELETE FROM t").danger,
            Some(Danger::DeleteWithoutWhere)
        );
        assert_eq!(c("EXPLAIN DELETE FROM t").danger, None);
    }

    #[test]
    fn explain_analyze() {
        let i = c("EXPLAIN ANALYZE DELETE FROM t WHERE id = 1");
        assert_eq!(
            (i.kind, i.is_write, i.returns_rows),
            (K::Explain, true, true)
        );
        assert!(c("EXPLAIN (ANALYZE, BUFFERS) UPDATE t SET a = 1 WHERE b = 2").is_write);
        assert!(!c("EXPLAIN (ANALYZE false) UPDATE t SET a = 1 WHERE b = 2").is_write);
        assert!(!c("EXPLAIN ANALYZE SELECT 1").is_write);
        assert!(!c("EXPLAIN UPDATE t SET a = 1").is_write);
        assert!(!cm("EXPLAIN FORMAT=JSON SELECT 1").is_write);
        let i = cm("EXPLAIN users");
        assert_eq!(i.target.map(|t| t.name).as_deref(), Some("users"));
        assert!(!classify("EXPLAIN QUERY PLAN SELECT 1", &sqlite()).is_write);
    }

    #[test]
    fn transactions_and_session() {
        for (q, tx) in [
            ("BEGIN", TxCommand::Begin),
            (
                "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE",
                TxCommand::Begin,
            ),
            ("START TRANSACTION READ ONLY", TxCommand::Begin),
            ("COMMIT", TxCommand::Commit),
            ("END", TxCommand::Commit),
            ("ROLLBACK TO SAVEPOINT a", TxCommand::Rollback),
            ("SAVEPOINT a", TxCommand::Savepoint),
            ("RELEASE SAVEPOINT a", TxCommand::Release),
            (
                "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
                TxCommand::SetTransaction,
            ),
            (
                "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
                TxCommand::SetTransaction,
            ),
        ] {
            let i = c(q);
            assert_eq!(i.kind, K::Transaction(tx), "{q}");
            assert!(!i.is_write && i.is_transaction_control(), "{q}");
        }
        // A block that merely starts with BEGIN is not a transaction
        let i = classify("BEGIN INSERT INTO t VALUES (1); END", &tsql());
        assert!(i.is_write && i.kind == K::Other);
        let i = c("SET search_path TO app");
        assert_eq!((i.kind, i.is_write), (K::Set, false));
        assert!(cm("SET GLOBAL max_connections = 10").is_write);
        assert!(cm("SET @@global.sql_mode = ''").is_write);
        assert!(!cm("SET @x = 1").is_write);
        assert!(cm("SET PASSWORD = 'x'").is_write);
        let i = cm("USE shop");
        assert_eq!((i.kind, i.is_write), (K::Use, false));
        // `RESET ALL` lifts a read-only session (it used to count as no write).
        assert!(c("RESET ALL").is_write);
        assert!(!c("RESET search_path").is_write);
        assert!(cm("RESET MASTER").is_write);
    }

    #[test]
    fn lifting_read_only_is_a_write() {
        for q in [
            "RESET ALL",
            "reset default_transaction_read_only",
            "RESET transaction_read_only",
            "DISCARD ALL",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE, READ WRITE",
            "SET default_transaction_read_only = off",
            "SET LOCAL transaction_read_only TO false",
            "SET default_transaction_read_only TO DEFAULT",
            "BEGIN ISOLATION LEVEL SERIALIZABLE, READ WRITE",
            "SELECT set_config('default_transaction_read_only', 'off', false)",
            "SELECT pg_catalog.set_config('transaction_read_only', 'off', true)",
            "SELECT \"set_config\"('default_transaction_read_only', 'off', false)",
            "SELECT set_config(current_setting('app.name'), 'off', false)",
            "SELECT set_config('default_transaction_' || 'read_only', 'off', false)",
            "VALUES (set_config('default_transaction_read_only', 'off', false))",
            "WITH x AS (SELECT set_config('default_transaction_read_only', 'off', false)) SELECT * FROM x",
            // ClickHouse
            "SET readonly = 0",
        ] {
            assert!(c(q).is_write, "{q}");
        }
        for q in [
            "RESET search_path",
            "RESET ROLE",
            "DISCARD PLANS",
            "DISCARD TEMP",
            "SELECT set_config('app.user_id', '5', false)",
            "SET default_transaction_read_only = on",
            "SET readonly = 1",
        ] {
            assert!(!c(q).is_write, "{q}");
        }
        for q in [
            "SET SESSION TRANSACTION READ WRITE",
            "SET TRANSACTION READ WRITE",
            "SET transaction_read_only=0",
            "SET @@session.transaction_read_only = OFF",
            "SET @@transaction_read_only = 0",
            "SET SESSION tx_read_only = 0",
            "START TRANSACTION READ WRITE",
        ] {
            assert!(cm(q).is_write, "{q}");
        }
        assert!(!cm("SET SESSION TRANSACTION READ ONLY").is_write);
        assert!(!cm("SET transaction_read_only = 1").is_write);
        let s = sqlite();
        for q in [
            "PRAGMA query_only = 0",
            "PRAGMA query_only = OFF",
            "PRAGMA query_only(false)",
            "PRAGMA main.query_only = 0",
            "PRAGMA optimize",
            "PRAGMA incremental_vacuum",
            "PRAGMA wal_checkpoint(TRUNCATE)",
        ] {
            assert!(classify(q, &s).is_write, "{q}");
        }
        assert!(!classify("PRAGMA query_only", &s).is_write);
    }

    #[test]
    fn hidden_writes() {
        // Several statements given as one: any write counts.
        let i = c("SELECT 1; DELETE FROM t");
        assert_eq!(
            (i.kind, i.is_write, i.danger),
            (K::Select, true, Some(Danger::DeleteWithoutWhere))
        );
        assert!(!c("SELECT 1; SELECT 2;").is_write);
        assert_eq!(
            classify("PRINT 'x'; DELETE FROM t", &tsql()).danger,
            Some(Danger::DeleteWithoutWhere)
        );
        assert_eq!(c("DELETE FROM t WHERE a = 1;").danger, None);
        // A body's statements are not the statement's dangers.
        let i = cm("CREATE PROCEDURE p() BEGIN UPDATE t SET a = 1; DELETE FROM u; END");
        assert_eq!((i.kind, i.is_write, i.danger), (K::Create, true, None));
        // COPY of a data-modifying query runs it.
        let i = c("COPY (DELETE FROM t RETURNING *) TO STDOUT");
        assert_eq!(
            (i.is_write, i.danger),
            (true, Some(Danger::DeleteWithoutWhere))
        );
        assert!(
            c("COPY (WITH d AS (UPDATE t SET a = 1 RETURNING a) SELECT * FROM d) TO STDOUT")
                .is_write
        );
        assert!(!c("COPY (SELECT * FROM t) TO STDOUT").is_write);
        assert!(c("COPY t FROM PROGRAM 'curl x'").is_write);
        assert!(c("COPY t TO PROGRAM 'cat > /tmp/x'").is_write);
        // INTO a template variable or parameter: replaced by any text before running.
        assert!(c("SELECT * INTO ${target} FROM t").is_write);
        assert!(c("SELECT * INTO :target FROM t").is_write);
        assert!(!cm("SELECT a INTO @v FROM t").is_write);
        // MySQL / MariaDB executable comments, also spread over several.
        assert!(cm("SELECT * FROM t /*!INTO*/ /*!OUTFILE '/tmp/x'*/").is_write);
        assert!(cm("SELECT * FROM t /*!INTO*/ OUTFILE '/tmp/x'").is_write);
        assert!(cm("/*!SELECT 1*/ /*!INTO OUTFILE '/tmp/x'*/").is_write);
        assert!(cm("SELECT * FROM t /*M!INTO OUTFILE '/tmp/x'*/").is_write);
        assert!(cm("/*M!100100 DROP TABLE t */").is_write);
        assert!(!cm("/*!40101 SET NAMES utf8 */ /*!40103 SET TIME_ZONE='+00:00' */").is_write);
        // T-SQL
        let t = |sql: &str| classify(sql, &tsql());
        assert!(t("WAITFOR (RECEIVE TOP (1) * FROM q)").is_write);
        assert!(!t("WAITFOR DELAY '00:00:01'").is_write);
        assert!(t("SELECT 1\nDISABLE TRIGGER trg ON t").is_write);
        assert!(t("SELECT 1\nCHECKPOINT").is_write);
        assert!(!t("SELECT enable, disable FROM t").is_write);
        // Unknown statements stay writes.
        assert!(cm("HANDLER t OPEN").is_write);
        assert!(t("sp_who").is_write);
        assert!(c("NOTIFY ch").is_write);
    }

    #[test]
    fn calls_copy_maintenance_pragma_lock() {
        let i = c("CALL do_things(1)");
        assert_eq!((i.kind, i.is_write), (K::Call, true));
        assert_eq!(i.target.map(|t| t.name).as_deref(), Some("do_things"));
        assert!(classify("EXEC dbo.p @a = 1", &tsql()).is_write);
        assert!(c("DO $$ BEGIN END $$").is_write);
        assert!(c("COPY t FROM '/tmp/x.csv'").is_write);
        assert!(c("COPY t FROM STDIN").is_write);
        assert!(!c("COPY (SELECT 1) TO STDOUT").is_write);
        assert!(c("COPY t TO '/tmp/out.csv'").is_write);
        for q in [
            "VACUUM ANALYZE t",
            "ANALYZE t",
            "REINDEX TABLE t",
            "CLUSTER t",
            "REFRESH MATERIALIZED VIEW v",
        ] {
            let i = c(q);
            assert_eq!((i.kind, i.is_write), (K::Maintenance, true), "{q}");
        }
        for q in ["OPTIMIZE TABLE t", "CHECK TABLE t", "REPAIR TABLE t"] {
            assert_eq!(cm(q).kind, K::Maintenance, "{q}");
        }
        let s = sqlite();
        let i = classify("PRAGMA foreign_keys = ON", &s);
        assert_eq!(
            (i.kind, i.is_write, i.returns_rows),
            (K::Pragma, true, false)
        );
        let i = classify("PRAGMA table_info(users)", &s);
        assert_eq!((i.is_write, i.returns_rows), (false, true));
        let i = classify("PRAGMA journal_mode", &s);
        assert_eq!((i.is_write, i.returns_rows), (false, true));
        assert!(classify("PRAGMA cache_size(1000)", &s).is_write);
        let i = c("LOCK TABLE t IN ACCESS EXCLUSIVE MODE");
        assert_eq!((i.kind, i.is_write), (K::Lock, true));
        assert!(!cm("UNLOCK TABLES").is_write);
    }

    #[test]
    fn ddl_targets() {
        let t = |q: &str| c(q).target.map(|t| (t.schema, t.name));
        assert_eq!(
            t("CREATE TABLE IF NOT EXISTS app.items (id int)"),
            Some((Some("app".into()), "items".into()))
        );
        assert_eq!(
            t("CREATE OR REPLACE VIEW v AS SELECT 1"),
            Some((None, "v".into()))
        );
        assert_eq!(
            t("CREATE UNIQUE INDEX CONCURRENTLY idx ON t (a)"),
            Some((None, "idx".into()))
        );
        assert_eq!(t("CREATE INDEX ON t (a)"), None);
        assert_eq!(
            t("CREATE TEMP TABLE \"Odd Name\" (a int)"),
            Some((None, "Odd Name".into()))
        );
        assert_eq!(
            t("DROP VIEW IF EXISTS s.v CASCADE"),
            Some((Some("s".into()), "v".into()))
        );
        assert_eq!(
            t("ALTER TABLE ONLY s.t ADD COLUMN x int"),
            Some((Some("s".into()), "t".into()))
        );
        assert_eq!(c("CREATE TABLE t (a int)").kind, K::Create);
        assert_eq!(c("COMMENT ON TABLE t IS 'x'").kind, K::Comment);
        assert_eq!(c("GRANT SELECT ON t TO bob").kind, K::Grant);
        assert_eq!(c("REVOKE ALL ON t FROM bob").kind, K::Revoke);
        assert_eq!(cm("RENAME TABLE a TO b").kind, K::Rename);
        let i = classify("SELECT * FROM db.dbo.t", &tsql());
        assert_eq!(
            i.target,
            Some(QualifiedName {
                catalog: Some("db".into()),
                schema: Some("dbo".into()),
                name: "t".into()
            })
        );
    }

    #[test]
    fn special_statements() {
        let i = c("@set x = 1");
        assert_eq!(
            (i.kind, i.is_write, i.first_keyword.as_str()),
            (K::ClientCommand, false, "@SET")
        );
        let i = c("");
        assert_eq!((i.kind, i.is_write), (K::Other, false));
        let i = c("-- only a comment");
        assert!(!i.is_write);
        let i = cm("/*!40101 SET NAMES utf8 */");
        assert_eq!((i.kind, i.is_write), (K::Set, false));
        let i = cm("/*!40000 ALTER TABLE `t` DISABLE KEYS */");
        assert_eq!((i.kind, i.is_write), (K::Alter, true));
        // Unknown statements are writes
        assert!(c("FROBNICATE everything").is_write);
        assert!(c("1 + 1").is_write);
        let i = c("PREPARE q AS SELECT 1");
        assert!(!i.is_write);
        assert!(c("PREPARE q AS DELETE FROM t").is_write);
        assert!(!c("DECLARE c CURSOR FOR SELECT * FROM t").is_write);
        assert!(!classify("DECLARE @x INT", &tsql()).is_write);
        let i = c("FETCH 10 FROM c");
        assert!(i.returns_rows && !i.is_write);
        // SQL after a client command line in the same statement (client commands not split).
        let i = c("@set x = 1\nDELETE FROM t");
        assert_eq!((i.kind, i.is_write), (K::Delete, true));
        let off = SplitOptions {
            client_commands: false,
            ..Default::default()
        };
        let r = split_and_classify("@set x = 1\nDROP TABLE t;", &pg(), &off);
        assert!(r[0].1.is_write, "{r:?}");
        let i = c("@set a = 1\n@set b = 2");
        assert_eq!((i.kind, i.is_write), (K::ClientCommand, false));
    }

    #[test]
    fn split_and_classify_script() {
        let src = "SELECT 1;\nDELETE FROM t;\n@set a = 1\nUPDATE t SET x = 1 WHERE y = 2";
        let r = split_and_classify(src, &pg(), &SplitOptions::default());
        let kinds: Vec<_> = r.iter().map(|(_, i)| i.kind).collect();
        assert_eq!(
            kinds,
            vec![K::Select, K::Delete, K::ClientCommand, K::Update]
        );
        assert_eq!(r[1].1.danger, Some(Danger::DeleteWithoutWhere));
        assert_eq!(r[3].1.danger, None);
    }

    #[test]
    fn random_inputs_never_panic() {
        let mut rng = Rng::new(11);
        for _ in 0..500 {
            let src = random_sql(&mut rng, 30);
            for syn in all_syntaxes() {
                let _ = classify(&src, &syn);
            }
            for l in [
                ternarygrid_core::QueryLanguage::Mongo,
                ternarygrid_core::QueryLanguage::Search,
            ] {
                let _ = classify_command(&src, l);
                let _ = classify_command(&format!("POST /{src}"), l);
            }
        }
    }

    #[test]
    fn redis_mongo_and_rest_commands() {
        use ternarygrid_core::QueryLanguage as L;
        let c = |s: &str, l| classify_command(s, l).unwrap();
        assert!(!c("HGETALL user:1", L::Redis).is_write);
        assert!(c("SET a 1", L::Redis).is_write);
        assert_eq!(c("FLUSHDB", L::Redis).danger, Some(Danger::Truncate));
        assert!(!c("CLIENT LIST", L::Redis).is_write);
        assert!(!c("db.users.find({a: 1}).sort({b: 1})", L::Mongo).is_write);
        assert!(c("db.users.updateOne({_id: 1}, {$set: {a: 2}})", L::Mongo).is_write);
        assert_eq!(
            c("db.users.deleteMany({})", L::Mongo).danger,
            Some(Danger::DeleteWithoutWhere)
        );
        assert_eq!(c("db.users.deleteMany({a: 1})", L::Mongo).danger, None);
        assert_eq!(
            c("db.getCollection(\"x\").drop()", L::Mongo).danger,
            Some(Danger::Drop)
        );
        assert!(!c("GET /logs/_search", L::Search).is_write);
        assert!(!c("POST /logs/_search\n{}", L::Search).is_write);
        assert!(c("PUT /logs", L::Search).is_write);
        assert_eq!(c("DELETE /logs", L::Search).danger, Some(Danger::Drop));
        assert!(classify_command("SELECT 1", L::Search).is_none());
        assert!(classify_command("SELECT 1", L::Sql).is_none());
    }

    #[test]
    fn mongo_writes() {
        use ternarygrid_core::QueryLanguage as L;
        let w = |s: &str| classify_command(s, L::Mongo).unwrap().is_write;
        for s in [
            "db.orders.aggregate([{$match: {a: 1}}, {$out: \"copy\"}])",
            "db.orders.aggregate([{\"$merge\": {into: \"copy\"}}])",
            "db.orders.aggregate([{'$out': 'copy'}])",
            "db.orders.aggregate([{'\\u0024out': 'copy'}])",
            "db.x.insert({a: 1})",
            "db.x.update({}, {$set: {a: 1}})",
            "db.x.save({a: 1})",
            "db.x.findAndModify({query: {}, remove: true})",
            "db.x.mapReduce(m, r, {out: 'y'})",
            "db.createView('v', 'x', [])",
            "db.x.dropIndexes()",
            "db.x.createIndexes([{a: 1}])",
            "db.createUser({user: 'u', pwd: 'p', roles: []})",
            "db.dropUser('u')",
            "db.grantRolesToUser('u', ['root'])",
            "db.runCommand({ping: 1})",
            "db.x.deleteMany  ({})",
            "db.x.deleteMany /* ( */ ({})",
            "db.x // c\n  .deleteMany({})",
            "db.x . deleteMany({})",
            "db[\"x\"].drop()",
            "db.x[\"drop\"]()",
            "db.x['drop']()",
            "db.x.drop?.()",
            "db.x.drop.call(db.x)",
            "db.x.drop\u{a0}()",
            "db.getCollection(\"x\").drop()",
            "db.getSiblingDB('a').x.updateMany({}, {$set: {a: 1}})",
            "db.x.find({a: \"b\"}).forEach(d => db.y.insertOne(d))",
            "db.x.find({a: /\"/}).drop()",
            "sh.enableSharding('a')",
            "use shop",
        ] {
            assert!(w(s), "{s}");
        }
        for s in [
            "db.users.find({name: /a.drop(/i, n: 'x.drop()'}).sort({a: -1}).limit(5)",
            "db.orders.aggregate([{$match: {a: 1}}, {$group: {_id: '$a', n: {$sum: 1}}}, {$project: {m: {$mergeObjects: ['$a', '$b']}}}])",
            "db.x.countDocuments({at: {$gt: ISODate('2024-01-01')}, _id: ObjectId('65f0a1b2c3d4e5f6a7b8c9d0')})",
            "db.x.find().count()",
            "db.x.find({d: new Date()})",
            "db.getCollectionNames()",
            "db.getSiblingDB('a').getCollection('b').find() // .drop()",
            "show dbs",
        ] {
            assert!(!w(s), "{s}");
        }
        let d = |s: &str| classify_command(s, L::Mongo).unwrap().danger;
        assert_eq!(
            d("db.x.deleteMany  ( { } )"),
            Some(Danger::DeleteWithoutWhere)
        );
        assert_eq!(d("db.x.remove()"), Some(Danger::DeleteWithoutWhere));
        assert_eq!(
            d("db.x.updateMany({}, {$set: {a: 1}})"),
            Some(Danger::UpdateWithoutWhere)
        );
        assert_eq!(d("db[\"x\"].drop()"), Some(Danger::Drop));
        assert_eq!(d("db.x.updateMany({a: 1}, {$set: {a: 1}})"), None);
    }

    #[test]
    fn search_requests_match_whole_path_segments() {
        use ternarygrid_core::QueryLanguage as L;
        let w = |s: &str| classify_command(s, L::Search).unwrap().is_write;
        for s in [
            "POST /product_catalog/_doc\n{\"a\": 1}",
            "POST /my_stats/_doc",
            "POST /item_count/_doc",
            "POST /my_sql_logs/_doc",
            "POST /web_search/_doc",
            "POST /logs/_doc/_search",
            "POST /logs/_mapping\n{\"properties\": {}}",
            "PUT /logs/_mapping\n{}",
            "POST /logs/_delete_by_query\n{\"query\": {\"match_all\": {}}}",
            "POST /logs/_update_by_query",
            "POST /_bulk\n{}",
            "POST /_reindex\n{}",
            "POST /logs/%5Fdoc",
            "POST /logs/_search/../_doc",
            "POST /_cat/indices",
            "GET /logs/_refresh",
            "POST\n/logs/_search",
            "POST /_plugins/_sql\n{\"query\": \"DELETE FROM logs WHERE a = 1\"}",
        ] {
            assert!(w(s), "{s}");
        }
        for s in [
            "POST /logs/_search?q=_doc",
            "post /logs/_count",
            "POST /_search/scroll\n{}",
            "POST /_sql?format=txt\n{\"query\": \"SELECT * FROM logs\"}",
            "POST /_plugins/_sql\n{\"query\": \"SELECT 1\"}",
            "POST /_mget\n{}",
            "POST /.kibana/_search",
            "GET /product_catalog/_doc/1",
            "GET /logs/_mapping",
            "GET /_cat/indices?v",
            "HEAD /logs",
        ] {
            assert!(!w(s), "{s}");
        }
        assert_eq!(
            classify_command("POST /logs/_delete_by_query", L::Search)
                .unwrap()
                .danger,
            Some(Danger::DeleteWithoutWhere)
        );
        assert!(classify_command("GET", L::Search).is_none());
    }
}
