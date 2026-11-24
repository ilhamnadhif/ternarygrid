//! SQL Server [`Session`].

use crate::connect::{self, MsClient};
use crate::cursor::{self, ExecMode, Shared, Statement, TaskReturn};
use crate::dialect::MsSqlDialect;
use crate::error::map_error;
use crate::sqltext::{self, StmtClass};
use crate::{admin, catalog, plan};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use ternarygrid_core::admin::*;
use ternarygrid_core::ddl::*;
use ternarygrid_core::dialect::Dialect;
use ternarygrid_core::driver::{ServerInfo, parse_version_number};
use ternarygrid_core::meta::*;
use ternarygrid_core::plan::QueryPlan;
use ternarygrid_core::value::ColumnMeta;
use ternarygrid_core::{CancelHandle, ConnectParams, DbError, DbKind, DbResult, ExecOptions, QueryCursor, ResultHeader, Row, ServerMessage, Session, TransactionMode, Value};
use tokio::sync::oneshot;

/// Cancels the statement running on a session (TDS attention).
pub struct SqlServerCancelHandle {
    shared: Arc<Shared>,
}

#[async_trait]
impl CancelHandle for SqlServerCancelHandle {
    async fn cancel(&self) -> DbResult<()> {
        self.shared.cancel_active();
        Ok(())
    }
}

/// Session state left behind when `explain_impl` is cancelled mid-flight or fails cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ExplainDirty {
    Clean = 0,
    Showplan = 1,
    StatisticsOnly = 2,
    AnalyzeNewTx = 3,
    AnalyzeSavepoint = 4,
}

impl ExplainDirty {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Showplan,
            2 => Self::StatisticsOnly,
            3 => Self::AnalyzeNewTx,
            4 => Self::AnalyzeSavepoint,
            _ => Self::Clean,
        }
    }

    fn cleanup_batches(self) -> &'static [&'static str] {
        match self {
            Self::Clean => &[],
            Self::Showplan => &["SET SHOWPLAN_XML OFF"],
            Self::StatisticsOnly => &["SET STATISTICS XML OFF"],
            Self::AnalyzeNewTx => &["IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION", "SET STATISTICS XML OFF"],
            Self::AnalyzeSavepoint => &["IF XACT_STATE() = 1 ROLLBACK TRANSACTION ternarygrid_explain", "SET STATISTICS XML OFF"],
        }
    }
}

/// Drop guard that unconditionally clears `suppress_refresh` even when an
/// `explain_impl` future is dropped/cancelled mid-flight, while leaving
/// `explain_dirty` armed until cleanup succeeds.
struct ExplainGuard {
    suppress_refresh: Arc<AtomicBool>,
    dirty: Arc<AtomicU8>,
}

impl ExplainGuard {
    fn enter(suppress_refresh: Arc<AtomicBool>, dirty: Arc<AtomicU8>) -> Self {
        suppress_refresh.store(true, Ordering::SeqCst);
        Self { suppress_refresh, dirty }
    }

    fn arm(&self, state: ExplainDirty) {
        self.dirty.store(state as u8, Ordering::SeqCst);
    }

    fn resume_refresh(&self) {
        self.suppress_refresh.store(false, Ordering::SeqCst);
    }

    fn disarm(&self) {
        self.dirty.store(ExplainDirty::Clean as u8, Ordering::SeqCst);
    }
}

impl Drop for ExplainGuard {
    fn drop(&mut self) {
        self.suppress_refresh.store(false, Ordering::SeqCst);
    }
}

