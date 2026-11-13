//! Connection management: configured connections, live sessions, tunnels,
//! credentials and per-connection metadata caches.

pub mod meta;
pub mod session;

use crate::backend::{Backend, Job, Loadable};
use meta::MetadataCache;
use session::SessionHandle;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{Duration, Instant, SystemTime};
use ternarygrid_config::ConnectionStore;
use ternarygrid_config::settings::TimeZoneChoice;
use ternarygrid_core::config::{AuthMethod, ConnectionConfig, ConnectionType, SecretSlot};
use ternarygrid_core::driver::{ConnectParams, Driver, ServerInfo, TransactionMode};
use ternarygrid_core::registry::DriverRegistry;
use ternarygrid_core::{DbError, DbKind, DbResult, Dialect, Secret};

/// Where secrets come from (Keychain or encrypted vault).
pub trait SecretSource: Send + Sync {
    fn get(&self, key: &str) -> DbResult<Option<Secret>>;
    fn set(&self, key: &str, value: &Secret) -> DbResult<()>;
    fn delete(&self, key: &str) -> DbResult<()>;
    /// True while the store must be unlocked (encrypted vault).
    fn is_locked(&self) -> bool {
        false
    }
}

/// Opens SSH tunnels / proxy forwarders for a connection.
#[async_trait::async_trait]
pub trait NetConnector: Send + Sync {
    /// Returns `None` when the connection needs no tunnel.
    async fn open(&self, cfg: &ConnectionConfig, creds: &Credentials, host: &str, port: u16) -> DbResult<Option<TunnelGuard>>;
}

/// A running tunnel; dropping it stops forwarding.
pub struct TunnelGuard {
    pub local_host: String,
    pub local_port: u16,
    pub alive: Box<dyn Fn() -> bool + Send + Sync>,
    pub guard: Box<dyn std::any::Any + Send + Sync>,
    /// Makes sure the tunnel still carries traffic (after the computer slept): SSH
    /// sessions that no longer answer are replaced, and what went through them ends.
    pub check: Option<TunnelCheck>,
}

/// See [`TunnelGuard::check`].
pub type TunnelCheck = Box<dyn Fn() -> futures::future::BoxFuture<'static, DbResult<()>> + Send + Sync>;

/// Credentials collected before connecting (from the secret store or prompts).
#[derive(Clone, Default)]
pub struct Credentials {
    pub password: Option<Secret>,
    pub ssh_password: Option<Secret>,
    pub ssh_passphrase: Option<Secret>,
    pub proxy_password: Option<Secret>,
}

/// Something the user must enter before a connection can be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptField {
    Password,
    SshPassword,
    SshPassphrase,
    ProxyPassword,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnStatus {
    Disconnected,
    Connecting,
    Connected,
    /// Connected, and being checked (the computer slept: its connections may be dead
    /// without knowing it yet).
    Checking,
    Failed(String),
    /// The connection broke after being established (it connects again by itself).
    Lost,
}

/// Every connection made and every revive gets a new number: a tab whose session
/// failed tries again by itself once its connection's number changed.
static EPOCHS: AtomicU64 = AtomicU64::new(0);

fn next_epoch() -> u64 {
    EPOCHS.fetch_add(1, AtomicOrdering::Relaxed) + 1
}

/// How long a connection being checked may take to answer before it counts as lost.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// The computer slept (or TernaryGrid did not run) between two polls at `prev` and
/// `now`: the wall clock moved on more than the monotonic one (which stops during
/// sleep on macOS and Linux), or the polls, due every few seconds while connections
/// are open, were more than a minute apart (Windows' monotonic clock counts sleep).
/// A false alarm only costs a check of the connections.
pub fn slept(prev: (Instant, SystemTime), now: (Instant, SystemTime)) -> bool {
    let mono = now.0.saturating_duration_since(prev.0);
    // A clock set back is no sleep.
    let Ok(wall) = now.1.duration_since(prev.1) else { return false };
    wall > mono + Duration::from_secs(10) || wall > Duration::from_secs(60)
}

/// A connected connection.
pub struct LiveConnection {
    pub config: ConnectionConfig,
    pub conn_type: ConnectionType,
    pub driver: Arc<dyn Driver>,
    pub dialect: Arc<dyn Dialect>,
    /// Effective parameters (local tunnel endpoint, password) used to open more sessions.
    params: ConnectParams,
    tunnel: Option<TunnelGuard>,
    /// Session used by the navigator and metadata loading.
    pub main: SessionHandle,
    /// Sessions for other databases of the server (PostgreSQL / SQL Server).
    pub db_sessions: HashMap<String, Loadable<SessionHandle>>,
    /// Sessions opened for tabs (editors, data tabs): closed on disconnect too,
    /// so that none keeps running with the old settings, transactions or locks.
    tab_sessions: Arc<parking_lot::Mutex<Vec<crate::conn::session::WeakSession>>>,
    /// Opens `main` again after it was lost.
    revive: Revive,
    /// The connection is being checked ([`ConnectionManager::check_all`]).
    verify: Option<Job<()>>,
    /// Changes when the connection is opened again (see [`EPOCHS`]).
    epoch: u64,
    pub server: ServerInfo,
    pub meta: MetadataCache,
    pub connected_at: Instant,
    pub current_database: Option<String>,
    pub current_schema: Option<String>,
    /// Server-level object folders of the dialect (roles, settings …).
    pub catalog_folders: Arc<Vec<ternarygrid_core::catalog::CatalogFolder>>,
    /// Stops the keep-alive task when the connection is dropped.
    _keepalive: KeepAlive,
}

/// The main session was lost (the computer slept, the network dropped, the
/// server restarted): it is opened again in the background, at once and then
/// less and less often while the server does not answer, so the connection
/// turns green again by itself. A session a tab opens meanwhile (its Retry)
/// shows that the server answers again: the next attempt is made at once.
/// When the SSH tunnel or proxy ended too, the whole connection is opened
/// again (by [`ConnectionManager::poll`], which has the credentials).
#[derive(Default)]
struct Revive {
    job: Option<ReviveJob>,
    /// No attempt before this.
    next: Option<Instant>,
    failures: u32,
    /// Set when a session of the connection was opened.
    answered: Arc<std::sync::atomic::AtomicBool>,
}

enum ReviveJob {
    /// The main session, through the tunnel that still runs (or none).
    Session(Job<DbResult<SessionHandle>>),
    /// A new tunnel and main session.
    Connection(Job<DbResult<Connected>>),
}

/// Background keep-alive: pings the main session every `keep_alive_secs`
/// on the tokio runtime, independent of UI frames (the UI does not repaint
/// while idle). Cancelled when dropped.
struct KeepAlive(tokio_util::sync::CancellationToken);

impl Drop for KeepAlive {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl KeepAlive {
    fn start(backend: &Backend, session: &SessionHandle, secs: u32) -> KeepAlive {
        let token = tokio_util::sync::CancellationToken::new();
        if secs > 0 {
            backend.spawn_detached(keep_alive_loop(session.downgrade(), secs, token.clone()));
        }
        KeepAlive(token)
    }
}

/// Ping `session` every `secs` seconds until it is closed or dropped, or `stop`
/// is cancelled.
async fn keep_alive_loop(session: session::WeakSession, secs: u32, stop: tokio_util::sync::CancellationToken) {
    let period = Duration::from_secs(secs.max(5) as u64);
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(period) => {}
        }
        let Some(session) = session.upgrade() else { break };
        if session.is_closed() {
            break;
        }
        if let Err(e) = session.ping().await {
            tracing::warn!("keep-alive failed: {e}");
        }
    }
}

impl LiveConnection {
    /// Every open session of the connection: main, other databases', tabs'.
    fn sessions(&self) -> Vec<SessionHandle> {
        let mut out = vec![self.main.clone()];
        out.extend(self.db_sessions.values().filter_map(|l| l.get().cloned()));
        out.extend(self.tab_sessions.lock().iter().filter_map(|w| w.upgrade()));
        out
    }

