//! Shared, serialised access to one database session plus its single open cursor.

use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use ternarygrid_core::driver::{CancelHandle, ExecOptions, QueryCursor, ResultHeader, ServerMessage, Session, TransactionMode};
use ternarygrid_core::{DbError, DbKind, DbResult, Dialect, Row, Value};

/// One statement executed inside the current transaction (transaction log, FR-C5).
#[derive(Debug, Clone)]
pub struct TxLogEntry {
    pub sql: String,
    pub at: chrono::DateTime<chrono::Local>,
    pub duration: Duration,
    pub rows: Option<u64>,
    pub ok: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TxState {
    pub mode: TransactionMode,
    pub in_transaction: bool,
    pub log: Vec<TxLogEntry>,
    /// Writing statements logged in the transaction. Counted apart from the log, which
    /// drops its oldest entries: an early INSERT stays pending after many SELECTs.
    pub writes: usize,
}

impl TxState {
    /// Changes waiting for a commit or a rollback: the writing statements of the open
    /// transaction. Manual commit mode opens one for a SELECT too, which leaves nothing
    /// to commit.
    pub fn pending_writes(&self) -> usize {
        if self.in_transaction { self.writes } else { 0 }
    }
}

struct OpenCursor {
    token: u64,
    cursor: Box<dyn QueryCursor>,
}

struct Shared {
    session: tokio::sync::Mutex<Box<dyn Session>>,
    cursor: tokio::sync::Mutex<Option<OpenCursor>>,
    cancel: Option<Arc<dyn CancelHandle>>,
    kind: DbKind,
    dialect: Arc<dyn Dialect>,
    label: String,
    database: Option<String>,
    /// Operations running (an execute and a fetch may overlap): busy while any is.
    busy: AtomicUsize,
    closed: AtomicBool,
    /// Closed by [`SessionHandle::close`] (not lost: the server or the network ended it).
    closed_here: AtomicBool,
    next_token: AtomicU64,
    tx: Mutex<TxState>,
    busy_since: Mutex<Option<Instant>>,
    /// Last use of the open cursor.
    cursor_used: Mutex<Instant>,
    /// A task watches the open cursor for idleness (one per session).
    cursor_watched: AtomicBool,
    /// Rollbacks done: results saved inside the transaction show stale rows after one.
    rollbacks: AtomicU64,
    /// Cancelled when the session is given up ([`SessionHandle::abandon`]): every
    /// operation still waiting on it ends with a connection error. A connection that
    /// went silent (half-open after the computer slept) would keep them waiting until
    /// the operating system gives up on the socket, or for ever behind a tunnel.
    kill: tokio_util::sync::CancellationToken,
}

/// How long a close waits for the session (a statement still running, the server's
/// goodbye) before the session is given up.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// What [`SessionHandle::check`] found.
#[derive(Debug)]
pub enum Health {
    /// The server answered.
    Alive,
    /// The session is in use (a statement runs, a result is open): not asked.
    Busy,
    /// The connection is gone: the session was given up (closed, lost).
    Dead(DbError),
    /// No answer in time (a connection that went silent): the session was given up as lost.
    Silent,
}

/// An open cursor not read for this long is closed: on the server it holds a
/// snapshot and table locks (PostgreSQL: an ALTER of the table waits for it).
const CURSOR_IDLE: Duration = Duration::from_secs(300);

/// How long an open cursor of `kind` may stay unread. SQLite locks the whole file:
/// an unfinished read blocks every writer (other tabs, other programs) until it ends,
/// so its cursor is closed almost at once and the next page runs the query again.
fn cursor_idle(kind: DbKind) -> Duration {
    match kind {
        DbKind::Sqlite => Duration::from_secs(1),
        _ => CURSOR_IDLE,
    }
}

/// Cloneable handle to a session. All access goes through [`SessionHandle::lock`],
/// which first closes any cursor left open on the session (the driver contract
/// allows only one open cursor per session).
#[derive(Clone)]
pub struct SessionHandle {
    inner: Arc<Shared>,
}

/// A session that is not kept open by this reference (closed once its tabs drop it).
#[derive(Clone)]
pub struct WeakSession(std::sync::Weak<Shared>);

impl WeakSession {
    pub fn upgrade(&self) -> Option<SessionHandle> {
        self.0.upgrade().map(|inner| SessionHandle { inner })
    }
}

/// Result of [`SessionHandle::execute`].
pub struct Executed {
    pub token: u64,
    pub header: Option<ResultHeader>,
    pub messages: Vec<ServerMessage>,
}

impl SessionHandle {
    pub fn new(session: Box<dyn Session>, label: impl Into<String>, database: Option<String>) -> Self {
        let kind = session.kind();
        let dialect = session.dialect();
        let cancel = session.cancel_handle();
        let mode = session.transaction_mode();
        SessionHandle {
            inner: Arc::new(Shared {
                session: tokio::sync::Mutex::new(session),
                cursor: tokio::sync::Mutex::new(None),
                cancel,
                kind,
                dialect,
                label: label.into(),
                database,
                busy: AtomicUsize::new(0),
                closed: AtomicBool::new(false),
                closed_here: AtomicBool::new(false),
                next_token: AtomicU64::new(1),
                tx: Mutex::new(TxState { mode, ..Default::default() }),
                busy_since: Mutex::new(None),
                cursor_used: Mutex::new(Instant::now()),
                cursor_watched: AtomicBool::new(false),
                rollbacks: AtomicU64::new(0),
                kill: tokio_util::sync::CancellationToken::new(),
            }),
        }
    }

    pub fn kind(&self) -> DbKind {
        self.inner.kind
    }

    pub fn downgrade(&self) -> WeakSession {
        WeakSession(Arc::downgrade(&self.inner))
    }

    /// Cancel a running statement, then close (Disconnect: a long query does not hold it up).
    pub async fn cancel_and_close(&self) {
        // Closed (and so not used again) at once, also while the cancel waits.
        self.mark_closed_here();
        if self.is_busy() {
            let _ = tokio::time::timeout(CLOSE_WAIT, self.cancel()).await;
        }
        self.close().await;
    }

    /// Read as closed from now on (TernaryGrid closes it; the close itself follows).
    pub fn mark_closed_here(&self) {
        self.inner.closed_here.store(true, Ordering::Relaxed);
        self.inner.closed.store(true, Ordering::Relaxed);
    }