/// One SQL Server connection.
///
/// The underlying connection is re-established transparently when it had to
/// be discarded (failed cancellation, network error); the current database
/// and transaction mode are restored. A transaction that was open at that
/// moment is lost: the next `commit` reports it.
pub struct SqlServerSession {
    params: ConnectParams,
    client: Option<MsClient>,
    pending: Option<oneshot::Receiver<TaskReturn>>,
    shared: Arc<Shared>,
    /// Id of the open cursor (0: none).
    cursor_open: Arc<AtomicU64>,
    info: ServerInfo,
    dialect: Arc<MsSqlDialect>,
    mode: TransactionMode,
    cancel: Arc<SqlServerCancelHandle>,
    closed: bool,
    lost_transaction: bool,
    /// Warning for the next statement after a transparent reconnect.
    reconnect_notice: Option<ServerMessage>,
    /// Skip the post-statement `@@TRANCOUNT` refresh (while a SHOWPLAN /
    /// STATISTICS XML option is on, it would return a plan).
    suppress_refresh: Arc<AtomicBool>,
    /// Non-clean when a previous `explain_impl` was cancelled or failed cleanup.
    explain_dirty: Arc<AtomicU8>,
    /// `SERVERPROPERTY('EngineEdition')` (5 = Azure SQL Database).
    pub(crate) engine_edition: i32,
    /// The session ran `SET IMPLICIT_TRANSACTIONS` / `SET ANSI_DEFAULTS`: any later statement
    /// may open a transaction (an UPDATE held its locks unseen, and closing the tab rolled it
    /// back without asking), so `@@TRANCOUNT` is read after each one.
    implicit_transactions: bool,
    /// Prefer fell back to an unencrypted connection.
    plain_fallback: bool,
}

struct InitInfo {
    info: ServerInfo,
    database: Option<String>,
    edition: i32,
}

async fn init_client(client: &mut MsClient, manual: bool) -> DbResult<InitInfo> {
    let mut sql = String::from("SET TEXTSIZE 2147483647;\n");
    if manual {
        sql.push_str("SET IMPLICIT_TRANSACTIONS ON;\n");
    }
    sql.push_str(
        "SELECT @@VERSION, CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)), CAST(@@SPID AS int), \
         CAST(SERVERPROPERTY('EngineEdition') AS int), DB_NAME()",
    );
    let row = client
        .simple_query(sql)
        .await
        .map_err(|e| map_error(e, None))?
        .into_row()
        .await
        .map_err(|e| map_error(e, None))?
        .ok_or_else(|| DbError::connection("The server did not return its version"))?;
    let full: Option<&str> = row.try_get(0).map_err(|e| map_error(e, None))?;
    let product_version: Option<&str> = row.try_get(1).map_err(|e| map_error(e, None))?;
    let spid: Option<i32> = row.try_get(2).map_err(|e| map_error(e, None))?;
    let edition: Option<i32> = row.try_get(3).map_err(|e| map_error(e, None))?;
    let db: Option<&str> = row.try_get(4).map_err(|e| map_error(e, None))?;
    let edition = edition.unwrap_or(0);
    let full = full.unwrap_or_default();
    let version_line = full.lines().next().unwrap_or("").trim().to_string();
    let product = match edition {
        5 => "Azure SQL Database",
        8 => "Azure SQL Managed Instance",
        6 | 11 => "Azure Synapse Analytics",
        _ => "Microsoft SQL Server",
    };
    let pv = product_version.unwrap_or_default().to_string();
    Ok(InitInfo {
        info: ServerInfo {
            product: product.to_string(),
            version: if version_line.is_empty() { pv.clone() } else { full.split_whitespace().collect::<Vec<_>>().join(" ") },
            version_number: parse_version_number(&pv),
            session_id: spid.map(|s| s.to_string()),
        },
        database: db.map(str::to_string),
        edition,
    })
}

/// Choose batch or `sp_executesql` for a statement.
pub(crate) fn exec_mode(sql: &str, params: &[Value]) -> ExecMode {
    if !params.is_empty() {
        return ExecMode::Rpc;
    }
    // Plain DML runs through sp_executesql to get per-statement row counts,
    // unless it contains statements whose effect is scoped to the batch.
    if sqltext::classify(sql) == StmtClass::Dml
        && !sqltext::is_scope_sensitive(sql)
        && sqltext::split_batches(sql).len() <= 1
        && !sql.lines().any(|l| l.trim().eq_ignore_ascii_case("GO"))
    {
        ExecMode::Rpc
    } else {
        ExecMode::Batch
    }
}

