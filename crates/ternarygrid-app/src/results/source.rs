//! Incremental fetching of one result set into a bounded-memory `RowStore`
//! (FR-D1): first page on execute, "fetch next page", "fetch all", refresh,
//! total row count. When the cursor was closed in between (another statement
//! needed the session) the query is transparently re-executed and the
//! already fetched rows are skipped.

use crate::backend::{Backend, Job, Loadable};
use crate::conn::session::{FetchError, SessionHandle};
use crate::ops::{OpHandle, OpState};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use ternarygrid_core::driver::{ExecOptions, ResultHeader, ServerMessage};
use ternarygrid_core::rowstore::{RowStore, SharedRowStore};
use ternarygrid_core::{ColumnMeta, DbError, DbResult, Dialect, Value};

/// How long a stopped fetch waits for the server's cancel, and a session afterwards
/// for the server's answer.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// Outcome of a fetch job.
pub struct FetchBatch {
    pub added: usize,
    pub exhausted: bool,
    pub token: Option<u64>,
    pub messages: Vec<ServerMessage>,
    /// The columns of the statement when it was (re-)executed: another table after
    /// foreign key navigation, a changed table after a refresh.
    pub columns: Option<Vec<ColumnMeta>>,
    /// Stopped by its operation's Cancel before the end.
    pub cancelled: bool,
}

pub struct ResultSource {
    pub session: SessionHandle,
    pub sql: String,
    pub params: Vec<Value>,
    pub columns: Vec<ColumnMeta>,
    pub store: SharedRowStore,
    pub token: Option<u64>,
    pub exhausted: bool,
    fetch_job: Option<Job<DbResult<FetchBatch>>>,
    /// Operation of the running fetch ("fetch all"); the fetch ends it.
    fetch_op: Option<OpHandle>,
    /// Rows the running (or last) fetch asked for: fetched again on a new session.
    fetch_want: usize,
    /// Stopped while its query ran: the server was asked to cancel it; a fetch that has
    /// not ended by then is stopped here (a connection that went silent never answers).
    stop_deadline: Option<Instant>,
    /// Rows of the cursor's result in the store (saved new rows are not), less the
    /// saved deletions: how far a re-executed query skips to continue.
    server_rows: usize,
    /// The store's length when the running fetch started: what it adds are server rows.
    fetch_store_len: usize,
    pub error: Option<DbError>,
    pub messages: Vec<ServerMessage>,
    pub executed_at: chrono::DateTime<chrono::Local>,
    pub exec_duration: Duration,
    pub fetch_started: Option<Instant>,
    pub total_count: Loadable<u64>,
    /// Incremented whenever rows were appended (grid refresh hint).
    pub generation: u64,
    /// Incremented when the columns changed (requery of another statement).
    pub columns_version: u64,
    pub budget: usize,
    /// The statement may run again to read past a closed cursor (a query, not
    /// `UPDATE … RETURNING` or `CALL`: those would change the data again).
    pub rerun_safe: bool,
}

impl ResultSource {
    /// Wrap a freshly executed cursor whose first header was `Rows(columns)`.
    pub fn new(session: SessionHandle, sql: String, params: Vec<Value>, columns: Vec<ColumnMeta>, token: Option<u64>, budget: usize) -> Self {
        ResultSource {
            session,
            sql,
            params,
            columns,
            store: RowStore::shared(budget),
            token,
            exhausted: token.is_none(),
            fetch_job: None,
            fetch_op: None,
            fetch_want: 0,
            stop_deadline: None,
            server_rows: 0,
            fetch_store_len: 0,
            error: None,
            messages: Vec::new(),
            executed_at: chrono::Local::now(),
            exec_duration: Duration::ZERO,
            fetch_started: None,
            total_count: Loadable::NotLoaded,
            generation: 0,
            columns_version: 0,
            budget,
            rerun_safe: true,
        }
    }