    /// Give the session up at once, without waiting for it: it reads as closed, every
    /// operation waiting on it ends with a connection error, and the driver drops its
    /// connection where it can. For a session that no longer answers. `lost`: the
    /// connection failed ([`is_lost`](Self::is_lost)), else TernaryGrid closed it.
    pub fn abandon(&self, lost: bool) {
        if !lost {
            self.inner.closed_here.store(true, Ordering::Relaxed);
        }
        self.inner.closed.store(true, Ordering::Relaxed);
        if !self.inner.kill.is_cancelled() {
            self.inner.kill.cancel();
            if let Some(c) = &self.inner.cancel {
                c.abort_connection();
            }
        }
    }

    /// Given up by [`abandon`](Self::abandon) (or a close that had to).
    pub fn is_abandoned(&self) -> bool {
        self.inner.kill.is_cancelled()
    }

    /// The error of an operation ended because the session was closed or given up.
    fn closed_error(&self) -> DbError {
        if self.inner.closed_here.load(Ordering::Relaxed) { DbError::connection("The session was closed") } else { DbError::connection("The connection to the server was lost") }
    }

    /// `op`, or a connection error as soon as the session is given up.
    async fn killable<T, E: From<DbError>>(&self, op: impl std::future::Future<Output = Result<T, E>>) -> Result<T, E> {
        tokio::select! {
            biased;
            _ = self.inner.kill.cancelled() => Err(E::from(self.closed_error())),
            r = op => r,
        }
    }

    /// `op`, or nothing once the session is given up.
    async fn unless_killed(&self, op: impl std::future::Future<Output = ()>) {
        tokio::select! {
            biased;
            _ = self.inner.kill.cancelled() => {}
            () = op => {}
        }
    }

    pub fn dialect(&self) -> Arc<dyn Dialect> {
        self.inner.dialect.clone()
    }

    pub fn label(&self) -> &str {
        &self.inner.label
    }

    pub fn database(&self) -> Option<&str> {
        self.inner.database.as_deref()
    }

    pub fn same(&self, other: &SessionHandle) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// A statement is currently running on this session.
    pub fn is_busy(&self) -> bool {
        self.inner.busy.load(Ordering::Relaxed) > 0
    }

    pub fn busy_for(&self) -> Option<Duration> {
        self.inner.busy_since.lock().map(|t| t.elapsed())
    }