/// Re-read `@@TRANCOUNT` / `DB_NAME()` after `sql` (besides after any error): in manual
/// mode, inside an open transaction (a procedure may commit it), and when the SQL itself
/// controls transactions (`BEGIN TRAN`, `COMMIT`, `ROLLBACK`, `SAVE TRAN`) or the database.
fn refresh_after(mode: TransactionMode, in_transaction: bool, sql: &str) -> bool {
    mode == TransactionMode::Manual || in_transaction || sqltext::touches_session_state(sql)
}

impl SqlServerSession {
    /// Connect and initialise a session.
    pub async fn connect(params: &ConnectParams) -> DbResult<SqlServerSession> {
        let (mut client, plain_fallback) = connect::open_reporting(params).await?;
        let manual = !params.auto_commit;
        let init = init_client(&mut client, manual).await?;
        let shared = Arc::new(Shared::default());
        shared.set_db(init.database.clone());
        Ok(SqlServerSession {
            params: params.clone(),
            client: Some(client),
            pending: None,
            cancel: Arc::new(SqlServerCancelHandle { shared: shared.clone() }),
            shared,
            cursor_open: Arc::new(AtomicU64::new(0)),
            info: init.info,
            dialect: Arc::new(MsSqlDialect::new()),
            mode: if manual { TransactionMode::Manual } else { TransactionMode::Auto },
            closed: false,
            lost_transaction: false,
            reconnect_notice: None,
            suppress_refresh: Arc::new(AtomicBool::new(false)),
            explain_dirty: Arc::new(AtomicU8::new(ExplainDirty::Clean as u8)),
            engine_edition: init.edition,
            implicit_transactions: false,
            plain_fallback,
        })
    }

    /// Re-open the connection and restore database and transaction mode.
    async fn reconnect(&mut self) -> DbResult<()> {
        let was_in_tx = self.shared.in_transaction.load(Ordering::SeqCst);
        let db = self.shared.db();
        let mut client = connect::open(&self.params).await?;
        let init = init_client(&mut client, self.mode == TransactionMode::Manual).await?;
        if let Some(db) = db.filter(|d| init.database.as_deref() != Some(d.as_str())) {
            let sql = format!("USE {}", crate::dialect::quote(&db));
            let restored = async { client.simple_query(sql).await?.into_results().await }.await;
            match restored {
                Ok(_) => self.shared.set_db(Some(db)),
                Err(e) => {
                    tracing::debug!("could not restore database {db}: {e}");
                    self.shared.set_db(init.database.clone());
                }
            }
        } else {
            self.shared.set_db(init.database.clone());
        }
        self.info = init.info;
        if was_in_tx {
            self.lost_transaction = true;
        }
        self.reconnect_notice = Some(ServerMessage::new(
            "WARNING",
            if was_in_tx {
                "The connection was re-established: the open transaction was rolled back and session state \
                 (temporary tables, SET options) was lost"
            } else {
                "The connection was re-established: session state (temporary tables, SET options) was lost"
            },
        ));
        self.shared.in_transaction.store(false, Ordering::SeqCst);
        self.client = Some(client);
        Ok(())
    }

    /// Get exclusive use of the connection, waiting for a previous
    /// statement task to hand it back and reconnecting if it was lost.
    async fn acquire(&mut self) -> DbResult<MsClient> {
        if self.closed {
            return Err(DbError::connection("The session is closed"));
        }
        if self.pending.is_some() {
            if self.cursor_open.load(Ordering::SeqCst) != 0 {
                return Err(DbError::internal("A result set is still open on this session; close it before running another statement"));
            }
            if let Some(rx) = self.pending.as_mut() {
                let ret = rx.await;
                self.pending = None;
                self.client = ret.ok().and_then(|r| r.client);
            }
        }
        if self.client.is_none() {
            self.reconnect().await?;
        }
        self.client.take().ok_or_else(|| DbError::internal("No connection available"))
    }

