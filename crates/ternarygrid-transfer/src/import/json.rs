//! JSON reader: arrays of objects, JSON Lines, DBeaver's `{"table": [...]}`
//! wrapper and the `{"columns": [...], "rows": [[...]]}` layout written by the
//! JSON export.
//!
//! The document is streamed: one record at a time is copied out of the input
//! and parsed by a small parser that keeps number texts exactly (decimals
//! never pass through `f64`). Nested objects and arrays become JSON text.
//! The rows of a wrapper object (`{"total": 2, "items": [...]}`) are streamed
//! as well (its other members are skipped): whether such an array holds the
//! rows or is a value of one record of a stream of objects is only known at
//! the end of the object, so up to 8 MiB of it are kept until then; a larger
//! array is taken for the rows and streamed.
//!
//! [`JsonInputFormat::Auto`] reads JSON Lines when the first line holds one
//! complete value and more values follow. Otherwise a top-level array or a
//! wrapper object must be the whole document (content after it is an error
//! rather than silently ignored), and a plain object starts a stream of objects.

use crate::convert::kind_from_name;
use crate::encoding::DecodingReader;
use crate::error::{file_error, parse_error};
use crate::source::RowSource;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Chain, Cursor, Read};
use std::path::Path;
use ternarygrid_core::{ColumnMeta, DataKind, DbError, DbResult, Row, Value};

/// Structure of the JSON input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JsonInputFormat {
    /// Detect from the first characters.
    #[default]
    Auto,
    /// A top-level array of records.
    Array,
    /// One record per line (or any sequence of top-level values).
    Lines,
}

/// Options of the JSON reader.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JsonReadOptions {
    /// Input structure.
    pub format: JsonInputFormat,
    /// Input encoding label (`auto`: BOM or UTF-8).
    pub encoding: String,
    /// Records read ahead to determine the columns.
    pub sample_rows: usize,
}

impl Default for JsonReadOptions {
    fn default() -> Self {
        JsonReadOptions { format: JsonInputFormat::Auto, encoding: "auto".into(), sample_rows: 100 }
    }
}

enum Record {
    Object(Vec<(String, Value)>),
    Array(Vec<Value>),
    Scalar(Value),
}

/// Decode the content of a JSON string literal (without the quotes).
fn unescape(raw: &[u8]) -> Result<String, String> {
    if !raw.contains(&b'\\') {
        return Ok(String::from_utf8_lossy(raw).into_owned());
    }
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut i = 0;
    let hex4 = |b: &[u8], at: usize| -> Option<u32> {
        let s = std::str::from_utf8(b.get(at..at + 4)?).ok()?;
        u32::from_str_radix(s, 16).ok()
    };
    while i < raw.len() {
        let b = raw[i];
        if b != b'\\' {
            out.push(b);
            i += 1;
            continue;
        }
        let esc = *raw.get(i + 1).ok_or("unterminated escape")?;
        i += 2;
        let c = match esc {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{08}',
            b'f' => '\u{0C}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let hi = hex4(raw, i).ok_or("invalid \\u escape")?;
                i += 4;
                if (0xD800..0xDC00).contains(&hi) && raw.get(i) == Some(&b'\\') && raw.get(i + 1) == Some(&b'u') {
                    match hex4(raw, i + 2) {
                        Some(lo) if (0xDC00..0xE000).contains(&lo) => {
                            i += 6;
                            char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).unwrap_or('\u{FFFD}')
                        }
                        _ => '\u{FFFD}',
                    }
                } else {
                    char::from_u32(hi).unwrap_or('\u{FFFD}')
                }
            }
            other => return Err(format!("invalid escape \\{}", other as char)),
        };
        let mut tmp = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn number_value(tok: &str) -> Value {
    if !tok.contains(['.', 'e', 'E']) {
        if let Ok(i) = tok.parse::<i64>() {
            return Value::Int(i);
        }
        if let Ok(u) = tok.parse::<u64>() {
            return Value::UInt(u);
        }
    }
    Value::decimal(tok)
}