    pub fn is_closed(&self) -> bool {
        if self.inner.closed.load(Ordering::Relaxed) {
            return true;
        }
        // The server or the network may have ended the connection while it was idle
        // (idle timeout, server restart): the driver knows without a round trip.
        // A busy session is not waited for; its statement reports the error.
        if let Ok(s) = self.inner.session.try_lock()
            && s.is_closed()
        {
            self.inner.closed.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// Closed without [`close`](Self::close): the connection to the server was lost.
    pub fn is_lost(&self) -> bool {
        self.is_closed() && !self.inner.closed_here.load(Ordering::Relaxed)
    }

    pub fn tx_state(&self) -> TxState {
        self.inner.tx.lock().clone()
    }

    /// An open transaction with changes (manual commit mode opens one for a SELECT too).
    pub fn has_uncommitted_writes(&self) -> bool {
        self.inner.tx.lock().pending_writes() > 0
    }

    pub fn clear_tx_log(&self) {
        let mut tx = self.inner.tx.lock();
        tx.log.clear();
    }

    /// Cancel the running statement (out of band).
    pub async fn cancel(&self) -> DbResult<()> {
        match &self.inner.cancel {
            Some(c) => c.cancel().await,
            None => Err(DbError::unsupported("Cancelling is not supported for this database")),
        }
    }

    pub fn can_cancel(&self) -> bool {
        self.inner.cancel.is_some()
    }

    /// Close the open cursor (if any).
    pub async fn close_cursor(&self) {
        self.unless_killed(async {
            let taken = self.inner.cursor.lock().await.take();
            if let Some(mut c) = taken {
                let _ = c.cursor.close().await;
            }
        })
        .await
    }

    /// A cursor is open on the session (a result being read page by page).
    pub fn has_open_cursor(&self) -> bool {
        self.inner.cursor.try_lock().map(|c| c.is_some()).unwrap_or(true)
    }

    /// Exclusive access to the session, after closing any open cursor.
    /// The cursor is closed once the session is ours: closed before, a caller
    /// waiting behind a running `execute` would get the session with that
    /// execute's new cursor still open.
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, Box<dyn Session>> {
        let guard = self.inner.session.lock().await;
        self.close_cursor().await;
        guard
    }

    /// Busy until the guard is dropped (also when the operation is cancelled mid-await).
    /// Counted: a fetch that ends does not make a running execute look idle.
    fn busy(&self) -> BusyGuard<'_> {
        let mut since = self.inner.busy_since.lock();
        if self.inner.busy.fetch_add(1, Ordering::Relaxed) == 0 {
            *since = Some(Instant::now());
        }
        BusyGuard(self)
    }

    /// A connection error means the session's connection is gone: given up, so that
    /// nothing else waits on it.
    fn note_error(&self, e: &DbError) {
        if e.is_connection_lost() {
            self.abandon(true);
        }
    }

    fn refresh_tx(&self, s: &dyn Session) {
        let mut tx = self.inner.tx.lock();
        tx.mode = s.transaction_mode();
        tx.in_transaction = s.in_transaction();
        if !tx.in_transaction {
            tx.log.clear();
            tx.writes = 0;
        }
        if s.is_closed() {
            self.inner.closed.store(true, Ordering::Relaxed);
        }
    }

    fn log_tx(&self, sql: &str, started: Instant, rows: Option<u64>, ok: bool) {
        let mut tx = self.inner.tx.lock();
        if tx.in_transaction || tx.mode == TransactionMode::Manual {
            let sql: String = sql.chars().take(4000).collect();
            if ternarygrid_sql::classify(&sql, self.inner.dialect.syntax()).is_write {
                tx.writes += 1;
            }
            tx.log.push(TxLogEntry { sql, at: chrono::Local::now(), duration: started.elapsed(), rows, ok });
            if tx.log.len() > 2000 {
                tx.log.remove(0);
            }
        }
    }

    /// Execute `sql` and keep the cursor open on the session. Returns the first
    /// result header; rows are read with [`SessionHandle::fetch`].
    pub async fn execute(&self, sql: &str, params: &[Value], opts: &ExecOptions) -> DbResult<Executed> {
        let started = Instant::now();
        let busy = self.busy();
        let res = self
            .killable(async {
                let mut guard = self.lock().await;
                let was_in_tx = guard.in_transaction();
                let mut cursor = guard.execute(sql, params, opts).await?;
                let header = cursor.next_result().await;
                let messages = cursor.take_messages();
                self.refresh_tx(guard.as_ref());
                // A ROLLBACK typed in an editor ends the transaction like the Rollback button:
                // results saved inside it reload.
                if was_in_tx && !guard.in_transaction() && ends_with_rollback(sql) {
                    self.inner.rollbacks.fetch_add(1, Ordering::Relaxed);
                }
                let header = header?;
                let token = self.inner.next_token.fetch_add(1, Ordering::Relaxed);
                // Store the cursor before releasing the session so no other
                // operation can slip in while the cursor owns the connection.
                let rows = matches!(header, Some(ResultHeader::Rows(_)));
                *self.inner.cursor.lock().await = Some(OpenCursor { token, cursor });
                *self.inner.cursor_used.lock() = Instant::now();
                drop(guard);
                // Rows left to read hold the cursor open (an INSERT's is gone at once).
                if rows {
                    self.watch_idle_cursor();
                }
                Ok(Executed { token, header, messages })
            })
            .await;
        drop(busy);
        match &res {
            Ok(ex) => {
                let rows = match &ex.header {
                    Some(ResultHeader::Update { rows_affected, .. }) => *rows_affected,
                    _ => None,
                };
                self.log_tx(sql, started, rows, true);
            }
            Err(e) => {
                self.note_error(e);
                self.log_tx(sql, started, None, false);
                // Transaction state may have changed (e.g. failed transaction).
                if let Ok(g) = self.inner.session.try_lock() {
                    self.refresh_tx(g.as_ref());
                }
            }
        }
        res
    }

    /// Fetch up to `max` rows from the open cursor identified by `token`.
    /// Returns `Err(CursorClosed)` when the cursor was closed in between
    /// (another operation needed the session).
    pub async fn fetch(&self, token: u64, max: usize) -> Result<FetchOutcome, FetchError> {
        let busy = self.busy();
        let res = self
            .killable(async {
                let mut slot = self.inner.cursor.lock().await;
                match slot.as_mut() {
                    Some(c) if c.token == token => match c.cursor.fetch(max).await {
                        Ok(rows) => {
                            *self.inner.cursor_used.lock() = Instant::now();
                            let messages = c.cursor.take_messages();
                            Ok(FetchOutcome { exhausted: rows.len() < max, rows, messages })
                        }
                        Err(e) => Err(FetchError::Db(e)),
                    },
                    _ => Err(FetchError::CursorClosed),
                }
            })
            .await;
        drop(busy);
        if let Err(FetchError::Db(e)) = &res {
            self.note_error(e);
        }
        res
    }

    /// Advance the open cursor to its next result (multi-result statements).
    pub async fn next_result(&self, token: u64) -> Result<Option<ResultHeader>, FetchError> {
        let res = self
            .killable(async {
                let mut slot = self.inner.cursor.lock().await;
                match slot.as_mut() {
                    Some(c) if c.token == token => c.cursor.next_result().await.map_err(FetchError::Db),
                    _ => Err(FetchError::CursorClosed),
                }
            })
            .await;
        if let Err(FetchError::Db(e)) = &res {
            self.note_error(e);
        }
        res
    }

    /// Close the open cursor once it was not read for [`cursor_idle`]; the result
    /// fetches its further rows by running the query again. One task per session
    /// watches whichever cursor is open, and ends when none is.
    fn watch_idle_cursor(&self) {
        if self.inner.cursor_watched.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            self.inner.cursor_watched.store(false, Ordering::Release);
            return;
        };
        let weak = self.downgrade();
        let limit = cursor_idle(self.inner.kind);
        rt.spawn(async move {
            let mut wait = limit;
            loop {
                tokio::time::sleep(wait).await;
                let Some(s) = weak.upgrade() else { return };
                // Busy (a fetch is running): looked at again later.
                let token = match s.inner.cursor.try_lock() {
                    Ok(slot) => slot.as_ref().map(|c| c.token),
                    Err(_) => {
                        wait = limit.min(Duration::from_secs(30));
                        continue;
                    }
                };
                let Some(token) = token else {
                    s.inner.cursor_watched.store(false, Ordering::Release);
                    return;
                };
                let idle = s.inner.cursor_used.lock().elapsed();
                if idle >= limit && !s.is_busy() {
                    s.close_cursor_token(token).await;
                    s.inner.cursor_watched.store(false, Ordering::Release);
                    return;
                }
                wait = limit.saturating_sub(idle).max(limit.min(Duration::from_secs(10)));
            }
        });
    }

    /// Whether the cursor identified by `token` is still open.
    pub async fn cursor_open(&self, token: u64) -> bool {
        self.killable(async { Ok::<_, DbError>(self.inner.cursor.lock().await.as_ref().is_some_and(|c| c.token == token)) }).await.unwrap_or(false)
    }

    /// Close the cursor only if it is the one identified by `token`.
    pub async fn close_cursor_token(&self, token: u64) {
        self.unless_killed(async {
            let mut slot = self.inner.cursor.lock().await;
            if slot.as_ref().is_some_and(|c| c.token == token)
                && let Some(mut c) = slot.take()
            {
                let _ = c.cursor.close().await;
            }
        })
        .await
    }

    /// Run `sql` to completion and return the first row set (metadata-style queries).
    pub async fn query_all(&self, sql: &str, params: &[Value]) -> DbResult<(Vec<ternarygrid_core::ColumnMeta>, Vec<Row>)> {
        let r = self
            .killable(async {
                let mut guard = self.lock().await;
                let r = ternarygrid_core::driver::query_all(guard.as_mut(), sql, params).await;
                self.refresh_tx(guard.as_ref());
                r
            })
            .await;
        if let Err(e) = &r {
            self.note_error(e);
        }
        r
    }

