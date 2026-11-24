//! Database value model shared by all drivers, the grid and data transfer.
//!
//! [`Value`] is deliberately small (24 bytes): heap payloads are boxed slices,
//! and database specific types without a natural Rust representation
//! (intervals, ranges, inet, geometry as EWKT, enums ...) are carried as
//! [`Value::Text`] while [`ColumnMeta::kind`] tells how to interpret them.

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::fmt::Write as _;
use uuid::Uuid;

/// One row of a result set.
pub type Row = Vec<Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    /// Exact numeric kept as text to avoid precision loss (NUMERIC, DECIMAL, MONEY).
    Decimal(Box<str>),
    Text(Box<str>),
    Bytes(Box<[u8]>),
    Date(NaiveDate),
    Time(NaiveTime),
    DateTime(NaiveDateTime),
    DateTimeTz(DateTime<FixedOffset>),
    Uuid(Uuid),
    /// JSON document as text (compact or as returned by the server).
    Json(Box<str>),
    Array(Box<[Value]>),
}

/// Logical type of a column, independent of the database specific type name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum DataKind {
    Bool,
    Int,
    Float,
    Decimal,
    #[default]
    Text,
    Binary,
    Date,
    Time,
    TimeTz,
    DateTime,
    DateTimeTz,
    Interval,
    Uuid,
    Json,
    Xml,
    Array,
    Geometry,
    Enum,
    Network,
    Bit,
    Range,
    Other,
}

impl DataKind {
    pub fn is_numeric(self) -> bool {
        matches!(self, DataKind::Int | DataKind::Float | DataKind::Decimal)
    }

    pub fn is_temporal(self) -> bool {
        matches!(self, DataKind::Date | DataKind::Time | DataKind::TimeTz | DataKind::DateTime | DataKind::DateTimeTz | DataKind::Interval)
    }

    pub fn is_textual(self) -> bool {
        matches!(self, DataKind::Text | DataKind::Enum | DataKind::Xml | DataKind::Json)
    }

    /// Heuristic mapping from a database type name to a logical kind. Drivers
    /// should map their own types precisely; this is a fallback for generic code
    /// (imports, migrations, tests).
    pub fn guess_from_type_name(type_name: &str) -> DataKind {
        let t = type_name.trim().to_ascii_lowercase();
        if t.ends_with("[]") || t.starts_with('_') || t.starts_with("array") || t.starts_with("list") {
            return DataKind::Array;
        }
        let base = t.split(['(', ' ']).next().unwrap_or("");
        match base {
            "bool" | "boolean" | "bit" if t == "bit(1)" || base != "bit" => DataKind::Bool,
            "bit" | "varbit" => DataKind::Bit,
            "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "int2" | "int4" | "int8" | "serial" | "bigserial" | "smallserial" | "serial4" | "serial8"
            | "oid" | "year" | "hugeint" | "ubigint" | "uinteger" | "usmallint" | "utinyint" | "int16" | "int32" | "int64" | "int128" | "int256" | "uint8" | "uint16"
            | "uint32" | "uint64" | "int1" | "long" => DataKind::Int,
            "real" | "float" | "float4" | "float8" | "double" | "float32" | "float64" => DataKind::Float,
            "numeric" | "decimal" | "money" | "smallmoney" | "number" | "dec" | "fixed" => DataKind::Decimal,
            "date" | "date32" => DataKind::Date,
            "time" if t.contains("with time zone") => DataKind::TimeTz,
            "time" | "time without time zone" => DataKind::Time,
            "timetz" => DataKind::TimeTz,
            "timestamp" if t.contains("with time zone") => DataKind::DateTimeTz,
            "timestamptz" | "datetimeoffset" => DataKind::DateTimeTz,
            "timestamp" | "datetime" | "datetime2" | "smalldatetime" | "datetime64" => DataKind::DateTime,
            "interval" => DataKind::Interval,
            "uuid" | "uniqueidentifier" => DataKind::Uuid,
            "json" | "jsonb" => DataKind::Json,
            "xml" => DataKind::Xml,
            "bytea" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "binary" | "varbinary" | "image" | "raw" | "bytes" => DataKind::Binary,
            "geometry" | "geography" | "point" | "linestring" | "polygon" | "multipoint" | "multilinestring" | "multipolygon" | "geometrycollection" | "box2d" | "box3d" => {
                DataKind::Geometry
            }
            "inet" | "cidr" | "macaddr" | "macaddr8" | "ipv4" | "ipv6" => DataKind::Network,
            "enum" | "set" => DataKind::Enum,
            "int4range" | "int8range" | "numrange" | "tsrange" | "tstzrange" | "daterange" | "int4multirange" | "int8multirange" | "nummultirange" | "tsmultirange"
            | "tstzmultirange" | "datemultirange" => DataKind::Range,
            "char" | "character" | "varchar" | "nvarchar" | "nchar" | "text" | "tinytext" | "mediumtext" | "longtext" | "ntext" | "string" | "citext" | "name" | "clob"
            | "bpchar" | "character varying" | "fixedstring" | "lowcardinality" | "sysname" => DataKind::Text,
            _ => DataKind::Other,
        }
    }
}

/// Where a result column comes from. Present when the driver could resolve the
/// base table of the column; required for editing result sets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColumnSource {
    /// Catalog/database (PostgreSQL database, SQL Server catalog). Usually `None`.
    pub database: Option<String>,
    /// Schema used to qualify the table in SQL (MySQL database, SQLite attached db name).
    pub schema: Option<String>,
    pub table: String,
    pub column: String,
}

/// Description of a result set column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ColumnMeta {
    /// Label shown in the result header.
    pub name: String,
    /// Database specific type name, e.g. `int4`, `varchar(255)`, `geometry`.
    pub type_name: String,
    pub kind: DataKind,
    pub nullable: Option<bool>,
    pub precision: Option<u32>,
    pub scale: Option<u32>,
    pub max_length: Option<u32>,
    pub auto_increment: Option<bool>,
    #[serde(default)]
    pub default_value: Option<String>,
    pub source: Option<ColumnSource>,
}

impl ColumnMeta {
    pub fn new(name: impl Into<String>, type_name: impl Into<String>, kind: DataKind) -> Self {
        ColumnMeta { name: name.into(), type_name: type_name.into(), kind, ..Default::default() }
    }

    pub fn with_source(mut self, source: ColumnSource) -> Self {
        self.source = Some(source);
        self
    }

    pub fn is_numeric(&self) -> bool {
        self.kind.is_numeric()
    }
}

/// How binary values are rendered as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BinaryFormat {
    #[default]
    Hex,
    Base64,
    /// Interpret as UTF-8 (lossy).
    Text,
}

/// How booleans are rendered in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BoolStyle {
    TrueFalse,
    #[default]
    Checkbox,
    OneZero,
}

/// User configurable data formatting ("format data" settings).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FormatOptions {
    pub null_text: String,
    /// chrono strftime patterns
    pub date_format: String,
    pub time_format: String,
    pub datetime_format: String,
    pub show_fractional_seconds: bool,
    /// Maximum fraction digits for floats; `None` keeps the shortest exact representation.
    pub float_max_decimals: Option<usize>,
    pub thousands_separator: bool,
    pub binary_format: BinaryFormat,
    /// Maximum number of bytes shown for binary values in a grid cell.
    pub binary_max_len: usize,
    /// Text longer than this is truncated in grid cells (the value panel shows everything).
    pub max_text_len: usize,
    /// Saved as `bool_display`: settings of earlier versions (true / false
    /// by default) switch to the check boxes once.
    #[serde(rename = "bool_display")]
    pub bool_style: BoolStyle,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            null_text: "NULL".into(),
            date_format: "%Y-%m-%d".into(),
            time_format: "%H:%M:%S".into(),
            datetime_format: "%Y-%m-%d %H:%M:%S".into(),
            show_fractional_seconds: true,
            float_max_decimals: None,
            thousands_separator: true,
            binary_format: BinaryFormat::Hex,
            binary_max_len: 64,
            max_text_len: 512,
            bool_style: BoolStyle::Checkbox,
        }
    }
}

fn group_thousands(int_part: &str) -> String {
    let (sign, digits) = match int_part.strip_prefix('-') {
        Some(d) => ("-", d),
        None => ("", int_part),
    };
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("{sign}{out}")
}

fn group_number_text(s: &str) -> String {
    let (int_part, rest) = match s.find(['.', 'e', 'E']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    if int_part.trim_start_matches('-').chars().all(|c| c.is_ascii_digit()) { format!("{}{}", group_thousands(int_part), rest) } else { s.to_string() }
}

fn float_text(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else {
        let s = format!("{f}");
        s
    }
}

/// `s` on one line: each line break shows as `¶`, the rest of the text is kept.
pub fn single_line(s: &str) -> Cow<'_, str> {
    if s.contains(['\n', '\r']) { Cow::Owned(s.replace("\r\n", "¶").replace(['\n', '\r'], "¶")) } else { Cow::Borrowed(s) }
}

fn truncate_chars(s: &str, max: usize) -> Cow<'_, str> {
    if s.len() <= max {
        return Cow::Borrowed(s);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(format!("{}…", &s[..end]))
}