/// Parser over one captured JSON value.
struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn err(&self, msg: &str) -> String {
        format!("{msg} at offset {}", self.i)
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err(self.err("expected a string"));
        }
        self.i += 1;
        let start = self.i;
        while let Some(c) = self.peek() {
            match c {
                b'"' => {
                    let s = unescape(&self.b[start..self.i])?;
                    self.i += 1;
                    return Ok(s);
                }
                b'\\' => self.i += 2,
                _ => self.i += 1,
            }
        }
        Err(self.err("unterminated string"))
    }

    fn literal(&mut self, word: &str) -> Result<(), String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(())
        } else {
            Err(self.err("invalid literal"))
        }
    }

    fn number(&mut self) -> Result<&'a str, String> {
        let start = self.i;
        while self.peek().is_some_and(|c| matches!(c, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
            self.i += 1;
        }
        let tok = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
        if crate::sink::is_json_number(tok) { Ok(tok) } else { Err(self.err("invalid number")) }
    }

    /// Validate and skip one value.
    fn skip(&mut self, depth: usize) -> Result<(), String> {
        if depth > 512 {
            return Err(self.err("nesting too deep"));
        }
        self.ws();
        match self.peek() {
            Some(b'{') => {
                self.i += 1;
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(());
                }
                loop {
                    self.ws();
                    self.string()?;
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err(self.err("expected ':'"));
                    }
                    self.i += 1;
                    self.skip(depth + 1)?;
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(());
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(());
                }
                loop {
                    self.skip(depth + 1)?;
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(());
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => self.string().map(|_| ()),
            Some(b't') => self.literal("true"),
            Some(b'f') => self.literal("false"),
            Some(b'n') => self.literal("null"),
            Some(b'-' | b'0'..=b'9') => self.number().map(|_| ()),
            _ => Err(self.err("expected a value")),
        }
    }

    /// A field value: scalars become typed values, objects / arrays JSON text.
    fn value(&mut self) -> Result<Value, String> {
        self.ws();
        match self.peek() {
            Some(b'{' | b'[') => {
                let start = self.i;
                self.skip(0)?;
                let raw = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
                Ok(Value::json(raw))
            }
            Some(b'"') => Ok(Value::text(self.string()?)),
            Some(b't') => self.literal("true").map(|_| Value::Bool(true)),
            Some(b'f') => self.literal("false").map(|_| Value::Bool(false)),
            Some(b'n') => self.literal("null").map(|_| Value::Null),
            Some(b'-' | b'0'..=b'9') => Ok(number_value(self.number()?)),
            _ => Err(self.err("expected a value")),
        }
    }

    /// The whole captured value as one record.
    fn record(&mut self) -> Result<Record, String> {
        let rec = self.item()?;
        self.ws();
        if self.i != self.b.len() {
            return Err(self.err("unexpected trailing characters"));
        }
        Ok(rec)
    }

    /// One record at the position (an element of an array).
    fn item(&mut self) -> Result<Record, String> {
        self.ws();
        let rec = match self.peek() {
            Some(b'{') => {
                self.i += 1;
                let mut members = Vec::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        let key = self.string()?;
                        self.ws();
                        if self.peek() != Some(b':') {
                            return Err(self.err("expected ':'"));
                        }
                        self.i += 1;
                        let v = self.value()?;
                        members.push((key, v));
                        self.ws();
                        match self.peek() {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.err("expected ',' or '}'")),
                        }
                    }
                }
                Record::Object(members)
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                } else {
                    loop {
                        items.push(self.value()?);
                        self.ws();
                        match self.peek() {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.err("expected ',' or ']'")),
                        }
                    }
                }
                Record::Array(items)
            }
            _ => Record::Scalar(self.value()?),
        };
        Ok(rec)
    }
}

/// Byte stream with the minimal tokenizer needed to walk the top level.
struct Stream<R: BufRead> {
    r: R,
    line: u64,
}

impl<R: BufRead> Stream<R> {
    fn io(e: std::io::Error) -> DbError {
        DbError::io(format!("Cannot read JSON input: {e}"))
    }

    fn peek(&mut self) -> DbResult<Option<u8>> {
        Ok(self.r.fill_buf().map_err(Self::io)?.first().copied())
    }

    fn bump(&mut self) -> DbResult<Option<u8>> {
        let b = self.peek()?;
        if let Some(c) = b {
            self.r.consume(1);
            if c == b'\n' {
                self.line += 1;
            }
        }
        Ok(b)
    }

    fn skip_ws(&mut self) -> DbResult<Option<u8>> {
        loop {
            match self.peek()? {
                Some(b' ' | b'\t' | b'\r' | b'\n') => {
                    self.bump()?;
                }
                other => return Ok(other),
            }
        }
    }

    /// [`Self::skip_ws`], appending the skipped whitespace to `out`.
    fn skip_ws_keep(&mut self, out: &mut Vec<u8>) -> DbResult<Option<u8>> {
        loop {
            match self.peek()? {
                Some(c @ (b' ' | b'\t' | b'\r' | b'\n')) => {
                    out.push(c);
                    self.bump()?;
                }
                other => return Ok(other),
            }
        }
    }

    fn error(&self, msg: impl std::fmt::Display) -> DbError {
        parse_error("JSON", format!("line {}", self.line), msg)
    }