    /// Run `sql` ignoring results; returns affected rows.
    pub async fn execute_update(&self, sql: &str, params: &[Value]) -> DbResult<Option<u64>> {
        let started = Instant::now();
        let r = self
            .killable(async {
                let mut guard = self.lock().await;
                let r = ternarygrid_core::driver::execute_update(guard.as_mut(), sql, params).await;
                self.refresh_tx(guard.as_ref());
                r
            })
            .await;
        match &r {
            Ok(n) => self.log_tx(sql, started, *n, true),
            Err(e) => {
                self.note_error(e);
                self.log_tx(sql, started, None, false);
            }
        }
        r
    }

    /// Run data-change statements atomically (FR-D2 "save in one transaction").
    ///
    /// In auto-commit mode a transaction is opened, committed on success and
    /// rolled back on the first error, then auto-commit is restored. In manual
    /// mode the statements join the user's open transaction (nothing is
    /// committed). Returns the total number of affected rows and whether the
    /// changes are already committed.
    pub async fn apply_changes(&self, stmts: &[(String, Vec<Value>)]) -> DbResult<(u64, bool)> {
        self.apply_changes_reading(stmts, &[]).await.map(|a| (a.rows, a.committed))
    }

    /// [`apply_changes`](Self::apply_changes) that also keeps, per statement, the first
    /// row it returned (`INSERT … RETURNING`) and the generated id (MySQL).
    /// `expect_one[i]`: statement `i` must change exactly one row (a grid UPDATE / DELETE by
    /// its key); otherwise the save fails and is undone (0: changed or deleted by someone
    /// else; more: the key does not identify one row).
    pub async fn apply_changes_reading(&self, stmts: &[(String, Vec<Value>)], expect_one: &[bool]) -> DbResult<Applied> {
        let r = self.killable(self.apply_changes_locked(stmts, expect_one)).await;
        if let Err(e) = &r {
            self.note_error(e);
        }
        r
    }

    async fn apply_changes_locked(&self, stmts: &[(String, Vec<Value>)], expect_one: &[bool]) -> DbResult<Applied> {
        let mut guard = self.lock().await;
        // A transaction already open (manual commit, or BEGIN in an auto-commit editor):
        // the statements join it and nothing is committed or rolled back for the user.
        let in_tx = guard.in_transaction();
        let manual = guard.transaction_mode() == TransactionMode::Manual || in_tx;
        // A database without transactions (ClickHouse) applies each statement as it runs.
        let mut no_tx = false;
        if !manual && let Err(e) = guard.set_transaction_mode(TransactionMode::Manual).await {
            if e.kind != ternarygrid_core::ErrorKind::Unsupported {
                return Err(e);
            }
            no_tx = true;
        }
        // In the user's open transaction a failure must not leave the batch half applied:
        // the grid keeps every change pending, saving again would repeat the first ones.
        // Without one, a savepoint would itself be the transaction (SQLite: RELEASE commits),
        // and on failure the whole batch is rolled back instead.
        let savepoint = match (in_tx, guard.dialect().kind()) {
            (false, _) | (_, ternarygrid_core::DbKind::ClickHouse | ternarygrid_core::DbKind::Cassandra) => None,
            (true, ternarygrid_core::DbKind::SqlServer) => Some(("SAVE TRANSACTION ternarygrid_save", "ROLLBACK TRANSACTION ternarygrid_save", None)),
            // Oracle has no RELEASE SAVEPOINT.
            (true, ternarygrid_core::DbKind::Oracle) => Some(("SAVEPOINT ternarygrid_save", "ROLLBACK TO SAVEPOINT ternarygrid_save", None)),
            (true, _) => Some(("SAVEPOINT ternarygrid_save", "ROLLBACK TO SAVEPOINT ternarygrid_save", Some("RELEASE SAVEPOINT ternarygrid_save"))),
        };
        let savepoint = match savepoint {
            Some(sp) if ternarygrid_core::driver::execute_update(guard.as_mut(), sp.0, &[]).await.is_ok() => Some(sp),
            _ => None,
        };
        // No savepoint in the user's transaction (it failed already, or the database has
        // none): a failure after the first statement could not be undone without ending
        // the transaction, so several statements are not started.
        if in_tx && savepoint.is_none() && stmts.len() > 1 {
            self.refresh_tx(guard.as_ref());
            return Err(DbError::query(
                "The open transaction cannot take a savepoint (it failed, or the database has none), so the changes could not be saved all or nothing: commit or roll it back, then save again.",
            ));
        }
        let mut total = 0u64;
        let mut failure = None;
        let mut results = Vec::with_capacity(stmts.len());
        // How far the driver's row count can be trusted: SQL Server reports none with
        // NOCOUNT and adds a trigger's; MySQL counts changed rows (an unchanged value is 0).
        let (zero_means_missing, many_means_ambiguous) = match self.kind() {
            DbKind::Postgres | DbKind::CockroachDb | DbKind::Sqlite | DbKind::Oracle => (true, true),
            DbKind::MySql | DbKind::MariaDb => (false, true),
            _ => (false, false),
        };
        for (sql, params) in stmts {
            let started = Instant::now();
            let checked = run_reading(guard.as_mut(), sql, params).await.and_then(|r| match r.rows {
                Some(n) if expect_one.get(results.len()).copied().unwrap_or(false) && ((n == 0 && zero_means_missing) || (n > 1 && many_means_ambiguous)) => Err(if n == 0 {
                    DbError::query("The row was not found: it was changed or deleted since it was read. Refresh and edit it again.")
                } else {
                    DbError::query(format!("The statement would change {n} rows instead of one: the key does not identify a single row."))
                }),
                _ => Ok(r),
            });
            match checked {
                Ok(r) => {
                    // No count (Cassandra, SQL Server's NOCOUNT): the statement was made for one row.
                    total += r.rows.unwrap_or(1);
                    self.log_tx(sql, started, r.rows, true);
                    results.push(r);
                }
                Err(e) => {
                    self.note_error(&e);
                    self.log_tx(sql, started, None, false);
                    failure = Some(e.with_context(sql.chars().take(200).collect::<String>()));
                    break;
                }
            }
        }
        if let Some((_, rollback, release)) = savepoint {
            if failure.is_some() {
                let _ = ternarygrid_core::driver::execute_update(guard.as_mut(), rollback, &[]).await;
            }
            if let Some(release) = release {
                let _ = ternarygrid_core::driver::execute_update(guard.as_mut(), release, &[]).await;
            }
        }
        let result = match failure {
            Some(e) => {
                // Nothing of the user's was in the transaction: all of the batch goes.
                if !in_tx && savepoint.is_none() && !no_tx {
                    let _ = guard.rollback().await;
                }
                Err(e)
            }
            None if no_tx => Ok(Applied { rows: total, committed: true, results }),
            None if !manual => guard.commit().await.map(|_| Applied { rows: total, committed: true, results }),
            // Manual mode without a transaction before: it holds just this save, left open to commit.
            None => Ok(Applied { rows: total, committed: false, results }),
        };
        if !manual && !no_tx {
            let _ = guard.set_transaction_mode(TransactionMode::Auto).await;
        }
        self.refresh_tx(guard.as_ref());
        result
    }