    fn start(&mut self, client: MsClient, stmt: Statement) -> Box<dyn QueryCursor> {
        let (cur, rx) = cursor::spawn(client, stmt, self.shared.clone(), self.cursor_open.clone());
        self.pending = Some(rx);
        Box::new(cur)
    }

    /// Execute without checking `explain_dirty` (used by `explain_impl` and its recovery).
    async fn exec_with_raw(&mut self, sql: &str, params: &[Value], mode: Option<ExecMode>, opts: &ExecOptions) -> DbResult<Box<dyn QueryCursor>> {
        let client = self.acquire().await?;
        if sqltext::sets_implicit_transactions(sql) {
            self.implicit_transactions = true;
        }
        let suppress = self.suppress_refresh.load(Ordering::SeqCst);
        let stmt = Statement {
            sql: sql.to_string(),
            params: params.to_vec(),
            mode: mode.unwrap_or_else(|| exec_mode(sql, params)),
            timeout: opts.timeout,
            resolve_sources: opts.resolve_sources,
            refresh_state: !suppress && (self.implicit_transactions || refresh_after(self.mode, self.shared.in_transaction.load(Ordering::SeqCst), sql)),
            refresh_on_error: !suppress,
            tag: sqltext::first_keyword(sql),
            notice: self.reconnect_notice.take(),
            zone: crate::types::Zone::of(self.params.property("time_zone")),
        };
        Ok(self.start(client, stmt))
    }

    async fn results_raw(&mut self, sql: &str, params: &[Value]) -> DbResult<Vec<(Vec<ColumnMeta>, Vec<Row>)>> {
        let mode = if params.is_empty() { Some(ExecMode::Batch) } else { Some(ExecMode::Rpc) };
        let mut c = self.exec_with_raw(sql, params, mode, &ExecOptions::default()).await?;
        let mut out = Vec::new();
        let res = async {
            while let Some(h) = c.next_result().await? {
                if let ResultHeader::Rows(cols) = h {
                    let mut rows = Vec::new();
                    loop {
                        let b = c.fetch(1024).await?;
                        if b.is_empty() {
                            break;
                        }
                        rows.extend(b);
                    }
                    out.push((cols, rows));
                }
            }
            Ok::<(), DbError>(())
        }
        .await;
        c.close().await?;
        res.map(|_| out)
    }

    async fn batch_raw(&mut self, sql: &str) -> DbResult<()> {
        self.results_raw(sql, &[]).await.map(|_| ())
    }

    /// Recover session state if a previous `explain_impl` was cancelled or failed
    /// cleanup. If recovery fails, closes the session so a tainted connection is
    /// never silently reused.
    async fn recover_explain_if_dirty(&mut self) -> DbResult<()> {
        let state = ExplainDirty::from_u8(self.explain_dirty.load(Ordering::SeqCst));
        if state == ExplainDirty::Clean {
            return Ok(());
        }
        let guard = ExplainGuard::enter(self.suppress_refresh.clone(), self.explain_dirty.clone());
        let batches = state.cleanup_batches();
        for (idx, sql) in batches.iter().enumerate() {
            if idx + 1 == batches.len() {
                guard.resume_refresh();
            }
            if let Err(e) = self.batch_raw(sql).await {
                self.closed = true;
                self.client = None;
                self.pending = None;
                return Err(DbError::connection(format!("Failed to recover SQL Server session after interrupted EXPLAIN: {}", e.message)));
            }
        }
        guard.disarm();
        Ok(())
    }

    /// Execute with an explicit mode (internal use).
    pub(crate) async fn exec_with(&mut self, sql: &str, params: &[Value], mode: Option<ExecMode>, opts: &ExecOptions) -> DbResult<Box<dyn QueryCursor>> {
        self.recover_explain_if_dirty().await?;
        self.exec_with_raw(sql, params, mode, opts).await
    }