    /// Copy one complete value into `out`. With `depth > 0` the stream is
    /// already inside that many open containers (whose opening bytes the caller wrote to `out`).
    fn capture(&mut self, out: &mut Vec<u8>, depth: usize) -> DbResult<()> {
        let mut depth = depth;
        if depth == 0 {
            match self.skip_ws()? {
                None => return Err(self.error("unexpected end of input")),
                Some(b'{' | b'[') => {}
                Some(b'"') => {
                    out.push(b'"');
                    self.bump()?;
                    return self.capture_string_rest(out);
                }
                Some(_) => {
                    // Scalar: up to the next structural character or whitespace.
                    while let Some(c) = self.peek()? {
                        if matches!(c, b',' | b']' | b'}' | b' ' | b'\t' | b'\r' | b'\n') {
                            break;
                        }
                        out.push(c);
                        self.bump()?;
                    }
                    return Ok(());
                }
            }
        }
        let mut in_string = false;
        let mut escaped = false;
        loop {
            let buf = self.r.fill_buf().map_err(Self::io)?;
            if buf.is_empty() {
                return Err(self.error("unexpected end of input inside a value"));
            }
            let mut end = None;
            for (i, &b) in buf.iter().enumerate() {
                if b == b'\n' {
                    self.line += 1;
                }
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if b == b'\\' {
                        escaped = true;
                    } else if b == b'"' {
                        in_string = false;
                    }
                    continue;
                }
                match b {
                    b'"' => in_string = true,
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            end = Some(i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let n = end.unwrap_or(buf.len());
            out.extend_from_slice(&buf[..n]);
            self.r.consume(n);
            if end.is_some() {
                return Ok(());
            }
        }
    }

    fn capture_string_rest(&mut self, out: &mut Vec<u8>) -> DbResult<()> {
        let mut escaped = false;
        loop {
            let b = self.bump()?.ok_or_else(|| self.error("unterminated string"))?;
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                return Ok(());
            }
        }
    }

    fn read_string(&mut self) -> DbResult<String> {
        let mut raw = Vec::new();
        if self.bump()? != Some(b'"') {
            return Err(self.error("expected a string"));
        }
        raw.push(b'"');
        self.capture_string_rest(&mut raw)?;
        let inner = &raw[1..raw.len() - 1];
        unescape(inner).map_err(|e| self.error(e))
    }

    fn expect_byte(&mut self, b: u8) -> DbResult<()> {
        match self.skip_ws()? {
            Some(c) if c == b => {
                self.bump()?;
                Ok(())
            }
            _ => Err(self.error(format!("expected '{}'", b as char))),
        }
    }
}

enum Mode {
    Lines,
    /// Elements of an array; `wrapped`: the array is a member of the top-level
    /// object, whose remaining members are skipped after it.
    Array {
        first: bool,
        wrapped: bool,
    },
    Done,
}

/// The input, after the bytes read ahead by the JSON Lines detection.
type JsonInput = Chain<Cursor<Vec<u8>>, BufReader<DecodingReader<Box<dyn Read + Send>>>>;

/// At most this much is read ahead to find the end of the first line.
const LOOKAHEAD: u64 = 1024 * 1024;

/// Bytes of an array of objects kept while it is unknown whether it holds the
/// rows of a wrapper object or is a value of one record (see the module docs).
const WRAPPER_BUFFER: usize = 8 * 1024 * 1024;

/// An array that may hold the rows of a wrapper: its text and the byte ranges of its elements.
struct Candidate {
    raw: Vec<u8>,
    elements: Vec<(usize, usize)>,
    /// The first member of the object (preferred, like DBeaver's `{"table": [...]}`).
    first: bool,
}

/// Consume whitespace into `head`; true when a non-whitespace byte follows.
fn skip_ws_into(r: &mut impl BufRead, head: &mut Vec<u8>) -> std::io::Result<bool> {
    loop {
        if head.len() as u64 >= LOOKAHEAD {
            return Ok(false);
        }
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok(false);
        }
        let n = buf.iter().position(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n')).unwrap_or(buf.len());
        head.extend_from_slice(&buf[..n]);
        let more = n < buf.len();
        r.consume(n);
        if more {
            return Ok(true);
        }
    }
}

/// JSON Lines detection: the first line holds one complete object or array
/// and another value follows on the next lines. Returns the bytes read ahead
/// (to be read again) and the verdict.
fn detect_lines(r: &mut impl BufRead) -> std::io::Result<(Vec<u8>, bool)> {
    let mut head = Vec::new();
    if !skip_ws_into(r, &mut head)? {
        return Ok((head, false));
    }
    let start = head.len();
    r.by_ref().take(LOOKAHEAD).read_until(b'\n', &mut head)?;
    if !head.ends_with(b"\n") {
        return Ok((head, false));
    }
    let line = &head[start..];
    let mut p = Parser { b: line, i: 0 };
    let complete = matches!(line.first(), Some(b'{' | b'[')) && p.skip(0).is_ok() && {
        p.ws();
        p.i == line.len()
    };
    if !complete {
        return Ok((head, false));
    }
    let more = skip_ws_into(r, &mut head)?;
    Ok((head, more))
}

/// Streams the records of a JSON document as rows.
pub struct JsonSource {
    stream: Stream<JsonInput>,
    mode: Mode,
    columns: Vec<ColumnMeta>,
    index: HashMap<String, usize>,
    pending: VecDeque<Row>,
    /// Records parsed ahead (elements of a buffered wrapper array).
    buffered: VecDeque<Record>,
    buf: Vec<u8>,
    unknown: Vec<String>,
    wrapper_buffer: usize,
}

/// Keys under which an API response puts its rows (`{"total": 2, "items": [...]}`).
fn is_rows_key(key: &str) -> bool {
    const KEYS: [&str; 14] = ["items", "data", "results", "result", "records", "rows", "values", "list", "entries", "hits", "content", "elements", "objects", "docs"];
    KEYS.iter().any(|k| key.eq_ignore_ascii_case(k))
}

fn column_from_descriptor(v: &serde_json::Value, i: usize) -> ColumnMeta {
    match v {
        serde_json::Value::String(s) => ColumnMeta::new(s.clone(), "", DataKind::Text),
        serde_json::Value::Object(o) => {
            let name = o.get("name").and_then(|n| n.as_str()).map(str::to_string).unwrap_or_else(|| format!("column{}", i + 1));
            let type_name = o.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let kind = o
                .get("kind")
                .and_then(|k| k.as_str())
                .and_then(kind_from_name)
                .unwrap_or_else(|| if type_name.is_empty() { DataKind::Text } else { DataKind::guess_from_type_name(&type_name) });
            ColumnMeta::new(name, type_name, kind)
        }
        _ => ColumnMeta::new(format!("column{}", i + 1), "", DataKind::Text),
    }
}

impl JsonSource {
    /// Open a JSON or JSON Lines file.
    pub fn open(path: impl AsRef<Path>, options: JsonReadOptions) -> DbResult<Self> {
        let path = path.as_ref();
        let file = std::fs::File::open(path).map_err(|e| file_error("Cannot open", path, e))?;
        Self::from_reader(file, options)
    }

