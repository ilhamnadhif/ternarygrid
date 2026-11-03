//! [`SearchSession`]: Elasticsearch and OpenSearch over HTTP(S). SQL goes to
//! the SQL endpoint and pages with its cursor; REST requests are passed
//! through and their hits shown as rows. The blocking HTTP client runs on
//! blocking threads.

use crate::dialect::{SearchDialect, is_rest};
use async_trait::async_trait;
use base64::Engine;
use serde_json::{Value as J, json};
use std::collections::VecDeque;
use std::io::Read;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;
use ternarygrid_core::admin::{KillMode, Metric, SessionList, TabularData};
use ternarygrid_core::config::SslMode;
use ternarygrid_core::ddl::{ColumnDef, SchemaSnapshot, SnapshotOptions, TableDef};
use ternarygrid_core::dialect::Dialect;
use ternarygrid_core::driver::{ServerInfo, parse_version_number};
use ternarygrid_core::meta::*;
use ternarygrid_core::plan::{PlanNode, QueryPlan};
use ternarygrid_core::value::{ColumnMeta, DataKind};
use ternarygrid_core::{
    CancelHandle, ConnectParams, DbError, DbKind, DbResult, ErrorKind, ExecOptions, QueryCursor,
    ResultHeader, Row, ServerMessage, Session, TransactionMode, Value, tr, trf,
};
use ureq::Agent;
use ureq::config::Config;
use ureq::tls::{Certificate, ClientCert, PemItem, PrivateKey, RootCerts, TlsConfig, TlsProvider};
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// Rows per SQL page.
const FETCH: u64 = 1000;
/// How long Elasticsearch keeps an SQL cursor between pages: longer than the
/// application leaves an unread cursor open (5 minutes) before closing it.
const PAGE_TIMEOUT: &str = "10m";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Product {
    Elasticsearch,
    OpenSearch,
}

/// Resolver sending every lookup to a fixed address: through an SSH tunnel or
/// proxy the URL carries the real server name (TLS SNI and certificate
/// verification) while the socket goes to the local forwarder.
#[derive(Debug)]
struct FixedResolver {
    addrs: Vec<SocketAddr>,
}

impl Resolver for FixedResolver {
    fn resolve(
        &self,
        _uri: &ureq::http::Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let mut out = self.empty();
        for a in self.addrs.iter().take(16) {
            out.push(*a);
        }
        if out.is_empty() {
            Err(ureq::Error::HostNotFound)
        } else {
            Ok(out)
        }
    }
}

/// TLS settings of a connection: the trusted roots (the system's and/or the
/// connection's CA file) when it verifies, and the client certificate.
fn tls_config(params: &ConnectParams) -> DbResult<TlsConfig> {
    let mut b = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .unversioned_rustls_crypto_provider(
            Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        );
    let ca = params.ssl.ca_cert.as_deref().filter(|c| !c.is_empty());
    if params.ssl.mode.verifies_chain() {
        let mut roots: Vec<Certificate<'static>> = Vec::new();
        if !params.ssl.only_custom_ca || ca.is_none() {
            roots.extend(
                rustls_native_certs::load_native_certs()
                    .certs
                    .into_iter()
                    .map(|c| Certificate::from_der(c.as_ref()).to_owned()),
            );
        }
        if let Some(ca) = ca {
            roots.extend(load_pem_certs(ca)?);
        }
        b = if roots.is_empty() {
            b.root_certs(RootCerts::WebPki)
        } else {
            b.root_certs(RootCerts::new_with_certs(&roots))
        };
    } else {
        b = b.disable_verification(true);
    }
    let cert = params.ssl.client_cert.as_deref().filter(|c| !c.is_empty());
    let key = params.ssl.client_key.as_deref().filter(|c| !c.is_empty());
    match (cert, key) {
        (Some(cert), Some(key)) => {
            let certs = load_pem_certs(cert)?;
            let pem = read_file(key, "client key")?;
            let key = PrivateKey::from_pem(&pem)
                .map_err(|e| DbError::config(format!("Invalid client key '{key}': {e}")))?;
            b = b.client_cert(Some(ClientCert::new_with_certs(&certs, key)));
        }
        (None, None) => {}
        _ => {
            return Err(DbError::config(
                "A client certificate needs both the certificate and the key file",
            ));
        }
    }
    Ok(b.build())
}

fn read_file(path: &str, what: &str) -> DbResult<Vec<u8>> {
    std::fs::read(path)
        .map_err(|e| DbError::config(format!("Cannot read {what} file '{path}': {e}")))
}

/// Certificates of a PEM file.
fn load_pem_certs(path: &str) -> DbResult<Vec<Certificate<'static>>> {
    let pem = read_file(path, "certificate")?;
    let mut out = Vec::new();
    for item in ureq::tls::parse_pem(&pem) {
        match item {
            Ok(PemItem::Certificate(c)) => out.push(c),
            Ok(_) => {}
            Err(e) => return Err(DbError::config(format!("Invalid PEM in '{path}': {e}"))),
        }
    }
    if out.is_empty() {
        return Err(DbError::config(format!("No certificate found in '{path}'")));
    }
    Ok(out)
}

/// The HTTP client of a session (cheap to clone, shared by its cursors).
#[derive(Clone)]
struct Http {
    agent: Agent,
    base: String,
    auth: Option<String>,
}

