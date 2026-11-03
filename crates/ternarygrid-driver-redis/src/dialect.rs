//! Redis "dialect": commands instead of SQL. Keys are the navigator's tables;
//! reading one is the command of its type, paged by the driver.

use ternarygrid_core::DbKind;
use ternarygrid_core::ddl::*;
use ternarygrid_core::dialect::{Dialect, IdentCase, SqlSyntax};
use ternarygrid_core::meta::ObjectRef;
use ternarygrid_core::value::{DataKind, Value};

/// Commands for highlighting and completion.
pub(crate) const COMMANDS: &[&str] = &[
    "APPEND",
    "AUTH",
    "BITCOUNT",
    "BITPOS",
    "BLPOP",
    "BRPOP",
    "BZPOPMAX",
    "BZPOPMIN",
    "CLIENT",
    "CLUSTER",
    "COMMAND",
    "CONFIG",
    "COPY",
    "DBSIZE",
    "DECR",
    "DECRBY",
    "DEL",
    "DISCARD",
    "DUMP",
    "ECHO",
    "EVAL",
    "EVALSHA",
    "EXEC",
    "EXISTS",
    "EXPIRE",
    "EXPIREAT",
    "EXPIRETIME",
    "FLUSHALL",
    "FLUSHDB",
    "FUNCTION",
    "GEOADD",
    "GEODIST",
    "GEOPOS",
    "GEOSEARCH",
    "GET",
    "GETDEL",
    "GETEX",
    "GETRANGE",
    "GETSET",
    "HDEL",
    "HEXISTS",
    "HGET",
    "HGETALL",
    "HINCRBY",
    "HINCRBYFLOAT",
    "HKEYS",
    "HLEN",
    "HMGET",
    "HRANDFIELD",
    "HSCAN",
    "HSET",
    "HSETNX",
    "HSTRLEN",
    "HVALS",
    "INCR",
    "INCRBY",
    "INCRBYFLOAT",
    "INFO",
    "JSON.ARRAPPEND",
    "JSON.DEL",
    "JSON.GET",
    "JSON.SET",
    "JSON.TYPE",
    "KEYS",
    "LASTSAVE",
    "LATENCY",
    "LINDEX",
    "LINSERT",
    "LLEN",
    "LMOVE",
    "LPOP",
    "LPOS",
    "LPUSH",
    "LPUSHX",
    "LRANGE",
    "LREM",
    "LSET",
    "LTRIM",
    "MEMORY",
    "MGET",
    "MOVE",
    "MSET",
    "MSETNX",
    "MULTI",
    "OBJECT",
    "PERSIST",
    "PEXPIRE",
    "PEXPIREAT",
    "PFADD",
    "PFCOUNT",
    "PFMERGE",
    "PING",
    "PSETEX",
    "PTTL",
    "PUBLISH",
    "PUBSUB",
    "RANDOMKEY",
    "RENAME",
    "RENAMENX",
    "RESTORE",
    "RPOP",
    "RPUSH",
    "RPUSHX",
    "SADD",
    "SCAN",
    "SCARD",
    "SCRIPT",
    "SDIFF",
    "SDIFFSTORE",
    "SELECT",
    "SET",
    "SETEX",
    "SETNX",
    "SETRANGE",
    "SINTER",
    "SINTERCARD",
    "SISMEMBER",
    "SLOWLOG",
    "SMEMBERS",
    "SMISMEMBER",
    "SMOVE",
    "SORT",
    "SPOP",
    "SRANDMEMBER",
    "SREM",
    "SSCAN",
    "STRLEN",
    "SUNION",
    "SWAPDB",
    "TIME",
    "TOUCH",
    "TTL",
    "TYPE",
    "UNLINK",
    "WAIT",
    "XACK",
    "XADD",
    "XAUTOCLAIM",
    "XCLAIM",
    "XDEL",
    "XGROUP",
    "XINFO",
    "XLEN",
    "XPENDING",
    "XRANGE",
    "XREAD",
    "XREADGROUP",
    "XREVRANGE",
    "XTRIM",
    "ZADD",
    "ZCARD",
    "ZCOUNT",
    "ZDIFF",
    "ZINCRBY",
    "ZINTER",
    "ZLEXCOUNT",
    "ZMSCORE",
    "ZPOPMAX",
    "ZPOPMIN",
    "ZRANDMEMBER",
    "ZRANGE",
    "ZRANGEBYLEX",
    "ZRANGEBYSCORE",
    "ZRANK",
    "ZREM",
    "ZREMRANGEBYRANK",
    "ZREMRANGEBYSCORE",
    "ZREVRANGE",
    "ZREVRANGEBYSCORE",
    "ZREVRANK",
    "ZSCAN",
    "ZSCORE",
    "ZUNION",
    "ZUNIONSTORE",
    "COUNT",
    "MATCH",
    "TYPE",
    "WITHSCORES",
    "LIMIT",
    "NX",
    "XX",
    "EX",
    "PX",
    "KEEPTTL",
];