    /// Read JSON from any reader.
    pub fn from_reader(reader: impl Read + Send + 'static, options: JsonReadOptions) -> DbResult<Self> {
        Self::open_reader(reader, options, WRAPPER_BUFFER)
    }

    fn open_reader(reader: impl Read + Send + 'static, options: JsonReadOptions, wrapper_buffer: usize) -> DbResult<Self> {
        let decoded = DecodingReader::new(Box::new(reader) as Box<dyn Read + Send>, &options.encoding)?;
        let mut input = BufReader::with_capacity(64 * 1024, decoded);
        // `{"tags": [{…}], "id": 1}` on the first line of JSON Lines must not be
        // taken for a wrapper of the `tags` rows: JSON Lines are detected first.
        let (head, lines) = if options.format == JsonInputFormat::Auto { detect_lines(&mut input).map_err(Stream::<JsonInput>::io)? } else { (Vec::new(), false) };
        let stream = Stream { r: Cursor::new(head).chain(input), line: 1 };
        let mut src = JsonSource {
            stream,
            mode: Mode::Done,
            columns: Vec::new(),
            index: HashMap::new(),
            pending: VecDeque::new(),
            buffered: VecDeque::new(),
            buf: Vec::new(),
            unknown: Vec::new(),
            wrapper_buffer,
        };
        let mut first_record: Option<Record> = None;
        let mut declared: Option<Vec<ColumnMeta>> = None;
        match (options.format, src.stream.skip_ws()?) {
            (_, None) => {}
            (JsonInputFormat::Lines, _) => src.mode = Mode::Lines,
            (JsonInputFormat::Auto, _) if lines => src.mode = Mode::Lines,
            (JsonInputFormat::Array | JsonInputFormat::Auto, Some(b'[')) => {
                src.stream.bump()?;
                src.mode = Mode::Array { first: true, wrapped: false };
            }
            (JsonInputFormat::Array, _) => return Err(src.stream.error("expected '[' at the start of the document")),
            (JsonInputFormat::Auto, Some(b'{')) => first_record = src.open_object(&mut declared)?,
            (JsonInputFormat::Auto, _) => src.mode = Mode::Lines,
        }
        let mut sample: Vec<Record> = first_record.into_iter().collect();
        while sample.len() < options.sample_rows.max(1) {
            match src.next_record()? {
                Some(r) => sample.push(r),
                None => break,
            }
        }
        let mut names: Vec<ColumnMeta> = declared.unwrap_or_default();
        for r in &sample {
            match r {
                Record::Object(members) => {
                    for (k, _) in members {
                        if !names.iter().any(|c| &c.name == k) {
                            names.push(ColumnMeta::new(k.clone(), "", DataKind::Text));
                        }
                    }
                }
                Record::Array(items) => {
                    while names.len() < items.len() {
                        let n = names.len() + 1;
                        names.push(ColumnMeta::new(format!("column{n}"), "", DataKind::Text));
                    }
                }
                Record::Scalar(_) => {
                    if names.is_empty() {
                        names.push(ColumnMeta::new("value", "", DataKind::Text));
                    }
                }
            }
        }
        src.index = names.iter().enumerate().map(|(i, c)| (c.name.clone(), i)).collect();
        src.columns = names;
        let rows: Vec<Row> = sample.into_iter().map(|r| src.make_row(r)).collect();
        src.pending.extend(rows);
        Ok(src)
    }