    /// Wrap rows already fetched by the execution engine.
    #[allow(clippy::too_many_arguments)]
    pub fn from_executed(
        session: SessionHandle,
        sql: String,
        params: Vec<Value>,
        columns: Vec<ColumnMeta>,
        store: SharedRowStore,
        token: Option<u64>,
        exhausted: bool,
        budget: usize,
    ) -> Self {
        let mut s = Self::new(session, sql, params, columns, token, budget);
        s.server_rows = store.lock().len();
        s.store = store;
        s.exhausted = exhausted;
        s
    }

    pub fn row_count(&self) -> usize {
        self.store.lock().len()
    }

    pub fn is_fetching(&self) -> bool {
        self.fetch_job.is_some()
    }

    pub fn has_more(&self) -> bool {
        !self.exhausted
    }

    /// Fetch up to `n` more rows (`usize::MAX` = all) in the background.
    pub fn fetch_more(&mut self, backend: &Backend, n: usize, op: Option<OpHandle>) {
        if self.fetch_job.is_some() || self.exhausted {
            return;
        }
        self.error = None;
        self.fetch_started = Some(Instant::now());
        self.fetch_store_len = self.store.lock().len();
        self.fetch_op = op.clone();
        self.fetch_want = n;
        self.stop_deadline = None;
        let session = self.session.clone();
        let store = self.store.clone();
        let sql = self.sql.clone();
        let params = self.params.clone();
        let token = self.token;
        let rerun_safe = self.rerun_safe;
        let skip = self.server_rows;
        self.fetch_job = Some(backend.spawn(async move {
            // The operation ends with the fetch, also when the result is no longer shown.
            let end = OpEnd(op.clone());
            let r = fetch_rows(session, store, sql, params, token, rerun_safe, skip, n, op).await;
            end.done(&r);
            r
        }));
    }

    /// Apply finished fetches.
    pub fn poll(&mut self) {
        self.poll_with(None);
    }

    /// [`poll`](Self::poll), able to give up a session whose stopped query never ended.
    pub fn poll_with(&mut self, backend: Option<&Backend>) {
        if let Some(t) = self.stop_deadline
            && Instant::now() >= t
            && self.fetch_job.as_ref().is_some_and(|j| !j.is_finished())
        {
            self.stop_deadline = None;
            self.cancel_fetch();
            self.error = Some(DbError::query(ternarygrid_core::tr("The query was stopped: the server did not answer the cancel.")));
            if let Some(backend) = backend {
                let s = self.session.clone();
                backend.spawn_detached(async move { s.settle_after_cancel(STOP_WAIT).await });
            }
        }
        if let Some(job) = self.fetch_job.as_mut() {
            if let Some(res) = job.poll() {
                self.fetch_job = None;
                self.stop_deadline = None;
                // The first page of a new run (refresh, server filter): its time is the query's.
                if self.server_rows == 0
                    && self.fetch_store_len == 0
                    && let Some(t) = self.fetch_started
                {
                    self.exec_duration = t.elapsed();
                }
                // Rows the fetch appended, also when it failed half way.
                self.count_fetched();
                match res {
                    Ok(b) => {
                        let mut columns = b.columns;
                        // The same columns without their tables (a driver that did not look them
                        // up again) keep the known ones: the result stays editable.
                        if let Some(cols) = columns.as_mut()
                            && cols.len() == self.columns.len()
                            && cols.iter().all(|c| c.source.is_none())
                            && cols.iter().zip(&self.columns).all(|(a, b)| a.name == b.name && a.type_name == b.type_name)
                        {
                            for (c, old) in cols.iter_mut().zip(&self.columns) {
                                c.source = old.source.clone();
                            }
                        }
                        if let Some(cols) = columns
                            && cols.iter().map(|c| (&c.name, &c.type_name, &c.source)).ne(self.columns.iter().map(|c| (&c.name, &c.type_name, &c.source)))
                        {
                            self.columns = cols;
                            self.columns_version += 1;
                        }
                        self.exhausted = b.exhausted;
                        self.token = b.token;
                        self.messages.extend(b.messages);
                        self.generation += 1;
                        self.fetch_op = None;
                    }
                    Err(e) => {
                        self.fetch_op = None;
                        self.error = Some(e);
                    }
                }
            } else {
                // rows are appended progressively by the job
                self.generation += 1;
            }
        }
        self.total_count.poll();
    }