fn time_text(t: &NaiveTime, fractional: bool) -> String {
    if fractional && t.nanosecond() != 0 {
        let micros = t.nanosecond() / 1000;
        if t.nanosecond().is_multiple_of(1000) {
            format!("{}.{:06}", t.format("%H:%M:%S"), micros).trim_end_matches('0').to_string()
        } else {
            format!("{}.{:09}", t.format("%H:%M:%S"), t.nanosecond()).trim_end_matches('0').to_string()
        }
    } else {
        t.format("%H:%M:%S").to_string()
    }
}

/// A time for grid cells, as DBeaver shows it: milliseconds always three
/// digits (`10:16:43.539`, `10:16:43.000`), so a column's values line up. The
/// full precision is in [`Value::to_text`] (copy, editing, the value panel).
fn time_ms(t: &NaiveTime) -> String {
    format!("{}.{:03}", t.format("%H:%M:%S"), (t.nanosecond() / 1_000_000).min(999))
}

/// An offset for grid cells: `+07:00` (hours and minutes), after a space.
fn offset_display(o: &FixedOffset) -> String {
    let secs = o.local_minus_utc();
    let sign = if secs < 0 { '-' } else { '+' };
    let secs = secs.abs();
    format!(" {sign}{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

fn offset_text(o: &FixedOffset) -> String {
    let secs = o.local_minus_utc();
    let sign = if secs < 0 { '-' } else { '+' };
    let secs = secs.abs();
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if m == 0 { format!("{sign}{h:02}") } else { format!("{sign}{h:02}:{m:02}") }
}

impl Value {
    pub fn text(s: impl Into<String>) -> Value {
        Value::Text(s.into().into_boxed_str())
    }
    pub fn decimal(s: impl Into<String>) -> Value {
        Value::Decimal(s.into().into_boxed_str())
    }
    pub fn json(s: impl Into<String>) -> Value {
        Value::Json(s.into().into_boxed_str())
    }
    pub fn bytes(b: impl Into<Vec<u8>>) -> Value {
        Value::Bytes(b.into().into_boxed_slice())
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) | Value::Decimal(s) | Value::Json(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::UInt(u) => i64::try_from(*u).ok(),
            Value::Bool(b) => Some(*b as i64),
            Value::Float(f) if f.fract() == 0.0 && f.is_finite() => Some(*f as i64),
            Value::Decimal(s) | Value::Text(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::UInt(u) => Some(*u as f64),
            Value::Float(f) => Some(*f),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Decimal(s) | Value::Text(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Int(i) => Some(*i != 0),
            Value::UInt(u) => Some(*u != 0),
            Value::Text(s) => parse_bool(s),
            _ => None,
        }
    }

    /// Name of the variant, for diagnostics.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::UInt(_) => "uint",
            Value::Float(_) => "float",
            Value::Decimal(_) => "decimal",
            Value::Text(_) => "text",
            Value::Bytes(_) => "bytes",
            Value::Date(_) => "date",
            Value::Time(_) => "time",
            Value::DateTime(_) => "datetime",
            Value::DateTimeTz(_) => "datetimetz",
            Value::Uuid(_) => "uuid",
            Value::Json(_) => "json",
            Value::Array(_) => "array",
        }
    }

    /// Canonical text form, independent of user formatting settings. Used for
    /// copy, CSV export and as the starting text of inline editing.
    /// `Null` becomes an empty string; binary becomes lowercase hex.
    pub fn to_text(&self) -> Cow<'_, str> {
        match self {
            Value::Null => Cow::Borrowed(""),
            Value::Bool(b) => Cow::Borrowed(if *b { "true" } else { "false" }),
            Value::Int(i) => Cow::Owned(i.to_string()),
            Value::UInt(u) => Cow::Owned(u.to_string()),
            Value::Float(f) => Cow::Owned(float_text(*f)),
            Value::Decimal(s) | Value::Text(s) | Value::Json(s) => Cow::Borrowed(s),
            Value::Bytes(b) => Cow::Owned(hex::encode(b)),
            Value::Date(d) => Cow::Owned(d.format("%Y-%m-%d").to_string()),
            Value::Time(t) => Cow::Owned(time_text(t, true)),
            Value::DateTime(dt) => Cow::Owned(format!("{} {}", dt.date().format("%Y-%m-%d"), time_text(&dt.time(), true))),
            Value::DateTimeTz(dt) => {
                Cow::Owned(format!("{} {}{}", dt.naive_local().date().format("%Y-%m-%d"), time_text(&dt.naive_local().time(), true), offset_text(dt.offset())))
            }
            Value::Uuid(u) => Cow::Owned(u.hyphenated().to_string()),
            Value::Array(items) => {
                let parts: Vec<String> = items
                    .iter()
                    .map(|v| match v {
                        Value::Null => "NULL".to_string(),
                        Value::Text(s) => {
                            if s.is_empty() || s.contains([',', '{', '}', '"', '\\', ' ']) || s.eq_ignore_ascii_case("null") {
                                format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
                            } else {
                                s.to_string()
                            }
                        }
                        other => other.to_text().into_owned(),
                    })
                    .collect();
                Cow::Owned(format!("{{{}}}", parts.join(",")))
            }
        }
    }

    /// Text for a grid cell using user formatting settings.
    pub fn display(&self, opts: &FormatOptions) -> Cow<'_, str> {
        match self {
            Value::Null => Cow::Owned(opts.null_text.clone()),
            Value::Bool(b) => match opts.bool_style {
                BoolStyle::OneZero => Cow::Borrowed(if *b { "1" } else { "0" }),
                BoolStyle::Checkbox => Cow::Borrowed(if *b { "☑" } else { "☐" }),
                BoolStyle::TrueFalse => Cow::Borrowed(if *b { "true" } else { "false" }),
            },
            Value::Int(i) => {
                let s = i.to_string();
                if opts.thousands_separator { Cow::Owned(group_thousands(&s)) } else { Cow::Owned(s) }
            }
            Value::UInt(u) => {
                let s = u.to_string();
                if opts.thousands_separator { Cow::Owned(group_thousands(&s)) } else { Cow::Owned(s) }
            }
            Value::Float(f) => {
                let s = match opts.float_max_decimals {
                    Some(d) if f.is_finite() => {
                        let s = format!("{f:.d$}");
                        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
                    }
                    _ => float_text(*f),
                };
                if opts.thousands_separator { Cow::Owned(group_number_text(&s)) } else { Cow::Owned(s) }
            }
            Value::Decimal(s) => {
                if opts.thousands_separator {
                    Cow::Owned(group_number_text(s))
                } else {
                    Cow::Borrowed(s)
                }
            }
            Value::Text(s) | Value::Json(s) => match single_line(s) {
                Cow::Borrowed(t) => truncate_chars(t, opts.max_text_len),
                Cow::Owned(t) => Cow::Owned(truncate_chars(&t, opts.max_text_len).into_owned()),
            },
            Value::Bytes(b) => {
                let shown = &b[..b.len().min(opts.binary_max_len)];
                let more = if b.len() > shown.len() { "…" } else { "" };
                match opts.binary_format {
                    BinaryFormat::Hex => Cow::Owned(format!("0x{}{}", hex::encode_upper(shown), more)),
                    BinaryFormat::Base64 => {
                        use base64::Engine;
                        Cow::Owned(format!("{}{}", base64::engine::general_purpose::STANDARD.encode(shown), more))
                    }
                    BinaryFormat::Text => Cow::Owned(format!("{}{}", String::from_utf8_lossy(shown), more)),
                }
            }
            Value::Date(d) => Cow::Owned(strftime(|out| write!(out, "{}", d.format(&opts.date_format))).unwrap_or_else(|| self.to_text().into_owned())),
            Value::Time(t) => {
                if opts.show_fractional_seconds && opts.time_format == "%H:%M:%S" {
                    Cow::Owned(time_ms(t))
                } else {
                    Cow::Owned(strftime(|out| write!(out, "{}", t.format(&opts.time_format))).unwrap_or_else(|| self.to_text().into_owned()))
                }
            }
            Value::DateTime(dt) => {
                if opts.show_fractional_seconds && opts.datetime_format == "%Y-%m-%d %H:%M:%S" {
                    Cow::Owned(format!("{} {}", dt.date().format("%Y-%m-%d"), time_ms(&dt.time())))
                } else {
                    Cow::Owned(strftime(|out| write!(out, "{}", dt.format(&opts.datetime_format))).unwrap_or_else(|| self.to_text().into_owned()))
                }
            }
            Value::DateTimeTz(dt) => {
                let local = dt.naive_local();
                if opts.show_fractional_seconds && opts.datetime_format == "%Y-%m-%d %H:%M:%S" {
                    Cow::Owned(format!("{} {}{}", local.date().format("%Y-%m-%d"), time_ms(&local.time()), offset_display(dt.offset())))
                } else {
                    Cow::Owned(strftime(|out| write!(out, "{}{}", local.format(&opts.datetime_format), offset_display(dt.offset()))).unwrap_or_else(|| self.to_text().into_owned()))
                }
            }
            Value::Uuid(_) | Value::Array(_) => {
                let t = self.to_text().into_owned();
                Cow::Owned(truncate_chars(&t, opts.max_text_len).into_owned())
            }
        }
    }

    /// Parse user input (inline editing, imports) into a value of `kind`.
    /// An empty input yields `Null` for every non-textual kind.
    pub fn parse_as(input: &str, kind: DataKind) -> Result<Value, String> {
        let t = input.trim();
        if t.is_empty() && !matches!(kind, DataKind::Text | DataKind::Enum | DataKind::Xml) {
            return Ok(Value::Null);
        }
        match kind {
            DataKind::Bool => parse_bool(t).map(Value::Bool).ok_or_else(|| format!("'{t}' is not a boolean")),
            DataKind::Int => {
                if let Ok(i) = t.parse::<i64>() {
                    Ok(Value::Int(i))
                } else if let Ok(u) = t.parse::<u64>() {
                    Ok(Value::UInt(u))
                } else {
                    Err(format!("'{t}' is not an integer"))
                }
            }
            DataKind::Float => match t {
                "NaN" | "nan" => Ok(Value::Float(f64::NAN)),
                "Infinity" | "inf" => Ok(Value::Float(f64::INFINITY)),
                "-Infinity" | "-inf" => Ok(Value::Float(f64::NEG_INFINITY)),
                _ => t.parse::<f64>().map(Value::Float).map_err(|_| format!("'{t}' is not a number")),
            },
            DataKind::Decimal => {
                if is_decimal_text(t) {
                    Ok(Value::decimal(t))
                } else {
                    Err(format!("'{t}' is not a decimal number"))
                }
            }
            // A timestamp at midnight is its date (a time of day would be lost: no date).
            DataKind::Date => parse_date(t)
                .or_else(|| datetime_ignoring_zone(t).filter(|d| d.time() == NaiveTime::MIN).map(|d| d.date()))
                .map(Value::Date)
                .ok_or_else(|| format!("'{t}' is not a date (expected YYYY-MM-DD)")),
            // A zone after the time is ignored: the column has none.
            DataKind::Time => {
                parse_time(t).or_else(|| split_zone(t).and_then(|(rest, _)| time_only(rest))).map(Value::Time).ok_or_else(|| format!("'{t}' is not a time (expected HH:MM:SS)"))
            }
            DataKind::DateTime => datetime_ignoring_zone(t).map(Value::DateTime).ok_or_else(|| format!("'{t}' is not a timestamp (expected YYYY-MM-DD HH:MM:SS)")),
            DataKind::DateTimeTz => parse_datetime_tz(t).map(Value::DateTimeTz).ok_or_else(|| format!("'{t}' is not a timestamp with time zone")),
            DataKind::Uuid => Uuid::parse_str(t).map(Value::Uuid).map_err(|_| format!("'{t}' is not a UUID")),
            DataKind::Json => serde_json::from_str::<serde_json::Value>(t).map(|_| Value::json(t)).map_err(|e| format!("Invalid JSON: {e}")),
            DataKind::Binary => parse_hex_bytes(t).map(Value::bytes).ok_or_else(|| "Binary values must be hexadecimal (optionally prefixed with 0x or \\x)".to_string()),
            _ => Ok(Value::text(input)),
        }
    }

    /// Total order used for client side sorting. Nulls sort first.
    pub fn cmp_for_sort(&self, other: &Value) -> Ordering {
        use Value::*;
        fn rank(v: &Value) -> u8 {
            match v {
                Null => 0,
                Bool(_) => 1,
                Int(_) | UInt(_) | Float(_) | Decimal(_) => 2,
                Date(_) | DateTime(_) => 3,
                DateTimeTz(_) => 4,
                Time(_) => 5,
                Uuid(_) => 6,
                Text(_) | Json(_) => 7,
                Bytes(_) => 8,
                Array(_) => 9,
            }
        }
        match (self, other) {
            (Null, Null) => Ordering::Equal,
            (Bool(a), Bool(b)) => a.cmp(b),
            (Int(a), Int(b)) => a.cmp(b),
            (UInt(a), UInt(b)) => a.cmp(b),
            (Int(a), UInt(b)) => (*a as i128).cmp(&(*b as i128)),
            (UInt(a), Int(b)) => (*a as i128).cmp(&(*b as i128)),
            (Float(a), Float(b)) => cmp_f64(*a, *b),
            (Int(a), Float(b)) => cmp_i128_f64(*a as i128, *b),
            (Float(a), Int(b)) => cmp_i128_f64(*b as i128, *a).reverse(),
            (UInt(a), Float(b)) => cmp_i128_f64(*a as i128, *b),
            (Float(a), UInt(b)) => cmp_i128_f64(*b as i128, *a).reverse(),
            (Decimal(a), Decimal(b)) => ExactNum::parse_decimal(a).cmp(&ExactNum::parse_decimal(b)),
            (Int(a), Decimal(b)) => ExactNum::Dec(ExactDec::from_i128(*a as i128)).cmp(&ExactNum::parse_decimal(b)),
            (Decimal(a), Int(b)) => ExactNum::parse_decimal(a).cmp(&ExactNum::Dec(ExactDec::from_i128(*b as i128))),
            (UInt(a), Decimal(b)) => ExactNum::Dec(ExactDec::from_i128(*a as i128)).cmp(&ExactNum::parse_decimal(b)),
            (Decimal(a), UInt(b)) => ExactNum::parse_decimal(a).cmp(&ExactNum::Dec(ExactDec::from_i128(*b as i128))),
            (Float(a), Decimal(b)) => cmp_f64_decimal(*a, b),
            (Decimal(a), Float(b)) => cmp_f64_decimal(*b, a).reverse(),
            (Text(a), Text(b)) | (Json(a), Json(b)) => a.cmp(b),
            (Date(a), Date(b)) => a.cmp(b),
            (Time(a), Time(b)) => a.cmp(b),
            (DateTime(a), DateTime(b)) => a.cmp(b),
            (DateTimeTz(a), DateTimeTz(b)) => a.cmp(b),
            (Date(a), DateTime(b)) => a.and_hms_opt(0, 0, 0).unwrap().cmp(b),
            (DateTime(a), Date(b)) => a.cmp(&b.and_hms_opt(0, 0, 0).unwrap()),
            (Uuid(a), Uuid(b)) => a.cmp(b),
            (Bytes(a), Bytes(b)) => a.cmp(b),
            (Array(a), Array(b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    let o = x.cmp_for_sort(y);
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                a.len().cmp(&b.len())
            }
            (a, b) => rank(a).cmp(&rank(b)).then_with(|| a.to_text().cmp(&b.to_text())),
        }
    }

    /// Approximate heap bytes owned by this value (excluding the 24 inline bytes).
    pub fn heap_size(&self) -> usize {
        match self {
            Value::Decimal(s) | Value::Text(s) | Value::Json(s) => s.len(),
            Value::Bytes(b) => b.len(),
            Value::Array(items) => items.len() * std::mem::size_of::<Value>() + items.iter().map(Value::heap_size).sum::<usize>(),
            _ => 0,
        }
    }

    /// Convert to a JSON value (for JSON export, AI context, clipboard).
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Int(i) => J::from(*i),
            Value::UInt(u) => J::from(*u),
            Value::Float(f) => serde_json::Number::from_f64(*f).map(J::Number).unwrap_or_else(|| J::String(float_text(*f))),
            Value::Decimal(s) => match s.parse::<serde_json::Number>() {
                Ok(n) => J::Number(n),
                Err(_) => J::String(s.to_string()),
            },
            Value::Json(s) => serde_json::from_str(s).unwrap_or_else(|_| J::String(s.to_string())),
            Value::Array(items) => J::Array(items.iter().map(Value::to_json).collect()),
            Value::Bytes(b) => {
                use base64::Engine;
                J::String(base64::engine::general_purpose::STANDARD.encode(b))
            }
            other => J::String(other.to_text().into_owned()),
        }
    }

    /// Append a compact binary encoding of the value (used for spilling rows to disk).
    pub fn encode(&self, out: &mut Vec<u8>) {
        fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        match self {
            Value::Null => out.push(0),
            Value::Bool(b) => {
                out.push(1);
                out.push(*b as u8);
            }
            Value::Int(i) => {
                out.push(2);
                out.extend_from_slice(&i.to_le_bytes());
            }
            Value::UInt(u) => {
                out.push(3);
                out.extend_from_slice(&u.to_le_bytes());
            }
            Value::Float(f) => {
                out.push(4);
                out.extend_from_slice(&f.to_le_bytes());
            }
            Value::Decimal(s) => {
                out.push(5);
                put_bytes(out, s.as_bytes());
            }
            Value::Text(s) => {
                out.push(6);
                put_bytes(out, s.as_bytes());
            }
            Value::Bytes(b) => {
                out.push(7);
                put_bytes(out, b);
            }
            Value::Date(d) => {
                out.push(8);
                out.extend_from_slice(&d.num_days_from_ce().to_le_bytes());
            }
            Value::Time(t) => {
                out.push(9);
                out.extend_from_slice(&t.num_seconds_from_midnight().to_le_bytes());
                out.extend_from_slice(&t.nanosecond().to_le_bytes());
            }
            Value::DateTime(dt) => {
                out.push(10);
                out.extend_from_slice(&dt.date().num_days_from_ce().to_le_bytes());
                out.extend_from_slice(&dt.time().num_seconds_from_midnight().to_le_bytes());
                out.extend_from_slice(&dt.time().nanosecond().to_le_bytes());
            }
            Value::DateTimeTz(dt) => {
                out.push(11);
                let local = dt.naive_local();
                out.extend_from_slice(&local.date().num_days_from_ce().to_le_bytes());
                out.extend_from_slice(&local.time().num_seconds_from_midnight().to_le_bytes());
                out.extend_from_slice(&local.time().nanosecond().to_le_bytes());
                out.extend_from_slice(&dt.offset().local_minus_utc().to_le_bytes());
            }
            Value::Uuid(u) => {
                out.push(12);
                out.extend_from_slice(u.as_bytes());
            }
            Value::Json(s) => {
                out.push(13);
                put_bytes(out, s.as_bytes());
            }
            Value::Array(items) => {
                out.push(14);
                out.extend_from_slice(&(items.len() as u32).to_le_bytes());
                for v in items.iter() {
                    v.encode(out);
                }
            }
        }
    }

    /// Decode a value previously written by [`Value::encode`], advancing `input`.
    pub fn decode(input: &mut &[u8]) -> Option<Value> {
        fn take<'a>(input: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
            if input.len() < n {
                return None;
            }
            let (a, b) = input.split_at(n);
            *input = b;
            Some(a)
        }
        fn u32_(input: &mut &[u8]) -> Option<u32> {
            Some(u32::from_le_bytes(take(input, 4)?.try_into().ok()?))
        }
        fn i32_(input: &mut &[u8]) -> Option<i32> {
            Some(i32::from_le_bytes(take(input, 4)?.try_into().ok()?))
        }
        fn bytes<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
            let n = u32_(input)? as usize;
            take(input, n)
        }
        fn string(input: &mut &[u8]) -> Option<Box<str>> {
            Some(String::from_utf8(bytes(input)?.to_vec()).ok()?.into_boxed_str())
        }
        fn date(input: &mut &[u8]) -> Option<NaiveDate> {
            NaiveDate::from_num_days_from_ce_opt(i32_(input)?)
        }
        fn time(input: &mut &[u8]) -> Option<NaiveTime> {
            let secs = u32_(input)?;
            let nanos = u32_(input)?;
            NaiveTime::from_num_seconds_from_midnight_opt(secs, nanos)
        }
        let tag = *take(input, 1)?.first()?;
        Some(match tag {
            0 => Value::Null,
            1 => Value::Bool(take(input, 1)?[0] != 0),
            2 => Value::Int(i64::from_le_bytes(take(input, 8)?.try_into().ok()?)),
            3 => Value::UInt(u64::from_le_bytes(take(input, 8)?.try_into().ok()?)),
            4 => Value::Float(f64::from_le_bytes(take(input, 8)?.try_into().ok()?)),
            5 => Value::Decimal(string(input)?),
            6 => Value::Text(string(input)?),
            7 => Value::Bytes(bytes(input)?.to_vec().into_boxed_slice()),
            8 => Value::Date(date(input)?),
            9 => Value::Time(time(input)?),
            10 => {
                let d = date(input)?;
                let t = time(input)?;
                Value::DateTime(NaiveDateTime::new(d, t))
            }
            11 => {
                let d = date(input)?;
                let t = time(input)?;
                let off = FixedOffset::east_opt(i32_(input)?)?;
                let local = NaiveDateTime::new(d, t);
                Value::DateTimeTz(local.and_local_timezone(off).single()?)
            }
            12 => Value::Uuid(Uuid::from_slice(take(input, 16)?).ok()?),
            13 => Value::Json(string(input)?),
            14 => {
                let n = u32_(input)? as usize;
                let mut items = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    items.push(Value::decode(input)?);
                }
                Value::Array(items.into_boxed_slice())
            }
            _ => return None,
        })
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("NULL"),
            other => f.write_str(&other.to_text()),
        }
    }
}