    /// Walk a top-level object: a wrapper (`{"t": [...]}`, `{"total": 2, "items": [...]}`,
    /// `{"columns":..,"rows":..}`) switches to the records of its array; a plain
    /// object is the first record of a stream of objects (or the only record).
    fn open_object(&mut self, declared: &mut Option<Vec<ColumnMeta>>) -> DbResult<Option<Record>> {
        self.stream.bump()?;
        let mut members: Vec<(String, Value)> = Vec::new();
        let mut candidate: Option<Candidate> = None;
        loop {
            match self.stream.skip_ws()? {
                Some(b'}') => {
                    self.stream.bump()?;
                    // The whole document is this object: a wrapper of the array, not a record of a stream.
                    if let Some(c) = candidate
                        && self.stream.skip_ws()?.is_none()
                    {
                        for &(a, b) in &c.elements {
                            let rec = Parser { b: &c.raw[a..b], i: 0 }.record().map_err(|e| self.stream.error(e))?;
                            self.buffered.push_back(rec);
                        }
                        self.mode = Mode::Done;
                        return Ok(None);
                    }
                    self.mode = Mode::Lines;
                    return Ok(Some(Record::Object(members)));
                }
                Some(b',') => {
                    self.stream.bump()?;
                }
                Some(b'"') => {
                    let key = self.stream.read_string()?;
                    self.stream.expect_byte(b':')?;
                    match self.stream.skip_ws()? {
                        Some(b'[') if key == "columns" => {
                            let mut raw = Vec::new();
                            self.stream.capture(&mut raw, 0)?;
                            let parsed: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| self.stream.error(e))?;
                            let cols = parsed.as_array().map(|a| a.iter().enumerate().map(|(i, v)| column_from_descriptor(v, i)).collect());
                            *declared = cols;
                        }
                        Some(b'[') => {
                            self.stream.bump()?;
                            let next = self.stream.skip_ws()?;
                            // `rows` of the columns + rows layout.
                            if key == "rows" {
                                self.mode = Mode::Array { first: true, wrapped: true };
                                return Ok(None);
                            }
                            // The rows of a wrapper: its first member when it is an array of
                            // objects (DBeaver's `{"table": [...]}`), or an array of objects
                            // under a key APIs use for their rows (`{"total": 2, "items": [{…}]}`,
                            // but not `{"id": 1, "orders": [{…}]}`, one record with a nested array).
                            let first = members.is_empty() && (next == Some(b'{') || next == Some(b']'));
                            if first || (next == Some(b'{') && is_rows_key(&key)) {
                                let (array, complete) = self.buffer_array(first)?;
                                if !complete {
                                    // Too large to be a value of one record: the wrapper's rows, streamed.
                                    for &(a, b) in &array.elements {
                                        let rec = Parser { b: &array.raw[a..b], i: 0 }.record().map_err(|e| self.stream.error(e))?;
                                        self.buffered.push_back(rec);
                                    }
                                    self.mode = Mode::Array { first: true, wrapped: true };
                                    return Ok(None);
                                }
                                members.push((key, Value::json(String::from_utf8_lossy(&array.raw).into_owned())));
                                if candidate.as_ref().is_none_or(|c| !c.first && c.raw.len() < array.raw.len()) {
                                    candidate = Some(array);
                                }
                                continue;
                            }
                            // An array valued member of a plain record.
                            let mut raw = vec![b'['];
                            self.stream.capture(&mut raw, 1)?;
                            members.push((key, Value::json(String::from_utf8_lossy(&raw).into_owned())));
                        }
                        _ => {
                            let mut raw = Vec::new();
                            self.stream.capture(&mut raw, 0)?;
                            let v = Parser { b: &raw, i: 0 }.value().map_err(|e| self.stream.error(e))?;
                            members.push((key, v));
                        }
                    }
                }
                None => return Err(self.stream.error("unexpected end of input")),
                Some(c) => return Err(self.stream.error(format!("unexpected character '{}'", c as char))),
            }
        }
    }

    /// Read an array (its `[` consumed) element by element, and whether it
    /// ended before `wrapper_buffer` bytes (else the stream is left before the
    /// next element).
    fn buffer_array(&mut self, first: bool) -> DbResult<(Candidate, bool)> {
        let mut raw = vec![b'['];
        let mut elements = Vec::new();
        if self.stream.skip_ws_keep(&mut raw)? == Some(b']') {
            self.stream.bump()?;
            raw.push(b']');
            return Ok((Candidate { raw, elements, first }, true));
        }
        loop {
            let start = raw.len();
            self.stream.capture(&mut raw, 0)?;
            elements.push((start, raw.len()));
            match self.stream.skip_ws_keep(&mut raw)? {
                Some(b',') => {
                    self.stream.bump()?;
                    raw.push(b',');
                }
                Some(b']') => {
                    self.stream.bump()?;
                    raw.push(b']');
                    return Ok((Candidate { raw, elements, first }, true));
                }
                None => return Err(self.stream.error("unexpected end of input inside the array")),
                Some(c) => return Err(self.stream.error(format!("expected ',' or ']' but found '{}'", c as char))),
            }
            if raw.len() > self.wrapper_buffer {
                return Ok((Candidate { raw, elements, first }, false));
            }
            self.stream.skip_ws_keep(&mut raw)?;
        }
    }