    /// Stop a running "fetch all" (the cursor stays open); its operation ends as cancelled.
    pub fn cancel_fetch(&mut self) {
        if let Some(mut j) = self.fetch_job.take() {
            j.abort();
            self.count_fetched();
            self.fetch_op = None;
        }
    }

    /// Stop the running fetch (Cancel in the grid). A query that is still
    /// running is cancelled on the server, so the session stays usable and the
    /// fetch ends with the server's "cancelled"; reading more pages of an open
    /// cursor is stopped here.
    ///
    /// Whatever the server does, the fetch ends: one whose query the server does not
    /// cancel within [`STOP_WAIT`] is stopped here, and a session that then does not
    /// answer is given up (the result reads on through a new one).
    pub fn stop(&mut self, backend: &Backend) {
        if self.fetch_job.is_none() {
            return;
        }
        if self.token.is_none() && self.session.can_cancel() {
            let session = self.session.clone();
            backend.spawn_detached(async move {
                let _ = tokio::time::timeout(STOP_WAIT, session.cancel()).await;
            });
            self.stop_deadline = Some(Instant::now() + STOP_WAIT);
            if let Some(ctx) = backend.context() {
                ctx.request_repaint_after(STOP_WAIT);
            }
        } else {
            // A page takes a moment: one still waited for after seconds came from a
            // connection that may have gone silent.
            let slow = self.fetch_started.is_some_and(|t| t.elapsed() >= STOP_WAIT);
            self.cancel_fetch();
            if slow {
                let s = self.session.clone();
                backend.spawn_detached(async move { s.settle_after_cancel(STOP_WAIT).await });
            }
        }
    }

    /// The result's session was replaced (lost, reconnected): read on through `session`.
    /// A fetch still waiting on the old one, or one that failed with it, runs again from
    /// the rows already shown (the query is run again and skips them).
    pub fn rehome(&mut self, backend: &Backend, session: SessionHandle) {
        if self.session.same(&session) {
            return;
        }
        let refetch = self.fetch_job.is_some() || self.error.as_ref().is_some_and(DbError::is_connection_lost);
        self.cancel_fetch();
        self.stop_deadline = None;
        self.session = session;
        // Its cursor was on the old session (a token there may name another cursor here).
        self.token = None;
        if !self.rerun_safe {
            // A statement that writes is not run again behind the user's back.
            self.exhausted = true;
            return;
        }
        if refetch && !self.exhausted {
            self.error = None;
            self.fetch_more(backend, self.fetch_want.max(1), None);
        }
    }

    /// The result is no longer shown: close its cursor on the server now (it holds a
    /// snapshot and locks) instead of when it goes idle.
    pub fn release(&mut self, backend: &Backend) {
        self.cancel_fetch();
        if let Some(t) = self.token.take() {
            let s = self.session.clone();
            backend.spawn_detached(async move { s.close_cursor_token(t).await });
        }
    }

    /// Saved deletions of `n` server rows: a re-executed query no longer returns them.
    pub fn rows_deleted(&mut self, n: usize) {
        self.server_rows = self.server_rows.saturating_sub(n);
    }

    /// Rows of the store the server returned (not rows inserted by a save).
    pub fn server_rows(&self) -> usize {
        self.server_rows
    }

    fn count_fetched(&mut self) {
        let now = self.store.lock().len();
        self.server_rows += now.saturating_sub(self.fetch_store_len);
        self.fetch_store_len = now;
    }