    /// A catalog folder of this connection's dialect.
    pub fn catalog_folder(&self, id: &str) -> Option<&ternarygrid_core::catalog::CatalogFolder> {
        self.catalog_folders.iter().find(|f| f.id == id)
    }

    pub fn kind(&self) -> DbKind {
        self.config.kind
    }

    pub fn tunnel_alive(&self) -> bool {
        self.tunnel.as_ref().is_none_or(|t| (t.alive)())
    }

    pub fn params(&self) -> &ConnectParams {
        &self.params
    }

    /// The server has several databases, each needing its own session (PostgreSQL).
    pub fn multiple_databases(&self) -> bool {
        self.driver.info().capabilities.multiple_databases
    }

    /// Session for `database` (`None` or the current database → main session).
    pub fn session_for(&self, database: Option<&str>) -> Option<SessionHandle> {
        match database {
            None => Some(self.main.clone()),
            Some(db) if Some(db) == self.current_database.as_deref() => Some(self.main.clone()),
            Some(db) => {
                if !self.driver.info().capabilities.multiple_databases {
                    return Some(self.main.clone());
                }
                self.db_sessions.get(db).and_then(|l| l.get()).cloned()
            }
        }
    }

    /// Make sure a session for `database` is being opened; returns it once ready.
    pub fn ensure_session(&mut self, backend: &Backend, database: Option<&str>) -> Option<SessionHandle> {
        if let Some(s) = self.session_for(database) {
            return Some(s);
        }
        let db = database?.to_string();
        let entry = self.db_sessions.entry(db.clone()).or_default();
        entry.poll();
        if entry.is_not_loaded() {
            let driver = self.driver.clone();
            let mut params = self.params.clone();
            params.database = Some(db.clone());
            let cfg = self.config.clone();
            let label = format!("{} / {db}", cfg.name);
            *entry = Loadable::Loading(backend.spawn(async move {
                let mut s = driver.connect(&params).await?;
                init_session(s.as_mut(), &cfg, &params).await?;
                let handle = SessionHandle::new(s, label, Some(db));
                // Kept alive like the main session; it ends with the session.
                if cfg.init.keep_alive_secs > 0 {
                    tokio::spawn(keep_alive_loop(handle.downgrade(), cfg.init.keep_alive_secs, tokio_util::sync::CancellationToken::new()));
                }
                Ok(handle)
            }));
        }
        None
    }

    /// When a session of a tab or of another database lost its connection (the data
    /// tab's insert failed with "connection closed"), the connection is checked: if the
    /// server is gone, it shows as lost (red) instead of connected.
    fn check_health(&mut self, backend: &Backend, timeout: Duration) {
        // Each closed session is looked at once: tab sessions are forgotten, a closed
        // session of another database is opened again on its next use.
        let mut lost = {
            let mut list = self.tab_sessions.lock();
            let n = list.iter().filter_map(|w| w.upgrade()).filter(|s| s.is_lost()).count();
            list.retain(|w| w.upgrade().is_some_and(|s| !s.is_closed()));
            n
        };
        self.db_sessions.retain(|_, l| match l.get() {
            Some(s) if s.is_closed() => {
                lost += usize::from(s.is_lost());
                false
            }
            _ => true,
        });
        if lost > 0 && !self.main.is_closed() {
            self.start_verify(backend, timeout);
        }
        self.revive_main(backend);
    }

    /// See [`Revive`]: takes a finished attempt, and starts the next one when the
    /// tunnel still runs ([`Self::needs_new_tunnel`] otherwise).
    fn revive_main(&mut self, backend: &Backend) {
        use std::sync::atomic::Ordering;
        let done = match self.revive.job.as_mut() {
            Some(ReviveJob::Session(job)) => job.poll().map(|r| r.map(|s| (s, None))),
            Some(ReviveJob::Connection(job)) => job.poll().map(|r| {
                r.map(|c| {
                    let main = SessionHandle::new(c.session, format!("{} (main)", self.config.name), self.current_database.clone());
                    (main, Some((c.tunnel, c.params, c.server)))
                })
            }),
            None => None,
        };
        if let Some(r) = done {
            self.revive.job = None;
            match r {
                Ok((s, connection)) => {
                    tracing::info!("{}: connected again", self.config.name);
                    if let Some((tunnel, params, server)) = connection {
                        // The other databases' sessions went through the old tunnel.
                        self.db_sessions.clear();
                        self.tunnel = tunnel;
                        self.params = params;
                        self.server = server;
                    }
                    self._keepalive = KeepAlive::start(backend, &s, self.config.init.keep_alive_secs);
                    self.main = s;
                    self.epoch = next_epoch();
                    self.revive.next = None;
                    self.revive.failures = 0;
                    // What failed to load meanwhile loads again.
                    self.meta.forget_failed();
                    return;
                }
                Err(e) => {
                    tracing::info!("{}: not connected again yet: {e}", self.config.name);
                    self.revive.failures += 1;
                    let wait = Duration::from_secs((5u64 << self.revive.failures.min(4)).min(60));
                    self.revive.next = Some(Instant::now() + wait);
                    if let Some(ctx) = backend.context() {
                        ctx.request_repaint_after(wait);
                    }
                }
            }
        }
        if self.revive.job.is_some() || !self.main.is_lost() || !self.tunnel_alive() {
            return;
        }
        let answered = self.revive.answered.swap(false, Ordering::Relaxed);
        if !answered && !self.revive_due() {
            return;
        }
        let driver = self.driver.clone();
        let params = self.params.clone();
        let cfg = self.config.clone();
        let database = self.current_database.clone();
        self.revive.job = Some(ReviveJob::Session(backend.spawn(async move {
            let mut s = driver.connect(&params).await?;
            init_session(s.as_mut(), &cfg, &params).await?;
            Ok(SessionHandle::new(s, format!("{} (main)", cfg.name), database))
        })));
    }

    /// Check the connection's sessions now (see [`verify_sessions`]); a check already
    /// running is not repeated.
    fn start_verify(&mut self, backend: &Backend, timeout: Duration) {
        if self.verify.is_some() || self.main.is_closed() {
            return;
        }
        let mut others: Vec<SessionHandle> = self.db_sessions.values().filter_map(|l| l.get().cloned()).collect();
        others.extend(self.tab_sessions.lock().iter().filter_map(|w| w.upgrade()));
        others.retain(|s| !s.is_closed());
        let tunnel = self.tunnel.as_ref().and_then(|t| t.check.as_ref()).map(|check| check());
        let main = self.main.clone();
        self.verify = Some(backend.spawn(verify_sessions(main, others, tunnel, timeout)));
    }

    fn revive_due(&self) -> bool {
        self.revive.next.is_none_or(|t| Instant::now() >= t)
    }

    /// The SSH tunnel or proxy ended (the computer slept): time to open the
    /// connection again as a whole.
    fn needs_new_tunnel(&self) -> bool {
        self.revive.job.is_none() && !self.tunnel_alive() && self.revive_due()
    }

    /// Why the session for `database` could not be opened (shown instead of "Loading…").
    pub fn session_error(&self, database: Option<&str>) -> Option<String> {
        self.db_sessions.get(database?).and_then(|l| l.error()).map(|e| e.full_text())
    }

    /// Forget sessions that failed to open, so that the next use tries again.
    pub fn retry_failed_sessions(&mut self) {
        self.db_sessions.retain(|_, l| l.error().is_none());
    }

    /// Open an additional session (separate connection per editor, FR-C7).
    pub fn open_session_job(&self, backend: &Backend, label: String, database: Option<String>) -> Job<DbResult<SessionHandle>> {
        backend.spawn(self.open_session_future(label, database))
    }