    /// All result sets of `sql` (batch mode).
    pub(crate) async fn results(&mut self, sql: &str, params: &[Value]) -> DbResult<Vec<(Vec<ColumnMeta>, Vec<Row>)>> {
        self.recover_explain_if_dirty().await?;
        self.results_raw(sql, params).await
    }

    /// Rows of the first result set.
    pub(crate) async fn rows(&mut self, sql: &str, params: &[Value]) -> DbResult<Vec<Row>> {
        Ok(self.results(sql, params).await?.into_iter().next().map(|(_, r)| r).unwrap_or_default())
    }

    /// First value of the first row.
    pub(crate) async fn scalar(&mut self, sql: &str, params: &[Value]) -> DbResult<Value> {
        Ok(self.rows(sql, params).await?.into_iter().next().and_then(|r| r.into_iter().next()).unwrap_or(Value::Null))
    }

    /// Run a batch and discard results.
    pub(crate) async fn batch(&mut self, sql: &str) -> DbResult<()> {
        self.results(sql, &[]).await.map(|_| ())
    }

    /// Connected to Azure SQL Database (no cross-database queries).
    pub(crate) fn is_azure_db(&self) -> bool {
        self.engine_edition == 5
    }

    async fn explain_impl(&mut self, sql: &str, analyze: bool) -> DbResult<QueryPlan> {
        self.recover_explain_if_dirty().await?;
        let (on, off) = if analyze { ("SET STATISTICS XML ON", "SET STATISTICS XML OFF") } else { ("SET SHOWPLAN_XML ON", "SET SHOWPLAN_XML OFF") };
        // STATISTICS XML runs the statement: its changes are rolled back (in a transaction
        // of its own, or to a savepoint of the one open), as EXPLAIN ANALYZE elsewhere.
        let in_tx = analyze && self.scalar("SELECT @@TRANCOUNT", &[]).await.ok().and_then(|v| v.as_i64()).unwrap_or(0) > 0;
        let (begin, undo) = if in_tx {
            ("SAVE TRANSACTION ternarygrid_explain", "IF XACT_STATE() = 1 ROLLBACK TRANSACTION ternarygrid_explain")
        } else {
            ("BEGIN TRANSACTION", "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION")
        };
        let guard = ExplainGuard::enter(self.suppress_refresh.clone(), self.explain_dirty.clone());
        guard.arm(if analyze { ExplainDirty::StatisticsOnly } else { ExplainDirty::Showplan });
        let mut on_res = self.batch_raw(on).await;
        let mut began = false;
        if analyze && on_res.is_ok() {
            guard.arm(if in_tx { ExplainDirty::AnalyzeSavepoint } else { ExplainDirty::AnalyzeNewTx });
            began = true;
            on_res = self.batch_raw(begin).await;
        }
        let res = match on_res {
            Ok(()) => self.results_raw(sql, &[]).await,
            Err(e) => Err(e),
        };
        let undo_res = if began { self.batch_raw(undo).await } else { Ok(()) };
        if began && undo_res.is_ok() {
            guard.arm(ExplainDirty::StatisticsOnly);
        }
        guard.resume_refresh();
        // the OFF batch refreshes the transaction state (STATISTICS XML executes the statement)
        let off_res = self.batch_raw(off).await;
        if undo_res.is_ok() && off_res.is_ok() {
            guard.disarm();
        }
        let results = res?;
        undo_res?;
        off_res?;
        let mut docs = Vec::new();
        for (cols, rows) in results {
            if cols.len() == 1 && cols[0].name.contains("Showplan") {
                for r in rows {
                    if let Some(v) = r.into_iter().next()
                        && !v.is_null()
                    {
                        docs.push(v.to_text().into_owned());
                    }
                }
            }
        }
        if docs.is_empty() {
            return Err(DbError::query("The server returned no execution plan for this statement"));
        }
        plan::build_plan(&docs, analyze)
    }
}