    /// Re-run the query from scratch.
    pub fn refresh(&mut self, backend: &Backend, first_page: usize) {
        self.cancel_fetch();
        let budget = self.budget;
        self.store = RowStore::shared(budget);
        self.server_rows = 0;
        self.token = None;
        self.exhausted = false;
        self.total_count = Loadable::NotLoaded;
        self.executed_at = chrono::Local::now();
        self.fetch_more(backend, first_page, None);
    }

    /// Replace the SQL (server-side filter / sort) and re-run.
    pub fn requery(&mut self, backend: &Backend, sql: String, first_page: usize) {
        self.sql = sql;
        self.refresh(backend, first_page);
    }

    /// `SELECT COUNT(*)` over the query (FR "calculate total row count").
    pub fn count_total(&mut self, backend: &Backend, dialect: Arc<dyn Dialect>) {
        if self.total_count.is_loading() {
            return;
        }
        let sql = dialect.count_query(&self.sql);
        let session = self.session.clone();
        let params = self.params.clone();
        self.total_count = Loadable::Loading(backend.spawn(async move {
            let (_, rows) = session.query_all(&sql, &params).await?;
            rows.first().and_then(|r| r.first()).and_then(Value::as_i64).map(|n| n as u64).ok_or_else(|| DbError::query("COUNT(*) returned no value"))
        }));
    }
}

impl Drop for ResultSource {
    /// A "fetch all" of a result no longer shown stops at its next page (and its
    /// operation ends as cancelled).
    fn drop(&mut self) {
        if let Some(op) = &self.fetch_op {
            op.cancel.cancel();
        }
    }
}

/// Ends the operation of a fetch with its outcome, or as cancelled when the
/// fetch is aborted before.
struct OpEnd(Option<OpHandle>);

impl OpEnd {
    fn done(&self, r: &DbResult<FetchBatch>) {
        if let Some(op) = &self.0 {
            op.finish(match r {
                Ok(b) if b.cancelled => OpState::Cancelled,
                Ok(b) => OpState::Succeeded(ternarygrid_core::trf("{} rows", &[&b.added])),
                Err(e) => OpState::Failed(e.to_string()),
            });
        }
    }
}

impl Drop for OpEnd {
    fn drop(&mut self) {
        if let Some(op) = &self.0 {
            op.finish(OpState::Cancelled);
        }
    }
}