    /// After the wrapper's array: skip the remaining members of the object.
    fn close_wrapper(&mut self) -> DbResult<()> {
        loop {
            match self.stream.skip_ws()? {
                Some(b'}') => {
                    self.stream.bump()?;
                    return Ok(());
                }
                Some(b',') => {
                    self.stream.bump()?;
                    self.stream.skip_ws()?;
                    self.stream.read_string()?;
                    self.stream.expect_byte(b':')?;
                    self.buf.clear();
                    let mut buf = std::mem::take(&mut self.buf);
                    let r = self.stream.capture(&mut buf, 0);
                    self.buf = buf;
                    r?;
                }
                None => return Err(self.stream.error("unexpected end of input")),
                Some(c) => return Err(self.stream.error(format!("unexpected character '{}'", c as char))),
            }
        }
    }

    fn next_record(&mut self) -> DbResult<Option<Record>> {
        if let Some(rec) = self.buffered.pop_front() {
            return Ok(Some(rec));
        }
        match self.mode {
            Mode::Done => return Ok(None),
            Mode::Lines => {
                if self.stream.skip_ws()?.is_none() {
                    self.mode = Mode::Done;
                    return Ok(None);
                }
            }
            Mode::Array { first, wrapped } => {
                let c = self.stream.skip_ws()?;
                match c {
                    Some(b']') => {
                        self.stream.bump()?;
                        self.mode = Mode::Done;
                        if wrapped {
                            self.close_wrapper()?;
                        }
                        // Content after the document would be silently lost.
                        if self.stream.skip_ws()?.is_some() {
                            return Err(self.stream.error("unexpected content after the end of the JSON document (for one record per line, read it as JSON Lines)"));
                        }
                        return Ok(None);
                    }
                    Some(b',') if !first => {
                        self.stream.bump()?;
                    }
                    None => return Err(self.stream.error("unexpected end of input inside the array")),
                    _ if first => {}
                    Some(c) => return Err(self.stream.error(format!("expected ',' or ']' but found '{}'", c as char))),
                }
                self.mode = Mode::Array { first: false, wrapped };
            }
        }
        self.buf.clear();
        let mut buf = std::mem::take(&mut self.buf);
        let res = self.stream.capture(&mut buf, 0);
        let rec = res.and_then(|_| Parser { b: &buf, i: 0 }.record().map_err(|e| self.stream.error(e)));
        self.buf = buf;
        rec.map(Some)
    }

    /// Encoding used to read the document (after BOM detection).
    pub fn encoding(&self) -> &'static str {
        self.stream.r.get_ref().1.get_ref().encoding().name()
    }

    fn make_row(&mut self, rec: Record) -> Row {
        let mut row = vec![Value::Null; self.columns.len()];
        match rec {
            Record::Object(members) => {
                for (k, v) in members {
                    match self.index.get(&k) {
                        Some(&i) => row[i] = v,
                        None => {
                            if !self.unknown.contains(&k) && self.unknown.len() < 100 {
                                self.unknown.push(k);
                            }
                        }
                    }
                }
            }
            Record::Array(items) => {
                for (i, v) in items.into_iter().enumerate() {
                    if i < row.len() {
                        row[i] = v;
                    }
                }
            }
            Record::Scalar(v) => {
                if let Some(slot) = row.first_mut() {
                    *slot = v;
                }
            }
        }
        row
    }
}

#[async_trait]
impl RowSource for JsonSource {
    fn binary_is_base64(&self) -> bool {
        true
    }

    fn columns(&self) -> &[ColumnMeta] {
        &self.columns
    }

    async fn next_batch(&mut self, max: usize) -> DbResult<Vec<Row>> {
        let max = max.max(1);
        let mut out = Vec::with_capacity(max.min(4096));
        while out.len() < max {
            if let Some(r) = self.pending.pop_front() {
                out.push(r);
                continue;
            }
            match self.next_record()? {
                Some(rec) => {
                    let row = self.make_row(rec);
                    out.push(row);
                }
                None => break,
            }
        }
        Ok(out)
    }