    /// The opening of an additional session, to await in a background job
    /// (the AI assistant's queries).
    pub fn open_session_future(&self, label: String, database: Option<String>) -> impl std::future::Future<Output = DbResult<SessionHandle>> + Send + 'static {
        self.open_session_future_with(label, database, false)
    }

    /// [`open_session_future`](Self::open_session_future) for a session that only
    /// reads (the AI assistant's queries): the driver makes it read-only where the
    /// database has the mode, whatever the connection's own setting.
    pub fn open_read_only_session_future(&self, label: String, database: Option<String>) -> impl std::future::Future<Output = DbResult<SessionHandle>> + Send + 'static {
        self.open_session_future_with(label, database, true)
    }

    fn open_session_future_with(&self, label: String, database: Option<String>, read_only: bool) -> impl std::future::Future<Output = DbResult<SessionHandle>> + Send + 'static {
        let driver = self.driver.clone();
        let mut params = self.params.clone();
        params.read_only |= read_only;
        if database.is_some() {
            params.database = database.clone();
        }
        let cfg = self.config.clone();
        let tab_sessions = self.tab_sessions.clone();
        let answered = self.revive.answered.clone();
        async move {
            let mut s = driver.connect(&params).await?;
            init_session(s.as_mut(), &cfg, &params).await?;
            answered.store(true, std::sync::atomic::Ordering::Relaxed);
            let handle = SessionHandle::new(s, label, database);
            // Weak: a session its tab dropped (closed while opening) closes instead of staying open.
            let mut list = tab_sessions.lock();
            list.retain(|w| w.upgrade().is_some_and(|s| !s.is_closed()));
            list.push(handle.downgrade());
            Ok(handle)
        }
    }
}

/// Check a connection's sessions (the computer slept, or a session of it was lost):
/// each must answer within `timeout` or is given up as lost (a lost main session
/// connects again by itself, and tabs open new sessions). A session running a
/// statement is not interrupted, unless the connection turned out dead (main, or
/// another session asked in its place, gave no answer): then every session of it is
/// given up.
async fn verify_sessions(main: SessionHandle, others: Vec<SessionHandle>, tunnel: Option<futures::future::BoxFuture<'static, DbResult<()>>>, timeout: Duration) {
    use session::Health;
    // Through a tunnel, its SSH sessions first: one that no longer answers is replaced,
    // and the connections it carried end (they read as lost below).
    if let Some(check) = tunnel {
        match tokio::time::timeout(timeout, check).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::info!("tunnel check: {e}"),
            Err(_) => tracing::info!("tunnel check: no answer within {} s", timeout.as_secs()),
        }
    }
    let main_health = main.check(timeout).await;
    let silent = |h: &Health| matches!(h, Health::Silent);
    let mut dead = silent(&main_health);
    if !dead {
        // Each other session answers for itself. When main could not be asked (in use),
        // one with a result left open (a data tab between pages) is asked instead: its
        // result is closed first, as one left unread for long (it reads on by running its
        // query again). When main answered, such results are left alone: their sessions
        // went the same way as main.
        let probe_results = matches!(main_health, Health::Busy);
        let checks = others.iter().map(|s| async move {
            if probe_results && !s.is_busy() && s.has_open_cursor() && tokio::time::timeout(timeout, s.close_cursor()).await.is_err() {
                s.abandon(true);
                return Health::Silent;
            }
            s.check(timeout).await
        });
        let results = futures::future::join_all(checks).await;
        // An idle session that did not answer while main could not be asked: the
        // connection is dead.
        dead = matches!(main_health, Health::Busy) && results.iter().any(silent);
    }
    if dead {
        tracing::info!("{}: the server does not answer; connecting again", main.label());
        main.abandon(true);
        for s in &others {
            s.abandon(true);
        }
    }
}

/// Apply transaction mode, default schema and init SQL after connecting.
pub async fn init_session(s: &mut dyn ternarygrid_core::Session, cfg: &ConnectionConfig, params: &ConnectParams) -> DbResult<()> {
    // A database without transactions (ClickHouse) stays in auto-commit: its session
    // reports that mode, so the editors show what really happens.
    if !params.auto_commit
        && let Err(e) = s.set_transaction_mode(TransactionMode::Manual).await
    {
        if e.kind != ternarygrid_core::ErrorKind::Unsupported {
            return Err(e);
        }
        tracing::warn!("{}: manual commit is not available, auto-commit stays on: {e}", cfg.name);
    }
    if let Some(schema) = cfg.init.default_schema.as_deref().filter(|s| !s.is_empty())
        && let Err(e) = s.set_current_schema(schema).await
    {
        tracing::warn!("cannot set default schema: {e}");
    }
    for stmt in split_simple(&cfg.init.init_sql) {
        ternarygrid_core::driver::execute_update(s, &stmt, &[]).await.map_err(|e| e.with_context("Initialization SQL"))?;
    }
    Ok(())
}