pub fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "1" | "yes" | "y" | "on" => Some(true),
        "false" | "f" | "0" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

/// Strip balanced enclosing parentheses (`((0))` → `0`, `(false)` → `false`)
/// without altering expressions like `(a) + (b)` or `nextval('seq'::regclass)`.
pub fn display_default(expr: &str) -> &str {
    let mut s = expr.trim();
    loop {
        if s.len() >= 2 && s.starts_with('(') && s.ends_with(')') {
            let mut depth = 0i32;
            let mut in_quote = false;
            let mut encloses_all = true;
            let bytes = s.as_bytes();
            for (i, &b) in bytes.iter().enumerate() {
                if b == b'\'' {
                    in_quote = !in_quote;
                } else if !in_quote {
                    if b == b'(' {
                        depth += 1;
                    } else if b == b')' {
                        depth -= 1;
                        if depth == 0 && i + 1 < bytes.len() {
                            encloses_all = false;
                            break;
                        }
                    }
                }
            }
            if encloses_all && depth == 0 {
                s = s[1..s.len() - 1].trim();
                continue;
            }
        }
        break;
    }
    s
}

/// Interpret a SQL column default expression as a boolean literal, if it is one
/// (`false`, `true`, `0`, `1`, `((0))`, `'false'::boolean`, `b'1'`, `'f'`, …).
pub fn parse_default_bool(expr: &str) -> Option<bool> {
    let mut s = display_default(expr);
    // Strip a top-level PostgreSQL type cast suffix (`'false'::boolean`, `0::bit(1)`).
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut cast_pos = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\'' {
            in_quote = !in_quote;
        } else if !in_quote {
            if b == b'(' {
                depth += 1;
            } else if b == b')' {
                depth -= 1;
            } else if depth == 0 && b == b':' && i + 1 < bytes.len() && bytes[i + 1] == b':' {
                cast_pos = Some(i);
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    if let Some(idx) = cast_pos {
        let cast = s[idx + 2..].trim();
        if !cast.is_empty() && cast.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '(' | ')')) {
            s = display_default(s[..idx].trim());
        }
    }
    if (s.starts_with("b'") || s.starts_with("B'")) && s.ends_with('\'') && s.len() >= 3 {
        s = &s[2..s.len() - 1];
    } else if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s = &s[1..s.len() - 1];
    }
    parse_bool(s)
}