/// Fetch `n` rows, re-executing the query when the cursor was closed.
#[allow(clippy::too_many_arguments)]
async fn fetch_rows(
    session: SessionHandle,
    store: SharedRowStore,
    sql: String,
    params: Vec<Value>,
    mut token: Option<u64>,
    rerun_safe: bool,
    server_rows: usize,
    n: usize,
    op: Option<OpHandle>,
) -> DbResult<FetchBatch> {
    let already = store.lock().len();
    let mut messages = Vec::new();
    let page = 1000.min(n.max(1));
    let mut added = 0usize;
    let mut columns = None;
    // (Re)open the cursor if needed and skip what we already have.
    let reopen = match token {
        Some(t) => !session.cursor_open(t).await,
        None => true,
    };
    // A closed cursor of a statement that writes is not re-executed behind the user's back
    // (an explicit refresh starts without a token and does run it again).
    if reopen && token.is_some() && !rerun_safe {
        let m = ServerMessage::new("WARNING", ternarygrid_core::tr("The cursor of this result was closed; run the statement again to see the remaining rows."));
        return Ok(FetchBatch { added: 0, exhausted: true, token: None, messages: vec![m], columns: None, cancelled: false });
    }
    if reopen {
        // With the columns' tables: without them the result would turn read-only after a
        // refresh, a server-side filter or sort, or a cursor closed while idle.
        let ex = session.execute(&sql, &params, &ExecOptions { resolve_sources: true, ..Default::default() }).await?;
        messages.extend(ex.messages);
        match ex.header {
            Some(ResultHeader::Rows(cols)) => columns = Some(cols),
            _ => return Ok(FetchBatch { added: 0, exhausted: true, token: None, messages, columns: None, cancelled: false }),
        }
        token = Some(ex.token);
        let mut skip = server_rows;
        while skip > 0 {
            let chunk = skip.min(5000);
            match session.fetch(ex.token, chunk).await {
                Ok(out) => {
                    let got = out.rows.len();
                    skip -= got.min(skip);
                    if out.exhausted || got == 0 {
                        return Ok(FetchBatch { added: 0, exhausted: true, token: None, messages, columns, cancelled: false });
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    let Some(tok) = token else { return Ok(FetchBatch { added: 0, exhausted: true, token: None, messages, columns, cancelled: false }) };
    let mut exhausted = false;
    let mut cancelled = false;
    while added < n {
        if let Some(op) = &op
            && op.cancel.is_cancelled()
        {
            cancelled = true;
            break;
        }
        let want = page.min(n - added);
        match session.fetch(tok, want).await {
            Ok(out) => {
                let got = out.rows.len();
                messages.extend(out.messages);
                {
                    let mut s = store.lock();
                    s.extend(out.rows);
                }
                added += got;
                if let Some(op) = &op {
                    op.progress.done.store((already + added) as u64, Ordering::Relaxed);
                }
                if out.exhausted || got < want {
                    exhausted = true;
                    break;
                }
            }
            Err(FetchError::CursorClosed) => {
                return Err(DbError::internal("The cursor was closed while fetching; refresh the result."));
            }
            Err(FetchError::Db(e)) => return Err(e),
        }
    }
    if exhausted {
        session.close_cursor_token(tok).await;
    }
    Ok(FetchBatch { added, exhausted, token: if exhausted { None } else { Some(tok) }, messages, columns, cancelled })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::Operations;

    fn sqlite(backend: &Backend) -> SessionHandle {
        use ternarygrid_core::Driver;
        let mut cfg = ternarygrid_core::ConnectionConfig::new(ternarygrid_core::DbKind::Sqlite, "fetch");
        cfg.file_path = Some(":memory:".into());
        let s = backend.block_on(ternarygrid_driver_sqlite::SqliteDriver::new().connect(&ternarygrid_core::ConnectParams::from_config(&cfg, None))).unwrap();
        SessionHandle::new(s, "fetch", None)
    }

    const MANY: &str = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000000) SELECT i FROM n";

    fn wait(ops: &mut Operations, what: &str) {
        let t0 = Instant::now();
        while ops.running() > 0 {
            assert!(t0.elapsed() < Duration::from_secs(20), "{what}: the operation never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
        ops.collect();
    }

    /// A database file with the numbers 1..=`n` in `t`.
    fn numbered(backend: &Backend, dir: &std::path::Path, n: usize) -> String {
        let file = dir.join("n.db").display().to_string();
        let s = SessionHandle::new(backend.block_on(crate::conn::session::testing::sqlite_on(&file)), "setup", None);
        backend.block_on(s.execute_update("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])).unwrap();
        let fill = format!("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {n}) INSERT INTO t SELECT i FROM n");
        backend.block_on(s.execute_update(&fill, &[])).unwrap();
        backend.block_on(s.close());
        file
    }

    fn settle(src: &mut ResultSource, backend: &Backend, what: &str) {
        let t0 = Instant::now();
        while src.is_fetching() {
            assert!(t0.elapsed() < Duration::from_secs(20), "{what}: the fetch never ended");
            src.poll_with(Some(backend));
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn ids(src: &ResultSource) -> Vec<i64> {
        src.store.lock().column_values(0).iter().map(|v| v.as_i64().unwrap()).collect()
    }

    /// Stop on a query whose session went silent (no cancel ever answered): the fetch
    /// ends anyway, and the session is given up so that the next try opens a new one.
    #[test]
    fn stopping_a_query_that_never_answers_ends_it() {
        let backend = Backend::new().unwrap();
        let (session, silent) = backend.block_on(crate::conn::session::testing::silenceable(":memory:", "silent"));
        silent.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut src = ResultSource::new(session.clone(), "SELECT 1".into(), Vec::new(), Vec::new(), None, 1 << 20);
        src.exhausted = false;
        src.fetch_more(&backend, 100, None);
        std::thread::sleep(Duration::from_millis(50));
        assert!(src.is_fetching());
        let t0 = Instant::now();
        src.stop(&backend);
        settle(&mut src, &backend, "stop");
        assert!(t0.elapsed() < STOP_WAIT * 2, "{:?}", t0.elapsed());
        assert!(src.error.is_some(), "says it was stopped");
        let t0 = Instant::now();
        while !session.is_lost() {
            assert!(t0.elapsed() < Duration::from_secs(20), "the silent session is given up");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The result's session was replaced (lost, reconnected): a fetch that waited on the
    /// old one reads on through the new one, and later pages continue where the rows end.
    #[test]
    fn a_result_reads_on_through_a_new_session() {
        let backend = Backend::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = numbered(&backend, dir.path(), 1000);
        let (old, silent) = backend.block_on(crate::conn::session::testing::silenceable(&file, "old"));
        let mut src = ResultSource::new(old.clone(), "SELECT id FROM t ORDER BY id".into(), Vec::new(), Vec::new(), None, 1 << 20);
        src.exhausted = false;
        src.fetch_more(&backend, 100, None);
        settle(&mut src, &backend, "first page");
        assert_eq!(ids(&src), (1..=100).collect::<Vec<_>>());
        // The old session went silent while the next page was read (its query runs
        // again: SQLite closes a cursor left unread at once).
        silent.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = src.token {
            backend.block_on(old.close_cursor_token(t));
        }
        src.fetch_more(&backend, 100, None);
        std::thread::sleep(Duration::from_millis(50));
        assert!(src.is_fetching(), "waits on the silent session");
        old.abandon(true);
        let new = SessionHandle::new(backend.block_on(crate::conn::session::testing::sqlite_on(&file)), "new", None);
        src.rehome(&backend, new.clone());
        assert!(src.session.same(&new));
        settle(&mut src, &backend, "the page again on the new session");
        assert!(src.error.is_none(), "{:?}", src.error);
        assert_eq!(ids(&src), (1..=200).collect::<Vec<_>>(), "no row twice, none missing");
        src.fetch_more(&backend, 100, None);
        settle(&mut src, &backend, "the next page");
        assert_eq!(ids(&src), (1..=300).collect::<Vec<_>>());
    }

    #[test]
    fn a_fetch_all_ends_its_operation() {
        let backend = Backend::new().unwrap();
        let session = sqlite(&backend);
        let mut ops = Operations::default();
        // Cancelled from the operations panel: cancelled, not succeeded.
        let mut src = ResultSource::new(session.clone(), MANY.into(), Vec::new(), Vec::new(), None, 1 << 20);
        src.exhausted = false;
        let op = ops.start("all");
        let id = op.id;
        src.fetch_more(&backend, usize::MAX, Some(op.clone()));
        std::thread::sleep(Duration::from_millis(50));
        op.cancel.cancel();
        wait(&mut ops, "cancel");
        assert!(matches!(ops.state(id), Some(OpState::Cancelled)), "{:?}", ops.state(id));
        // The result is no longer shown (its panel dropped) while fetching: the fetch stops
        // and the operation ends.
        let mut src = ResultSource::new(session, MANY.into(), Vec::new(), Vec::new(), None, 1 << 20);
        src.exhausted = false;
        let op = ops.start("all");
        let id = op.id;
        src.fetch_more(&backend, usize::MAX, Some(op));
        std::thread::sleep(Duration::from_millis(50));
        drop(src);
        wait(&mut ops, "drop");
        assert!(matches!(ops.state(id), Some(OpState::Cancelled)), "{:?}", ops.state(id));
    }
}