#[async_trait]
impl Session for SqlServerSession {
    fn kind(&self) -> DbKind {
        DbKind::SqlServer
    }

    fn dialect(&self) -> Arc<dyn Dialect> {
        self.dialect.clone()
    }

    fn server_info(&self) -> &ServerInfo {
        &self.info
    }

    fn connect_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        // tiberius adds a CA to the system's roots: it cannot trust that one alone.
        let ssl = &self.params.ssl;
        if ssl.only_custom_ca && connect::encryption_for(ssl.mode).1 && ssl.ca_cert.as_deref().is_some_and(|c| !c.trim().is_empty()) {
            out.push(
                ternarygrid_core::tr("SQL Server trusts the CA file in addition to the system's root certificates: \"only the custom CA\" cannot be enforced for it.").to_string(),
            );
        }
        if self.plain_fallback {
            out.push(ternarygrid_core::tr("The connection is not encrypted: TLS could not be used, and the SSL mode Prefer fell back to a plain connection. Set the SSL mode to Require to refuse such connections.").to_string())
        }
        out
    }

    fn cancel_handle(&self) -> Option<Arc<dyn CancelHandle>> {
        Some(self.cancel.clone())
    }

    async fn ping(&mut self) -> DbResult<()> {
        let v = self.scalar("SELECT 1", &[]).await?;
        if v.as_i64() == Some(1) { Ok(()) } else { Err(DbError::connection("Unexpected ping response")) }
    }

    async fn close(&mut self) -> DbResult<()> {
        if self.closed {
            return Ok(());
        }
        if let Some(rx) = self.pending.take()
            && self.cursor_open.load(Ordering::SeqCst) == 0
            && let Ok(r) = rx.await
        {
            self.client = r.client;
        }
        self.closed = true;
        self.suppress_refresh.store(false, Ordering::SeqCst);
        self.explain_dirty.store(ExplainDirty::Clean as u8, Ordering::SeqCst);
        if let Some(c) = self.client.take() {
            c.close().await.map_err(|e| map_error(e, None))?;
        }
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed || (self.client.is_none() && self.pending.is_none())
    }

    async fn execute(&mut self, sql: &str, params: &[Value], opts: &ExecOptions) -> DbResult<Box<dyn QueryCursor>> {
        self.exec_with(sql, params, None, opts).await
    }

    fn transaction_mode(&self) -> TransactionMode {
        self.mode
    }

    async fn set_transaction_mode(&mut self, mode: TransactionMode) -> DbResult<()> {
        match mode {
            TransactionMode::Manual => self.batch("SET IMPLICIT_TRANSACTIONS ON").await?,
            TransactionMode::Auto => self.batch("WHILE @@TRANCOUNT > 0 COMMIT TRANSACTION;\nSET IMPLICIT_TRANSACTIONS OFF").await?,
        }
        self.mode = mode;
        Ok(())
    }

    fn in_transaction(&self) -> bool {
        self.shared.in_transaction.load(Ordering::SeqCst)
    }

    async fn commit(&mut self) -> DbResult<()> {
        if self.lost_transaction {
            self.lost_transaction = false;
            return Err(DbError::connection("The connection was re-established; the transaction that was open was rolled back by the server")
                .with_hint("Repeat the changes and commit again"));
        }
        self.batch("WHILE @@TRANCOUNT > 0 COMMIT TRANSACTION").await
    }

    async fn rollback(&mut self) -> DbResult<()> {
        self.lost_transaction = false;
        self.batch("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await
    }

    async fn current_database(&mut self) -> DbResult<Option<String>> {
        let v = self.scalar("SELECT DB_NAME()", &[]).await?;
        let db = v.as_str().map(str::to_string);
        self.shared.set_db(db.clone());
        Ok(db)
    }

    async fn current_schema(&mut self) -> DbResult<Option<String>> {
        let v = self.scalar("SELECT SCHEMA_NAME()", &[]).await?;
        Ok(v.as_str().map(str::to_string))
    }

    async fn set_current_schema(&mut self, _schema: &str) -> DbResult<()> {
        Err(DbError::unsupported(
            "SQL Server has no per-session current schema: the default schema belongs to the database user \
             (ALTER USER ... WITH DEFAULT_SCHEMA = ...). Qualify object names with the schema instead.",
        ))
    }

    async fn set_current_database(&mut self, database: &str) -> DbResult<()> {
        self.batch(&format!("USE {}", crate::dialect::quote(database))).await?;
        self.shared.set_db(Some(database.to_string()));
        Ok(())
    }

    async fn list_databases(&mut self) -> DbResult<Vec<DatabaseInfo>> {
        catalog::list_databases(self).await
    }

    async fn list_schemas(&mut self) -> DbResult<Vec<SchemaInfo>> {
        catalog::list_schemas(self, None).await
    }

    async fn list_objects(&mut self, schema: &SchemaRef, kind: ObjectKind) -> DbResult<Vec<ObjectInfo>> {
        catalog::list_objects(self, schema, kind).await
    }

    async fn list_children(&mut self, parent: &ObjectRef, kind: ObjectKind) -> DbResult<Vec<ObjectInfo>> {
        catalog::list_children(self, parent, kind).await
    }

    async fn table_definition(&mut self, table: &ObjectRef) -> DbResult<TableDef> {
        catalog::table_definition(self, table).await
    }

    async fn view_definition(&mut self, view: &ObjectRef) -> DbResult<ViewDef> {
        catalog::view_definition(self, view).await
    }

    async fn routine_definition(&mut self, routine: &ObjectRef) -> DbResult<RoutineDef> {
        catalog::routine_definition(self, routine).await
    }

    async fn sequence_definition(&mut self, sequence: &ObjectRef) -> DbResult<SequenceDef> {
        catalog::sequence_definition(self, sequence).await
    }

    async fn object_ddl(&mut self, obj: &ObjectRef) -> DbResult<String> {
        catalog::object_ddl(self, obj).await
    }

    async fn object_properties(&mut self, obj: &ObjectRef) -> DbResult<Vec<Property>> {
        catalog::object_properties(self, obj).await
    }

    async fn object_dependencies(&mut self, obj: &ObjectRef) -> DbResult<Vec<Dependency>> {
        catalog::object_dependencies(self, obj).await
    }

    async fn object_statistics(&mut self, obj: &ObjectRef) -> DbResult<Vec<Property>> {
        catalog::object_statistics(self, obj).await
    }

    async fn search_metadata(&mut self, search: &MetadataSearch) -> DbResult<Vec<ObjectInfo>> {
        catalog::search_metadata(self, search).await
    }

    async fn schema_snapshot(&mut self, schema: &SchemaRef, opts: &SnapshotOptions) -> DbResult<SchemaSnapshot> {
        catalog::schema_snapshot(self, schema, opts).await
    }

    async fn explain(&mut self, sql: &str, analyze: bool) -> DbResult<QueryPlan> {
        self.explain_impl(sql, analyze).await
    }

    async fn list_sessions(&mut self) -> DbResult<SessionList> {
        admin::list_sessions(self).await
    }

    async fn kill_session(&mut self, id: &str, mode: KillMode) -> DbResult<()> {
        admin::kill_session(self, id, mode).await
    }

    async fn list_locks(&mut self) -> DbResult<LockList> {
        admin::list_locks(self).await
    }

    async fn server_metrics(&mut self) -> DbResult<Vec<Metric>> {
        admin::server_metrics(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(exec_mode("SELECT 1", &[]), ExecMode::Batch);
        assert_eq!(exec_mode("SELECT @P1", &[Value::Int(1)]), ExecMode::Rpc);
        assert_eq!(exec_mode("UPDATE t SET a = 1", &[]), ExecMode::Rpc);
        assert_eq!(exec_mode("SET NOCOUNT ON; UPDATE t SET a = 1", &[]), ExecMode::Batch);
        assert_eq!(exec_mode("INSERT INTO t VALUES (1)\nGO\nINSERT INTO t VALUES (2)", &[]), ExecMode::Batch);
        assert_eq!(exec_mode("CREATE TABLE t (a int)", &[]), ExecMode::Batch);
        assert_eq!(exec_mode("BEGIN TRAN; DELETE FROM t", &[]), ExecMode::Batch);
    }

    #[test]
    fn state_refresh() {
        use TransactionMode::*;
        for sql in ["BEGIN TRAN", "begin transaction t1", "COMMIT", "ROLLBACK TRAN", "SAVE TRANSACTION s", "USE other", "SET XACT_ABORT ON"] {
            assert!(refresh_after(Auto, false, sql), "{sql}");
        }
        assert!(!refresh_after(Auto, false, "UPDATE t SET a = 1"));
        assert!(!refresh_after(Auto, false, "SELECT N'commit' AS [rollback]"));
        // an open explicit transaction can end without transaction SQL (procedure, deadlock)
        assert!(refresh_after(Auto, true, "UPDATE t SET a = 1"));
        assert!(refresh_after(Manual, false, "SELECT 1"));
    }

    #[tokio::test]
    async fn explain_guard_resets_suppress_and_tracks_dirty_on_cancellation() {
        let suppress = Arc::new(AtomicBool::new(false));
        let dirty = Arc::new(AtomicU8::new(ExplainDirty::Clean as u8));

        // Simulate `explain_impl` getting cancelled while suspended in the middle of EXPLAIN ANALYZE.
        let s = suppress.clone();
        let d = dirty.clone();
        let fut = async move {
            let guard = ExplainGuard::enter(s, d);
            guard.arm(ExplainDirty::AnalyzeNewTx);
            std::future::pending::<()>().await;
            guard.disarm();
        };
        let mut fut = Box::pin(fut);
        // Poll once so the future enters ExplainGuard, arms dirty, and suspends.
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        assert!(suppress.load(Ordering::SeqCst));
        assert_eq!(ExplainDirty::from_u8(dirty.load(Ordering::SeqCst)), ExplainDirty::AnalyzeNewTx);

        // Dropping (cancelling) the future resets `suppress_refresh` immediately,
        // while keeping `explain_dirty` armed for recovery before the next query.
        drop(fut);
        assert!(!suppress.load(Ordering::SeqCst));
        let state = ExplainDirty::from_u8(dirty.load(Ordering::SeqCst));
        assert_eq!(state, ExplainDirty::AnalyzeNewTx);
        assert_eq!(state.cleanup_batches(), &["IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION", "SET STATISTICS XML OFF"]);

        // Savepoint variant rolls back only to the explain savepoint.
        assert_eq!(ExplainDirty::AnalyzeSavepoint.cleanup_batches(), &["IF XACT_STATE() = 1 ROLLBACK TRANSACTION ternarygrid_explain", "SET STATISTICS XML OFF"]);
        assert_eq!(ExplainDirty::Showplan.cleanup_batches(), &["SET SHOWPLAN_XML OFF"]);
        assert_eq!(ExplainDirty::StatisticsOnly.cleanup_batches(), &["SET STATISTICS XML OFF"]);

        // Completing recovery disarms `explain_dirty` and leaves `suppress_refresh` false.
        {
            let guard = ExplainGuard::enter(suppress.clone(), dirty.clone());
            assert!(suppress.load(Ordering::SeqCst));
            guard.resume_refresh();
            assert!(!suppress.load(Ordering::SeqCst));
            guard.disarm();
        }
        assert!(!suppress.load(Ordering::SeqCst));
        assert_eq!(ExplainDirty::from_u8(dirty.load(Ordering::SeqCst)), ExplainDirty::Clean);
    }
}