pub fn is_decimal_text(t: &str) -> bool {
    let t = t.strip_prefix(['-', '+']).unwrap_or(t);
    if t.is_empty() {
        return false;
    }
    if t.eq_ignore_ascii_case("nan") || t.eq_ignore_ascii_case("infinity") {
        return true;
    }
    let (mantissa, exp) = match t.find(['e', 'E']) {
        Some(i) => (&t[..i], Some(&t[i + 1..])),
        None => (t, None),
    };
    let mut dot = false;
    let mut digits = 0;
    for c in mantissa.chars() {
        match c {
            '0'..='9' => digits += 1,
            '.' if !dot => dot = true,
            _ => return false,
        }
    }
    if digits == 0 {
        return false;
    }
    match exp {
        None => true,
        Some(e) => {
            let e = e.strip_prefix(['-', '+']).unwrap_or(e);
            !e.is_empty() && e.chars().all(|c| c.is_ascii_digit())
        }
    }
}

fn date_only(t: &str) -> Option<NaiveDate> {
    for f in ["%Y-%m-%d", "%Y/%m/%d", "%d.%m.%Y", "%Y%m%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(t, f) {
            return Some(d);
        }
    }
    None
}

fn time_only(t: &str) -> Option<NaiveTime> {
    for f in ["%H:%M:%S%.f", "%H:%M:%S", "%H:%M"] {
        if let Ok(v) = NaiveTime::parse_from_str(t, f) {
            return Some(v);
        }
    }
    None
}

/// A date and time without a zone; a date alone is midnight.
fn naive_datetime(t: &str) -> Option<NaiveDateTime> {
    for f in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y/%m/%d %H:%M:%S%.f", "%d.%m.%Y %H:%M:%S"] {
        if let Ok(v) = NaiveDateTime::parse_from_str(t, f) {
            return Some(v);
        }
    }
    date_only(t).and_then(|d| d.and_hms_opt(0, 0, 0))
}

/// `t` without the time zone at its end (`+07`, `-05:30`, `+0700`, `Z`, `UTC`,
/// `GMT`) and that zone's offset.
fn split_zone(t: &str) -> Option<(&str, FixedOffset)> {
    let t = t.trim_end();
    for name in ["UTC", "GMT", "Z", "z"] {
        if let Some(rest) = t.strip_suffix(name)
            && rest.ends_with(|c: char| c.is_ascii_digit() || c == ' ')
        {
            return Some((rest.trim_end(), FixedOffset::east_opt(0)?));
        }
    }
    let i = t.rfind(['+', '-'])?;
    let (rest, zone) = t.split_at(i);
    // After a time: not the '-' of a date.
    if !rest.contains(':') {
        return None;
    }
    let digits: String = zone[1..].chars().filter(|c| *c != ':').collect();
    if !matches!(digits.len(), 2 | 4) || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (h, m): (i32, i32) = (digits[..2].parse().ok()?, if digits.len() == 4 { digits[2..].parse().ok()? } else { 0 });
    let secs = (h * 3600 + m * 60) * if zone.starts_with('-') { -1 } else { 1 };
    Some((rest.trim_end(), FixedOffset::east_opt(secs)?))
}