    fn take_warnings(&mut self) -> Vec<String> {
        let mut w = Vec::new();
        if !self.unknown.is_empty() {
            w.push(format!("Keys not present in the first records were ignored: {}", std::mem::take(&mut self.unknown).join(", ")));
        }
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read(json: &str, format: JsonInputFormat) -> (Vec<String>, Vec<Row>) {
        let opts = JsonReadOptions { format, ..Default::default() };
        let mut s = JsonSource::from_reader(std::io::Cursor::new(json.as_bytes().to_vec()), opts).unwrap();
        let names = s.columns().iter().map(|c| c.name.clone()).collect();
        let mut rows = Vec::new();
        loop {
            let b = s.next_batch(3).await.unwrap();
            if b.is_empty() {
                break;
            }
            rows.extend(b);
        }
        (names, rows)
    }

    #[tokio::test]
    async fn array_of_objects() {
        let json = r#"[ {"id": 1, "price": 12345678901234567890.123, "tags": ["a", {"b": 2}], "ok": true},
            {"id": 18446744073709551615, "name": "x\"yé😀", "price": null} ]"#;
        let (names, rows) = read(json, JsonInputFormat::Auto).await;
        assert_eq!(names, vec!["id", "price", "tags", "ok", "name"]);
        assert_eq!(rows[0][1], Value::decimal("12345678901234567890.123"));
        assert_eq!(rows[0][2], Value::json(r#"["a", {"b": 2}]"#));
        assert_eq!(rows[0][3], Value::Bool(true));
        assert_eq!(rows[1][0], Value::UInt(u64::MAX));
        assert_eq!(rows[1][4], Value::text("x\"yé😀"));
        assert_eq!(rows[1][1], Value::Null);
    }

    #[tokio::test]
    async fn json_lines_and_wrappers() {
        let lines = "{\"a\":1,\"b\":[1,2]}\n\n{\"a\":2}\n";
        let (names, rows) = read(lines, JsonInputFormat::Auto).await;
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1], Value::json("[1,2]"));
        let wrapped = r#"{"users": [{"id": 1}, {"id": 2}]}"#;
        let (_, rows) = read(wrapped, JsonInputFormat::Auto).await;
        assert_eq!(rows, vec![vec![Value::Int(1)], vec![Value::Int(2)]]);
        // Metadata before the array.
        let paged = r#"{"total": 2, "items": [{"id": 1}, {"id": 2}], "next": null}"#;
        let (names, rows) = read(paged, JsonInputFormat::Auto).await;
        assert_eq!(names, vec!["id"]);
        assert_eq!(rows, vec![vec![Value::Int(1)], vec![Value::Int(2)]]);
        // One record with a nested array of objects stays a record.
        let one = r#"{"id": 1, "name": "x", "orders": [{"sku": "a"}, {"sku": "b"}]}"#;
        let (names, rows) = read(one, JsonInputFormat::Auto).await;
        assert_eq!(names, vec!["id", "name", "orders"]);
        assert_eq!(rows.len(), 1);
        // A record with an array of objects in a JSON Lines stream stays a record.
        let nested = "{\"id\": 1, \"tags\": [{\"t\": 1}]}\n{\"id\": 2, \"tags\": []}\n";
        let (names, rows) = read(nested, JsonInputFormat::Auto).await;
        assert_eq!(names, vec!["id", "tags"]);
        assert_eq!(rows.len(), 2);
        let layout = r#"{"table": "t", "columns": [{"name": "id", "type": "int4", "kind": "int"}, "name"], "rows": [[1, "a"], [2, null]]}"#;
        let opts = JsonReadOptions::default();
        let mut s = JsonSource::from_reader(std::io::Cursor::new(layout.as_bytes().to_vec()), opts).unwrap();
        assert_eq!(s.columns()[0].kind, DataKind::Int);
        assert_eq!(s.columns()[1].name, "name");
        assert_eq!(s.next_batch(10).await.unwrap(), vec![vec![Value::Int(1), Value::text("a")], vec![Value::Int(2), Value::Null]]);
    }

    #[tokio::test]
    async fn json_lines_are_detected_before_wrappers() {
        // The first record starts with an array of objects: not a wrapper of its elements.
        let lines = "{\"tags\": [{\"t\": 1}], \"id\": 1}\n{\"tags\": [], \"id\": 2}\n{\"id\": 3}\n";
        let (names, rows) = read(lines, JsonInputFormat::Auto).await;
        assert_eq!(names, vec!["tags", "id"]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec![Value::json("[{\"t\": 1}]"), Value::Int(1)]);
        assert_eq!(rows[2], vec![Value::Null, Value::Int(3)]);
        // ... or with a `rows` member, or a rows key after other members
        let lines = "{\"rows\": [1, 2], \"id\": 1}\r\n\r\n{\"id\": 2, \"items\": [{\"a\": 1}]}\n";
        let (names, rows) = read(lines, JsonInputFormat::Auto).await;
        assert_eq!(names, vec!["rows", "id", "items"]);
        assert_eq!(rows.len(), 2);
        // JSON Lines of arrays.
        let (names, rows) = read("[1, \"a\"]\n[2, \"b\"]\n", JsonInputFormat::Auto).await;
        assert_eq!(names, vec!["column1", "column2"]);
        assert_eq!(rows, vec![vec![Value::Int(1), Value::text("a")], vec![Value::Int(2), Value::text("b")]]);
        // A one-line document followed by a newline is still one document.
        let (_, rows) = read("[{\"id\": 1}, {\"id\": 2}]\n", JsonInputFormat::Auto).await;
        assert_eq!(rows.len(), 2);
        let (_, rows) = read("{\"users\": [{\"id\": 1}, {\"id\": 2}]}\n", JsonInputFormat::Auto).await;
        assert_eq!(rows, vec![vec![Value::Int(1)], vec![Value::Int(2)]]);
    }

