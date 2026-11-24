//! Small T-SQL lexer and text utilities: statement classification, `GO`
//! batch splitting, scope analysis for `sp_executesql`, module header
//! parsing and query shaping helpers.

/// Token category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    /// Keyword or unquoted identifier (also `#temp` names).
    Word,
    /// `[ident]` or `"ident"`.
    Quoted,
    /// `'...'` or `N'...'`.
    Str,
    /// Numeric or `0x..` binary literal.
    Number,
    /// `@var` / `@@function`.
    Variable,
    /// Any other single character.
    Punct,
}

/// One token with its byte offset and parenthesis depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tok<'a> {
    /// Category.
    pub kind: TokKind,
    /// Source text.
    pub text: &'a str,
    /// Byte offset in the source.
    pub start: usize,
    /// Parenthesis nesting depth at the token (0 = top level).
    pub depth: i32,
}

impl Tok<'_> {
    /// Case-insensitive keyword comparison.
    pub fn is(&self, word: &str) -> bool {
        self.kind == TokKind::Word && self.text.eq_ignore_ascii_case(word)
    }

    /// Byte offset just after the token.
    pub fn end(&self) -> usize {
        self.start + self.text.len()
    }
}

fn is_word_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '#'
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '#' || c == '$' || c == '@'
}

/// Tokenize T-SQL, skipping whitespace and comments (block comments nest).
pub fn tokenize(sql: &str) -> Vec<Tok<'_>> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut depth = 0i32;
    while i < sql.len() {
        let c = match sql[i..].chars().next() {
            Some(c) => c,
            None => break,
        };
        let clen = c.len_utf8();
        if c.is_whitespace() {
            i += clen;
            continue;
        }
        // line comment
        if sql[i..].starts_with("--") {
            i = sql[i..].find('\n').map(|n| i + n + 1).unwrap_or(sql.len());
            continue;
        }
        // nested block comment
        if sql[i..].starts_with("/*") {
            let mut level = 0;
            let mut j = i;
            while j < sql.len() {
                if sql[j..].starts_with("/*") {
                    level += 1;
                    j += 2;
                } else if sql[j..].starts_with("*/") {
                    level -= 1;
                    j += 2;
                    if level == 0 {
                        break;
                    }
                } else {
                    j += sql[j..].chars().next().map(char::len_utf8).unwrap_or(1);
                }
            }
            i = j.min(sql.len());
            continue;
        }
        let start = i;
        // N'...' national string
        let string_start = if c == '\'' {
            Some(i)
        } else if (c == 'N' || c == 'n') && bytes.get(i + 1) == Some(&b'\'') {
            Some(i + 1)
        } else {
            None
        };
        if let Some(q) = string_start {
            let mut j = q + 1;
            loop {
                match sql[j..].find('\'') {
                    Some(n) => {
                        j += n + 1;
                        if bytes.get(j) == Some(&b'\'') {
                            j += 1;
                            continue;
                        }
                        break;
                    }
                    None => {
                        j = sql.len();
                        break;
                    }
                }
            }
            out.push(Tok { kind: TokKind::Str, text: &sql[start..j], start, depth });
            i = j;
            continue;
        }
        if c == '[' || c == '"' {
            let close = if c == '[' { ']' } else { '"' };
            let mut j = i + 1;
            loop {
                match sql[j..].find(close) {
                    Some(n) => {
                        j += n + 1;
                        if sql[j..].starts_with(close) {
                            j += 1;
                            continue;
                        }
                        break;
                    }
                    None => {
                        j = sql.len();
                        break;
                    }
                }
            }
            out.push(Tok { kind: TokKind::Quoted, text: &sql[start..j], start, depth });
            i = j;
            continue;
        }
        if c == '@' {
            let mut j = i + 1;
            while j < sql.len() {
                let d = match sql[j..].chars().next() {
                    Some(d) => d,
                    None => break,
                };
                if is_word_char(d) {
                    j += d.len_utf8();
                } else {
                    break;
                }
            }
            out.push(Tok { kind: TokKind::Variable, text: &sql[start..j], start, depth });
            i = j;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let mut j = i + 1;
            while j < sql.len() {
                let d = bytes[j] as char;
                let exponent_sign = (d == '+' || d == '-') && matches!(bytes[j - 1], b'e' | b'E') && !sql[start..].starts_with("0x");
                if d.is_ascii_alphanumeric() || d == '.' || exponent_sign {
                    j += 1;
                } else {
                    break;
                }
            }
            out.push(Tok { kind: TokKind::Number, text: &sql[start..j], start, depth });
            i = j;
            continue;
        }
        if is_word_start(c) {
            let mut j = i + clen;
            while j < sql.len() {
                let d = match sql[j..].chars().next() {
                    Some(d) => d,
                    None => break,
                };
                if is_word_char(d) {
                    j += d.len_utf8();
                } else {
                    break;
                }
            }
            out.push(Tok { kind: TokKind::Word, text: &sql[start..j], start, depth });
            i = j;
            continue;
        }
        if c == ')' {
            depth -= 1;
        }
        out.push(Tok { kind: TokKind::Punct, text: &sql[start..start + clen], start, depth });
        if c == '(' {
            depth += 1;
        }
        i += clen;
    }
    out
}