pub const SYNTAX: SqlSyntax = SqlSyntax {
    keywords: COMMANDS,
    functions: &[],
    data_types: &[
        "string",
        "hash",
        "list",
        "set",
        "zset",
        "stream",
        "ReJSON-RL",
    ],
    identifier_quotes: &[],
    double_quoted_strings: true,
    backslash_escapes: true,
    e_strings: false,
    n_strings: false,
    x_strings: false,
    dollar_quoting: false,
    hash_comments: true,
    nested_block_comments: false,
    dash_comment_needs_space: true,
    question_params: false,
    dollar_params: false,
    at_variables: false,
    batch_separator: None,
    delimiter_command: false,
    compound_blocks: false,
    plsql_blocks: false,
    newline_ends_statement: true,
    extra_ident_chars: ":.-{}*",
    ident_case: IdentCase::Preserve,
    case_sensitive_names: true,
};

/// A key or value as a command argument: bare when it is safe, else quoted.
pub(crate) fn arg(s: &str) -> String {
    let bare = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || ":._-{}/@|+=".contains(c));
    if bare {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Split a command line into arguments as `redis-cli` does: `"…"` with
/// escapes (`\n`, `\xHH` …), `'…'` literally.
pub(crate) fn split_args(line: &str) -> Result<Vec<Vec<u8>>, String> {
    let mut args = Vec::new();
    let mut chars = line.trim().chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else { break };
        let mut cur: Vec<u8> = Vec::new();
        let push = |cur: &mut Vec<u8>, c: char| {
            let mut buf = [0u8; 4];
            cur.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        };
        match first {
            '"' => {
                chars.next();
                let mut closed = false;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => {
                            closed = true;
                            break;
                        }
                        '\\' => match chars.next() {
                            Some('n') => cur.push(b'\n'),
                            Some('r') => cur.push(b'\r'),
                            Some('t') => cur.push(b'\t'),
                            Some('b') => cur.push(8),
                            Some('a') => cur.push(7),
                            Some('x') => {
                                let h: String =
                                    [chars.next(), chars.next()].into_iter().flatten().collect();
                                cur.push(
                                    u8::from_str_radix(&h, 16)
                                        .map_err(|_| format!("Invalid escape \\x{h}"))?,
                                );
                            }
                            Some(o) => push(&mut cur, o),
                            None => return Err("Unterminated quote".into()),
                        },
                        c => push(&mut cur, c),
                    }
                }
                if !closed {
                    return Err("Unbalanced quotes".into());
                }
            }
            '\'' => {
                chars.next();
                let mut closed = false;
                while let Some(c) = chars.next() {
                    if c == '\'' {
                        if chars.peek() == Some(&'\'') {
                            chars.next();
                            cur.push(b'\'');
                            continue;
                        }
                        closed = true;
                        break;
                    }
                    if c == '\\' && chars.peek() == Some(&'\'') {
                        chars.next();
                        cur.push(b'\'');
                        continue;
                    }
                    push(&mut cur, c);
                }
                if !closed {
                    return Err("Unbalanced quotes".into());
                }
            }
            _ => {
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() {
                        break;
                    }
                    push(&mut cur, c);
                    chars.next();
                }
            }
        }
        args.push(cur);
    }
    Ok(args)
}

/// Driver command returning the number of rows of the command after it
/// (`TERNARYGRID.COUNT LRANGE k 10 20`): the row count where no Redis
/// command gives it (`SCAN 0 MATCH …`, ranges, single replies).
pub(crate) const COUNT_COMMAND: &str = "TERNARYGRID.COUNT";