/// Minimal `;` splitter for initialization scripts (quotes aware).
pub fn split_simple(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in sql.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' | '`' => {
                    quote = Some(c);
                    cur.push(c);
                }
                ';' => {
                    if !cur.trim().is_empty() {
                        out.push(cur.trim().to_string());
                    }
                    cur.clear();
                }
                _ => cur.push(c),
            },
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Time zone set on new sessions (see [`TimeZoneChoice`]): a zone name for
/// PostgreSQL, an offset for MySQL / MariaDB (zone names need the server's
/// time zone tables).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTimeZone {
    /// IANA name (`Asia/Jakarta`), `UTC`, or a POSIX zone for an offset.
    pub name: String,
    /// `+07:00`.
    pub offset: String,
    /// The computer's zone (else UTC): for drivers that convert values themselves.
    pub local: bool,
}

/// The session time zone of `choice`; `None` keeps the server's.
pub fn session_time_zone(choice: TimeZoneChoice) -> Option<SessionTimeZone> {
    match choice {
        TimeZoneChoice::Server => None,
        TimeZoneChoice::Utc => Some(SessionTimeZone { name: "UTC".into(), offset: "+00:00".into(), local: false }),
        TimeZoneChoice::System => {
            let secs = chrono::Local::now().offset().local_minus_utc();
            let name = iana_time_zone::get_timezone().ok().filter(|n| n.contains('/') || n == "UTC").unwrap_or_else(|| posix_zone(secs));
            Some(SessionTimeZone { name, offset: format_offset(secs), local: true })
        }
    }
}

/// A POSIX zone for an offset (`<+07>-07` for 7 hours east of UTC: POSIX
/// counts the other way round).
fn posix_zone(secs: i32) -> String {
    let m = secs.unsigned_abs() / 60;
    let (h, m) = (m / 60, m % 60);
    let (sign, posix) = if secs < 0 { ('-', '+') } else { ('+', '-') };
    if m == 0 { format!("<{sign}{h:02}>{posix}{h}") } else { format!("<{sign}{h:02}{m:02}>{posix}{h}:{m:02}") }
}

/// `+07:00` for 25200 seconds east of UTC.
fn format_offset(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let m = secs.unsigned_abs() / 60;
    format!("{sign}{:02}:{:02}", m / 60, m % 60)
}

/// The time zone choices with their labels ("Computer (Asia/Jakarta, +07:00)").
pub fn time_zone_choices() -> Vec<(TimeZoneChoice, String)> {
    let system = session_time_zone(TimeZoneChoice::System).map(|tz| format!("{}, {}", tz.name, tz.offset)).unwrap_or_default();
    vec![
        (TimeZoneChoice::System, ternarygrid_core::trf("Computer ({})", &[&system])),
        (TimeZoneChoice::Utc, "UTC".to_string()),
        (TimeZoneChoice::Server, ternarygrid_core::tr("Server's own").to_string()),
    ]
}

/// Short name of the time zone of `choice` for the status bar (`WIB`, `UTC`).
pub fn time_zone_label(choice: TimeZoneChoice) -> String {
    match choice {
        TimeZoneChoice::Server => ternarygrid_core::tr("Server").to_string(),
        TimeZoneChoice::Utc => "UTC".into(),
        TimeZoneChoice::System => zone_abbreviation().unwrap_or_else(|| format_offset(chrono::Local::now().offset().local_minus_utc())),
    }
}

/// The computer's time zone abbreviation (`WIB`, `CEST`).
#[cfg(unix)]
fn zone_abbreviation() -> Option<String> {
    // SAFETY: `localtime_r` fills the `tm` given; `tm_zone` points to a static
    // NUL-terminated string (or is null).
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() || tm.tm_zone.is_null() {
            return None;
        }
        let s = std::ffi::CStr::from_ptr(tm.tm_zone).to_string_lossy().into_owned();
        (!s.is_empty()).then_some(s)
    }
}

#[cfg(not(unix))]
fn zone_abbreviation() -> Option<String> {
    None
}

/// Set the session time zone on `params` unless the connection's driver
/// properties set one: a zone name for PostgreSQL, CockroachDB and ClickHouse
/// (23.6+) and Oracle, an offset for MySQL / MariaDB; SQL Server converts
/// its `datetimeoffset` values (`local` or `utc`). SQLite has no time zone.
pub fn apply_time_zone(params: &mut ConnectParams, tz: &SessionTimeZone) {
    let convert = if tz.local { "local" } else { "utc" };
    let (key, value, names): (&str, &str, &[&str]) = match params.kind {
        DbKind::Postgres | DbKind::CockroachDb => ("timezone", &tz.name, &["timezone"]),
        DbKind::MySql | DbKind::MariaDb => ("time_zone", &tz.offset, &["time_zone", "timezone", "servertimezone"]),
        DbKind::ClickHouse => ("time_zone", &tz.name, &["time_zone", "setting.session_timezone"]),
        DbKind::Oracle => ("time_zone", &tz.name, &["time_zone"]),
        DbKind::SqlServer => ("time_zone", convert, &["time_zone"]),
        _ => return,
    };
    if params.properties.keys().any(|k| names.contains(&k.to_ascii_lowercase().as_str())) {
        return;
    }
    params.properties.insert(key.into(), value.to_string());
}

struct Connected {
    session: Box<dyn ternarygrid_core::Session>,
    params: ConnectParams,
    tunnel: Option<TunnelGuard>,
    server: ServerInfo,
    database: Option<String>,
    schema: Option<String>,
}

/// Runs a shell command (pre-connect hook, password command).
pub async fn run_shell(command: &str, timeout: Duration) -> DbResult<String> {
    ternarygrid_net::run_shell_command(command, timeout).await
}

async fn connect_task(
    driver: Arc<dyn Driver>,
    cfg: ConnectionConfig,
    conn_type: ConnectionType,
    creds: Credentials,
    net: Arc<dyn NetConnector>,
    pgpass: Option<Arc<dyn Fn(&str, u16, &str, &str) -> Option<Secret> + Send + Sync>>,
    time_zone: Option<SessionTimeZone>,
) -> DbResult<Connected> {
    if let Some(cmd) = cfg.init.shell_before_connect.as_deref().filter(|c| !c.trim().is_empty()) {
        run_shell(cmd, Duration::from_secs(60)).await.map_err(|e| e.with_context("Shell command before connect"))?;
    }
    let password = match &cfg.auth {
        AuthMethod::Password | AuthMethod::Prompt => creds.password.clone(),
        AuthMethod::Pgpass => {
            let user = cfg.user.clone().unwrap_or_default();
            let db = cfg.database.clone().unwrap_or_default();
            pgpass.as_ref().and_then(|f| f(&cfg.host, cfg.effective_port(), &db, &user))
        }
        AuthMethod::Command { command } => Some(ternarygrid_net::password_from_command(command, Duration::from_secs(30)).await.map_err(|e| e.with_context("Password command"))?),
        AuthMethod::None => None,
    };
    let mut params = ConnectParams::from_config(&cfg, password);
    params.auto_commit = cfg.auto_commit.unwrap_or(conn_type.auto_commit);
    if let Some(tz) = &time_zone {
        apply_time_zone(&mut params, tz);
    }
    // An in-memory database is one per connection, not one per session (editor, data tab, navigator).
    if cfg.kind == DbKind::Sqlite && params.file_path.as_deref().is_some_and(|p| p.as_os_str() == ":memory:") {
        params.file_path = Some(PathBuf::from(format!(":memory:{}", cfg.id)));
    }
    let mut tunnel = None;
    if !cfg.kind.is_file_based()
        && let Some(t) = net.open(&cfg, &creds, &cfg.host, cfg.effective_port()).await?
    {
        params.tls_server_name = Some(cfg.host.clone());
        params.host = t.local_host.clone();
        params.port = t.local_port;
        tunnel = Some(t);
    }
    let timeout = params.connect_timeout + Duration::from_secs(5);
    let mut session =
        tokio::time::timeout(timeout, driver.connect(&params)).await.map_err(|_| DbError::timeout(format!("Connection timed out after {}s", timeout.as_secs())))??;
    init_session(session.as_mut(), &cfg, &params).await?;
    let database = session.current_database().await.ok().flatten();
    let schema = session.current_schema().await.ok().flatten();
    let server = session.server_info().clone();
    Ok(Connected { session, params, tunnel, server, database, schema })
}

/// A connection opened without the UI (tasks, `ternarygrid --run-task`). Dropping
/// it closes the tunnel; close the session first.
pub struct HeadlessConnection {
    pub session: Box<dyn ternarygrid_core::Session>,
    pub params: ConnectParams,
    pub driver: Arc<dyn Driver>,
    pub config: ConnectionConfig,
    _tunnel: Option<TunnelGuard>,
}

/// Connect to a saved connection with saved credentials only (no prompts).
pub async fn connect_saved(
    registry: &DriverRegistry,
    store: &ConnectionStore,
    secrets: &dyn SecretSource,
    net: Arc<dyn NetConnector>,
    id: &str,
    time_zone: TimeZoneChoice,
) -> DbResult<HeadlessConnection> {
    let cfg = store.get(id).cloned().ok_or_else(|| DbError::config(format!("Unknown connection {id}")))?;
    if matches!(cfg.auth, AuthMethod::Prompt) {
        return Err(DbError::auth(format!("The connection \"{}\" asks for its password at every connect; save the password to use it in tasks", cfg.name)));
    }
    let driver = registry.get(cfg.kind).ok_or_else(|| DbError::unsupported(format!("No driver for {}", cfg.kind)))?;
    let net_cfg = store.effective_network(&cfg);
    // Only read the secrets this connection uses (each read may hit the OS keychain).
    let get = |slot, used: bool| if used { secrets.get(&store.network_secret_key(&cfg, slot)).ok().flatten() } else { None };
    let password_used = !cfg.kind.is_file_based() && matches!(cfg.auth, AuthMethod::Password);
    let creds = Credentials {
        password: get(SecretSlot::Password, password_used),
        ssh_password: get(SecretSlot::SshPassword, net_cfg.ssh.is_some()),
        ssh_passphrase: get(SecretSlot::SshPassphrase, net_cfg.ssh.is_some()),
        proxy_password: get(SecretSlot::ProxyPassword, net_cfg.proxy.is_some()),
    };
    let mut eff = cfg.clone();
    eff.ssh = net_cfg.ssh;
    eff.proxy = net_cfg.proxy;
    let conn_type = store.connection_type(&cfg.connection_type);
    let pgpass: Option<Arc<dyn Fn(&str, u16, &str, &str) -> Option<Secret> + Send + Sync>> =
        Some(Arc::new(|h: &str, p: u16, d: &str, u: &str| ternarygrid_config::import::pgpass::lookup(h, p, d, u)));
    let c = connect_task(driver.clone(), eff, conn_type, creds, net, pgpass, session_time_zone(time_zone)).await?;
    Ok(HeadlessConnection { session: c.session, params: c.params, driver, config: cfg, _tunnel: c.tunnel })
}

/// Configured + live connections.
pub struct ConnectionManager {
    pub registry: DriverRegistry,
    /// Configured connections, folders, connection types and network profiles.
    pub store: ConnectionStore,
    live: HashMap<String, LiveConnection>,
    connecting: HashMap<String, Job<DbResult<Connected>>>,
    /// Connects given up while running (disconnected, removed): what they open is
    /// closed when they finish, with the after-disconnect command.
    abandoned: Vec<(Job<DbResult<Connected>>, Option<String>)>,
    failed: HashMap<String, String>,
    /// Connect errors already reported (their connection stays red until it connects).
    failed_shown: HashSet<String>,
    pub secrets: Arc<dyn SecretSource>,
    pub net: Arc<dyn NetConnector>,
    pub pgpass: Option<Arc<dyn Fn(&str, u16, &str, &str) -> Option<Secret> + Send + Sync>>,
    /// Time zone of new sessions (Settings › General).
    pub time_zone: TimeZoneChoice,
    /// Ids whose connect finished this frame (for UI reactions like expanding the tree).
    pub just_connected: Vec<String>,
    /// Clocks at the last poll: a sleep of the computer shows as a gap between them.
    last_poll: Option<(Instant, SystemTime)>,
    /// How long a connection being checked may take to answer ([`CHECK_TIMEOUT`]).
    pub check_timeout: Duration,
    /// The store changed and should be written to disk.
    pub dirty: bool,
}

impl ConnectionManager {
    pub fn new(registry: DriverRegistry, secrets: Arc<dyn SecretSource>, net: Arc<dyn NetConnector>) -> Self {
        ConnectionManager {
            registry,
            store: ConnectionStore::new(),
            live: HashMap::new(),
            connecting: HashMap::new(),
            abandoned: Vec::new(),
            failed: HashMap::new(),
            failed_shown: HashSet::new(),
            secrets,
            net,
            pgpass: None,
            time_zone: TimeZoneChoice::default(),
            just_connected: Vec::new(),
            last_poll: None,
            check_timeout: CHECK_TIMEOUT,
            dirty: false,
        }
    }

    pub fn configs(&self) -> &[ConnectionConfig] {
        &self.store.connections
    }

    pub fn folders(&self) -> &[String] {
        &self.store.folders
    }

    /// Built-in and user defined connection types.
    pub fn connection_types(&self) -> Vec<ConnectionType> {
        self.store.all_connection_types()
    }

    pub fn config(&self, id: &str) -> Option<&ConnectionConfig> {
        self.store.get(id)
    }

    pub fn config_mut(&mut self, id: &str) -> Option<&mut ConnectionConfig> {
        self.dirty = true;
        self.store.get_mut(id)
    }

    pub fn connection_type(&self, id: &str) -> ConnectionType {
        self.store.connection_type(id)
    }

    pub fn type_of(&self, conn_id: &str) -> ConnectionType {
        self.config(conn_id).map(|c| self.connection_type(&c.connection_type)).unwrap_or_default()
    }

    /// Marker colour of a connection (explicit colour or connection type colour).
    pub fn color_of(&self, conn_id: &str) -> Option<[u8; 3]> {
        self.config(conn_id).map(|c| self.store.color(c))
    }

    pub fn upsert(&mut self, cfg: ConnectionConfig) {
        // A live connection shows the new name, colour, type and navigator settings
        // at once (its sessions keep what they were opened with until a reconnect).
        let conn_type = self.connection_type(&cfg.connection_type);
        if let Some(live) = self.live.get_mut(&cfg.id) {
            live.config = cfg.clone();
            live.conn_type = conn_type;
        }
        let r = if self.store.get(&cfg.id).is_some() { self.store.update(cfg) } else { self.store.add(cfg).map(|_| ()) };
        if let Err(e) = r {
            tracing::warn!("cannot store connection: {e}");
        }
        self.dirty = true;
    }

    pub fn remove(&mut self, backend: &Backend, id: &str) {
        self.disconnect(backend, id);
        self.store.remove(id);
        for key in ConnectionStore::secret_keys_for(id) {
            let _ = self.secrets.delete(&key);
        }
        self.dirty = true;
    }

    /// Configuration with the effective SSH / proxy settings (network profile) applied.
    pub fn effective_config(&self, cfg: &ConnectionConfig) -> ConnectionConfig {
        let net = self.store.effective_network(cfg);
        let mut c = cfg.clone();
        c.ssh = net.ssh;
        c.proxy = net.proxy;
        c
    }

    fn secret_for(&self, cfg: &ConnectionConfig, slot: SecretSlot) -> Option<Secret> {
        self.secrets.get(&self.store.network_secret_key(cfg, slot)).ok().flatten()
    }

    pub fn status(&self, id: &str) -> ConnStatus {
        if self.connecting.contains_key(id) {
            return ConnStatus::Connecting;
        }
        if let Some(l) = self.live.get(id) {
            if l.main.is_closed() || !l.tunnel_alive() {
                return ConnStatus::Lost;
            }
            if l.verify.is_some() {
                return ConnStatus::Checking;
            }
            return ConnStatus::Connected;
        }
        if let Some(e) = self.failed.get(id) {
            return ConnStatus::Failed(e.clone());
        }
        ConnStatus::Disconnected
    }

    pub fn is_connected(&self, id: &str) -> bool {
        self.live.contains_key(id)
    }

    pub fn is_connecting(&self) -> bool {
        !self.connecting.is_empty()
    }

    /// Changes whenever the connection `id` is opened again (connect, reconnect, a lost
    /// main session revived); 0 when it is not connected.
    pub fn epoch(&self, id: &str) -> u64 {
        self.live.get(id).map_or(0, |l| l.epoch)
    }

    /// Check every connection now: each session must answer, or it is given up (the
    /// connection turns red, connects again by itself, and its tabs read on new
    /// sessions). Done by [`poll`](Self::poll) when the computer slept.
    pub fn check_all(&mut self, backend: &Backend) {
        let timeout = self.check_timeout;
        for live in self.live.values_mut() {
            live.start_verify(backend, timeout);
        }
    }

    /// Connect with a draft configuration and disconnect again ("Test connection").
    /// Returns a short server description on success.
    pub fn test_connection(&self, backend: &Backend, cfg: ConnectionConfig, prompted: Credentials) -> Job<DbResult<String>> {
        let driver = self.registry.get(cfg.kind);
        let conn_type = self.connection_type(&cfg.connection_type);
        let creds = self.credentials_for(&cfg, &prompted);
        let cfg = self.effective_config(&cfg);
        let net = self.net.clone();
        let pgpass = self.pgpass.clone();
        // With the sessions' time zone: a test fails as a connect would.
        let time_zone = session_time_zone(self.time_zone);
        backend.spawn(async move {
            let driver = driver.ok_or_else(|| DbError::unsupported(format!("No driver for {}", cfg.kind)))?;
            let started = Instant::now();
            let mut c = connect_task(driver, cfg, conn_type, creds, net, pgpass, time_zone).await?;
            let info = format!("{} {} ({} ms)", c.server.product, c.server.version.split_whitespace().next().unwrap_or(""), started.elapsed().as_millis());
            let _ = c.session.close().await;
            drop(c.tunnel.take());
            Ok(info)
        })
    }

    pub fn live(&self, id: &str) -> Option<&LiveConnection> {
        self.live.get(id)
    }

    pub fn live_mut(&mut self, id: &str) -> Option<&mut LiveConnection> {
        self.live.get_mut(id)
    }

    pub fn live_ids(&self) -> Vec<String> {
        self.live.keys().cloned().collect()
    }

    /// Which secrets must be prompted for because they are not saved.
    pub fn missing_credentials(&self, id: &str) -> Vec<PromptField> {
        let Some(cfg) = self.config(id) else { return Vec::new() };
        let mut out = Vec::new();
        let saved = |slot| self.secret_for(cfg, slot).is_some();
        match cfg.auth {
            AuthMethod::Prompt => out.push(PromptField::Password),
            AuthMethod::Password if !cfg.kind.is_file_based() && !(cfg.save_password && saved(SecretSlot::Password)) => out.push(PromptField::Password),
            _ => {}
        }
        let net = self.store.effective_network(cfg);
        if let Some(ssh) = net.ssh.as_ref() {
            match &ssh.auth {
                ternarygrid_core::config::SshAuth::Password if !(ssh.save_credentials && saved(SecretSlot::SshPassword)) => out.push(PromptField::SshPassword),
                _ => {}
            }
        }
        if let Some(p) = net.proxy.as_ref().filter(|p| p.user.is_some())
            && !(p.save_credentials && saved(SecretSlot::ProxyPassword))
        {
            out.push(PromptField::ProxyPassword);
        }
        out
    }

    /// Collect saved credentials, overlaid with the prompted ones.
    pub fn credentials(&self, id: &str, prompted: &Credentials) -> Credentials {
        match self.config(id) {
            Some(cfg) => self.credentials_for(cfg, prompted),
            None => prompted.clone(),
        }
    }

    /// Saved credentials of `cfg` (also an unsaved draft), overlaid with the prompted ones.
    pub fn credentials_for(&self, cfg: &ConnectionConfig, prompted: &Credentials) -> Credentials {
        // Only read the secrets the connection can use (each read may hit the OS keychain).
        let net = self.store.effective_network(cfg);
        let get = |slot, used: bool| if used { self.secret_for(cfg, slot) } else { None };
        let password_used = !cfg.kind.is_file_based() && matches!(cfg.auth, AuthMethod::Password | AuthMethod::Prompt);
        Credentials {
            password: prompted.password.clone().or_else(|| get(SecretSlot::Password, password_used)),
            ssh_password: prompted.ssh_password.clone().or_else(|| get(SecretSlot::SshPassword, net.ssh.is_some())),
            ssh_passphrase: prompted.ssh_passphrase.clone().or_else(|| get(SecretSlot::SshPassphrase, net.ssh.is_some())),
            proxy_password: prompted.proxy_password.clone().or_else(|| get(SecretSlot::ProxyPassword, net.proxy.is_some())),
        }
    }

    /// Start connecting with the given (prompted) credentials.
    pub fn connect(&mut self, backend: &Backend, id: &str, prompted: Credentials) -> DbResult<()> {
        if self.live.contains_key(id) || self.connecting.contains_key(id) {
            return Ok(());
        }
        let cfg = self.config(id).map(|c| self.effective_config(c)).ok_or_else(|| DbError::config("Unknown connection"))?;
        let driver = self.registry.get(cfg.kind).ok_or_else(|| DbError::unsupported(format!("No driver for {}", cfg.kind)))?;
        let conn_type = self.connection_type(&cfg.connection_type);
        let creds = self.credentials(id, &prompted);
        let net = self.net.clone();
        let pgpass = self.pgpass.clone();
        self.failed.remove(id);
        self.failed_shown.remove(id);
        let time_zone = session_time_zone(self.time_zone);
        self.connecting.insert(id.to_string(), backend.spawn(connect_task(driver, cfg, conn_type, creds, net, pgpass, time_zone)));
        Ok(())
    }

    /// Poll connect jobs and keep-alive. Call once per frame.
    pub fn poll(&mut self, backend: &Backend) {
        self.just_connected.clear();
        let now = (Instant::now(), SystemTime::now());
        if let Some(prev) = self.last_poll.replace(now)
            && slept(prev, now)
            && !self.live.is_empty()
        {
            tracing::info!("the computer slept: checking the connections");
            self.check_all(backend);
        }
        self.abandoned.retain_mut(|(job, after)| match job.poll() {
            Some(Ok(c)) => {
                backend.spawn_detached(close_connected(c, after.take()));
                false
            }
            Some(Err(_)) => false,
            None => true,
        });
        let ids: Vec<String> = self.connecting.keys().cloned().collect();
        for id in ids {
            let done = self.connecting.get_mut(&id).and_then(|j| j.poll());
            if let Some(res) = done {
                self.connecting.remove(&id);
                match res {
                    Ok(c) => {
                        let Some(cfg) = self.config(&id).cloned() else { continue };
                        let Some(driver) = self.registry.get(cfg.kind) else { continue };
                        // The session's dialect: it knows the server (MySQL / MariaDB version,
                        // CockroachDB) and the connection's settings (SQLite foreign keys).
                        let dialect = c.session.dialect();
                        let catalog_folders = Arc::new(dialect.catalog_folders());
                        let conn_type = self.connection_type(&cfg.connection_type);
                        let main = SessionHandle::new(c.session, format!("{} (main)", cfg.name), c.database.clone());
                        let keepalive = KeepAlive::start(backend, &main, cfg.init.keep_alive_secs);
                        if let Some(cfg) = self.config_mut(&id) {
                            cfg.last_connected = Some(chrono::Utc::now().timestamp());
                        }
                        self.live.insert(
                            id.clone(),
                            LiveConnection {
                                config: cfg,
                                conn_type,
                                driver,
                                dialect,
                                params: c.params,
                                tunnel: c.tunnel,
                                main,
                                db_sessions: HashMap::new(),
                                tab_sessions: Arc::new(parking_lot::Mutex::new(Vec::new())),
                                revive: Revive::default(),
                                verify: None,
                                epoch: next_epoch(),
                                server: c.server,
                                meta: MetadataCache::default(),
                                connected_at: Instant::now(),
                                current_database: c.database,
                                current_schema: c.schema,
                                catalog_folders,
                                _keepalive: keepalive,
                            },
                        );
                        self.just_connected.push(id);
                    }
                    Err(e) => {
                        self.failed_shown.remove(&id);
                        self.failed.insert(id, e.full_text());
                    }
                }
            }
        }
        for live in self.live.values_mut() {
            live.meta.poll();
            for s in live.db_sessions.values_mut() {
                s.poll();
            }
            if live.verify.as_mut().is_some_and(|j| j.poll().is_some()) {
                live.verify = None;
            }
            live.check_health(backend, self.check_timeout);
        }
        let ids: Vec<String> = self.live.iter().filter(|(_, l)| l.needs_new_tunnel()).map(|(id, _)| id.clone()).collect();
        for id in ids {
            let Some(cfg) = self.config(&id).map(|c| self.effective_config(c)) else { continue };
            let Some(driver) = self.registry.get(cfg.kind) else { continue };
            let conn_type = self.connection_type(&cfg.connection_type);
            // The database password the connection was opened with (it may have been prompted).
            let prompted = Credentials { password: self.live[&id].params.password.clone(), ..Default::default() };
            let creds = self.credentials(&id, &prompted);
            let job = backend.spawn(connect_task(driver, cfg, conn_type, creds, self.net.clone(), self.pgpass.clone(), session_time_zone(self.time_zone)));
            if let Some(live) = self.live.get_mut(&id) {
                tracing::info!("{}: the tunnel ended; connecting again", live.config.name);
                live.revive.job = Some(ReviveJob::Connection(job));
            }
        }
    }

    /// `s` is a connection's own session (main or per database), not a tab's: closing a
    /// tab that uses it ends no transaction.
    pub fn is_shared_session(&self, s: &SessionHandle) -> bool {
        self.live.values().any(|l| l.main.same(s) || l.db_sessions.values().any(|d| d.get().is_some_and(|d| d.same(s))))
    }

    /// Take the connect error of `id` (shown once by the UI).
    pub fn take_error(&mut self, id: &str) -> Option<String> {
        let e = self.failed.get(id).cloned();
        self.failed_shown.insert(id.to_string());
        e
    }

    pub fn errors(&self) -> Vec<(String, String)> {
        self.failed.iter().filter(|(k, _)| !self.failed_shown.contains(*k)).map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// Disconnect, closing all sessions in the background.
    pub fn disconnect(&mut self, backend: &Backend, id: &str) {
        if let Some(live) = self.live.remove(id) {
            // Closed for its tabs at once: they open new sessions (after a reconnect)
            // instead of waiting on these, which may never answer again.
            for s in live.sessions() {
                s.mark_closed_here();
            }
            backend.spawn_detached(close_live(live));
        }
        // A connect still running: its session and tunnel are closed once it finishes.
        if let Some(job) = self.connecting.remove(id) {
            let after = self.config(id).and_then(|c| c.init.shell_after_disconnect.clone());
            self.abandoned.push((job, after));
        }
    }

    /// Connects given up that have not finished yet.
    pub fn has_abandoned_connects(&self) -> bool {
        !self.abandoned.is_empty()
    }

    pub fn disconnect_all(&mut self, backend: &Backend) {
        for id in self.live_ids() {
            self.disconnect(backend, &id);
        }
    }

    /// Disconnect everything and wait (at most `timeout`): TernaryGrid is quitting, and a
    /// detached task would not finish (sessions, tunnels, the after-disconnect commands).
    pub fn disconnect_all_and_wait(&mut self, backend: &Backend, timeout: Duration) {
        let lives: Vec<LiveConnection> = self.live_ids().iter().filter_map(|id| self.live.remove(id)).collect();
        self.connecting.clear();
        if lives.is_empty() {
            return;
        }
        backend.block_on(async move {
            let _ = tokio::time::timeout(timeout, async move {
                for live in lives {
                    close_live(live).await;
                }
            })
            .await;
        });
    }
}

/// Close a connection's sessions (together: each is bounded, one that does not answer
/// is given up) and tunnel, then run its after-disconnect command.
async fn close_live(live: LiveConnection) {
    let after = live.config.init.shell_after_disconnect.clone();
    let sessions = live.sessions();
    live.tab_sessions.lock().clear();
    futures::future::join_all(sessions.iter().map(|s| s.cancel_and_close())).await;
    drop(live.tunnel);
    run_after_disconnect(after).await;
}

/// Close what a connect given up opened.
async fn close_connected(mut c: Connected, after: Option<String>) {
    let _ = c.session.close().await;
    drop(c.tunnel.take());
    run_after_disconnect(after).await;
}

async fn run_after_disconnect(after: Option<String>) {
    if let Some(cmd) = after.filter(|c| !c.trim().is_empty())
        && let Err(e) = run_shell(&cmd, Duration::from_secs(60)).await
    {
        tracing::warn!("shell command after disconnect failed: {e}");
    }
}

/// Secret store that keeps everything in memory (tests, or when the real store is locked).
#[derive(Default)]
pub struct MemorySecrets {
    map: parking_lot::Mutex<HashMap<String, Secret>>,
}

impl SecretSource for MemorySecrets {
    fn get(&self, key: &str) -> DbResult<Option<Secret>> {
        Ok(self.map.lock().get(key).cloned())
    }
    fn set(&self, key: &str, value: &Secret) -> DbResult<()> {
        self.map.lock().insert(key.to_string(), value.clone());
        Ok(())
    }
    fn delete(&self, key: &str) -> DbResult<()> {
        self.map.lock().remove(key);
        Ok(())
    }
}

/// Secret source backed by the configured Keychain / encrypted vault.
pub struct ManagedSecrets(pub Arc<parking_lot::RwLock<ternarygrid_config::SecretsManager>>);

impl SecretSource for ManagedSecrets {
    fn get(&self, key: &str) -> DbResult<Option<Secret>> {
        self.0.read().get(key)
    }
    fn set(&self, key: &str, value: &Secret) -> DbResult<()> {
        self.0.read().set(key, value)
    }
    fn delete(&self, key: &str) -> DbResult<()> {
        self.0.read().delete(key)
    }
    fn is_locked(&self) -> bool {
        self.0.read().is_locked()
    }
}

/// Opens SSH tunnels and SOCKS5 / HTTP proxies through `ternarygrid-net`
/// (the connection's settings are already the effective ones).
pub struct TunnelNet {
    pub known_hosts: PathBuf,
}

#[async_trait::async_trait]
impl NetConnector for TunnelNet {
    async fn open(&self, cfg: &ConnectionConfig, creds: &Credentials, host: &str, port: u16) -> DbResult<Option<TunnelGuard>> {
        let ssh = cfg.ssh.clone().filter(|s| s.enabled);
        let proxy = cfg.proxy.clone().filter(|p| p.enabled);
        if ssh.is_none() && proxy.is_none() {
            return Ok(None);
        }
        let mut spec = ternarygrid_net::TunnelSpec::new(host, port, self.known_hosts.clone());
        if let Some(p) = proxy {
            spec = spec.with_proxy(p, creds.proxy_password.clone());
        }
        if let Some(s) = ssh {
            spec = spec.with_ssh(s, creds.ssh_password.clone(), creds.ssh_passphrase.clone());
        }
        let tunnel = Arc::new(ternarygrid_net::open_tunnel(spec).await?);
        let alive = tunnel.clone();
        let checked = tunnel.clone();
        let check: TunnelCheck = Box::new(move || {
            let t = checked.clone();
            Box::pin(async move { t.ensure_connected().await })
        });
        Ok(Some(TunnelGuard {
            local_host: tunnel.local_host(),
            local_port: tunnel.local_port(),
            alive: Box::new(move || alive.is_alive()),
            guard: Box::new(tunnel),
            check: Some(check),
        }))
    }
}

/// Connector for direct connections only (tests).
pub struct DirectNet;

#[async_trait::async_trait]
impl NetConnector for DirectNet {
    async fn open(&self, cfg: &ConnectionConfig, _creds: &Credentials, _host: &str, _port: u16) -> DbResult<Option<TunnelGuard>> {
        if cfg.ssh.as_ref().is_some_and(|s| s.enabled) || cfg.proxy.as_ref().is_some_and(|p| p.enabled) {
            return Err(DbError::unsupported("SSH tunnels and proxies are not available in this build"));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_time_zones() {
        assert_eq!(format_offset(25_200), "+07:00");
        assert_eq!(format_offset(-19_800), "-05:30");
        assert_eq!(posix_zone(25_200), "<+07>-7");
        assert_eq!(posix_zone(-19_800), "<-0530>+5:30");
        assert!(session_time_zone(TimeZoneChoice::Server).is_none());
        assert_eq!(session_time_zone(TimeZoneChoice::Utc).unwrap().name, "UTC");
        let system = session_time_zone(TimeZoneChoice::System).unwrap();
        assert!(system.offset.len() == 6 && !system.name.is_empty(), "{system:?}");
        let tz = SessionTimeZone { name: "Asia/Jakarta".into(), offset: "+07:00".into(), local: true };
        let params = |kind: DbKind, props: &[(&str, &str)]| {
            let mut p = ConnectParams::from_config(&ConnectionConfig::new(kind, "x"), None);
            p.properties.extend(props.iter().map(|(k, v)| (k.to_string(), v.to_string())));
            apply_time_zone(&mut p, &tz);
            p.properties
        };
        assert_eq!(params(DbKind::Postgres, &[]).get("timezone").map(String::as_str), Some("Asia/Jakarta"));
        assert_eq!(params(DbKind::MariaDb, &[]).get("time_zone").map(String::as_str), Some("+07:00"));
        // The connection's own time zone wins; others have no session time zone.
        assert_eq!(params(DbKind::MySql, &[("serverTimezone", "UTC")]).len(), 1);
        assert_eq!(params(DbKind::Postgres, &[("TimeZone", "UTC")]).get("TimeZone").map(String::as_str), Some("UTC"));
        assert_eq!(params(DbKind::CockroachDb, &[]).get("timezone").map(String::as_str), Some("Asia/Jakarta"));
        assert_eq!(params(DbKind::ClickHouse, &[]).get("time_zone").map(String::as_str), Some("Asia/Jakarta"));
        assert_eq!(params(DbKind::SqlServer, &[]).get("time_zone").map(String::as_str), Some("local"));
        assert_eq!(params(DbKind::Oracle, &[]).get("time_zone").map(String::as_str), Some("Asia/Jakarta"));
        assert!(params(DbKind::Sqlite, &[]).is_empty());
    }

    async fn sqlite_session(label: &str) -> SessionHandle {
        let mut cfg = ConnectionConfig::new(DbKind::Sqlite, label);
        cfg.file_path = Some(":memory:".into());
        let s = ternarygrid_driver_sqlite::SqliteDriver::new().connect(&ConnectParams::from_config(&cfg, None)).await.unwrap();
        SessionHandle::new(s, label, None)
    }

    #[tokio::test]
    async fn a_connection_ended_underneath_is_lost() {
        // The server ends the connection (idle timeout, restart): no error was seen yet.
        let h = sqlite_session("a").await;
        assert!(!h.is_closed());
        h.lock().await.close().await.unwrap();
        assert!(h.is_closed() && h.is_lost());
        // Closed by TernaryGrid: closed, not lost.
        let h = sqlite_session("b").await;
        h.close().await;
        assert!(h.is_closed() && !h.is_lost());
    }

    /// A SQLite connection whose after-disconnect command creates `marker`.
    #[cfg(unix)]
    fn manager_with(dir: &std::path::Path, marker: &std::path::Path, before: Option<&str>) -> (ConnectionManager, String) {
        let mut cfg = ConnectionConfig::new(DbKind::Sqlite, "rm");
        cfg.file_path = Some(dir.join("rm.db").display().to_string());
        cfg.init.shell_before_connect = before.map(str::to_string);
        cfg.init.shell_after_disconnect = Some(format!("touch '{}'", marker.display()));
        let mut cm = ConnectionManager::new(crate::driver_registry(), Arc::new(MemorySecrets::default()), Arc::new(DirectNet));
        let id = cfg.id.clone();
        cm.upsert(cfg);
        (cm, id)
    }

    #[cfg(unix)]
    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let t0 = Instant::now();
        while !done() {
            assert!(t0.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn removing_a_connected_connection_closes_it() {
        let backend = Backend::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("after");
        let (mut cm, id) = manager_with(dir.path(), &marker, None);
        cm.connect(&backend, &id, Credentials::default()).unwrap();
        wait_for("connect", || {
            cm.poll(&backend);
            cm.is_connected(&id)
        });
        let tab = backend.block_on(cm.live(&id).unwrap().open_session_future("tab".into(), None)).unwrap();
        cm.remove(&backend, &id);
        assert!(cm.config(&id).is_none() && !cm.is_connected(&id));
        // The tab's session closes and the after-disconnect command runs.
        wait_for("the tab session to close", || tab.is_closed() && !tab.is_lost());
        wait_for("the after-disconnect command", || marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_connect_given_up_is_closed_when_it_finishes() {
        let backend = Backend::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("after");
        let (mut cm, id) = manager_with(dir.path(), &marker, Some("sleep 0.3"));
        cm.connect(&backend, &id, Credentials::default()).unwrap();
        assert_eq!(cm.status(&id), ConnStatus::Connecting);
        cm.disconnect(&backend, &id);
        assert_eq!(cm.status(&id), ConnStatus::Disconnected);
        wait_for("the connect to end", || {
            cm.poll(&backend);
            !cm.has_abandoned_connects()
        });
        wait_for("the after-disconnect command", || marker.exists());
        assert!(!cm.is_connected(&id));
    }

    #[test]
    fn a_sleep_shows_in_the_clocks() {
        let (t, w) = (Instant::now(), SystemTime::now());
        let at = |mono: u64, wall: u64| (t + Duration::from_secs(mono), w + Duration::from_secs(wall));
        assert!(!slept((t, w), at(5, 5)), "polls 5 s apart");
        assert!(!slept((t, w), at(5, 9)), "a small clock correction");
        assert!(slept((t, w), at(5, 600)), "slept 10 minutes: the monotonic clock stopped");
        assert!(slept((t, w), at(70, 70)), "no poll for over a minute (a monotonic clock that counts sleep)");
        assert!(!slept((t, w), (t + Duration::from_secs(5), w - Duration::from_secs(3600))), "a clock set back");
    }

    #[cfg(unix)]
    fn connected(backend: &Backend, dir: &std::path::Path) -> (ConnectionManager, String, String) {
        let marker = dir.join("after");
        let (mut cm, id) = manager_with(dir, &marker, None);
        cm.connect(backend, &id, Credentials::default()).unwrap();
        wait_for("connect", || {
            cm.poll(backend);
            cm.is_connected(&id)
        });
        (cm, id, dir.join("rm.db").display().to_string())
    }

    /// After the computer slept every connection is checked: one that answers stays
    /// as it was; one that does not turns red and connects again by itself, and the
    /// other sessions on the same dead path are given up too (their tabs open new ones).
    #[cfg(unix)]
    #[test]
    fn connections_are_checked_after_a_sleep() {
        let backend = Backend::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (mut cm, id, file) = connected(&backend, dir.path());
        let epoch = cm.epoch(&id);
        assert!(epoch > 0);
        let main = cm.live(&id).unwrap().main.clone();
        // The poll notices the sleep in its clocks.
        cm.last_poll = Some((Instant::now(), SystemTime::now() - Duration::from_secs(600)));
        cm.poll(&backend);
        assert_eq!(cm.status(&id), ConnStatus::Checking);
        wait_for("checked", || {
            cm.poll(&backend);
            cm.status(&id) == ConnStatus::Connected
        });
        assert!(cm.live(&id).unwrap().main.same(&main) && !main.is_closed(), "a connection that answers is kept");
        assert_eq!(cm.epoch(&id), epoch);

        // Its sessions went silent (half-open after the sleep).
        let (silent_main, main_silent) = backend.block_on(session::testing::silenceable(&file, "main"));
        let (tab, tab_silent) = backend.block_on(session::testing::silenceable(&file, "tab"));
        cm.live_mut(&id).unwrap().main = silent_main.clone();
        cm.live(&id).unwrap().tab_sessions.lock().push(tab.downgrade());
        main_silent.store(true, std::sync::atomic::Ordering::SeqCst);
        tab_silent.store(true, std::sync::atomic::Ordering::SeqCst);
        cm.check_timeout = Duration::from_millis(300);
        cm.check_all(&backend);
        assert_eq!(cm.status(&id), ConnStatus::Checking, "not green before it answered");
        wait_for("lost", || {
            cm.poll(&backend);
            silent_main.is_lost()
        });
        assert!(tab.is_lost(), "the tab's session on the dead path is given up too");
        wait_for("connected again", || {
            cm.poll(&backend);
            cm.status(&id) == ConnStatus::Connected && !cm.live(&id).unwrap().main.same(&silent_main)
        });
        assert_ne!(cm.epoch(&id), epoch, "tabs see that the connection is back");
        let ping = backend.block_on(cm.live(&id).unwrap().main.ping());
        assert!(ping.is_ok(), "{ping:?}");
    }

    /// Disconnect (or Reconnect) while a tab's statement waits on a session that went
    /// silent: the tab sees its session closed at once, the statement ends, and the
    /// after-disconnect command runs.
    #[cfg(unix)]
    #[test]
    fn disconnecting_does_not_wait_for_a_silent_session() {
        let backend = Backend::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (mut cm, id, file) = connected(&backend, dir.path());
        let (tab, silent) = backend.block_on(session::testing::silenceable(&file, "tab"));
        cm.live(&id).unwrap().tab_sessions.lock().push(tab.downgrade());
        silent.store(true, std::sync::atomic::Ordering::SeqCst);
        let running = {
            let tab = tab.clone();
            backend.spawn(async move { tab.execute("SELECT 1", &[], &ternarygrid_core::driver::ExecOptions::default()).await.map(|_| ()) })
        };
        wait_for("the statement to wait", || tab.is_busy());
        cm.disconnect(&backend, &id);
        assert!(tab.is_closed(), "closed for its tab at once");
        assert_eq!(cm.status(&id), ConnStatus::Disconnected);
        let mut running = running;
        wait_for("the statement to end", || running.is_finished());
        assert!(running.poll().unwrap().is_err_and(|e| e.is_connection_lost()));
        wait_for("the after-disconnect command", || dir.path().join("after").exists());
    }

    #[test]
    fn simple_split() {
        assert_eq!(split_simple("set a=1; select ';'; \n"), vec!["set a=1".to_string(), "select ';'".to_string()]);
        assert!(split_simple("  ").is_empty());
    }
}