/// First keyword of the statement, upper case.
pub fn first_keyword(sql: &str) -> Option<String> {
    tokenize(sql).into_iter().find(|t| t.kind == TokKind::Word).map(|t| t.text.to_ascii_uppercase())
}

/// Broad statement category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StmtClass {
    /// Query returning rows.
    Select,
    /// INSERT / UPDATE / DELETE / MERGE.
    Dml,
    /// Anything else (DDL, EXEC, control flow, SET ...).
    Other,
}

/// Classify a statement (CTEs are looked through).
pub fn classify(sql: &str) -> StmtClass {
    let toks = tokenize(sql);
    let Some(first) = toks.iter().find(|t| t.kind != TokKind::Punct || t.text != ";") else {
        return StmtClass::Other;
    };
    let class_of = |t: &Tok<'_>| -> Option<StmtClass> {
        if t.is("SELECT") {
            Some(StmtClass::Select)
        } else if t.is("INSERT") || t.is("UPDATE") || t.is("DELETE") || t.is("MERGE") {
            Some(StmtClass::Dml)
        } else {
            None
        }
    };
    if first.is("WITH") {
        // first top-level statement keyword after the CTE definitions
        return toks.iter().filter(|t| t.depth == 0 && t.start > first.start).find_map(class_of).unwrap_or(StmtClass::Other);
    }
    if first.kind == TokKind::Punct && first.text == "(" {
        return StmtClass::Select;
    }
    class_of(first).unwrap_or(StmtClass::Other)
}

/// Session options whose `SET` only lasts for the current module scope.
const SESSION_OPTIONS: &[&str] = &[
    "ANSI_DEFAULTS",
    "ANSI_NULL_DFLT_OFF",
    "ANSI_NULL_DFLT_ON",
    "ANSI_NULLS",
    "ANSI_PADDING",
    "ANSI_WARNINGS",
    "ARITHABORT",
    "ARITHIGNORE",
    "CONCAT_NULL_YIELDS_NULL",
    "CONTEXT_INFO",
    "CURSOR_CLOSE_ON_COMMIT",
    "DATEFIRST",
    "DATEFORMAT",
    "DEADLOCK_PRIORITY",
    "FIPS_FLAGGER",
    "FMTONLY",
    "FORCEPLAN",
    "IDENTITY_INSERT",
    "IMPLICIT_TRANSACTIONS",
    "LANGUAGE",
    "LOCK_TIMEOUT",
    "NOCOUNT",
    "NOEXEC",
    "NUMERIC_ROUNDABORT",
    "OFFSETS",
    "PARSEONLY",
    "QUERY_GOVERNOR_COST_LIMIT",
    "QUOTED_IDENTIFIER",
    "REMOTE_PROC_TRANSACTIONS",
    "ROWCOUNT",
    "SHOWPLAN_ALL",
    "SHOWPLAN_TEXT",
    "SHOWPLAN_XML",
    "STATISTICS",
    "TEXTSIZE",
    "TRANSACTION",
    "XACT_ABORT",
    "NO_BROWSETABLE",
];