pub fn parse_date(t: &str) -> Option<NaiveDate> {
    date_only(t)
}

pub fn parse_time(t: &str) -> Option<NaiveTime> {
    time_only(t)
}

pub fn parse_datetime(t: &str) -> Option<NaiveDateTime> {
    naive_datetime(t)
}

/// A date and time, a zone in the text ignored: the date and time stay as
/// written (as PostgreSQL reads a `timestamp`).
fn datetime_ignoring_zone(t: &str) -> Option<NaiveDateTime> {
    naive_datetime(t).or_else(|| split_zone(t).and_then(|(rest, _)| naive_datetime(rest)))
}

pub fn parse_datetime_tz(t: &str) -> Option<DateTime<FixedOffset>> {
    if let Ok(v) = DateTime::parse_from_rfc3339(t) {
        return Some(v);
    }
    if let Some((rest, offset)) = split_zone(t)
        && let Some(naive) = naive_datetime(rest)
    {
        return naive.and_local_timezone(offset).single();
    }
    // No offset given: interpret in the local time zone.
    let naive = naive_datetime(t)?;
    let local = naive.and_local_timezone(chrono::Local).earliest()?;
    Some(local.fixed_offset())
}

/// `text` reads as SQL for the database to evaluate rather than as a value: a
/// function call (`now()`, `gen_random_uuid()`, `now() - interval '1 day'`) or a
/// date / time keyword (`CURRENT_TIMESTAMP`, `CURRENT_DATE`, `SYSDATE` …).
pub fn is_sql_expression(text: &str) -> bool {
    const KEYWORDS: [&str; 7] = ["CURRENT_TIMESTAMP", "CURRENT_DATE", "CURRENT_TIME", "LOCALTIMESTAMP", "LOCALTIME", "SYSDATE", "SYSTIMESTAMP"];
    let t = text.trim();
    let end = t.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).unwrap_or(t.len());
    let (word, rest) = t.split_at(end);
    if word.is_empty() || word.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return false;
    }
    let call = rest.trim_start().starts_with('(');
    let keyword = KEYWORDS.iter().any(|k| word.eq_ignore_ascii_case(k));
    (call || keyword) && balanced(t)
}

/// Parentheses closed and quotes ended (outside of quotes), no statement
/// separator, comment, top-level comma/assignment, SQL clause keywords, or
/// dialect-ambiguous quoting (backslashes inside quotes, dollar-quotes,
/// backticks, bracket identifiers, `E'...'` / `q'...'`): the text is inserted
/// verbatim into `UPDATE … SET c = <text> WHERE …` or `INSERT … VALUES (…)`.
fn balanced(t: &str) -> bool {
    const FORBIDDEN_ANY_DEPTH: &[&str] = &[
        "SELECT",
        "DELETE",
        "UPDATE",
        "INSERT",
        "DROP",
        "ALTER",
        "CREATE",
        "TRUNCATE",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "INTO",
        "WHERE",
        "SET",
        "RETURNING",
        "EXEC",
        "EXECUTE",
        "PRAGMA",
        "ATTACH",
        "DETACH",
        "COPY",
        "OUTFILE",
        "DUMPFILE",
        "GRANT",
        "REVOKE",
        "MERGE",
        "CALL",
        "DO",
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "SAVEPOINT",
        "TABLE",
        "WITH",
    ];
    const FORBIDDEN_TOP_LEVEL: &[&str] = &["FROM", "VALUES", "LIMIT", "OFFSET", "ORDER", "GROUP", "HAVING", "FOR", "ON", "JOIN"];
    let check_word = |word: &str, depth: i32| -> bool {
        if word.is_empty() {
            return true;
        }
        if FORBIDDEN_ANY_DEPTH.iter().any(|k| word.eq_ignore_ascii_case(k)) {
            return false;
        }
        if depth == 0 && FORBIDDEN_TOP_LEVEL.iter().any(|k| word.eq_ignore_ascii_case(k)) {
            return false;
        }
        true
    };

    let (mut depth, mut quote) = (0i32, None);
    let mut word = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\0' {
            return false;
        }
        match quote {
            Some(q) => {
                // Reject `\` inside quotes: MySQL treats `\'` as an escaped quote
                // while standard SQL / PostgreSQL treats it as literal `\` followed
                // by a closing quote, which would desync quote parity across dialects.
                if c == '\\' {
                    return false;
                }
                if c == q {
                    if chars.peek() == Some(&q) {
                        chars.next();
                    } else {
                        quote = None;
                    }
                }
            }
            None => {
                if c.is_alphanumeric() || c == '_' {
                    word.push(c);
                    continue;
                }
                // Reject dialect-specific quote prefixes (`E'...'`, `q'...'`, `nq'...'`).
                if c == '\'' && (word.eq_ignore_ascii_case("q") || word.eq_ignore_ascii_case("nq") || word.eq_ignore_ascii_case("e")) {
                    return false;
                }
                if !check_word(&word, depth) {
                    return false;
                }
                word.clear();
                match c {
                    '\'' | '"' => quote = Some(c),
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth < 0 {
                            return false;
                        }
                    }
                    ',' | '=' if depth == 0 => return false,
                    ';' | '#' | '$' | '`' | '[' | ']' | '\\' => return false,
                    '-' if chars.peek() == Some(&'-') => return false,
                    '/' if chars.peek() == Some(&'*') => return false,
                    '*' if chars.peek() == Some(&'/') => return false,
                    '&' if chars.peek() == Some(&'\'') => return false,
                    _ => {}
                }
            }
        }
    }
    if !check_word(&word, depth) {
        return false;
    }
    depth == 0 && quote.is_none()
}

/// Mathematical order on `f64`: `-Inf < finite (-0.0 == +0.0) < +Inf < NaN`.
fn cmp_f64(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => a.partial_cmp(&b).unwrap(),
    }
}

/// Exact comparison between an `i128` and an `f64` (`-Inf < finite < +Inf < NaN`).
fn cmp_i128_f64(i: i128, f: f64) -> Ordering {
    if f.is_nan() {
        return Ordering::Less;
    }
    if f.is_infinite() {
        return if f > 0.0 { Ordering::Less } else { Ordering::Greater };
    }
    // 2^127 is exact in IEEE 754 f64; i128 spans [-2^127, 2^127 - 1].
    const TWO_127: f64 = 170141183460469231731687303715884105728.0;
    if f < -TWO_127 {
        return Ordering::Greater;
    }
    if f >= TWO_127 {
        return Ordering::Less;
    }
    let t = f.trunc();
    let ti = t as i128;
    match i.cmp(&ti) {
        Ordering::Equal => {
            let frac = f - t;
            if frac > 0.0 {
                Ordering::Less
            } else if frac < 0.0 {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        o => o,
    }
}

/// Normalized exact decimal `(-1)^neg * digits * 10^exp` with no leading or
/// trailing `'0'` in `digits`. Zero is represented by empty `digits`, `neg = false`, `exp = 0`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExactDec {
    neg: bool,
    digits: Vec<u8>,
    exp: i128,
}

impl ExactDec {
    fn zero() -> Self {
        ExactDec { neg: false, digits: Vec::new(), exp: 0 }
    }

    fn from_digits(neg: bool, mut digits: Vec<u8>, mut exp: i128) -> Self {
        let lead = digits.iter().take_while(|&&b| b == b'0').count();
        if lead > 0 {
            digits.drain(..lead);
        }
        while digits.last() == Some(&b'0') {
            digits.pop();
            exp = exp.saturating_add(1);
        }
        if digits.is_empty() {
            return ExactDec::zero();
        }
        ExactDec { neg, digits, exp }
    }

    fn from_i128(n: i128) -> Self {
        if n == 0 {
            return ExactDec::zero();
        }
        ExactDec::from_digits(n < 0, n.unsigned_abs().to_string().into_bytes(), 0)
    }

    /// Exact decimal expansion of any finite `f64` (`m * 2^e`).
    fn from_finite_f64(f: f64) -> Self {
        debug_assert!(f.is_finite());
        let bits = f.to_bits();
        let neg = (bits >> 63) != 0;
        let raw_exp = ((bits >> 52) & 0x7ff) as i32;
        let raw_mant = bits & ((1u64 << 52) - 1);
        if raw_exp == 0 && raw_mant == 0 {
            return ExactDec::zero();
        }
        let (mut mant, mut exp2) = if raw_exp == 0 { (raw_mant, -1074i32) } else { ((1u64 << 52) | raw_mant, raw_exp - 1075) };
        let tz = mant.trailing_zeros();
        mant >>= tz;
        exp2 += tz as i32;

        const BASE: u64 = 1_000_000_000;
        let mut limbs = Vec::new();
        let mut v = mant;
        while v > 0 {
            limbs.push((v % BASE) as u32);
            v /= BASE;
        }
        let mul_small = |limbs: &mut Vec<u32>, factor: u64| {
            let mut carry = 0u64;
            for limb in limbs.iter_mut() {
                let prod = u64::from(*limb) * factor + carry;
                *limb = (prod % BASE) as u32;
                carry = prod / BASE;
            }
            while carry > 0 {
                limbs.push((carry % BASE) as u32);
                carry /= BASE;
            }
        };
        let exp10 = if exp2 >= 0 {
            let mut rem = exp2 as u32;
            while rem > 0 {
                let step = rem.min(29);
                mul_small(&mut limbs, 1u64 << step);
                rem -= step;
            }
            0i128
        } else {
            let k = (-exp2) as u32;
            let mut rem = k;
            while rem > 0 {
                let step = rem.min(13);
                mul_small(&mut limbs, 5u64.pow(step));
                rem -= step;
            }
            -i128::from(k)
        };
        let mut s = String::with_capacity(limbs.len() * 9);
        if let Some((&top, rest)) = limbs.split_last() {
            let _ = write!(s, "{top}");
            for &limb in rest.iter().rev() {
                let _ = write!(s, "{limb:09}");
            }
        }
        ExactDec::from_digits(neg, s.into_bytes(), exp10)
    }

    fn cmp_magnitude(&self, other: &Self) -> Ordering {
        match (self.digits.is_empty(), other.digits.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            (false, false) => {}
        }
        let msd_a = self.exp.saturating_add(self.digits.len() as i128);
        let msd_b = other.exp.saturating_add(other.digits.len() as i128);
        msd_a.cmp(&msd_b).then_with(|| {
            let n = self.digits.len().max(other.digits.len());
            for i in 0..n {
                let da = self.digits.get(i).copied().unwrap_or(b'0');
                let db = other.digits.get(i).copied().unwrap_or(b'0');
                let o = da.cmp(&db);
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        })
    }
}

impl Ord for ExactDec {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.neg, other.neg) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => self.cmp_magnitude(other),
            (true, true) => other.cmp_magnitude(self),
        }
    }
}