    pub async fn set_transaction_mode(&self, mode: TransactionMode) -> DbResult<()> {
        let r = self
            .killable(async {
                let mut guard = self.lock().await;
                let r = guard.set_transaction_mode(mode).await;
                self.refresh_tx(guard.as_ref());
                r
            })
            .await;
        if let Err(e) = &r {
            self.note_error(e);
        }
        r
    }

    pub async fn commit(&self) -> DbResult<()> {
        let r = self
            .killable(async {
                let mut guard = self.lock().await;
                let r = guard.commit().await;
                self.refresh_tx(guard.as_ref());
                r
            })
            .await;
        match &r {
            Ok(()) => self.inner.tx.lock().log.clear(),
            Err(e) => self.note_error(e),
        }
        r
    }

    pub async fn rollback(&self) -> DbResult<()> {
        let r = self
            .killable(async {
                let mut guard = self.lock().await;
                let r = guard.rollback().await;
                self.refresh_tx(guard.as_ref());
                r
            })
            .await;
        match &r {
            Ok(()) => {
                self.inner.tx.lock().log.clear();
                self.inner.rollbacks.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => self.note_error(e),
        }
        r
    }

    /// Number of rollbacks done on the session.
    pub fn rollbacks(&self) -> u64 {
        self.inner.rollbacks.load(Ordering::Relaxed)
    }

    pub async fn ping(&self) -> DbResult<()> {
        // Do not disturb an open cursor or running statement for a keep-alive.
        if self.is_busy() {
            return Ok(());
        }
        let Ok(mut guard) = self.inner.session.try_lock() else { return Ok(()) };
        if self.inner.cursor.try_lock().map(|c| c.is_some()).unwrap_or(true) {
            return Ok(());
        }
        // A connection silently dropped by the network may never answer: bounded.
        let r = match tokio::time::timeout(Duration::from_secs(20), guard.ping()).await {
            Ok(r) => r,
            Err(_) => Err(DbError::connection("The server did not answer the ping within 20 s")),
        };
        drop(guard);
        if let Err(e) = &r {
            self.note_error(e);
        }
        r
    }

    /// Ask the server whether the session still answers, within `timeout` (after the
    /// computer slept, a connection may look open and never answer again). A session
    /// that does not answer, or whose connection is gone, is given up as lost. Unlike
    /// [`ping`](Self::ping), a session running a statement or holding a result open is
    /// reported busy, not alive. A metadata read or a ping holding it is waited for:
    /// one that does not end within `timeout` waits on a connection that went silent.
    pub async fn check(&self, timeout: Duration) -> Health {
        if self.is_closed() {
            return Health::Dead(self.closed_error());
        }
        if self.is_busy() || self.has_open_cursor() {
            return Health::Busy;
        }
        let deadline = Instant::now() + timeout;
        let mut guard = match tokio::time::timeout(timeout, self.inner.session.lock()).await {
            Ok(g) => g,
            Err(_) if self.is_busy() || self.has_open_cursor() => return Health::Busy,
            Err(_) => {
                self.abandon(true);
                return Health::Silent;
            }
        };
        // A statement started meanwhile left its result open.
        if self.has_open_cursor() {
            return Health::Busy;
        }
        let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1));
        let r = tokio::time::timeout(left, guard.ping()).await;
        drop(guard);
        match r {
            Ok(Ok(())) => Health::Alive,
            Ok(Err(e)) if e.is_connection_lost() => {
                self.abandon(true);
                Health::Dead(e)
            }
            // Not a connection error: the server answered.
            Ok(Err(_)) => Health::Alive,
            Err(_) => {
                self.abandon(true);
                Health::Silent
            }
        }
    }