/// Does the batch contain statements whose effect would be lost (or that
/// would fail) when executed inside `sp_executesql`? (`USE`, session `SET`
/// options, transaction control, temporary table creation, `EXECUTE AS`.)
pub fn is_scope_sensitive(sql: &str) -> bool {
    let toks = tokenize(sql);
    for (i, t) in toks.iter().enumerate() {
        if t.kind != TokKind::Word {
            continue;
        }
        let next = toks.get(i + 1);
        let prev = i.checked_sub(1).and_then(|p| toks.get(p));
        let upper = t.text.to_ascii_uppercase();
        match upper.as_str() {
            "USE" | "REVERT" | "SETUSER" => return true,
            "SET" => {
                if let Some(n) = next
                    && n.kind == TokKind::Word
                    && SESSION_OPTIONS.iter().any(|o| n.text.eq_ignore_ascii_case(o))
                {
                    return true;
                }
            }
            "BEGIN" => {
                if next.is_some_and(|n| n.is("TRAN") || n.is("TRANSACTION") || n.is("DISTRIBUTED")) {
                    return true;
                }
            }
            "COMMIT" | "ROLLBACK" => return true,
            "SAVE" => {
                if next.is_some_and(|n| n.is("TRAN") || n.is("TRANSACTION")) {
                    return true;
                }
            }
            "EXECUTE" | "EXEC" => {
                if next.is_some_and(|n| n.is("AS")) {
                    return true;
                }
            }
            "OPEN" => {
                if next.is_some_and(|n| n.is("SYMMETRIC") || n.is("MASTER")) {
                    return true;
                }
            }
            "TABLE" | "INTO" => {
                let temp = next.is_some_and(|n| n.kind == TokKind::Word && n.text.starts_with('#'));
                let inserting = prev.is_some_and(|p| p.is("INSERT") || p.is("MERGE") || p.is("ALTER") || p.is("TRUNCATE") || p.is("DROP"));
                if temp && !inserting {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Does the SQL mention transaction control or `USE` (session state the
/// driver tracks)?
pub fn touches_session_state(sql: &str) -> bool {
    tokenize(sql).iter().any(|t| {
        t.kind == TokKind::Word
            && ["TRAN", "TRANSACTION", "COMMIT", "ROLLBACK", "USE", "IMPLICIT_TRANSACTIONS", "ANSI_DEFAULTS", "XACT_ABORT"].iter().any(|w| t.text.eq_ignore_ascii_case(w))
    })
}

/// Does the SQL set `IMPLICIT_TRANSACTIONS` or `ANSI_DEFAULTS` (which turns it on)?
pub fn sets_implicit_transactions(sql: &str) -> bool {
    tokenize(sql).iter().any(|t| t.kind == TokKind::Word && (t.text.eq_ignore_ascii_case("IMPLICIT_TRANSACTIONS") || t.text.eq_ignore_ascii_case("ANSI_DEFAULTS")))
}

/// Split a script on `GO [count]` separator lines (outside strings and
/// comments). Returns `(batch, repeat count)`; empty batches are dropped.
pub fn split_batches(sql: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_block = 0usize; // nested block comment level
    let mut in_string = false;
    let mut in_bracket = false;
    for line in sql.split_inclusive('\n') {
        if in_block == 0
            && !in_string
            && !in_bracket
            && let Some(count) = go_line(line)
        {
            if !current.trim().is_empty() {
                out.push((std::mem::take(&mut current), count));
            } else {
                current.clear();
            }
            continue;
        }
        // update lexical state for this line (byte-wise: all markers are
        // ASCII, so non-ASCII text never needs a char boundary)
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let rest = &b[i..];
            if in_block > 0 {
                if rest.starts_with(b"*/") {
                    in_block -= 1;
                    i += 2;
                } else if rest.starts_with(b"/*") {
                    in_block += 1;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if in_string {
                if b[i] == b'\'' {
                    in_string = false;
                }
                i += 1;
                continue;
            }
            if in_bracket {
                if b[i] == b']' {
                    in_bracket = false;
                }
                i += 1;
                continue;
            }
            if rest.starts_with(b"--") {
                break;
            }
            if rest.starts_with(b"/*") {
                in_block += 1;
                i += 2;
                continue;
            }
            match b[i] {
                b'\'' => in_string = true,
                b'[' => in_bracket = true,
                _ => {}
            }
            i += 1;
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        out.push((current, 1));
    }
    out
}

/// `GO`, `GO 5`, `go -- comment` → repeat count.
fn go_line(line: &str) -> Option<u32> {
    let t = line.trim();
    let t = match t.find("--") {
        Some(i) => t[..i].trim(),
        None => t,
    };
    let mut parts = t.split_whitespace();
    let first = parts.next()?;
    if !first.eq_ignore_ascii_case("GO") {
        return None;
    }
    match parts.next() {
        None => Some(1),
        Some(n) => {
            if parts.next().is_some() {
                return None;
            }
            n.parse::<u32>().ok().filter(|c| *c > 0)
        }
    }
}

/// 1-based character position of the start of `line` (1-based) in `sql`.
pub fn line_start_position(sql: &str, line: u32) -> Option<usize> {
    if line == 0 {
        return None;
    }
    if line == 1 {
        return Some(1);
    }
    let mut current = 1u32;
    for (chars, (i, c)) in sql.char_indices().enumerate() {
        let _ = i;
        if c == '\n' {
            current += 1;
            if current == line {
                return Some(chars + 2);
            }
        }
    }
    None
}

/// Remove redundant enclosing parentheses: `((0))` → `0`, `(getdate())` →
/// `getdate()`, while `(a)+(b)` is kept.
pub fn strip_outer_parens(expr: &str) -> String {
    let mut s = expr.trim();
    loop {
        if !(s.starts_with('(') && s.ends_with(')')) {
            return s.to_string();
        }
        // does the first '(' close at the very end?
        let toks = tokenize(s);
        let closes_at_end = toks.iter().find(|t| t.kind == TokKind::Punct && t.text == ")" && t.depth == 0).map(|t| t.start == s.len() - 1).unwrap_or(false);
        if !closes_at_end {
            return s.to_string();
        }
        s = s[1..s.len() - 1].trim();
    }
}

/// For `CREATE [OR ALTER] VIEW name [(cols)] [WITH opts] AS select`, the
/// SELECT part.
pub fn view_select(definition: &str) -> Option<&str> {
    let toks = tokenize(definition);
    let view_pos = toks.iter().position(|t| t.is("VIEW"))?;
    let as_tok = toks.iter().skip(view_pos + 1).find(|t| t.depth == 0 && t.is("AS"))?;
    Some(definition[as_tok.end()..].trim())
}

/// Result of analysing a SELECT for row limiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectShape {
    /// Statement is a (possibly CTE prefixed) SELECT.
    pub is_select: bool,
    /// Byte offset of the top-level `ORDER BY`, if any.
    pub order_by: Option<usize>,
    /// Top-level TOP / OFFSET / FOR / OPTION / INTO / UNION present.
    pub has_top: bool,
    /// `OFFSET` present.
    pub has_offset: bool,
    /// FOR XML / JSON / BROWSE or OPTION(...) present.
    pub has_trailer: bool,
    /// FOR XML / JSON / BROWSE present.
    pub has_for: bool,
    /// Byte offset of the top-level `OPTION (…)` (the statement's last clause).
    pub option_at: Option<usize>,
    /// UNION / EXCEPT / INTERSECT at top level.
    pub has_set_op: bool,
    /// `SELECT ... INTO`.
    pub has_into: bool,
    /// Byte offset where `TOP (n)` can be inserted (after SELECT [ALL|DISTINCT]).
    pub top_insert_at: Option<usize>,
    /// `WITH ...` query: byte offset of its main SELECT (a CTE cannot be wrapped in a derived table).
    pub cte_main_at: Option<usize>,
}

/// Inspect a statement for [`crate::dialect::MsSqlDialect::limit_query`].
pub fn select_shape(sql: &str) -> SelectShape {
    let toks = tokenize(sql);
    let mut shape = SelectShape {
        is_select: classify(sql) == StmtClass::Select,
        order_by: None,
        has_top: false,
        has_offset: false,
        has_trailer: false,
        has_for: false,
        option_at: None,
        has_set_op: false,
        has_into: false,
        top_insert_at: None,
        cte_main_at: None,
    };
    let starts_with_cte = toks.iter().find(|t| t.kind == TokKind::Word).is_some_and(|t| t.is("WITH"));
    let mut seen_main_select = false;
    for (i, t) in toks.iter().enumerate() {
        if t.depth != 0 || t.kind != TokKind::Word {
            continue;
        }
        let upper = t.text.to_ascii_uppercase();
        match upper.as_str() {
            "SELECT" if !seen_main_select => {
                seen_main_select = true;
                if starts_with_cte {
                    shape.cte_main_at = Some(t.start);
                }
                let mut at = t.end();
                if let Some(n) = toks.get(i + 1) {
                    if n.is("DISTINCT") || n.is("ALL") {
                        at = n.end();
                    }
                    if n.is("TOP") || toks.get(i + 2).is_some_and(|m| m.is("TOP")) {
                        shape.has_top = true;
                    }
                }
                shape.top_insert_at = Some(at);
            }
            "ORDER" if toks.get(i + 1).is_some_and(|n| n.is("BY")) => shape.order_by = Some(t.start),
            "OFFSET" => shape.has_offset = true,
            "FOR" => {
                shape.has_trailer = true;
                shape.has_for = true;
            }
            "OPTION" => {
                shape.has_trailer = true;
                shape.option_at = Some(t.start);
            }
            "UNION" | "EXCEPT" | "INTERSECT" => shape.has_set_op = true,
            "INTO" if seen_main_select => shape.has_into = true,
            _ => {}
        }
    }
    shape
}

/// Keywords that write data, change schema/security/server state, or invoke
/// side-effecting operations in T-SQL.
const FORBIDDEN_READ_ONLY_KEYWORDS: &[&str] = &[
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "EXEC",
    "EXECUTE",
    "GRANT",
    "REVOKE",
    "DENY",
    "BACKUP",
    "RESTORE",
    "BULK",
    "WRITETEXT",
    "UPDATETEXT",
    "READTEXT",
    "SHUTDOWN",
    "KILL",
    "RECONFIGURE",
    "CHECKPOINT",
    "SETUSER",
    "REVERT",
    "OPENROWSET",
    "OPENQUERY",
    "OPENDATASOURCE",
    "DBCC",
    "WAITFOR",
    "RAISERROR",
    "THROW",
    "ENABLE",
    "DISABLE",
    "MOVE",
    "RECEIVE",
    "SEND",
    "BEGIN",
    "INTO",
    "USE",
    "DECLARE",
    "SAVE",
];

fn is_on_off(tok: &Tok<'_>) -> bool {
    tok.is("ON") || tok.is("OFF")
}

/// Consume an optional sign plus integer token (`123`, `-1`, `+5`).
fn consume_int(toks: &[Tok<'_>]) -> Option<usize> {
    match toks.first() {
        Some(t) if t.kind == TokKind::Number => Some(1),
        Some(t) if t.kind == TokKind::Punct && (t.text == "-" || t.text == "+") => toks.get(1).is_some_and(|n| n.kind == TokKind::Number).then_some(2),
        _ => None,
    }
}

/// If `stmt` starts with a safe read-only `SET` option, return how many tokens it consumes.
fn consume_safe_set(stmt: &[Tok<'_>]) -> Option<usize> {
    if !stmt.first().is_some_and(|t| t.is("SET")) {
        return None;
    }
    let opt = stmt.get(1)?;
    if opt.kind != TokKind::Word {
        return None;
    }
    let upper = opt.text.to_ascii_uppercase();
    match upper.as_str() {
        "TRANSACTION" => {
            if !stmt.get(2).is_some_and(|t| t.is("ISOLATION")) || !stmt.get(3).is_some_and(|t| t.is("LEVEL")) {
                return None;
            }
            let lvl = stmt.get(4)?;
            if lvl.is("SNAPSHOT") || lvl.is("SERIALIZABLE") {
                Some(5)
            } else if (lvl.is("READ") && stmt.get(5).is_some_and(|t| t.is("UNCOMMITTED") || t.is("COMMITTED")))
                || (lvl.is("REPEATABLE") && stmt.get(5).is_some_and(|t| t.is("READ")))
            {
                Some(6)
            } else {
                None
            }
        }
        "NOCOUNT" | "ARITHABORT" | "ANSI_NULLS" | "QUOTED_IDENTIFIER" | "SHOWPLAN_XML" => stmt.get(2).is_some_and(is_on_off).then_some(3),
        "ROWCOUNT" | "DATEFIRST" | "QUERY_GOVERNOR_COST_LIMIT" => stmt.get(2).is_some_and(|t| t.kind == TokKind::Number).then_some(3),
        "LOCK_TIMEOUT" => consume_int(&stmt[2..]).map(|n| 2 + n),
        "DATEFORMAT" => stmt.get(2).is_some_and(|t| matches!(t.kind, TokKind::Word | TokKind::Str)).then_some(3),
        "LANGUAGE" => stmt.get(2).is_some_and(|t| matches!(t.kind, TokKind::Word | TokKind::Str | TokKind::Quoted)).then_some(3),
        "DEADLOCK_PRIORITY" => {
            if stmt.get(2).is_some_and(|t| t.is("LOW") || t.is("NORMAL") || t.is("HIGH")) {
                Some(3)
            } else {
                consume_int(&stmt[2..]).map(|n| 2 + n)
            }
        }
        "STATISTICS" => {
            let mut idx = 2;
            let mut saw_item = false;
            while let Some(t) = stmt.get(idx) {
                if t.is("IO") || t.is("TIME") || t.is("XML") || t.is("PROFILE") {
                    saw_item = true;
                    idx += 1;
                    if stmt.get(idx).is_some_and(|c| c.kind == TokKind::Punct && c.text == ",") {
                        saw_item = false;
                        idx += 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            if saw_item && stmt.get(idx).is_some_and(is_on_off) { Some(idx + 1) } else { None }
        }
        _ => None,
    }
}

/// If `stmt` starts with `COMMIT` or `ROLLBACK`, return how many tokens it consumes.
fn consume_tx_end(stmt: &[Tok<'_>]) -> Option<usize> {
    let first = stmt.first()?;
    if !(first.is("COMMIT") || first.is("ROLLBACK")) {
        return None;
    }
    match stmt.get(1) {
        Some(t) if t.is("WORK") => Some(2),
        Some(t) if t.is("TRAN") || t.is("TRANSACTION") => {
            if let Some(name) = stmt.get(2)
                && matches!(name.kind, TokKind::Word | TokKind::Quoted | TokKind::Variable)
                && !(name.is("SELECT") || name.is("WITH") || name.is("SET") || name.is("COMMIT") || name.is("ROLLBACK"))
            {
                Some(3)
            } else {
                Some(2)
            }
        }
        _ => Some(1),
    }
}

fn statement_is_read_only(stmt: &[Tok<'_>]) -> bool {
    if stmt.is_empty() {
        return true;
    }
    for (i, t) in stmt.iter().enumerate() {
        if t.kind == TokKind::Word {
            if FORBIDDEN_READ_ONLY_KEYWORDS.iter().any(|kw| t.text.eq_ignore_ascii_case(kw)) {
                // `INNER MERGE JOIN` / `OPTION (MERGE JOIN)` is a join hint, not a MERGE statement.
                if t.is("MERGE") && stmt.get(i + 1).is_some_and(|n| n.is("JOIN")) {
                    continue;
                }
                return false;
            }
            if t.is("NEXT") && stmt.get(i + 1).is_some_and(|n| n.is("VALUE")) && stmt.get(i + 2).is_some_and(|n| n.is("FOR")) {
                return false;
            }
        }
    }
    let mut pos = 0;
    while pos < stmt.len() {
        let rest = &stmt[pos..];
        if let Some(n) = consume_safe_set(rest).or_else(|| consume_tx_end(rest)) {
            pos += n;
            continue;
        }
        if rest.iter().skip(1).any(|t| t.is("SET")) {
            return false;
        }
        let mut idx = 0;
        while rest.get(idx).is_some_and(|t| t.kind == TokKind::Punct && t.text == "(") {
            idx += 1;
        }
        let Some(first) = rest.get(idx) else {
            return false;
        };
        if idx > 0 {
            return first.is("SELECT");
        }
        if first.is("SELECT") {
            return true;
        }
        if first.is("WITH") {
            return rest[1..].iter().any(|t| t.is("SELECT"));
        }
        return false;
    }
    true
}

/// Conservative check for T-SQL statements allowed on a read-only SQL Server connection.
pub fn is_read_only_sql(sql: &str) -> bool {
    let toks = tokenize(sql);
    toks.split(|t| t.kind == TokKind::Punct && t.text == ";").all(statement_is_read_only)
}

/// Unquote `[a]]b]` / `"x"` identifiers.
pub fn unquote_ident(s: &str) -> String {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return inner.replace("]]", "]");
    }
    if let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        return inner.replace("\"\"", "\"");
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let t = tokenize("SELECT [a]]b], N'it''s', @x, @@SPID, #tmp, 0x1F, 1.5e-3 /* c /* n */ */ -- x\n FROM t");
        let texts: Vec<&str> = t.iter().map(|t| t.text).collect();
        assert_eq!(texts, vec!["SELECT", "[a]]b]", ",", "N'it''s'", ",", "@x", ",", "@@SPID", ",", "#tmp", ",", "0x1F", ",", "1.5e-3", "FROM", "t"]);
        assert_eq!(t[3].kind, TokKind::Str);
        assert_eq!(t[7].kind, TokKind::Variable);
        let d = tokenize("f(a, (b))");
        assert_eq!(d.iter().find(|t| t.text == "b").unwrap().depth, 2);
        assert_eq!(d.last().unwrap().depth, 0);
    }

    #[test]
    fn classification() {
        assert_eq!(classify("select 1"), StmtClass::Select);
        assert_eq!(classify("  -- c\nUPDATE t SET a = 1"), StmtClass::Dml);
        assert_eq!(classify("WITH x AS (SELECT 1 a) DELETE FROM t WHERE a IN (SELECT a FROM x)"), StmtClass::Dml);
        assert_eq!(classify("WITH x AS (SELECT 1 a) SELECT * FROM x"), StmtClass::Select);
        assert_eq!(classify("EXEC sp_who"), StmtClass::Other);
        assert_eq!(classify("(SELECT 1) UNION (SELECT 2)"), StmtClass::Select);
        assert_eq!(classify("CREATE TABLE t (a int)"), StmtClass::Other);
        assert_eq!(first_keyword("/* x */ merge INTO t"), Some("MERGE".into()));
    }

    #[test]
    fn scope_sensitivity() {
        assert!(!is_scope_sensitive("UPDATE t SET a = 1 WHERE b = 2"));
        assert!(!is_scope_sensitive("INSERT INTO #t VALUES (1)"));
        assert!(!is_scope_sensitive("DELETE FROM t; SET @x = 1"));
        assert!(is_scope_sensitive("SET NOCOUNT ON; UPDATE t SET a = 1"));
        assert!(is_scope_sensitive("USE master"));
        assert!(is_scope_sensitive("BEGIN TRAN; UPDATE t SET a = 1"));
        assert!(is_scope_sensitive("UPDATE t SET a = 1; COMMIT"));
        assert!(is_scope_sensitive("SELECT a INTO #x FROM t"));
        assert!(is_scope_sensitive("CREATE TABLE #x (a int)"));
        assert!(is_scope_sensitive("EXECUTE AS USER = 'bob'"));
        assert!(!is_scope_sensitive("UPDATE t SET a = 'USE x; COMMIT'"));
    }

    #[test]
    fn batches() {
        let script = "SELECT 1\nGO\nSELECT 'GO\nGO'\ngo 3\n/* GO\nGO */\nSELECT 2 -- tail\nGO -- done\n\nGO\n";
        let b = split_batches(script);
        assert_eq!(b.len(), 3, "{b:?}");
        assert_eq!(b[0], ("SELECT 1\n".to_string(), 1));
        assert_eq!(b[1].1, 3);
        assert!(b[1].0.contains("GO'"));
        assert!(b[2].0.contains("SELECT 2"));
        assert_eq!(split_batches("SELECT 1").len(), 1);
        assert!(split_batches("GO\nGO").is_empty());
        assert_eq!(go_line("GOTO x"), None);
        // non-ASCII text in comments, identifiers and strings must not break the scanner
        let b = split_batches("SELECT é AS [ü] /* ç\n GO ğ */ -- ö\nGO\nSELECT N'ñ'");
        assert_eq!(b.len(), 2, "{b:?}");
        assert_eq!(go_line("go 2 x"), None);
    }

    #[test]
    fn positions_and_parens() {
        assert_eq!(line_start_position("a\nbé\nc", 3), Some(6));
        assert_eq!(line_start_position("a", 1), Some(1));
        assert_eq!(line_start_position("a", 4), None);
        assert_eq!(strip_outer_parens("((0))"), "0");
        assert_eq!(strip_outer_parens("(getdate())"), "getdate()");
        assert_eq!(strip_outer_parens("((1)+(2))"), "(1)+(2)");
        assert_eq!(strip_outer_parens("(N'a)b')"), "N'a)b'");
        assert_eq!(strip_outer_parens("([qty]>(0))"), "[qty]>(0)");
        assert_eq!(strip_outer_parens("x"), "x");
    }

    #[test]
    fn view_header() {
        assert_eq!(view_select("CREATE VIEW [dbo].[v] (a, b) WITH SCHEMABINDING AS\nSELECT a, b FROM t"), Some("SELECT a, b FROM t"));
        assert_eq!(view_select("create or alter view v as select 1 as [as]"), Some("select 1 as [as]"));
        assert_eq!(view_select("SELECT 1"), None);
    }

    #[test]
    fn shapes() {
        let s = select_shape("SELECT DISTINCT a FROM t ORDER BY a");
        assert!(s.is_select);
        assert!(s.order_by.is_some());
        assert_eq!(s.top_insert_at, Some("SELECT DISTINCT".len()));
        let s = select_shape("SELECT TOP 5 a FROM t");
        assert!(s.has_top);
        let s = select_shape("SELECT a FROM (SELECT a FROM u ORDER BY a OFFSET 0 ROWS) x");
        assert!(s.order_by.is_none());
        assert!(!s.has_offset);
        let s = select_shape("SELECT a FROM t UNION ALL SELECT b FROM u");
        assert!(s.has_set_op);
        let s = select_shape("SELECT a FROM t FOR XML PATH('')");
        assert!(s.has_trailer);
        assert_eq!(unquote_ident("[a]]b]"), "a]b");
        assert_eq!(unquote_ident("\"x\"\"y\""), "x\"y");
        assert_eq!(unquote_ident("plain"), "plain");
    }

    #[test]
    fn session_state_words() {
        assert!(touches_session_state("BEGIN TRANSACTION"));
        assert!(touches_session_state("use db1"));
        assert!(!touches_session_state("SELECT 'COMMIT'"));
    }

    #[test]
    fn read_only_sql_classification() {
        // Allowed reads, comments, quoted identifiers/strings, safe SET, COMMIT/ROLLBACK.
        for ok in [
            "",
            "   ; ; -- comment only\n/* nested /* comment */ */",
            "SELECT 1",
            "SELECT * FROM dbo.orders WHERE id = 1",
            "(SELECT 1) UNION (SELECT 2)",
            "SELECT * FROM a INNER MERGE JOIN b ON a.id = b.id",
            "WITH c AS (SELECT 1 AS a) SELECT * FROM c",
            "/* DELETE FROM t */ SELECT 'DELETE', N'INTO', [DROP], \"UPDATE\" FROM [CREATE] -- INSERT",
            "SELECT 1; SELECT 2;",
            "COMMIT",
            "COMMIT TRANSACTION",
            "ROLLBACK TRAN",
            "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED",
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ",
            "SET TRANSACTION ISOLATION LEVEL SNAPSHOT",
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "SET NOCOUNT ON",
            "SET NOCOUNT OFF",
            "SET ROWCOUNT 100",
            "SET LOCK_TIMEOUT 5000",
            "SET LOCK_TIMEOUT -1",
            "SET DATEFORMAT ymd",
            "SET DATEFIRST 1",
            "SET LANGUAGE us_english",
            "SET LANGUAGE N'English'",
            "SET ARITHABORT ON",
            "SET ANSI_NULLS ON",
            "SET QUOTED_IDENTIFIER OFF",
            "SET SHOWPLAN_XML ON",
            "SET SHOWPLAN_XML OFF",
            "SET STATISTICS IO ON",
            "SET STATISTICS IO, TIME OFF",
            "SET QUERY_GOVERNOR_COST_LIMIT 100",
            "SET DEADLOCK_PRIORITY LOW",
            "SET DEADLOCK_PRIORITY -5",
            "SET NOCOUNT ON; SELECT * FROM dbo.orders",
            "SET NOCOUNT ON\nSELECT * FROM dbo.orders",
            "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED\nSELECT * FROM dbo.orders",
        ] {
            assert!(is_read_only_sql(ok), "expected read-only: {ok}");
        }

        // Rejected writes, DDL, procedures, sequence advancement, SELECT INTO, unsafe SET, multi-statement writes.
        for bad in [
            "SELECT * INTO new_table FROM dbo.orders",
            "SELECT a INTO #tmp FROM dbo.orders",
            "WITH c AS (SELECT 1 AS a) SELECT a INTO dbo.copy FROM c",
            "INSERT INTO dbo.t VALUES (1)",
            "UPDATE dbo.t SET a = 1",
            "DELETE FROM dbo.t",
            "MERGE INTO dbo.t USING dbo.s ON t.id = s.id WHEN MATCHED THEN DELETE",
            "WITH c AS (SELECT 1 AS a) DELETE FROM dbo.t WHERE a IN (SELECT a FROM c)",
            "WITH c AS (SELECT 1 AS a) UPDATE dbo.t SET a = 2",
            "WITH c AS (SELECT 1 AS a) INSERT INTO dbo.t SELECT a FROM c",
            "CREATE TABLE dbo.t (a int)",
            "ALTER TABLE dbo.t ADD b int",
            "DROP TABLE dbo.t",
            "TRUNCATE TABLE dbo.t",
            "EXEC sp_who",
            "EXECUTE dbo.my_proc",
            "DBCC CHECKDB",
            "GRANT SELECT ON dbo.t TO bob",
            "REVOKE SELECT ON dbo.t FROM bob",
            "DENY SELECT ON dbo.t TO bob",
            "BACKUP DATABASE db TO DISK = 'x.bak'",
            "RESTORE DATABASE db FROM DISK = 'x.bak'",
            "BULK INSERT dbo.t FROM 'x.csv'",
            "CHECKPOINT",
            "RECONFIGURE",
            "SHUTDOWN",
            "KILL 55",
            "WAITFOR DELAY '00:00:01'",
            "RAISERROR('boom', 16, 1)",
            "THROW 50000, 'boom', 1",
            "BEGIN TRANSACTION",
            "BEGIN DELETE FROM dbo.t; END",
            "SELECT NEXT VALUE FOR dbo.my_seq",
            "WITH c AS (SELECT NEXT VALUE FOR dbo.my_seq AS v) SELECT * FROM c",
            "SELECT * FROM OPENROWSET('SQLNCLI', 'Server=x;', 'SELECT 1')",
            "SELECT * FROM OPENQUERY(linked, 'DELETE FROM t')",
            "SELECT * FROM OPENDATASOURCE('SQLNCLI', 'Data Source=x').db.dbo.t",
            "SELECT 1; DELETE FROM dbo.t",
            "SELECT 1\nDELETE FROM dbo.t",
            "SELECT 1\nSELECT * INTO dbo.copy FROM dbo.t",
            "SELECT 1\nSET IDENTITY_INSERT dbo.t ON",
            "SET @x = 1",
            "SET IDENTITY_INSERT dbo.t ON",
            "SET IMPLICIT_TRANSACTIONS ON",
            "SET TRANSACTION READ WRITE",
            "SET CONTEXT_INFO 0x01",
            "SET NOCOUNT ON\nsp_who",
            "SET NOCOUNT ON; DELETE FROM dbo.t",
            "SET STATISTICS IO, ON",
            "USE master",
            "DECLARE @x INT",
            "ENABLE TRIGGER ALL ON dbo.t",
            "DISABLE TRIGGER ALL ON dbo.t",
            "RECEIVE TOP (1) * FROM dbo.q",
            "SEND ON CONVERSATION @h MESSAGE TYPE [m]",
            "MOVE CONVERSATION @h TO @g",
            "sp_who",
        ] {
            assert!(!is_read_only_sql(bad), "expected rejected: {bad}");
        }
    }
}