impl Http {
    fn new(params: &ConnectParams) -> DbResult<Self> {
        let https = matches!(
            params.ssl.mode,
            SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull
        ) || params.property("https") == Some("true");
        let mut cfg = Agent::config_builder()
            .http_status_as_error(false)
            // Tunnels and proxies are local forwarders at `host:port`, not
            // proxies from the environment.
            .proxy(None)
            .timeout_connect(Some(params.connect_timeout))
            .timeout_resolve(Some(params.connect_timeout))
            .timeout_global(Some(Duration::from_secs(600)))
            .user_agent(format!(
                "{} (Elasticsearch driver)",
                params.application_name
            ));
        if https {
            cfg = cfg.tls_config(tls_config(params)?);
        }
        // `host:port` may be a local forwarder (SSH tunnel, proxy): the socket
        // goes there, TLS checks the real server name (`tls_server_name`).
        let tunnelled = https
            && params
                .tls_server_name
                .as_deref()
                .is_some_and(|n| n != params.host);
        let agent = if tunnelled {
            let addrs: Vec<SocketAddr> = (params.host.as_str(), params.port)
                .to_socket_addrs()
                .map_err(|e| DbError::connection(format!("{}: {e}", params.host)))?
                .collect();
            Agent::with_parts(
                cfg.build(),
                DefaultConnector::default(),
                FixedResolver { addrs },
            )
        } else {
            Agent::new_with_config(cfg.build())
        };
        let host = if https {
            params.tls_host()
        } else {
            params.host.as_str()
        };
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        let path = params
            .property("path")
            .unwrap_or("")
            .trim()
            .trim_end_matches('/')
            .to_string();
        let base = format!(
            "{}://{host}:{}{}",
            if https { "https" } else { "http" },
            params.port,
            path
        );
        let auth = if let Some(key) = params.property("api_key").filter(|k| !k.is_empty()) {
            Some(format!("ApiKey {key}"))
        } else {
            params.user.as_deref().filter(|u| !u.is_empty()).map(|u| {
                let pw = params.password.as_ref().map(|p| p.expose()).unwrap_or("");
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(format!("{u}:{pw}"))
                )
            })
        };
        Ok(Http { agent, base, auth })
    }

    /// One request; the JSON reply (or `{"text": …}`), errors as DbError.
    fn call(&self, method: &str, path: &str, body: Option<&str>) -> DbResult<J> {
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        let url = format!("{}{path}", self.base);
        let ndjson = path
            .split('?')
            .next()
            .is_some_and(|p| p.ends_with("_bulk") || p.ends_with("_msearch"));
        let ctype = if ndjson {
            "application/x-ndjson"
        } else {
            "application/json"
        };
        let body_text = body.map(|b| {
            if ndjson && !b.ends_with('\n') {
                format!("{b}\n")
            } else {
                b.to_string()
            }
        });
        macro_rules! go {
            ($rb:expr) => {{
                let mut rb = $rb.header("Accept", "application/json");
                if let Some(a) = &self.auth {
                    rb = rb.header("Authorization", a);
                }
                rb
            }};
        }
        let result = match (method, body_text) {
            ("GET", None) => go!(self.agent.get(&url)).call(),
            ("HEAD", _) => go!(self.agent.head(&url)).call(),
            ("DELETE", None) => go!(self.agent.delete(&url)).call(),
            ("GET", Some(b)) => go!(self.agent.get(&url))
                .header("Content-Type", ctype)
                .force_send_body()
                .send(b.as_bytes()),
            ("DELETE", Some(b)) => go!(self.agent.delete(&url))
                .header("Content-Type", ctype)
                .force_send_body()
                .send(b.as_bytes()),
            ("PUT", b) => go!(self.agent.put(&url))
                .header("Content-Type", ctype)
                .send(b.unwrap_or_default().as_bytes()),
            (_, b) => go!(self.agent.post(&url))
                .header("Content-Type", ctype)
                .send(b.unwrap_or_default().as_bytes()),
        };
        let response = result.map_err(|e| match e {
            ureq::Error::Timeout(_) => DbError::timeout(e.to_string()),
            ureq::Error::Tls(_) | ureq::Error::Rustls(_) => DbError::tls(e.to_string()),
            _ => DbError::connection(format!("{}: {e}", self.base)),
        })?;
        let status = response.status().as_u16();
        let mut text = String::new();
        response
            .into_body()
            .into_reader()
            .read_to_string(&mut text)
            .map_err(|e| DbError::connection(e.to_string()))?;
        let json: J = if text.trim().is_empty() {
            json!({"status": status})
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| json!({"text": text}))
        };
        if status >= 400 {
            let reason = json
                .pointer("/error/root_cause/0/reason")
                .or_else(|| json.pointer("/error/reason"))
                .or_else(|| json.pointer("/error/details"))
                .or_else(|| json.get("error"))
                .map(|r| {
                    r.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| r.to_string())
                })
                .unwrap_or_else(|| text.chars().take(500).collect());
            let kind = match status {
                401 | 403 => ErrorKind::Auth,
                400 if reason.contains("parsing_exception")
                    || reason.to_ascii_lowercase().contains("syntax")
                    || reason.contains("line ") =>
                {
                    ErrorKind::Syntax
                }
                _ => ErrorKind::Query,
            };
            let etype = json
                .pointer("/error/type")
                .and_then(J::as_str)
                .map(str::to_string);
            let mut e = DbError::new(kind, reason).with_code(status.to_string());
            if let Some(t) = etype {
                e = e.with_detail(t);
            }
            return Err(e);
        }
        Ok(json)
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> DbResult<T> + Send + 'static,
) -> DbResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| DbError::internal(e.to_string()))?
}

// ------------------------------------------------------------- conversion

fn kind_of(es_type: &str) -> DataKind {
    match es_type {
        "long" | "integer" | "short" | "byte" | "token_count" => DataKind::Int,
        "double" | "float" | "half_float" | "scaled_float" => DataKind::Float,
        "unsigned_long" => DataKind::Decimal,
        "boolean" => DataKind::Bool,
        "date" | "datetime" | "date_nanos" | "timestamp" => DataKind::DateTimeTz,
        "ip" => DataKind::Network,
        "object" | "nested" | "geo_point" | "geo_shape" | "flattened" | "struct" | "array" => {
            DataKind::Json
        }
        "binary" => DataKind::Binary,
        _ => DataKind::Text,
    }
}

fn value(v: &J, kind: DataKind) -> Value {
    match v {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => n
            .as_i64()
            .map(Value::Int)
            .or_else(|| n.as_u64().map(|u| Value::Decimal(u.to_string().into())))
            .unwrap_or_else(|| Value::Float(n.as_f64().unwrap_or(0.0))),
        J::String(s) if kind == DataKind::DateTimeTz => chrono::DateTime::parse_from_rfc3339(s)
            .map(Value::DateTimeTz)
            .unwrap_or_else(|_| Value::Text(s.as_str().into())),
        J::String(s) => Value::Text(s.as_str().into()),
        other => Value::Json(other.to_string().into()),
    }
}