impl PartialOrd for ExactDec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExactNum {
    NegInf,
    Dec(ExactDec),
    PosInf,
    NaN,
}

impl ExactNum {
    fn parse_decimal(s: &str) -> Self {
        let s = s.trim();
        if s.is_empty() {
            return ExactNum::NaN;
        }
        let (neg, rest) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        if rest.eq_ignore_ascii_case("nan") {
            return ExactNum::NaN;
        }
        if rest.eq_ignore_ascii_case("inf") || rest.eq_ignore_ascii_case("infinity") {
            return if neg { ExactNum::NegInf } else { ExactNum::PosInf };
        }
        let (mantissa, exp_opt) = match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let (int_part, frac_part) = match mantissa.split_once('.') {
            Some((a, b)) => (a, b),
            None => (mantissa, ""),
        };
        if (int_part.is_empty() && frac_part.is_empty()) || !int_part.bytes().all(|b| b.is_ascii_digit()) || !frac_part.bytes().all(|b| b.is_ascii_digit()) {
            return ExactNum::NaN;
        }
        let mut digits = Vec::with_capacity(int_part.len() + frac_part.len());
        digits.extend_from_slice(int_part.as_bytes());
        digits.extend_from_slice(frac_part.as_bytes());
        let exp_parsed: Result<i128, bool> = match exp_opt {
            None => Ok(0),
            Some(e) => {
                let (eneg, edigits) = match e.strip_prefix('-') {
                    Some(r) => (true, r),
                    None => (false, e.strip_prefix('+').unwrap_or(e)),
                };
                if edigits.is_empty() || !edigits.bytes().all(|b| b.is_ascii_digit()) {
                    return ExactNum::NaN;
                }
                e.parse::<i128>().map_err(|_| eneg)
            }
        };
        if digits.iter().all(|&b| b == b'0') {
            return ExactNum::Dec(ExactDec::zero());
        }
        let exp = match exp_parsed {
            Ok(v) => match v.checked_sub(frac_part.len() as i128) {
                Some(e) => e,
                None => return ExactNum::Dec(ExactDec { neg, digits: vec![b'1'], exp: i128::MIN }),
            },
            Err(false) => return if neg { ExactNum::NegInf } else { ExactNum::PosInf },
            Err(true) => return ExactNum::Dec(ExactDec { neg, digits: vec![b'1'], exp: i128::MIN }),
        };
        ExactNum::Dec(ExactDec::from_digits(neg, digits, exp))
    }

    fn rank(&self) -> u8 {
        match self {
            ExactNum::NegInf => 0,
            ExactNum::Dec(_) => 1,
            ExactNum::PosInf => 2,
            ExactNum::NaN => 3,
        }
    }
}

impl Ord for ExactNum {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (ExactNum::Dec(a), ExactNum::Dec(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for ExactNum {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn cmp_f64_decimal(f: f64, s: &str) -> Ordering {
    let dec = ExactNum::parse_decimal(s);
    if f.is_nan() {
        return ExactNum::NaN.cmp(&dec);
    }
    if f.is_infinite() {
        let nf = if f > 0.0 { ExactNum::PosInf } else { ExactNum::NegInf };
        return nf.cmp(&dec);
    }
    let ExactNum::Dec(ref b) = dec else {
        return ExactNum::Dec(ExactDec::zero()).cmp(&dec);
    };
    if f == 0.0 {
        return ExactDec::zero().cmp(b);
    }
    let f_neg = f.is_sign_negative();
    if b.digits.is_empty() {
        return if f_neg { Ordering::Less } else { Ordering::Greater };
    }
    match (f_neg, b.neg) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let approx_msd = f.abs().log10().floor() as i128 + 1;
    let b_msd = b.exp.saturating_add(b.digits.len() as i128);
    if approx_msd.saturating_add(2) < b_msd {
        return if f_neg { Ordering::Greater } else { Ordering::Less };
    }
    if approx_msd.saturating_sub(2) > b_msd {
        return if f_neg { Ordering::Less } else { Ordering::Greater };
    }
    ExactDec::from_finite_f64(f).cmp(b)
}

pub fn parse_hex_bytes(t: &str) -> Option<Vec<u8>> {
    let h =
        t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).or_else(|| t.strip_prefix("\\x")).or_else(|| t.strip_prefix("X'").and_then(|s| s.strip_suffix('\''))).unwrap_or(t);
    let h: String = h.chars().filter(|c| !c.is_whitespace()).collect();
    hex::decode(h).ok()
}

macro_rules! from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for Value { fn from(v: $t) -> Self { Value::Int(v as i64) } }
    )*};
}
from_int!(i8, i16, i32, i64, u8, u16, u32);

impl From<u64> for Value {
    fn from(v: u64) -> Self {
        Value::UInt(v)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}
impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Float(v as f64)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v.into_boxed_str())
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.into())
    }
}
impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Bytes(v.into_boxed_slice())
    }
}
impl From<NaiveDate> for Value {
    fn from(v: NaiveDate) -> Self {
        Value::Date(v)
    }
}
impl From<NaiveTime> for Value {
    fn from(v: NaiveTime) -> Self {
        Value::Time(v)
    }
}
impl From<NaiveDateTime> for Value {
    fn from(v: NaiveDateTime) -> Self {
        Value::DateTime(v)
    }
}
impl From<DateTime<FixedOffset>> for Value {
    fn from(v: DateTime<FixedOffset>) -> Self {
        Value::DateTimeTz(v)
    }
}
impl From<Uuid> for Value {
    fn from(v: Uuid) -> Self {
        Value::Uuid(v)
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(Value::Null)
    }
}