    #[tokio::test]
    async fn content_after_the_document_is_an_error() {
        // A streamed wrapper followed by more content: the loss is reported.
        let doc = "{\"users\": [\n{\"id\": 1},\n{\"id\": 2}\n]}\n{\"users\": [{\"id\": 3}]}\n";
        let r = JsonSource::open_reader(std::io::Cursor::new(doc.as_bytes().to_vec()), JsonReadOptions::default(), 8);
        assert!(r.is_err());
        let doc = "[\n{\"a\": 1}\n]\n[{\"a\": 2}]";
        assert!(JsonSource::from_reader(std::io::Cursor::new(doc.as_bytes().to_vec()), JsonReadOptions::default()).is_err());
        // Members after the wrapper's array are skipped.
        let doc = "{\n\"data\": [{\"id\": 1}],\n\"meta\": {\"pages\": [1, 2]}, \"next\": null\n}\n";
        let (names, rows) = read(doc, JsonInputFormat::Auto).await;
        assert_eq!(names, vec!["id"]);
        assert_eq!(rows, vec![vec![Value::Int(1)]]);
    }

    #[tokio::test]
    async fn envelope_rows_are_streamed() {
        // `{"total": …, "items": [...]}` larger than the buffer: rows are delivered
        // before the end of the array is read (the broken tail is only reached later).
        let doc = "{\"total\": 3, \"items\": [{\"id\": 1}, {\"id\": 2}, {\"id\": 3}, oops";
        let opts = JsonReadOptions { sample_rows: 1, ..Default::default() };
        let mut s = JsonSource::open_reader(std::io::Cursor::new(doc.as_bytes().to_vec()), opts, 12).unwrap();
        assert_eq!(s.columns()[0].name, "id");
        assert_eq!(s.next_batch(2).await.unwrap(), vec![vec![Value::Int(1)], vec![Value::Int(2)]]);
        assert_eq!(s.next_batch(1).await.unwrap(), vec![vec![Value::Int(3)]]);
        assert!(s.next_batch(1).await.is_err());
        // Within the buffer the whole document is checked first.
        let opts = JsonReadOptions { sample_rows: 1, ..Default::default() };
        assert!(JsonSource::from_reader(std::io::Cursor::new(doc.as_bytes().to_vec()), opts).is_err());
        // A large wrapper with metadata around its rows.
        let big = format!("{{\"total\": 50, \"items\": [{}], \"next\": null}}", (1..=50).map(|i| format!("{{\"id\": {i}}}")).collect::<Vec<_>>().join(", "));
        let mut s = JsonSource::open_reader(std::io::Cursor::new(big.into_bytes()), JsonReadOptions::default(), 64).unwrap();
        let mut ids = Vec::new();
        loop {
            let b = s.next_batch(7).await.unwrap();
            if b.is_empty() {
                break;
            }
            ids.extend(b.into_iter().map(|r| r[0].clone()));
        }
        assert_eq!(ids, (1..=50).map(Value::Int).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn stream_of_pretty_printed_objects() {
        // `jq` style output: objects spanning lines, one after another, with arrays of
        // objects (first member, rows key) that are values of each record.
        let doc = "{\n  \"tags\": [{\"t\": 1}],\n  \"id\": 1,\n  \"items\": [{\"a\": 1}]\n}\n{\n  \"tags\": [],\n  \"id\": 2\n}\n";
        let (names, rows) = read(doc, JsonInputFormat::Auto).await;
        assert_eq!(names, vec!["tags", "id", "items"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec![Value::json("[{\"t\": 1}]"), Value::Int(1), Value::json("[{\"a\": 1}]")]);
        assert_eq!(rows[1], vec![Value::json("[]"), Value::Int(2), Value::Null]);
    }

    #[tokio::test]
    async fn errors_are_reported() {
        let bad = "[{\"a\": 1}, {\"a\": tru}]";
        let opts = JsonReadOptions::default();
        assert!(JsonSource::from_reader(std::io::Cursor::new(bad.as_bytes().to_vec()), opts).is_err());
        let truncated = "[{\"a\": 1}, {\"a\"";
        assert!(JsonSource::from_reader(std::io::Cursor::new(truncated.as_bytes().to_vec()), JsonReadOptions::default()).is_err());
        let empty = "";
        let (names, rows) = read(empty, JsonInputFormat::Auto).await;
        assert!(names.is_empty() && rows.is_empty());
        let empty_array = "[]";
        let (_, rows) = read(empty_array, JsonInputFormat::Array).await;
        assert!(rows.is_empty());
    }
}