/// Objects as rows: their keys in order of appearance are the columns.
fn object_rows(objs: &[&J]) -> (Vec<ColumnMeta>, Vec<Row>) {
    let mut cols: Vec<(String, DataKind)> = Vec::new();
    for o in objs {
        if let J::Object(m) = o {
            for (k, v) in m {
                if !cols.iter().any(|(c, _)| c == k) {
                    let kind = match v {
                        J::Number(n) if n.is_i64() => DataKind::Int,
                        J::Number(_) => DataKind::Float,
                        J::Bool(_) => DataKind::Bool,
                        J::Object(_) | J::Array(_) => DataKind::Json,
                        _ => DataKind::Text,
                    };
                    cols.push((k.clone(), kind));
                }
            }
        }
    }
    let rows = objs
        .iter()
        .map(|o| {
            Row::from(
                cols.iter()
                    .map(|(c, k)| o.get(c).map_or(Value::Null, |v| value(v, *k)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    (
        cols.into_iter()
            .map(|(n, k)| ColumnMeta::new(n, "", k))
            .collect(),
        rows,
    )
}

/// A REST reply as rows: its hits (`_index`, `_id`, `_score` and the source
/// fields), the objects of an array, or (key, value) pairs.
fn rest_rows(reply: &J) -> (Vec<ColumnMeta>, Vec<Row>) {
    if let Some(hits) = reply.pointer("/hits/hits").and_then(J::as_array)
        && (!hits.is_empty() || reply.get("aggregations").is_none())
    {
        let flat: Vec<J> = hits
            .iter()
            .map(|h| {
                let mut m = serde_json::Map::new();
                for k in ["_index", "_id", "_score"] {
                    m.insert(k.into(), h.get(k).cloned().unwrap_or(J::Null));
                }
                if let Some(J::Object(src)) = h.get("_source") {
                    for (k, v) in src {
                        m.insert(k.clone(), v.clone());
                    }
                }
                J::Object(m)
            })
            .collect();
        return object_rows(&flat.iter().collect::<Vec<_>>());
    }
    if let J::Array(items) = reply
        && items.iter().all(J::is_object)
    {
        return object_rows(&items.iter().collect::<Vec<_>>());
    }
    if let Some(text) = reply.get("text").and_then(J::as_str) {
        let rows = text
            .lines()
            .map(|l| Row::from(vec![Value::Text(l.into())]))
            .collect();
        return (vec![ColumnMeta::new("response", "", DataKind::Text)], rows);
    }
    let rows = match reply {
        J::Object(m) => m
            .iter()
            .map(|(k, v)| {
                Row::from(vec![
                    Value::Text(k.as_str().into()),
                    value(v, DataKind::Other),
                ])
            })
            .collect(),
        other => vec![Row::from(vec![
            Value::Text("response".into()),
            value(other, DataKind::Other),
        ])],
    };
    (
        vec![
            ColumnMeta::new("key", "", DataKind::Text),
            ColumnMeta::new("value", "", DataKind::Other),
        ],
        rows,
    )
}

/// Mapping properties flattened to `a.b.c` paths with their types.
fn mapping_fields(props: &J, prefix: &str, out: &mut Vec<(String, String)>) {
    let Some(m) = props.as_object() else { return };
    for (name, def) in m {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        let ty =
            def.get("type")
                .and_then(J::as_str)
                .unwrap_or(if def.get("properties").is_some() {
                    "object"
                } else {
                    "unknown"
                });
        out.push((path.clone(), ty.to_string()));
        if let Some(p) = def.get("properties") {
            mapping_fields(p, &path, out);
        }
    }
}

struct SqlPager {
    http: Http,
    product: Product,
    cursor: Option<String>,
    kinds: Vec<DataKind>,
}

impl SqlPager {
    fn rows_of(&self, reply: &J) -> Vec<Row> {
        let rows = reply
            .get("rows")
            .or_else(|| reply.get("datarows"))
            .and_then(J::as_array)
            .cloned()
            .unwrap_or_default();
        rows.iter()
            .map(|r| {
                Row::from(
                    r.as_array()
                        .map(|a| {
                            a.iter()
                                .enumerate()
                                .map(|(i, v)| {
                                    value(v, self.kinds.get(i).copied().unwrap_or(DataKind::Other))
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                )
            })
            .collect()
    }
    fn endpoint(&self) -> &'static str {
        match self.product {
            Product::Elasticsearch => "/_sql?format=json",
            Product::OpenSearch => "/_plugins/_sql?format=jdbc",
        }
    }
    fn close_endpoint(&self) -> &'static str {
        match self.product {
            Product::Elasticsearch => "/_sql/close",
            Product::OpenSearch => "/_plugins/_sql/close",
        }
    }
    /// The request for the page after `cursor`.
    fn page_body(&self, cursor: &str) -> String {
        let mut body = json!({"cursor": cursor});
        // Each page extends the cursor's life by its `page_timeout` (default 45 s).
        // OpenSearch keeps cursors for its `plugins.sql.cursor.keep_alive`.
        if self.product == Product::Elasticsearch {
            body["page_timeout"] = json!(PAGE_TIMEOUT);
        }
        body.to_string()
    }
}

/// The SQL request for `sql` with its parameters.
fn sql_body(product: Product, sql: &str, params: &[Value]) -> DbResult<J> {
    let mut body = json!({"query": sql, "fetch_size": FETCH});
    if product == Product::Elasticsearch {
        body["page_timeout"] = json!(PAGE_TIMEOUT);
    }
    if params.is_empty() {
        return Ok(body);
    }
    // OpenSearch would run the statement without its values.
    if product == Product::OpenSearch {
        return Err(DbError::unsupported(tr(
            "OpenSearch SQL takes no query parameters; write the values into the statement",
        )));
    }
    body["params"] = J::Array(
        params
            .iter()
            .map(|v| match v {
                Value::Null => J::Null,
                Value::Bool(b) => J::Bool(*b),
                Value::Int(i) => json!(i),
                Value::Float(f) => json!(f),
                other => J::String(other.to_text().into_owned()),
            })
            .collect(),
    );
    Ok(body)
}

/// The first word of an SQL statement (after comments and parentheses), upper case.
fn sql_keyword(sql: &str) -> String {
    let mut rest = sql.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("--") {
            rest = r.split_once('\n').map_or("", |(_, r)| r).trim_start();
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, r)| r).trim_start();
        } else if let Some(r) = rest.strip_prefix('(') {
            rest = r.trim_start();
        } else {
            break;
        }
    }
    rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// SQL that only reads (OpenSearch's SQL can also DELETE).
fn sql_reads(sql: &str) -> bool {
    matches!(
        sql_keyword(sql).as_str(),
        "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "WITH" | ""
    )
}

/// `%XX` escapes of a URL path decoded (as the server reads them).
fn percent_decode(path: &str) -> String {
    let b = path.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = path
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// POST endpoints that only read: searches, counts, SQL and the like.
const READ_ENDPOINTS: &[&str] = &[
    "_search",
    "_msearch",
    "_count",
    "_explain",
    "_field_caps",
    "_validate",
    "_async_search",
    "_pit",
    "_mget",
    "_termvectors",
    "_mtermvectors",
    "_terms_enum",
    "_eql",
    "_ppl",
];

/// A REST request that only reads: GET and HEAD, and POST to a read endpoint
/// (the path's first `_` segment, so `/i/_doc/_search` indexes a document).
fn rest_reads(method: &str, path: &str, body: Option<&str>) -> bool {
    match method {
        "GET" | "HEAD" => true,
        "POST" => {
            let path = percent_decode(path.split(['?', '#']).next().unwrap_or(""));
            let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if segments.iter().any(|s| *s == "." || *s == "..") {
                return false;
            }
            let mut api = segments.into_iter().skip_while(|s| !s.starts_with('_'));
            let mut endpoint = api.next();
            if matches!(endpoint, Some("_plugins" | "_opendistro")) {
                endpoint = api.next();
            }
            match endpoint {
                // The SQL API (also `/translate`, `/close`, cursors).
                Some("_sql") => body
                    .and_then(|b| serde_json::from_str::<J>(b).ok())
                    .and_then(|b| b.get("query").and_then(J::as_str).map(sql_reads))
                    .unwrap_or(true),
                Some(e) => READ_ENDPOINTS.contains(&e),
                None => false,
            }
        }
        _ => false,
    }
}

struct SearchCursor {
    header: Option<ResultHeader>,
    rows: VecDeque<Row>,
    pager: Option<SqlPager>,
}

#[async_trait]
impl QueryCursor for SearchCursor {
    async fn next_result(&mut self) -> DbResult<Option<ResultHeader>> {
        Ok(self.header.take())
    }

    async fn fetch(&mut self, max_rows: usize) -> DbResult<Vec<Row>> {
        let want = max_rows.max(1);
        while self.rows.len() < want {
            let Some(pager) = self.pager.as_mut() else {
                break;
            };
            let Some(cursor) = pager.cursor.take() else {
                self.pager = None;
                break;
            };
            let http = pager.http.clone();
            let endpoint = pager.endpoint();
            let body = pager.page_body(&cursor);
            let reply = blocking(move || http.call("POST", endpoint, Some(&body))).await?;
            let rows = pager.rows_of(&reply);
            pager.cursor = reply
                .get("cursor")
                .and_then(J::as_str)
                .map(str::to_string)
                .filter(|c| !c.is_empty() && !rows.is_empty());
            self.rows.extend(rows);
        }
        let n = want.min(self.rows.len());
        Ok(self.rows.drain(..n).collect())
    }

    async fn close(&mut self) -> DbResult<()> {
        self.rows.clear();
        if let Some(p) = self.pager.take()
            && let Some(c) = p.cursor.clone()
        {
            let endpoint = p.close_endpoint();
            let http = p.http.clone();
            let _ = blocking(move || {
                http.call("POST", endpoint, Some(&json!({"cursor": c}).to_string()))
            })
            .await;
        }
        Ok(())
    }

    fn take_messages(&mut self) -> Vec<ServerMessage> {
        Vec::new()
    }
}

/// A cursor dropped without `close` still releases its server cursor (in the
/// background: the request blocks).
impl Drop for SearchCursor {
    fn drop(&mut self) {
        let Some(mut p) = self.pager.take() else {
            return;
        };
        let Some(cursor) = p.cursor.take() else {
            return;
        };
        let endpoint = p.close_endpoint();
        let body = json!({"cursor": cursor}).to_string();
        let close = move || {
            let _ = p.http.call("POST", endpoint, Some(&body));
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => drop(rt.spawn_blocking(close)),
            Err(_) => drop(std::thread::spawn(close)),
        }
    }
}

pub struct SearchSession {
    http: Http,
    product: Product,
    cluster: String,
    info: ServerInfo,
    dialect: Arc<SearchDialect>,
    mode: TransactionMode,
    closed: bool,
    /// Only requests that read are sent.
    read_only: bool,
}

/// `METHOD /path` and the JSON body after it.
fn split_request(text: &str) -> (String, String, Option<String>) {
    let text = text.trim();
    let (first, body) = match text.split_once('\n') {
        Some((f, b)) => (
            f.trim(),
            Some(b.trim().to_string()).filter(|b| !b.is_empty()),
        ),
        None => (text, None),
    };
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_ascii_uppercase();
    let path = parts.next().unwrap_or("/").to_string();
    // A body on the request line: `POST /i/_search {"query": …}`.
    let rest: String = parts.collect::<Vec<_>>().join(" ");
    let body = if rest.is_empty() {
        body
    } else {
        Some(match body {
            Some(b) => format!("{rest}\n{b}"),
            None => rest,
        })
    };
    (method, path, body)
}

impl SearchSession {
    pub async fn connect(params: &ConnectParams) -> DbResult<Self> {
        let http = Http::new(params)?;
        let h = http.clone();
        let root = blocking(move || h.call("GET", "/", None)).await?;
        let version = root
            .pointer("/version/number")
            .and_then(J::as_str)
            .unwrap_or_default()
            .to_string();
        let product =
            if root.pointer("/version/distribution").and_then(J::as_str) == Some("opensearch") {
                Product::OpenSearch
            } else {
                Product::Elasticsearch
            };
        let cluster = root
            .get("cluster_name")
            .and_then(J::as_str)
            .unwrap_or("cluster")
            .to_string();
        let name = if product == Product::OpenSearch {
            "OpenSearch"
        } else {
            "Elasticsearch"
        };
        let info = ServerInfo {
            product: name.into(),
            version_number: parse_version_number(&version),
            version,
            session_id: None,
        };
        Ok(SearchSession {
            http,
            product,
            cluster,
            info,
            dialect: Arc::new(SearchDialect::new()),
            mode: TransactionMode::Auto,
            closed: false,
            read_only: params.read_only,
        })
    }

    async fn call(&self, method: &str, path: &str, body: Option<String>) -> DbResult<J> {
        let http = self.http.clone();
        let (method, path) = (method.to_string(), path.to_string());
        blocking(move || http.call(&method, &path, body.as_deref())).await
    }

    async fn sql(&self, sql: &str, params: &[Value]) -> DbResult<Box<dyn QueryCursor>> {
        let body = sql_body(self.product, sql, params)?;
        let pager = SqlPager {
            http: self.http.clone(),
            product: self.product,
            cursor: None,
            kinds: Vec::new(),
        };
        let reply = self
            .call("POST", pager.endpoint(), Some(body.to_string()))
            .await?;
        let cols = reply
            .get("columns")
            .or_else(|| reply.get("schema"))
            .and_then(J::as_array)
            .cloned()
            .unwrap_or_default();
        let columns: Vec<ColumnMeta> = cols
            .iter()
            .map(|c| {
                let name = c
                    .get("alias")
                    .or_else(|| c.get("name"))
                    .and_then(J::as_str)
                    .unwrap_or("?");
                let ty = c.get("type").and_then(J::as_str).unwrap_or("");
                ColumnMeta::new(name, ty, kind_of(ty))
            })
            .collect();
        let mut pager = SqlPager {
            kinds: columns.iter().map(|c| c.kind).collect(),
            ..pager
        };
        let rows = pager.rows_of(&reply);
        pager.cursor = reply
            .get("cursor")
            .and_then(J::as_str)
            .map(str::to_string)
            .filter(|c| !c.is_empty());
        Ok(Box::new(SearchCursor {
            header: Some(ResultHeader::Rows(columns)),
            rows: rows.into(),
            pager: Some(pager),
        }))
    }

    async fn indices(&self) -> DbResult<Vec<J>> {
        let r = self.call("GET", "/_cat/indices?format=json&bytes=b&s=index&h=index,health,status,docs.count,store.size,pri,rep", None).await?;
        Ok(r.as_array().cloned().unwrap_or_default())
    }

    async fn mapping(&self, index: &str) -> DbResult<Vec<(String, String)>> {
        let r = self
            .call("GET", &format!("/{index}/_mapping"), None)
            .await?;
        let mut out = Vec::new();
        if let Some(m) = r.as_object() {
            for (_, def) in m.iter().take(1) {
                if let Some(p) = def.pointer("/mappings/properties") {
                    mapping_fields(p, "", &mut out);
                }
            }
        }
        Ok(out)
    }
}

fn num(v: Option<&J>) -> Option<i64> {
    v.and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

#[async_trait]
impl Session for SearchSession {
    fn kind(&self) -> DbKind {
        DbKind::Elasticsearch
    }

    fn dialect(&self) -> Arc<dyn Dialect> {
        self.dialect.clone()
    }

    fn server_info(&self) -> &ServerInfo {
        &self.info
    }

    fn cancel_handle(&self) -> Option<Arc<dyn CancelHandle>> {
        None
    }

    async fn ping(&mut self) -> DbResult<()> {
        self.call("GET", "/", None)
            .await
            .map(|_| ())
            .map_err(|e| DbError::connection(e.message))
    }

    async fn close(&mut self) -> DbResult<()> {
        self.closed = true;
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed
    }

    async fn execute(
        &mut self,
        sql: &str,
        params: &[Value],
        _opts: &ExecOptions,
    ) -> DbResult<Box<dyn QueryCursor>> {
        let text = sql.trim().trim_end_matches(';').trim();
        if !is_rest(text) {
            if self.read_only && !sql_reads(text) {
                return Err(DbError::read_only(trf(
                    "The connection is read-only: {} changes data",
                    &[&sql_keyword(text)],
                )));
            }
            return self.sql(text, params).await;
        }
        let (method, path, body) = split_request(text);
        let write = !rest_reads(&method, &path, body.as_deref());
        if write && self.read_only {
            return Err(DbError::read_only(trf(
                "The connection is read-only: {} changes data",
                &[&format!("{method} {path}")],
            )));
        }
        let reply = self.call(&method, &path, body).await?;
        if write && reply.pointer("/hits/hits").is_none() {
            let n = num(reply.get("deleted"))
                .or_else(|| num(reply.get("updated")))
                .or_else(|| {
                    num(reply.pointer("/items")).or_else(|| {
                        reply
                            .get("items")
                            .and_then(J::as_array)
                            .map(|a| a.len() as i64)
                    })
                });
            let result = reply.get("result").and_then(J::as_str).map(str::to_string);
            let tag = match result {
                Some(r) => format!("{method} {path} ({r})"),
                None => format!("{method} {path}"),
            };
            return Ok(Box::new(SearchCursor {
                header: Some(ResultHeader::Update {
                    rows_affected: n.map(|n| n.max(0) as u64),
                    last_insert_id: None,
                    tag: Some(tag),
                }),
                rows: VecDeque::new(),
                pager: None,
            }));
        }
        let (cols, rows) = rest_rows(&reply);
        Ok(Box::new(SearchCursor {
            header: Some(ResultHeader::Rows(cols)),
            rows: rows.into(),
            pager: None,
        }))
    }

    fn transaction_mode(&self) -> TransactionMode {
        self.mode
    }

    async fn set_transaction_mode(&mut self, mode: TransactionMode) -> DbResult<()> {
        self.mode = mode;
        Ok(())
    }

    fn in_transaction(&self) -> bool {
        false
    }

    async fn commit(&mut self) -> DbResult<()> {
        Ok(())
    }

    async fn rollback(&mut self) -> DbResult<()> {
        Err(DbError::unsupported(
            "Rolling back (every request is applied at once)",
        ))
    }

    async fn current_database(&mut self) -> DbResult<Option<String>> {
        Ok(None)
    }

    async fn current_schema(&mut self) -> DbResult<Option<String>> {
        Ok(Some(self.cluster.clone()))
    }

    async fn set_current_schema(&mut self, _schema: &str) -> DbResult<()> {
        Ok(())
    }

    async fn list_databases(&mut self) -> DbResult<Vec<DatabaseInfo>> {
        Ok(Vec::new())
    }

    /// The cluster (the navigator shows its indices directly).
    async fn list_schemas(&mut self) -> DbResult<Vec<SchemaInfo>> {
        Ok(vec![SchemaInfo {
            name: self.cluster.clone(),
            is_current: true,
            is_system: false,
            owner: None,
            comment: None,
        }])
    }

    async fn list_objects(
        &mut self,
        schema: &SchemaRef,
        kind: ObjectKind,
    ) -> DbResult<Vec<ObjectInfo>> {
        let s = schema.schema.clone().or(Some(self.cluster.clone()));
        match kind {
            ObjectKind::Table => Ok(self
                .indices()
                .await?
                .iter()
                .filter_map(|i| {
                    let name = i.get("index")?.as_str()?.to_string();
                    let mut o =
                        ObjectInfo::new(ObjectRef::new(ObjectKind::Table, s.clone(), name.clone()));
                    o.is_system = name.starts_with('.');
                    o.row_estimate = num(i.get("docs.count"));
                    o.size_bytes = num(i.get("store.size"));
                    o.detail = i.get("health").and_then(J::as_str).map(str::to_string);
                    Some(o)
                })
                .collect()),
            ObjectKind::View => {
                let r = self
                    .call(
                        "GET",
                        "/_cat/aliases?format=json&h=alias,index&s=alias",
                        None,
                    )
                    .await?;
                let mut out: Vec<ObjectInfo> = Vec::new();
                for a in r.as_array().cloned().unwrap_or_default() {
                    let (Some(alias), Some(index)) = (
                        a.get("alias").and_then(J::as_str),
                        a.get("index").and_then(J::as_str),
                    ) else {
                        continue;
                    };
                    match out.iter_mut().find(|o| o.obj.name == alias) {
                        Some(o) => {
                            o.detail =
                                Some(format!("{}, {index}", o.detail.clone().unwrap_or_default()))
                        }
                        None => {
                            let mut o =
                                ObjectInfo::new(ObjectRef::new(ObjectKind::View, s.clone(), alias));
                            o.is_system = alias.starts_with('.');
                            o.detail = Some(index.to_string());
                            out.push(o);
                        }
                    }
                }
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
    }

    async fn list_children(
        &mut self,
        parent: &ObjectRef,
        kind: ObjectKind,
    ) -> DbResult<Vec<ObjectInfo>> {
        if kind != ObjectKind::Column {
            return Ok(Vec::new());
        }
        Ok(self
            .mapping(&parent.name)
            .await?
            .into_iter()
            .map(|(name, ty)| {
                ObjectInfo::new(ObjectRef::child(ObjectKind::Column, parent, name)).with_detail(ty)
            })
            .collect())
    }

    async fn table_definition(&mut self, table: &ObjectRef) -> DbResult<TableDef> {
        let mut t = TableDef::new(table.schema.clone(), table.name.clone());
        t.columns = self
            .mapping(&table.name)
            .await?
            .into_iter()
            .filter(|(_, ty)| ty != "object")
            .enumerate()
            .map(|(i, (name, ty))| ColumnDef {
                ordinal: i as u32 + 1,
                kind: kind_of(&ty),
                ..ColumnDef::new(name, ty)
            })
            .collect();
        Ok(t)
    }

    /// `PUT /index` with its settings and mappings.
    async fn object_ddl(&mut self, obj: &ObjectRef) -> DbResult<String> {
        let r = self.call("GET", &format!("/{}", obj.name), None).await?;
        let Some((name, def)) = r.as_object().and_then(|m| m.iter().next()) else {
            return Err(DbError::query(format!("Index {} not found", obj.name)));
        };
        let mut settings = serde_json::Map::new();
        if let Some(idx) = def.pointer("/settings/index") {
            for k in [
                "number_of_shards",
                "number_of_replicas",
                "refresh_interval",
                "analysis",
            ] {
                if let Some(v) = idx.get(k) {
                    settings.insert(k.into(), v.clone());
                }
            }
        }
        let mut body = json!({"settings": {"index": settings}, "mappings": def.get("mappings").cloned().unwrap_or(J::Null)});
        if let Some(a) = def
            .get("aliases")
            .filter(|a| a.as_object().is_some_and(|m| !m.is_empty()))
        {
            body["aliases"] = a.clone();
        }
        Ok(format!(
            "PUT /{name}\n{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        ))
    }

    async fn object_properties(&mut self, obj: &ObjectRef) -> DbResult<Vec<Property>> {
        let r = self
            .call(
                "GET",
                &format!("/_cat/indices/{}?format=json&bytes=b", obj.name),
                None,
            )
            .await?;
        let Some(i) = r.as_array().and_then(|a| a.first()).cloned() else {
            return Ok(Vec::new());
        };
        let get = |k: &str| {
            i.get(k)
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_default()
        };
        Ok(vec![
            Property::new("Health", get("health")),
            Property::new("Status", get("status")),
            Property::new("Documents", get("docs.count")),
            Property::new("Deleted documents", get("docs.deleted")),
            Property::new("Size (bytes)", get("store.size")),
            Property::new("Primary shards", get("pri")),
            Property::new("Replicas", get("rep")),
        ])
    }

    async fn search_metadata(&mut self, search: &MetadataSearch) -> DbResult<Vec<ObjectInfo>> {
        let needle = search.pattern.trim().trim_matches('*').to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = self
            .list_objects(&SchemaRef::schema(self.cluster.clone()), ObjectKind::Table)
            .await?;
        out.retain(|o| !o.is_system && o.obj.name.to_lowercase().contains(&needle));
        out.truncate(search.limit);
        Ok(out)
    }

    /// Indices with their fields, from one `GET /_mapping`.
    async fn schema_snapshot(
        &mut self,
        schema: &SchemaRef,
        opts: &SnapshotOptions,
    ) -> DbResult<SchemaSnapshot> {
        let mut tables = Vec::new();
        if opts.columns {
            let r = self.call("GET", "/_mapping", None).await?;
            for (index, def) in r.as_object().cloned().unwrap_or_default() {
                if index.starts_with('.') {
                    continue;
                }
                let mut fields = Vec::new();
                if let Some(p) = def.pointer("/mappings/properties") {
                    mapping_fields(p, "", &mut fields);
                }
                let mut t = TableDef::new(schema.schema.clone(), index);
                t.columns = fields
                    .into_iter()
                    .filter(|(_, ty)| ty != "object")
                    .enumerate()
                    .map(|(i, (n, ty))| ColumnDef {
                        ordinal: i as u32 + 1,
                        kind: kind_of(&ty),
                        ..ColumnDef::new(n, ty)
                    })
                    .collect();
                tables.push(t);
            }
        }
        Ok(SchemaSnapshot {
            kind: Some(DbKind::Elasticsearch),
            schema: schema.schema.clone(),
            tables,
            ..Default::default()
        })
    }

    /// The query DSL the SQL becomes.
    async fn explain(&mut self, sql: &str, _analyze: bool) -> DbResult<QueryPlan> {
        let text = sql.trim().trim_end_matches(';');
        if is_rest(text) {
            return Err(DbError::unsupported(
                "Explain works on SQL; add \"profile\": true to a search request",
            ));
        }
        let path = match self.product {
            Product::Elasticsearch => "/_sql/translate",
            Product::OpenSearch => "/_plugins/_sql/_explain",
        };
        let r = self
            .call("POST", path, Some(json!({"query": text}).to_string()))
            .await?;
        let mut root = PlanNode::new("Query DSL");
        root.condition = r.get("query").map(|q| q.to_string());
        Ok(QueryPlan {
            roots: vec![root],
            text: serde_json::to_string_pretty(&r).unwrap_or_default(),
            raw: Some(r.to_string()),
            planning_ms: None,
            execution_ms: None,
            analyzed: false,
        })
    }

    /// Running search and write tasks.
    async fn list_sessions(&mut self) -> DbResult<SessionList> {
        let r = self
            .call("GET", "/_tasks?detailed=true&group_by=none", None)
            .await?;
        let mut data = TabularData::new(
            [
                "id",
                "node",
                "action",
                "running (ms)",
                "cancellable",
                "description",
            ]
            .map(String::from)
            .to_vec(),
        );
        for t in r
            .get("tasks")
            .and_then(J::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let id = format!(
                "{}:{}",
                t.get("node").and_then(J::as_str).unwrap_or(""),
                num(t.get("id")).unwrap_or(0)
            );
            data.rows.push(vec![
                Value::Text(id.into()),
                Value::Text(t.get("node").and_then(J::as_str).unwrap_or("").into()),
                Value::Text(t.get("action").and_then(J::as_str).unwrap_or("").into()),
                num(t.get("running_time_in_nanos"))
                    .map_or(Value::Null, |n| Value::Int(n / 1_000_000)),
                t.get("cancellable")
                    .and_then(J::as_bool)
                    .map_or(Value::Null, Value::Bool),
                Value::Text(
                    t.get("description")
                        .and_then(J::as_str)
                        .unwrap_or("")
                        .into(),
                ),
            ]);
        }
        Ok(SessionList {
            data,
            id_column: 0,
            query_column: Some(5),
            state_column: None,
            blocked_by_column: None,
        })
    }

    async fn kill_session(&mut self, id: &str, _mode: KillMode) -> DbResult<()> {
        self.call("POST", &format!("/_tasks/{id}/_cancel"), None)
            .await
            .map(|_| ())
    }

    async fn server_metrics(&mut self) -> DbResult<Vec<Metric>> {
        let health = self.call("GET", "/_cluster/health", None).await?;
        let stats = self.call("GET", "/_nodes/stats/jvm,indices", None).await?;
        let mut out = Vec::new();
        let status = match health.get("status").and_then(J::as_str) {
            Some("green") => 2.0,
            Some("yellow") => 1.0,
            _ => 0.0,
        };
        out.push(Metric::gauge(
            "Cluster",
            "status",
            "Status (2 green, 1 yellow, 0 red)",
            status,
            "",
        ));
        for (k, label) in [
            ("number_of_nodes", "Nodes"),
            ("active_shards", "Active shards"),
            ("unassigned_shards", "Unassigned shards"),
        ] {
            if let Some(v) = num(health.get(k)) {
                out.push(Metric::gauge("Cluster", k, label, v as f64, ""));
            }
        }
        let nodes = stats
            .get("nodes")
            .and_then(J::as_object)
            .cloned()
            .unwrap_or_default();
        let sum = |p: &str| -> f64 {
            nodes
                .values()
                .filter_map(|n| n.pointer(p).and_then(J::as_f64))
                .sum()
        };
        out.push(Metric::gauge(
            "Data",
            "docs",
            "Documents",
            sum("/indices/docs/count"),
            "",
        ));
        out.push(Metric::gauge(
            "Data",
            "store",
            "Store size",
            sum("/indices/store/size_in_bytes"),
            "bytes",
        ));
        out.push(Metric::gauge(
            "JVM",
            "heap",
            "Heap used",
            sum("/jvm/mem/heap_used_in_bytes"),
            "bytes",
        ));
        out.push(Metric::counter(
            "Operations",
            "search",
            "Searches",
            sum("/indices/search/query_total"),
            "",
        ));
        out.push(Metric::counter(
            "Operations",
            "index",
            "Indexed documents",
            sum("/indices/indexing/index_total"),
            "",
        ));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::mpsc::Receiver;
    use ternarygrid_core::config::ConnectionConfig;

    /// The connection's CA file and client certificate are used (they were ignored).
    #[test]
    fn tls_files_of_the_connection_are_used() {
        const CA: &str = "-----BEGIN CERTIFICATE-----\nMIIBpDCCAUmgAwIBAgIUL2XsEFf1kDk7vY67A16GzGsrhuowCgYIKoZIzj0EAwIw\nHjEcMBoGA1UEAwwTVGVybmFyeUdyaWQgVGVzdCBDQTAgFw0yMDAxMDEwMDAwMDBa\nGA8yMTIwMDEwMTAwMDAwMFowHjEcMBoGA1UEAwwTVGVybmFyeUdyaWQgVGVzdCBD\nQTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABLU6noWJYkgaGqqgE5iWtmEW1w2M\n6ZTOqqfnt/xCrUcLhVazZCwfOzyvc/uhxYp+CfW0Ah9/Lj5JE5TJYjt6W3KjYzBh\nMB0GA1UdDgQWBBQfodv+SRCKWh6Gln6RIZw7gngPSjAfBgNVHSMEGDAWgBQfodv+\nSRCKWh6Gln6RIZw7gngPSjAPBgNVHRMBAf8EBTADAQH/MA4GA1UdDwEB/wQEAwIB\nBjAKBggqhkjOPQQDAgNJADBGAiEAkyy4iZASrY8b4eAYKphrjfBXb6YpCzh0SP3J\nUaeQ7uwCIQC37CXScPmDgqF4DB901w/aH1kQaoHAVWZNnwHj2SwZrA==\n-----END CERTIFICATE-----\n";
        let dir = std::env::temp_dir().join(format!("tg-es-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ca = dir.join("ca.pem");
        std::fs::write(&ca, CA).unwrap();
        let empty = dir.join("empty.pem");
        std::fs::write(&empty, "not a certificate").unwrap();

        let mut p = params("db.example.com", 9200, SslMode::VerifyFull);
        p.ssl.ca_cert = Some(ca.to_string_lossy().into_owned());
        p.ssl.only_custom_ca = true;
        assert!(tls_config(&p).is_ok());

        p.ssl.ca_cert = Some(dir.join("missing.pem").to_string_lossy().into_owned());
        let e = tls_config(&p).unwrap_err();
        assert!(e.message.contains("missing.pem"), "{e}");
        p.ssl.ca_cert = Some(empty.to_string_lossy().into_owned());
        assert!(
            tls_config(&p).is_err(),
            "a CA file without a certificate is an error"
        );

        // A certificate without its key (or the other way round) is a configuration error.
        let mut p = params("db.example.com", 9200, SslMode::Require);
        p.ssl.client_cert = Some(ca.to_string_lossy().into_owned());
        assert!(tls_config(&p).unwrap_err().message.contains("both"));
        p.ssl.client_cert = None;
        p.ssl.client_key = Some(ca.to_string_lossy().into_owned());
        assert!(tls_config(&p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn params(host: &str, port: u16, mode: SslMode) -> ConnectParams {
        let mut cfg = ConnectionConfig::new(DbKind::Elasticsearch, "t");
        cfg.host = host.into();
        cfg.port = Some(port);
        cfg.ssl.mode = mode;
        ConnectParams::from_config(&cfg, None)
    }

    /// A server answering one request with `reply` (HTTP) or closing after
    /// the first bytes (`None`: TLS); the receiver gets what the client sent.
    fn one_request_server(reply: Option<&'static str>) -> (u16, Receiver<Vec<u8>>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut c, _)) = l.accept() else { return };
            c.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut got = Vec::new();
            let mut buf = [0u8; 8192];
            while let Ok(n) = c.read(&mut buf) {
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).into_owned();
                let complete = text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    body.len() >= len
                });
                if n == 0 || reply.is_none() || complete {
                    break;
                }
            }
            let _ = tx.send(got);
            if let Some(r) = reply {
                let _ = c.write_all(r.as_bytes());
            }
        });
        (port, rx)
    }

    #[test]
    fn tunnels_connect_to_the_forwarder_and_verify_the_real_name() {
        // Plain HTTP: the URL is the forwarder.
        let mut p = params("127.0.0.1", 41234, SslMode::Disable);
        p.tls_server_name = Some("es.example.invalid".into());
        assert_eq!(Http::new(&p).unwrap().base, "http://127.0.0.1:41234");
        // HTTPS: the URL carries the real name (SNI, certificate), the socket
        // goes to the forwarder.
        let (port, rx) = one_request_server(None);
        let mut p = params("127.0.0.1", port, SslMode::Require);
        p.tls_server_name = Some("es.example.invalid".into());
        let http = Http::new(&p).unwrap();
        assert_eq!(http.base, format!("https://es.example.invalid:{port}"));
        assert!(http.call("GET", "/", None).is_err());
        let hello = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("no connection");
        assert_eq!(hello.first(), Some(&0x16), "not a TLS handshake");
        assert!(
            hello.windows(18).any(|w| w == b"es.example.invalid"),
            "SNI is not the real server name"
        );
        // Without a tunnel the host is the server.
        let p = params("es.local", 9200, SslMode::Require);
        assert_eq!(Http::new(&p).unwrap().base, "https://es.local:9200");
    }

    #[test]
    fn sql_requests_keep_cursors_alive() {
        let b = sql_body(Product::Elasticsearch, "SELECT 1", &[Value::Int(3)]).unwrap();
        assert_eq!(b["page_timeout"], json!(PAGE_TIMEOUT));
        assert_eq!(b["params"], json!([3]));
        let b = sql_body(Product::OpenSearch, "SELECT 1", &[]).unwrap();
        assert!(b.get("page_timeout").is_none());
        // OpenSearch would drop the values.
        let e = sql_body(Product::OpenSearch, "SELECT ?", &[Value::Int(1)]).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported);
        let pager = |product| SqlPager {
            http: Http::new(&params("h", 1, SslMode::Disable)).unwrap(),
            product,
            cursor: None,
            kinds: Vec::new(),
        };
        let page: J = serde_json::from_str(&pager(Product::Elasticsearch).page_body("c1")).unwrap();
        assert_eq!(page, json!({"cursor": "c1", "page_timeout": PAGE_TIMEOUT}));
        let page: J = serde_json::from_str(&pager(Product::OpenSearch).page_body("c1")).unwrap();
        assert_eq!(page, json!({"cursor": "c1"}));
    }

    #[tokio::test]
    async fn a_dropped_cursor_closes_its_server_cursor() {
        let (port, rx) = one_request_server(Some(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 18\r\n\r\n{\"succeeded\":true}",
        ));
        let cursor = SearchCursor {
            header: None,
            rows: VecDeque::new(),
            pager: Some(SqlPager {
                http: Http::new(&params("127.0.0.1", port, SslMode::Disable)).unwrap(),
                product: Product::Elasticsearch,
                cursor: Some("sql-cursor-42".into()),
                kinds: Vec::new(),
            }),
        };
        drop(cursor);
        let req = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(10)))
            .await
            .unwrap()
            .expect("the cursor was not closed");
        let req = String::from_utf8_lossy(&req);
        assert!(req.starts_with("POST /_sql/close "), "{req}");
        assert!(req.contains("sql-cursor-42"), "{req}");
    }

    #[test]
    fn read_only_requests() {
        for (m, p, b) in [
            ("GET", "/logs/_search", None),
            ("HEAD", "/logs", None),
            ("GET", "/_cat/indices", None),
            ("POST", "/logs/_search", Some("{}")),
            ("POST", "/logs,metrics/_search?size=5", None),
            ("POST", "/_search/scroll", Some("{}")),
            ("POST", "/logs/_count", None),
            ("POST", "/_msearch", Some("{}\n{}")),
            (
                "POST",
                "/_sql?format=json",
                Some("{\"query\": \"SELECT * FROM logs\"}"),
            ),
            ("POST", "/_sql/translate", Some("{\"query\": \"select 1\"}")),
            ("POST", "/_sql/close", Some("{\"cursor\": \"c\"}")),
            (
                "POST",
                "/_plugins/_sql",
                Some("{\"query\": \"SHOW TABLES LIKE %\"}"),
            ),
            ("POST", "/logs/_explain/1", Some("{}")),
            ("POST", "/logs/_field_caps?fields=*", None),
            ("POST", "/logs/_validate/query", Some("{}")),
            ("POST", "/logs/_async_search", Some("{}")),
            ("POST", "/logs/_pit?keep_alive=1m", None),
            ("POST", "/%5Fsearch", None),
        ] {
            assert!(rest_reads(m, p, b), "{m} {p}");
        }
        for (m, p, b) in [
            ("PUT", "/logs", None),
            ("DELETE", "/logs", None),
            ("DELETE", "/_search/scroll", None),
            ("POST", "/logs/_doc", Some("{}")),
            ("POST", "/logs/_doc/_search", Some("{}")),
            ("POST", "/my_search_logs/_doc", Some("{}")),
            ("POST", "/logs/_delete_by_query", Some("{}")),
            ("POST", "/logs/_update_by_query", Some("{}")),
            ("POST", "/_bulk", Some("{}")),
            ("POST", "/logs/_refresh", None),
            ("POST", "/_aliases", Some("{}")),
            ("POST", "/logs", Some("{}")),
            ("POST", "/", None),
            ("POST", "/_search/../_delete_by_query", None),
            ("POST", "/logs/%5Fdelete_by_query", None),
            (
                "POST",
                "/_plugins/_sql",
                Some("{\"query\": \"DELETE FROM logs\"}"),
            ),
            ("POST", "/_tasks/n:1/_cancel", None),
            ("PATCH", "/logs", None),
        ] {
            assert!(!rest_reads(m, p, b), "{m} {p}");
        }
        assert!(sql_reads("  -- c\n/* d */ (select 1)"));
        assert!(sql_reads("DESCRIBE logs"));
        assert!(!sql_reads("DELETE FROM logs WHERE a = 1"));
        assert_eq!(sql_keyword("/* x */ delete from t"), "DELETE");
    }

    #[test]
    fn rest_requests_and_replies() {
        assert_eq!(
            split_request("GET /logs/_search\n{\"size\": 1}"),
            (
                "GET".into(),
                "/logs/_search".into(),
                Some("{\"size\": 1}".into())
            )
        );
        assert_eq!(
            split_request("delete /old"),
            ("DELETE".into(), "/old".into(), None)
        );
        let reply = json!({"hits": {"hits": [{"_index": "i", "_id": "1", "_score": 1.0, "_source": {"msg": "hi", "n": 3, "tags": ["a"]}}]}});
        let (cols, rows) = rest_rows(&reply);
        assert_eq!(
            cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["_index", "_id", "_score", "msg", "n", "tags"]
        );
        assert_eq!(rows[0].to_vec()[4], Value::Int(3));
        let (cols, _) = rest_rows(&json!([{"index": "a", "health": "green"}]));
        assert_eq!(cols.len(), 2);
        let mut f = Vec::new();
        mapping_fields(
            &json!({"user": {"properties": {"name": {"type": "keyword"}}}, "at": {"type": "date"}}),
            "",
            &mut f,
        );
        assert_eq!(
            f,
            vec![
                ("user".to_string(), "object".to_string()),
                ("user.name".into(), "keyword".into()),
                ("at".into(), "date".into())
            ]
        );
    }
}