/// Rows read for each key type: the navigator keeps the type in the
/// object's signature.
pub(crate) fn read_command(key: &str, ty: Option<&str>) -> String {
    let k = arg(key);
    match ty.unwrap_or("") {
        "string" => format!("GET {k}"),
        "hash" => format!("HSCAN {k} 0 COUNT 1000"),
        "list" => format!("LRANGE {k} 0 -1"),
        "set" => format!("SSCAN {k} 0 COUNT 1000"),
        "zset" => format!("ZRANGE {k} 0 -1 WITHSCORES"),
        "stream" => format!("XRANGE {k} - +"),
        "ReJSON-RL" => format!("JSON.GET {k}"),
        _ => format!("TYPE {k}"),
    }
}

pub struct RedisDialect {
    ddl: RedisDdl,
}

impl RedisDialect {
    pub fn new() -> Self {
        RedisDialect { ddl: RedisDdl }
    }
}

impl Default for RedisDialect {
    fn default() -> Self {
        Self::new()
    }
}

impl Dialect for RedisDialect {
    fn kind(&self) -> DbKind {
        DbKind::Redis
    }
    fn syntax(&self) -> &SqlSyntax {
        &SYNTAX
    }
    fn ddl(&self) -> &dyn DdlGenerator {
        &self.ddl
    }
    fn quote_ident(&self, ident: &str) -> String {
        arg(ident)
    }
    fn force_quote_ident(&self, ident: &str) -> String {
        arg(ident)
    }
    fn needs_quoting(&self, ident: &str) -> bool {
        arg(ident) != ident
    }
    fn qualified(&self, _schema: Option<&str>, name: &str) -> String {
        arg(name)
    }
    fn string_literal(&self, s: &str) -> String {
        arg(s)
    }
    fn literal(&self, value: &Value, _type_name: Option<&str>) -> String {
        match value {
            Value::Null => "\"\"".into(),
            other => arg(&other.to_text()),
        }
    }
    /// The driver pages every reply itself.
    fn limit_query(&self, sql: &str, _limit: u64, _offset: u64) -> String {
        sql.to_string()
    }
    /// The number of rows the command returns: the size of the key (or of
    /// the database) when the command reads all of it, else counted by the
    /// driver ([`COUNT_COMMAND`]).
    fn count_query(&self, sql: &str) -> String {
        let sql = sql.trim();
        let counted = || format!("{COUNT_COMMAND} {sql}");
        let Ok(args) = split_args(sql) else {
            return counted();
        };
        // The key, when it survives the round trip through text.
        let Some(key) = args
            .get(1)
            .and_then(|k| std::str::from_utf8(k).ok())
            .map(arg)
        else {
            return counted();
        };
        let word = |i: usize| {
            args.get(i)
                .map(|a| String::from_utf8_lossy(a).to_ascii_uppercase())
                .unwrap_or_default()
        };
        let n = args.len();
        // A scan from cursor 0 without MATCH / TYPE (COUNT is only a hint).
        let whole_scan = |cursor: usize| {
            word(cursor) == "0"
                && (n == cursor + 1 || n == cursor + 3 && word(cursor + 1) == "COUNT")
        };
        let all = |from: usize| word(from) == "0" && word(from + 1) == "-1";
        match word(0).as_str() {
            "SCAN" if whole_scan(1) => "DBSIZE".into(),
            "KEYS" if n == 2 && word(1) == "*" => "DBSIZE".into(),
            "HGETALL" if n == 2 => format!("HLEN {key}"),
            "SMEMBERS" if n == 2 => format!("SCARD {key}"),
            "HSCAN" if whole_scan(2) => format!("HLEN {key}"),
            "SSCAN" if whole_scan(2) => format!("SCARD {key}"),
            "ZSCAN" if whole_scan(2) => format!("ZCARD {key}"),
            "ZRANGE" if all(2) && (n == 4 || n == 5 && word(4) == "WITHSCORES") => {
                format!("ZCARD {key}")
            }
            "LRANGE" if n == 4 && all(2) => format!("LLEN {key}"),
            "XRANGE" if n == 4 && word(2) == "-" && word(3) == "+" => format!("XLEN {key}"),
            _ => counted(),
        }
    }
    fn aggregate_query(&self, sql: &str, _select_list: &str) -> String {
        self.count_query(sql)
    }
    fn select_table(
        &self,
        table: &ObjectRef,
        _columns: Option<&[String]>,
        _where_clause: Option<&str>,
        _order_by: Option<&str>,
        _limit: Option<u64>,
    ) -> String {
        read_command(&table.name, table.signature.as_deref())
    }
    fn truncate_table(&self, table: &ObjectRef) -> String {
        format!("DEL {}", arg(&table.name))
    }
}