/// Format with a user's chrono pattern: `None` instead of chrono's panic when
/// the pattern is invalid or does not fit the value (`%H` for a date, `%z`
/// without an offset).
fn strftime(f: impl FnOnce(&mut String) -> fmt::Result) -> Option<String> {
    let mut out = String::new();
    f(&mut out).ok().map(|()| out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_times_show_milliseconds_and_the_offset() {
        use chrono::TimeZone;
        let o = FormatOptions::default();
        let at = |micros: u32| NaiveDate::from_ymd_opt(2026, 10, 22).unwrap().and_hms_micro_opt(10, 16, 43, micros).unwrap();
        let tz = |micros: u32| Value::DateTimeTz(FixedOffset::east_opt(7 * 3600).unwrap().from_local_datetime(&at(micros)).unwrap());
        assert_eq!(tz(539_588).display(&o), "2026-10-22 10:16:43.539 +07:00");
        assert_eq!(tz(0).display(&o), "2026-10-22 10:16:43.000 +07:00", "the same width");
        assert_eq!(Value::DateTime(at(5_000)).display(&o), "2026-10-22 10:16:43.005");
        assert_eq!(Value::Time(at(120_000).time()).display(&o), "10:16:43.120");
        // Copy and editing keep every digit.
        assert_eq!(tz(539_588).to_text(), "2026-10-22 10:16:43.539588+07");
        let no_fraction = FormatOptions { show_fractional_seconds: false, ..FormatOptions::default() };
        assert_eq!(tz(539_588).display(&no_fraction), "2026-10-22 10:16:43 +07:00");
    }

    #[test]
    fn grid_shows_floats_and_text_as_copied() {
        // Grouped by default, as in the design; otherwise as copied.
        assert_eq!(Value::Float(1932.78).display(&FormatOptions::default()), "1,932.78");
        let o = FormatOptions { thousands_separator: false, ..Default::default() };
        assert_eq!(Value::Float(1932.7799999999997).display(&o), "1932.7799999999997");
        assert_eq!(Value::Float(0.1 + 0.2).display(&o), Value::Float(0.1 + 0.2).to_text());
        // Multi-line text keeps every line, joined by ¶.
        assert_eq!(Value::text("\r\n  {\n\"a\": 1\n  }").display(&o), "¶  {¶\"a\": 1¶  }");
    }

    #[test]
    fn date_patterns_that_do_not_fit_fall_back() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 31).unwrap();
        let dt = d.and_hms_opt(10, 5, 0).unwrap();
        let opts = FormatOptions { date_format: "%H:%M".into(), datetime_format: "%Y %z".into(), time_format: "%Y".into(), ..Default::default() };
        assert_eq!(Value::Date(d).display(&opts), "2024-01-31");
        assert_eq!(Value::DateTime(dt).display(&opts), "2024-01-31 10:05:00");
        assert_eq!(Value::Time(dt.time()).display(&opts), "10:05:00");
        let opts = FormatOptions { date_format: "%d.%m.%Y".into(), ..Default::default() };
        assert_eq!(Value::Date(d).display(&opts), "31.01.2024");
    }

    #[test]
    fn value_is_small() {
        assert!(std::mem::size_of::<Value>() <= 24, "{}", std::mem::size_of::<Value>());
    }

    #[test]
    fn encode_roundtrip() {
        let dt = NaiveDate::from_ymd_opt(2024, 2, 29).unwrap().and_hms_micro_opt(13, 14, 15, 123456).unwrap();
        let values = vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(-42),
            Value::UInt(u64::MAX),
            Value::Float(3.25),
            Value::decimal("12345678901234567890.123"),
            Value::text("héllo"),
            Value::bytes(vec![0u8, 1, 255]),
            Value::Date(dt.date()),
            Value::Time(dt.time()),
            Value::DateTime(dt),
            Value::DateTimeTz(dt.and_local_timezone(FixedOffset::east_opt(7 * 3600).unwrap()).unwrap()),
            Value::Uuid(Uuid::new_v4()),
            Value::json(r#"{"a":1}"#),
            Value::Array(vec![Value::Int(1), Value::Null, Value::text("x")].into_boxed_slice()),
        ];
        let mut buf = Vec::new();
        for v in &values {
            v.encode(&mut buf);
        }
        let mut input = &buf[..];
        for v in &values {
            assert_eq!(&Value::decode(&mut input).unwrap(), v);
        }
        assert!(input.is_empty());
    }

    #[test]
    fn parse_and_text() {
        assert_eq!(Value::parse_as("42", DataKind::Int).unwrap(), Value::Int(42));
        assert_eq!(Value::parse_as("", DataKind::Int).unwrap(), Value::Null);
        assert!(Value::parse_as("x", DataKind::Int).is_err());
        assert_eq!(Value::parse_as("t", DataKind::Bool).unwrap(), Value::Bool(true));
        assert!(Value::parse_as("1.5e3", DataKind::Decimal).is_ok());
        assert!(Value::parse_as("1.5.3", DataKind::Decimal).is_err());
        let v = Value::parse_as("2024-01-02 03:04:05", DataKind::DateTime).unwrap();
        assert_eq!(v.to_text(), "2024-01-02 03:04:05");
        let v = Value::parse_as("2024-01-02 03:04:05.5+07:00", DataKind::DateTimeTz).unwrap();
        assert_eq!(v.to_text(), "2024-01-02 03:04:05.5+07");
        assert_eq!(Value::parse_as("0xff00", DataKind::Binary).unwrap(), Value::bytes(vec![255u8, 0]));
        assert!(Value::parse_as("{bad", DataKind::Json).is_err());
    }

    #[test]
    fn default_expr_display_and_bool() {
        assert_eq!(display_default("nextval('ratios_id_seq'::regclass)"), "nextval('ratios_id_seq'::regclass)");
        assert_eq!(display_default("((0))"), "0");
        assert_eq!(display_default("(a) + (b)"), "(a) + (b)");
        assert_eq!(display_default("  (false)  "), "false");

        assert_eq!(parse_default_bool("false"), Some(false));
        assert_eq!(parse_default_bool("TRUE"), Some(true));
        assert_eq!(parse_default_bool("((0))"), Some(false));
        assert_eq!(parse_default_bool("((1))"), Some(true));
        assert_eq!(parse_default_bool("'false'::boolean"), Some(false));
        assert_eq!(parse_default_bool("'t'::bool"), Some(true));
        assert_eq!(parse_default_bool("b'1'"), Some(true));
        assert_eq!(parse_default_bool("B'0'"), Some(false));
        assert_eq!(parse_default_bool("0::bit(1)"), Some(false));
        assert_eq!(parse_default_bool("nextval('ratios_id_seq'::regclass)"), None);
        assert_eq!(parse_default_bool("now()"), None);
    }

    #[test]
    fn timestamps_as_typed_pasted_or_copied() {
        let text = |t: &str, k: DataKind| Value::parse_as(t, k).map(|v| v.to_text().into_owned());
        // As the grid shows them (space before the offset), with seconds of offset or a zone name.
        for (t, expected) in [
            ("2025-11-24 13:48:03.461 +07:00", "2025-11-24 13:48:03.461+07"),
            ("2025-11-24 13:48:03.461 +0700", "2025-11-24 13:48:03.461+07"),
            ("2025-11-24 13:48:03-05:30", "2025-11-24 13:48:03-05:30"),
            ("2025-11-24T13:48:03Z", "2025-11-24 13:48:03+00"),
            ("2025-11-24 13:48:03 UTC", "2025-11-24 13:48:03+00"),
            ("2025-11-24 13:48:03 GMT", "2025-11-24 13:48:03+00"),
        ] {
            assert_eq!(text(t, DataKind::DateTimeTz).as_deref(), Ok(expected), "{t}");
        }
        // Without a zone: the zone in the text is ignored (as PostgreSQL does).
        assert_eq!(text("2025-11-24 13:48:03.461 +07:00", DataKind::DateTime).as_deref(), Ok("2025-11-24 13:48:03.461"));
        assert_eq!(text("2025-11-24T13:48:03Z", DataKind::DateTime).as_deref(), Ok("2025-11-24 13:48:03"));
        // A date from a timestamp at midnight (no time of day lost), a time with a zone.
        assert_eq!(text("2025-11-24 00:00:00", DataKind::Date).as_deref(), Ok("2025-11-24"));
        assert_eq!(text("2025-11-24 00:00:00.000 +07:00", DataKind::Date).as_deref(), Ok("2025-11-24"));
        assert!(Value::parse_as("2025-11-24 13:48:03", DataKind::Date).is_err());
        assert_eq!(text("13:48:03.461 +07:00", DataKind::Time).as_deref(), Ok("13:48:03.461"));
        // Not timestamps.
        for t in ["2025-13-01 10:00", "now()", "13:48 +7", "2025-11-24 25:00"] {
            assert!(Value::parse_as(t, DataKind::DateTimeTz).is_err(), "{t}");
        }
    }

    #[test]
    fn sql_expressions() {
        for t in [
            "now()",
            "NOW()",
            " now() ",
            "CURRENT_TIMESTAMP",
            "current_date",
            "localtimestamp",
            "sysdate",
            "gen_random_uuid()",
            "pg_catalog.now()",
            "now() - interval '1 day'",
            "datetime('now', 'localtime')",
            "CURRENT_TIMESTAMP(3)",
        ] {
            assert!(is_sql_expression(t), "{t}");
        }
        for t in ["", "now", "abc", "2025-11-24", "1(2)", "now(", "now())", "f('a)", "{\"a\": 1}", "(1)", "current_timestamps"] {
            assert!(!is_sql_expression(t), "{t}");
        }
        // Inserted verbatim before `WHERE`: a comment or a second statement would cut it off.
        for t in ["abs(1) --", "abs(1) -- x", "now() /* x */", "now(); DELETE FROM t", "now() # x", "CURRENT_DATE --"] {
            assert!(!is_sql_expression(t), "{t}");
        }
        // Inside quotes they are text.
        for t in ["concat('a--b')", "concat('a;b', \"c/*d\")", "datetime('now', '#')", "concat('it''s')", "coalesce(a, b)", "extract(year from now())"] {
            assert!(is_sql_expression(t), "{t}");
        }
        // Regression tests: top-level comma/assignment, SQL clauses, backslash quote desync,
        // dollar-quoting, backticks, bracket identifiers, and nested subqueries must be rejected.
        for t in [
            "now(), is_admin = true",
            "f(1), role = 'admin'",
            "now() = now()",
            "now() WHERE 1=1",
            "now() SET x = 1",
            "now() UNION SELECT 1",
            "now() INTO outfile",
            "now()WHERE 1=1",
            "f('a\\''), is_admin = 1 WHERE 1=1; -- '",
            "f(\"a\\\"\"), is_admin = 1",
            "f($$; DROP TABLE t; --$$)",
            "f($tag$; DROP TABLE t; $tag$)",
            "f(`a`)",
            "f([a']), is_admin = 1 --'])",
            "f(E'a\\''), is_admin = 1",
            "f(q'[a']'), is_admin = 1",
            "coalesce((SELECT secret FROM users LIMIT 1), 'x')",
            "coalesce((TABLE secret_table LIMIT 1), 'x')",
            "coalesce((table users), 'x')",
            "coalesce((WITH x AS (VALUES (1)) TABLE x), 'x')",
            "f((WITH cte AS (VALUES (1)) VALUES (2)))",
            "f('a\0b')",
        ] {
            assert!(!is_sql_expression(t), "expected rejection for: {t}");
        }
    }

    #[test]
    fn cmp_for_sort_is_a_strict_total_preorder() {
        use chrono::TimeZone;
        let d1 = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
        let dt_mid = d1.and_hms_opt(0, 0, 0).unwrap();
        let dt_3 = d1.and_hms_opt(3, 0, 0).unwrap();
        let tz_east = FixedOffset::east_opt(8 * 3600).unwrap().from_local_datetime(&d1.and_hms_opt(5, 0, 0).unwrap()).unwrap();
        let tz_utc = FixedOffset::east_opt(0).unwrap().from_local_datetime(&d1.and_hms_opt(2, 0, 0).unwrap()).unwrap();
        let big = 1i64 << 53;
        let values = vec![
            Value::Null,
            Value::Bool(false),
            Value::Bool(true),
            Value::Float(f64::NEG_INFINITY),
            Value::decimal("-Infinity"),
            Value::decimal("-1e400"),
            Value::Int(i64::MIN),
            Value::Float(-9223372036854775808.0),
            Value::Int(-2),
            Value::Float(-1.5),
            Value::decimal("-1.5"),
            Value::Int(-1),
            Value::Float(-0.5),
            Value::Float(-0.0),
            Value::Int(0),
            Value::UInt(0),
            Value::Float(0.0),
            Value::decimal("-0.00"),
            Value::decimal("0"),
            Value::Float(f64::from_bits(1)),
            Value::decimal("0.1"),
            Value::decimal("0.100000000000000003"),
            Value::Float(0.1),
            Value::Float(0.5),
            Value::Int(1),
            Value::Float(1.5),
            Value::decimal("2"),
            Value::Int(big),
            Value::UInt(big as u64),
            Value::Float(big as f64),
            Value::decimal("9007199254740992"),
            Value::Int(big + 1),
            Value::UInt((big + 1) as u64),
            Value::decimal("9007199254740993"),
            Value::Float((big + 2) as f64),
            Value::Int(i64::MAX),
            Value::UInt(i64::MAX as u64),
            Value::Float(9223372036854775808.0),
            Value::UInt(u64::MAX),
            Value::decimal("18446744073709551615"),
            Value::Float(18446744073709551616.0),
            Value::Float(f64::MAX),
            Value::decimal("1e400"),
            Value::Float(f64::INFINITY),
            Value::decimal("Infinity"),
            Value::Float(f64::NAN),
            Value::decimal("NaN"),
            Value::decimal("not-a-number"),
            Value::Date(d1),
            Value::DateTime(dt_mid),
            Value::DateTime(dt_3),
            Value::Date(d2),
            Value::DateTimeTz(tz_east),
            Value::DateTimeTz(tz_utc),
            Value::Time(dt_3.time()),
            Value::text("a"),
            Value::json("{\"a\":1}"),
            Value::bytes(vec![1, 2]),
        ];

        // Exact numeric precision checks.
        assert_eq!(Value::Int(big).cmp_for_sort(&Value::Float(big as f64)), Ordering::Equal);
        assert_eq!(Value::Int(big + 1).cmp_for_sort(&Value::Float(big as f64)), Ordering::Greater);
        assert_eq!(Value::Float(big as f64).cmp_for_sort(&Value::Int(big + 1)), Ordering::Less);
        assert_eq!(Value::Int(big + 1).cmp_for_sort(&Value::decimal("9007199254740993")), Ordering::Equal);
        assert_eq!(Value::decimal("9007199254740993").cmp_for_sort(&Value::Float(big as f64)), Ordering::Greater);
        assert_eq!(Value::Int(i64::MAX).cmp_for_sort(&Value::Float(9223372036854775808.0)), Ordering::Less);
        assert_eq!(Value::Date(d1).cmp_for_sort(&Value::DateTime(dt_mid)), Ordering::Equal);

        // Exhaustive reflexivity, antisymmetry, and transitivity across every triple.
        for (i, a) in values.iter().enumerate() {
            assert_eq!(a.cmp_for_sort(a), Ordering::Equal, "reflexivity failed for {a:?}");
            for (j, b) in values.iter().enumerate() {
                let ab = a.cmp_for_sort(b);
                let ba = b.cmp_for_sort(a);
                assert_eq!(ab, ba.reverse(), "antisymmetry failed for ({i}:{a:?}, {j}:{b:?})");
                for (k, c) in values.iter().enumerate() {
                    let bc = b.cmp_for_sort(c);
                    let ac = a.cmp_for_sort(c);
                    match (ab, bc) {
                        (Ordering::Less, Ordering::Less | Ordering::Equal) | (Ordering::Equal, Ordering::Less) => {
                            assert_eq!(ac, Ordering::Less, "transitivity failed: ({i}:{a:?}) <= ({j}:{b:?}) <= ({k}:{c:?})");
                        }
                        (Ordering::Greater, Ordering::Greater | Ordering::Equal) | (Ordering::Equal, Ordering::Greater) => {
                            assert_eq!(ac, Ordering::Greater, "transitivity failed: ({i}:{a:?}) >= ({j}:{b:?}) >= ({k}:{c:?})");
                        }
                        (Ordering::Equal, Ordering::Equal) => {
                            assert_eq!(ac, Ordering::Equal, "equivalence failed: ({i}:{a:?}) == ({j}:{b:?}) == ({k}:{c:?})");
                        }
                        _ => {}
                    }
                }
            }
        }

        // Sorting reversed and shuffled copies never panics and produces a non-decreasing order.
        let mut rev: Vec<Value> = values.iter().rev().cloned().collect();
        rev.sort_by(|a, b| a.cmp_for_sort(b));
        for w in rev.windows(2) {
            assert_ne!(w[0].cmp_for_sort(&w[1]), Ordering::Greater, "out of order: {:?} > {:?}", w[0], w[1]);
        }

        // Verify fast cmp_i128_f64 agrees with ExactDec conversion on boundary pairs.
        let ints: &[i128] = &[i128::from(i64::MIN), -100, -2, -1, 0, 1, 2, i128::from(big), i128::from(big + 1), i128::from(i64::MAX), i128::from(u64::MAX)];
        let floats: &[f64] = &[
            -9223372036854775808.0,
            -2.5,
            -1.5,
            -1.0,
            -0.5,
            -0.0,
            0.0,
            f64::from_bits(1),
            0.5,
            1.0,
            1.5,
            big as f64,
            (big + 2) as f64,
            9223372036854775808.0,
            18446744073709551616.0,
            f64::MAX,
        ];
        for &i in ints {
            let di = ExactDec::from_i128(i);
            for &f in floats {
                let df = ExactDec::from_finite_f64(f);
                assert_eq!(cmp_i128_f64(i, f), di.cmp(&df), "mismatch for i={i}, f={f}");
            }
        }
    }

    #[test]
    fn display_options() {
        let o = FormatOptions { thousands_separator: true, ..Default::default() };
        assert_eq!(Value::Int(1234567).display(&o), "1,234,567");
        assert_eq!(Value::Int(-1234).display(&o), "-1,234");
        assert_eq!(Value::decimal("1234.50").display(&o), "1,234.50");
        assert_eq!(Value::Null.display(&o), "NULL");
        assert_eq!(Value::text("a\nb").display(&o), "a¶b");
    }

    #[test]
    fn sort_order() {
        let mut v = vec![Value::Int(3), Value::Null, Value::Float(1.5), Value::decimal("2")];
        v.sort_by(|a, b| a.cmp_for_sort(b));
        assert_eq!(v, vec![Value::Null, Value::Float(1.5), Value::decimal("2"), Value::Int(3)]);
    }

    #[test]
    fn guess_kinds() {
        assert_eq!(DataKind::guess_from_type_name("varchar(20)"), DataKind::Text);
        assert_eq!(DataKind::guess_from_type_name("timestamp with time zone"), DataKind::DateTimeTz);
        assert_eq!(DataKind::guess_from_type_name("int4[]"), DataKind::Array);
        assert_eq!(DataKind::guess_from_type_name("numeric(10,2)"), DataKind::Decimal);
        assert_eq!(DataKind::guess_from_type_name("boolean"), DataKind::Bool);
        assert_eq!(DataKind::guess_from_type_name("bit(1)"), DataKind::Bool);
        assert_eq!(DataKind::guess_from_type_name("bit(8)"), DataKind::Bit);
    }
}