    /// After a statement or a fetch was stopped by the user: the session must answer
    /// again within `timeout`, else it is given up as lost (its connection went silent,
    /// so the next run would wait the same way; it opens a new session instead). The
    /// result left open is closed first (it cannot be asked through). A session that
    /// holds changes not committed yet is left alone: a reconnect asks about them.
    pub async fn settle_after_cancel(&self, timeout: Duration) {
        if self.is_closed() || self.has_uncommitted_writes() {
            return;
        }
        // The stopped operation lets go of the session.
        let t0 = Instant::now();
        while self.is_busy() && t0.elapsed() < timeout {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if self.is_busy() {
            return;
        }
        if tokio::time::timeout(timeout, self.close_cursor()).await.is_err() {
            tracing::info!("{}: no answer after a cancel; the session is given up", self.label());
            self.abandon(true);
            return;
        }
        if matches!(self.check(timeout).await, Health::Silent) {
            tracing::info!("{}: no answer after a cancel; the session is given up", self.label());
        }
    }

    /// Close the session. It reads as closed at once; a statement still running or a
    /// server that does not answer holds the close up for [`CLOSE_WAIT`] at most, then
    /// the session is given up.
    pub async fn close(&self) {
        self.mark_closed_here();
        if tokio::time::timeout(CLOSE_WAIT, self.close_session()).await.is_err() {
            self.abandon(false);
            // Nothing waits on it any more: the driver's own close is quick now.
            let _ = tokio::time::timeout(CLOSE_WAIT, self.close_session()).await;
        }
    }

    async fn close_session(&self) {
        self.close_cursor().await;
        let mut guard = self.inner.session.lock().await;
        let _ = guard.close().await;
    }

    /// Tests: the connection ends as when the server or the network dropped it
    /// ([`is_lost`](Self::is_lost)).
    #[doc(hidden)]
    pub async fn close_from_server(&self) {
        self.close_cursor().await;
        let mut guard = self.inner.session.lock().await;
        let _ = guard.close().await;
        self.inner.closed.store(true, Ordering::Relaxed);
    }
}

/// Outcome of [`SessionHandle::apply_changes_reading`].
pub struct Applied {
    /// Rows changed.
    pub rows: u64,
    /// Committed (auto-commit), else left in the open transaction.
    pub committed: bool,
    /// Per statement, in order.
    pub results: Vec<StatementResult>,
}

/// What one statement of a save returned.
#[derive(Default)]
pub struct StatementResult {
    pub rows: Option<u64>,
    /// First row of its result (`RETURNING`).
    pub first_row: Option<Row>,
    /// Id generated by the statement (MySQL `LAST_INSERT_ID`).
    pub last_insert_id: Option<u64>,
}

/// Run `sql` to its end, keeping its first returned row and generated id.
/// `sql` rolls the transaction back (`ROLLBACK`, `ABORT`, not `ROLLBACK TO SAVEPOINT`).
fn ends_with_rollback(sql: &str) -> bool {
    let upper = sql.to_ascii_uppercase();
    upper.split(';').map(str::trim).filter(|s| !s.is_empty()).any(|s| (s.starts_with("ROLLBACK") || s.starts_with("ABORT")) && !s.contains(" TO "))
}

async fn run_reading(s: &mut dyn Session, sql: &str, params: &[Value]) -> DbResult<StatementResult> {
    let mut cursor = s.execute(sql, params, &ExecOptions::default()).await?;
    let mut out = StatementResult::default();
    let mut returned = 0u64;
    let mut counted = false;
    while let Some(h) = cursor.next_result().await? {
        match h {
            ResultHeader::Update { rows_affected, last_insert_id, .. } => {
                if let Some(n) = rows_affected {
                    out.rows = Some(out.rows.unwrap_or(0) + n);
                    counted = true;
                }
                out.last_insert_id = last_insert_id.filter(|id| *id > 0).or(out.last_insert_id);
            }
            ResultHeader::Rows(_) => loop {
                let rows = cursor.fetch(1024).await?;
                if rows.is_empty() {
                    break;
                }
                returned += rows.len() as u64;
                if out.first_row.is_none() {
                    out.first_row = rows.into_iter().next();
                }
            },
        }
    }
    cursor.close().await?;
    // `INSERT … RETURNING` reports its rows as a result, not as a count.
    if !counted && returned > 0 {
        out.rows = Some(returned);
    }
    Ok(out)
}

pub struct FetchOutcome {
    pub rows: Vec<Row>,
    pub exhausted: bool,
    pub messages: Vec<ServerMessage>,
}

#[derive(Debug)]
pub enum FetchError {
    CursorClosed,
    Db(DbError),
}

impl From<DbError> for FetchError {
    fn from(e: DbError) -> Self {
        FetchError::Db(e)
    }
}

impl From<FetchError> for DbError {
    fn from(e: FetchError) -> Self {
        match e {
            FetchError::CursorClosed => DbError::internal("The result cursor was closed"),
            FetchError::Db(e) => e,
        }
    }
}

struct BusyGuard<'a>(&'a SessionHandle);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        let mut since = self.0.inner.busy_since.lock();
        if self.0.inner.busy.fetch_sub(1, Ordering::Relaxed) == 1 {
            *since = None;
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A SQLite session that can go silent: from [`Wrapped::silence`] on, statements
    /// and pings wait for ever, as on a connection that died while the computer slept
    /// (half-open: what is sent is accepted, nothing ever comes back). It can also
    /// refuse savepoints (as a failed transaction, or a database without them).
    pub(crate) struct Wrapped {
        pub(crate) inner: Box<dyn ternarygrid_core::Session>,
        pub(crate) no_savepoints: bool,
        pub(crate) silent: Arc<AtomicBool>,
    }

    impl Wrapped {
        fn is_silent(&self) -> bool {
            self.silent.load(Ordering::SeqCst)
        }
    }

    /// A SQLite session on `file` (`:memory:` for its own database).
    pub(crate) async fn sqlite_on(file: &str) -> Box<dyn ternarygrid_core::Session> {
        use ternarygrid_core::Driver;
        let mut cfg = ternarygrid_core::ConnectionConfig::new(DbKind::Sqlite, "test");
        cfg.file_path = Some(file.into());
        ternarygrid_driver_sqlite::SqliteDriver::new().connect(&ternarygrid_core::ConnectParams::from_config(&cfg, None)).await.unwrap()
    }

    /// A session on `file` that goes silent when the flag returned is set.
    pub(crate) async fn silenceable(file: &str, label: &str) -> (SessionHandle, Arc<AtomicBool>) {
        let silent = Arc::new(AtomicBool::new(false));
        let w = Wrapped { inner: sqlite_on(file).await, no_savepoints: false, silent: silent.clone() };
        (SessionHandle::new(Box::new(w), label, None), silent)
    }

    #[async_trait::async_trait]
    impl ternarygrid_core::Session for Wrapped {
        fn kind(&self) -> ternarygrid_core::DbKind {
            self.inner.kind()
        }
        fn dialect(&self) -> std::sync::Arc<dyn ternarygrid_core::Dialect> {
            self.inner.dialect()
        }
        fn server_info(&self) -> &ternarygrid_core::driver::ServerInfo {
            self.inner.server_info()
        }
        fn cancel_handle(&self) -> Option<std::sync::Arc<dyn ternarygrid_core::CancelHandle>> {
            self.inner.cancel_handle()
        }
        async fn ping(&mut self) -> DbResult<()> {
            if self.is_silent() {
                std::future::pending::<()>().await;
            }
            self.inner.ping().await
        }
        async fn close(&mut self) -> DbResult<()> {
            self.inner.close().await
        }
        fn is_closed(&self) -> bool {
            self.inner.is_closed()
        }
        async fn execute(&mut self, sql: &str, params: &[Value], opts: &ExecOptions) -> DbResult<Box<dyn QueryCursor>> {
            if self.no_savepoints && sql.starts_with("SAVEPOINT") {
                return Err(DbError::query("no savepoints here"));
            }
            if self.is_silent() {
                std::future::pending::<()>().await;
            }
            self.inner.execute(sql, params, opts).await
        }
        fn transaction_mode(&self) -> TransactionMode {
            self.inner.transaction_mode()
        }
        async fn set_transaction_mode(&mut self, mode: TransactionMode) -> DbResult<()> {
            self.inner.set_transaction_mode(mode).await
        }
        fn in_transaction(&self) -> bool {
            self.inner.in_transaction()
        }
        async fn commit(&mut self) -> DbResult<()> {
            self.inner.commit().await
        }
        async fn rollback(&mut self) -> DbResult<()> {
            self.inner.rollback().await
        }
        async fn current_database(&mut self) -> DbResult<Option<String>> {
            self.inner.current_database().await
        }
        async fn current_schema(&mut self) -> DbResult<Option<String>> {
            self.inner.current_schema().await
        }
        async fn set_current_schema(&mut self, schema: &str) -> DbResult<()> {
            self.inner.set_current_schema(schema).await
        }
        async fn list_databases(&mut self) -> DbResult<Vec<ternarygrid_core::meta::DatabaseInfo>> {
            self.inner.list_databases().await
        }
        async fn list_schemas(&mut self) -> DbResult<Vec<ternarygrid_core::meta::SchemaInfo>> {
            self.inner.list_schemas().await
        }
        async fn list_objects(&mut self, schema: &ternarygrid_core::SchemaRef, kind: ternarygrid_core::ObjectKind) -> DbResult<Vec<ternarygrid_core::meta::ObjectInfo>> {
            self.inner.list_objects(schema, kind).await
        }
        async fn list_children(&mut self, parent: &ternarygrid_core::ObjectRef, kind: ternarygrid_core::ObjectKind) -> DbResult<Vec<ternarygrid_core::meta::ObjectInfo>> {
            self.inner.list_children(parent, kind).await
        }
        async fn table_definition(&mut self, table: &ternarygrid_core::ObjectRef) -> DbResult<ternarygrid_core::ddl::TableDef> {
            self.inner.table_definition(table).await
        }
        async fn object_ddl(&mut self, obj: &ternarygrid_core::ObjectRef) -> DbResult<String> {
            self.inner.object_ddl(obj).await
        }
        async fn search_metadata(&mut self, search: &ternarygrid_core::meta::MetadataSearch) -> DbResult<Vec<ternarygrid_core::meta::ObjectInfo>> {
            self.inner.search_metadata(search).await
        }
        async fn schema_snapshot(
            &mut self,
            schema: &ternarygrid_core::SchemaRef,
            opts: &ternarygrid_core::ddl::SnapshotOptions,
        ) -> DbResult<ternarygrid_core::ddl::SchemaSnapshot> {
            self.inner.schema_snapshot(schema, opts).await
        }
        async fn explain(&mut self, sql: &str, analyze: bool) -> DbResult<ternarygrid_core::plan::QueryPlan> {
            self.inner.explain(sql, analyze).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    async fn sqlite(label: &str) -> Box<dyn ternarygrid_core::Session> {
        use ternarygrid_core::Driver;
        let mut cfg = ternarygrid_core::ConnectionConfig::new(DbKind::Sqlite, label);
        cfg.file_path = Some(":memory:".into());
        ternarygrid_driver_sqlite::SqliteDriver::new().connect(&ternarygrid_core::ConnectParams::from_config(&cfg, None)).await.unwrap()
    }

    #[tokio::test]
    async fn a_save_without_a_savepoint_is_not_started_in_an_open_transaction() {
        let w = Wrapped { inner: sqlite("sp").await, no_savepoints: true, silent: Arc::new(AtomicBool::new(false)) };
        let h = SessionHandle::new(Box::new(w), "sp", None);
        h.execute_update("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[]).await.unwrap();
        h.set_transaction_mode(TransactionMode::Manual).await.unwrap();
        h.execute_update("INSERT INTO t VALUES (1)", &[]).await.unwrap();
        assert!(h.tx_state().in_transaction);
        // The second statement fails: without the guard the first stayed applied in the transaction.
        let stmts = vec![("INSERT INTO t VALUES (2)".to_string(), Vec::new()), ("INSERT INTO t VALUES (1)".to_string(), Vec::new())];
        let e = h.apply_changes(&stmts).await.expect_err("refused");
        assert!(e.message.contains("savepoint"), "{}", e.full_text());
        let (_, rows) = h.query_all("SELECT id FROM t ORDER BY id", &[]).await.unwrap();
        assert_eq!(rows.len(), 1, "nothing of the batch ran");
        // One statement is all or nothing by itself.
        let (n, committed) = h.apply_changes(&stmts[..1]).await.unwrap();
        assert_eq!((n, committed), (1, false));
    }

    #[tokio::test]
    async fn pending_changes_are_the_writes_and_outlive_the_log() {
        let h = SessionHandle::new(sqlite("pending").await, "pending", None);
        h.execute_update("CREATE TABLE t (id INTEGER)", &[]).await.unwrap();
        h.set_transaction_mode(TransactionMode::Manual).await.unwrap();
        let select = |h: &SessionHandle| {
            let h = h.clone();
            async move {
                h.execute("SELECT 1", &[], &ExecOptions::default()).await.unwrap();
                h.close_cursor().await;
            }
        };
        // Reads alone leave nothing to commit, logged or not.
        select(&h).await;
        select(&h).await;
        assert_eq!(h.tx_state().pending_writes(), 0);
        assert!(!h.has_uncommitted_writes());
        h.execute_update("INSERT INTO t VALUES (1)", &[]).await.unwrap();
        assert_eq!(h.tx_state().pending_writes(), 1);
        // The log keeps the last 2000 statements: the INSERT leaves it, and stays pending.
        for _ in 0..2001 {
            select(&h).await;
        }
        let tx = h.tx_state();
        assert!(tx.log.iter().all(|e| !e.sql.starts_with("INSERT")), "the INSERT left the log");
        assert_eq!(tx.pending_writes(), 1);
        assert!(h.has_uncommitted_writes());
        h.commit().await.unwrap();
        assert_eq!(h.tx_state().pending_writes(), 0);
        assert!(!h.has_uncommitted_writes());
    }

    #[tokio::test]
    async fn busy_while_any_operation_runs() {
        use ternarygrid_core::Driver;
        let mut cfg = ternarygrid_core::ConnectionConfig::new(ternarygrid_core::DbKind::Sqlite, "busy");
        cfg.file_path = Some(":memory:".into());
        let s = ternarygrid_driver_sqlite::SqliteDriver::new().connect(&ternarygrid_core::ConnectParams::from_config(&cfg, None)).await.unwrap();
        let h = SessionHandle::new(s, "busy", None);
        assert!(!h.is_busy());
        // An execute runs while a fetch of the previous result ends.
        let execute = h.busy();
        let fetch = h.busy();
        drop(fetch);
        assert!(h.is_busy() && h.busy_for().is_some(), "still busy with the execute");
        drop(execute);
        assert!(!h.is_busy() && h.busy_for().is_none());
    }

    /// A statement waits for ever (the session went silent) while it is closed, as on
    /// Disconnect or Reconnect: the session reads as closed at once, the statement ends
    /// with a connection error, and the close is bounded.
    #[tokio::test]
    async fn closing_a_silent_session_ends_its_statement() {
        let (h, silent) = silenceable(":memory:", "silent").await;
        silent.store(true, Ordering::SeqCst);
        let running = {
            let h = h.clone();
            tokio::spawn(async move { h.execute("SELECT 1", &[], &ExecOptions::default()).await.map(|_| ()) })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(h.is_busy() && !h.is_closed(), "the statement waits on the session");
        assert!(matches!(h.check(Duration::from_millis(100)).await, Health::Busy), "a busy session is not asked");
        let t0 = Instant::now();
        let closing = {
            let h = h.clone();
            tokio::spawn(async move { h.close().await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(h.is_closed(), "closed for its users at once");
        let err = tokio::time::timeout(Duration::from_secs(15), running).await.expect("the statement ends").unwrap().expect_err("an error");
        assert!(err.is_connection_lost(), "{}", err.full_text());
        tokio::time::timeout(Duration::from_secs(15), closing).await.expect("the close ends").unwrap();
        assert!(t0.elapsed() < CLOSE_WAIT * 3, "bounded: {:?}", t0.elapsed());
        assert!(h.is_closed() && !h.is_lost() && h.is_abandoned(), "closed by TernaryGrid, not lost");
    }

    /// Given up (the connection was found dead): every operation waiting on it ends,
    /// and later ones fail at once instead of waiting too.
    #[tokio::test]
    async fn an_abandoned_session_ends_every_operation() {
        let (h, silent) = silenceable(":memory:", "abandoned").await;
        h.execute_update("CREATE TABLE t (id INTEGER)", &[]).await.unwrap();
        silent.store(true, Ordering::SeqCst);
        let running = {
            let h = h.clone();
            tokio::spawn(async move { h.query_all("SELECT * FROM t", &[]).await.map(|_| ()) })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        h.abandon(true);
        assert!(h.is_closed() && h.is_lost());
        let err = tokio::time::timeout(Duration::from_secs(2), running).await.expect("ends at once").unwrap().expect_err("an error");
        assert!(err.is_connection_lost(), "{}", err.full_text());
        for r in [
            h.execute_update("SELECT 1", &[]).await.map(|_| ()),
            h.commit().await,
            h.rollback().await,
            h.set_transaction_mode(TransactionMode::Auto).await,
            h.execute("SELECT 1", &[], &ExecOptions::default()).await.map(|_| ()),
        ] {
            assert!(r.is_err_and(|e| e.is_connection_lost()));
        }
        assert!(matches!(h.fetch(1, 10).await, Err(FetchError::Db(e)) if e.is_connection_lost()));
        assert!(!h.cursor_open(1).await);
    }

    /// A check of a session that does not answer gives it up as lost; one that answers
    /// stays as it is.
    #[tokio::test]
    async fn a_check_finds_a_silent_session() {
        let (h, silent) = silenceable(":memory:", "check").await;
        assert!(matches!(h.check(Duration::from_secs(5)).await, Health::Alive));
        assert!(!h.is_closed());
        silent.store(true, Ordering::SeqCst);
        let t0 = Instant::now();
        assert!(matches!(h.check(Duration::from_millis(200)).await, Health::Silent));
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert!(h.is_closed() && h.is_lost() && h.is_abandoned());
        assert!(matches!(h.check(Duration::from_millis(200)).await, Health::Dead(_)));
    }

    /// After a cancel the session must answer again, else it is given up (so that the
    /// next run does not wait the same way). Changes not committed keep it.
    #[tokio::test]
    async fn a_session_silent_after_a_cancel_is_given_up_unless_it_holds_changes() {
        let (h, silent) = silenceable(":memory:", "settle").await;
        h.settle_after_cancel(Duration::from_millis(300)).await;
        assert!(!h.is_closed(), "it answers: kept");
        silent.store(true, Ordering::SeqCst);
        h.settle_after_cancel(Duration::from_millis(300)).await;
        assert!(h.is_lost(), "silent: given up");

        let (h, silent) = silenceable(":memory:", "changes").await;
        h.execute_update("CREATE TABLE t (id INTEGER)", &[]).await.unwrap();
        h.set_transaction_mode(TransactionMode::Manual).await.unwrap();
        h.execute_update("INSERT INTO t VALUES (1)", &[]).await.unwrap();
        assert!(h.has_uncommitted_writes());
        silent.store(true, Ordering::SeqCst);
        h.settle_after_cancel(Duration::from_millis(300)).await;
        assert!(!h.is_closed(), "uncommitted changes: left to a reconnect, which asks first");
    }

    #[test]
    fn typed_rollbacks() {
        assert!(ends_with_rollback("rollback"));
        assert!(ends_with_rollback("UPDATE t SET a = 1; ROLLBACK;"));
        assert!(ends_with_rollback("ABORT"));
        assert!(!ends_with_rollback("ROLLBACK TO SAVEPOINT s1"));
        assert!(!ends_with_rollback("COMMIT"));
    }
}