/// "DDL" of keys: deleting and renaming; the rest does not apply.
pub struct RedisDdl;

const NOT_REDIS: &str = "# Not available for Redis keys";

impl DdlGenerator for RedisDdl {
    fn create_table(&self, _table: &TableDef, _include_foreign_keys: bool) -> Vec<String> {
        vec![NOT_REDIS.into()]
    }
    fn drop_table(&self, _schema: Option<&str>, name: &str, _cascade: bool) -> String {
        format!("UNLINK {}", arg(name))
    }
    fn rename_table(&self, _schema: Option<&str>, old_name: &str, new_name: &str) -> String {
        format!("RENAME {} {}", arg(old_name), arg(new_name))
    }
    fn alter_table(
        &self,
        _from: &TableDef,
        _to: &TableDef,
        _renamed: &[(String, String)],
    ) -> Vec<String> {
        Vec::new()
    }
    fn add_column(&self, _table: &TableDef, _column: &ColumnDef) -> Vec<String> {
        Vec::new()
    }
    fn drop_column(&self, _table: &TableDef, _column: &str) -> String {
        NOT_REDIS.into()
    }
    fn create_index(&self, _table: &TableDef, _index: &IndexDef) -> String {
        NOT_REDIS.into()
    }
    fn drop_index(&self, _table: &TableDef, _index: &IndexDef) -> String {
        NOT_REDIS.into()
    }
    fn add_primary_key(&self, _table: &TableDef, _key: &KeyDef) -> String {
        NOT_REDIS.into()
    }
    fn drop_primary_key(&self, _table: &TableDef, _key: &KeyDef) -> String {
        NOT_REDIS.into()
    }
    fn add_unique_key(&self, _table: &TableDef, _key: &KeyDef) -> String {
        NOT_REDIS.into()
    }
    fn drop_unique_key(&self, _table: &TableDef, _key: &KeyDef) -> String {
        NOT_REDIS.into()
    }
    fn add_check(&self, _table: &TableDef, _check: &CheckDef) -> String {
        NOT_REDIS.into()
    }
    fn drop_check(&self, _table: &TableDef, _check: &CheckDef) -> String {
        NOT_REDIS.into()
    }
    fn add_foreign_key(&self, _table: &TableDef, _fk: &ForeignKeyDef) -> String {
        NOT_REDIS.into()
    }
    fn drop_foreign_key(&self, _table: &TableDef, _fk: &ForeignKeyDef) -> String {
        NOT_REDIS.into()
    }
    fn create_trigger(&self, _table: &TableDef, _trigger: &TriggerDef) -> Vec<String> {
        Vec::new()
    }
    fn drop_trigger(&self, _table: &TableDef, _trigger: &TriggerDef) -> String {
        NOT_REDIS.into()
    }
    fn create_view(&self, _view: &ViewDef, _or_replace: bool) -> Vec<String> {
        Vec::new()
    }
    fn drop_view(&self, _view: &ViewDef) -> String {
        NOT_REDIS.into()
    }
    fn create_sequence(&self, _seq: &SequenceDef) -> String {
        NOT_REDIS.into()
    }
    fn alter_sequence(&self, _from: &SequenceDef, _to: &SequenceDef) -> Vec<String> {
        Vec::new()
    }
    fn drop_sequence(&self, _seq: &SequenceDef) -> String {
        NOT_REDIS.into()
    }
    fn create_routine(&self, _routine: &RoutineDef) -> Vec<String> {
        Vec::new()
    }
    fn drop_routine(&self, _routine: &RoutineDef) -> String {
        NOT_REDIS.into()
    }
    fn create_schema(&self, _name: &str) -> String {
        NOT_REDIS.into()
    }
    fn drop_schema(&self, _name: &str, _cascade: bool) -> String {
        "FLUSHDB".into()
    }
    fn map_type(&self, _source: DbKind, _data_type: &str, _kind: DataKind) -> String {
        "string".into()
    }
    fn default_type_for(&self, _kind: DataKind) -> String {
        "string".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_like_redis_cli() {
        let a = split_args(r#"SET "user:1 name" 'it''s' plain "a\"b\n\x41""#).unwrap();
        let s: Vec<String> = a
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert_eq!(s, ["SET", "user:1 name", "it's", "plain", "a\"b\nA"]);
        assert!(split_args("GET \"open").is_err());
        assert_eq!(arg("user:1"), "user:1");
        assert_eq!(arg("has space"), "\"has space\"");
        assert_eq!(arg("q\"x"), "\"q\\\"x\"");
        let back = split_args(&format!("GET {}", arg("we\"ird key\n"))).unwrap();
        assert_eq!(back[1], b"we\"ird key\n");
    }

    #[test]
    fn counts_are_exact_or_left_to_the_driver() {
        let d = RedisDialect::new();
        for (read, count) in [
            ("SCAN 0", "DBSIZE"),
            ("SCAN 0 COUNT 500", "DBSIZE"),
            ("KEYS *", "DBSIZE"),
            ("HGETALL h", "HLEN h"),
            ("HSCAN h 0", "HLEN h"),
            ("SMEMBERS s", "SCARD s"),
            ("SSCAN s 0 COUNT 1000", "SCARD s"),
            ("ZSCAN z 0", "ZCARD z"),
            ("ZRANGE z 0 -1", "ZCARD z"),
            ("ZRANGE z 0 -1 WITHSCORES", "ZCARD z"),
            ("LRANGE l 0 -1", "LLEN l"),
            ("XRANGE x - +", "XLEN x"),
            ("lrange \"a b\" 0 -1", "LLEN \"a b\""),
        ] {
            assert_eq!(d.count_query(read), count, "{read}");
        }
        // Filters and ranges: no Redis command counts them.
        for read in [
            "SCAN 0 MATCH user:*",
            "SCAN 0 TYPE hash",
            "SCAN 0 COUNT 10 MATCH x*",
            "KEYS user:*",
            "HSCAN h 0 MATCH f*",
            "SSCAN s 0 MATCH m*",
            "ZSCAN z 0 MATCH m*",
            "LRANGE l 10 20",
            "ZRANGE z 0 9",
            "ZRANGE z 0 -1 REV",
            "ZRANGE z (1 5 BYSCORE",
            "XRANGE x 1-0 +",
            "XRANGE x - + COUNT 5",
            "GET k",
            "HGET h f",
            "LRANGE \"unbalanced",
            "DBSIZE",
        ] {
            assert_eq!(
                d.count_query(read),
                format!("{COUNT_COMMAND} {read}"),
                "{read}"
            );
        }
        // A key that is not UTF-8 is passed on as written.
        assert_eq!(
            d.count_query("LRANGE \"\\xff\" 0 -1"),
            format!("{COUNT_COMMAND} LRANGE \"\\xff\" 0 -1")
        );
    }

    #[test]
    fn keys_read_and_count_by_type() {
        let d = RedisDialect::new();
        let key = |ty: &str| {
            ObjectRef::new(
                ternarygrid_core::meta::ObjectKind::Table,
                Some("db0".into()),
                "user:1",
            )
            .with_signature(ty)
        };
        assert_eq!(
            d.select_table(&key("hash"), None, None, None, Some(200)),
            "HSCAN user:1 0 COUNT 1000"
        );
        assert_eq!(
            d.select_table(&key("zset"), None, None, None, None),
            "ZRANGE user:1 0 -1 WITHSCORES"
        );
        assert_eq!(d.count_query("HSCAN user:1 0 COUNT 1000"), "HLEN user:1");
        assert_eq!(
            d.count_query("GET \"a b\""),
            "TERNARYGRID.COUNT GET \"a b\""
        );
        assert_eq!(
            d.ddl().drop_table(Some("db0"), "user:1", false),
            "UNLINK user:1"
        );
    }
}
