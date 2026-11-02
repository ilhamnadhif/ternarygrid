//! Maintenance operations offered by the dialect (VACUUM, ANALYZE, REINDEX,
//! OPTIMIZE TABLE ..., TRUNCATE): option checkboxes, SQL preview and execution
//! on a dedicated session in auto-commit mode.

use super::{SessionState, ToolSession};
use crate::backend::{Job, Repainter};
use crate::command::AppCommand;
use crate::conn::session::SessionHandle;
use crate::cx::AppContext;
use crate::ops::{OpHandle, OpProgress, OpState};
use crate::sqllang::SqlLanguage;
use crate::tabs::TabView;
use crate::tabs::dbtools::look;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use ternarygrid_core::admin::{MaintenanceOp, TabularData};
use ternarygrid_core::driver::{ExecOptions, ResultHeader, ServerMessage, TransactionMode};
use ternarygrid_core::meta::{ObjectKind, ObjectRef};
use ternarygrid_core::{DbResult, Dialect, tr, trf};
use ternarygrid_ui::{kit, parts};
use tokio_util::sync::CancellationToken;

/// Rows kept per result set.
const MAX_ROWS: usize = 10_000;

/// Outcome of one maintenance statement.
#[derive(Debug, Clone, PartialEq)]
pub struct StatementResult {
    pub sql: String,
    /// Error text of a failed statement.
    pub error: Option<String>,
    pub affected: Option<u64>,
    /// Command tag reported by the server (`VACUUM`, `ANALYZE` ...).
    pub tag: Option<String>,
    /// Result rows (MySQL returns a status table for ANALYZE / OPTIMIZE / CHECK).
    pub rows: Option<TabularData>,
    /// Notices and warnings.
    pub messages: Vec<String>,
    pub duration: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Info,
    Notice,
    Ok,
    Error,
}

#[derive(Default)]
struct Shared {
    log: Vec<(Level, String)>,
    results: Vec<StatementResult>,
}

struct MaintRun {
    job: Option<Job<()>>,
    shared: Arc<Mutex<Shared>>,
    cancel: CancellationToken,
    op: OpHandle,
    session: SessionHandle,
    started: Instant,
    ended: Option<Instant>,
}

/// Maintenance of tables, indexes, materialized views or a whole database.
pub struct MaintenanceTab {
    /// Finished successfully: the pop-up closes.
    done: bool,
    conn: String,
    objects: Vec<ObjectRef>,
    op: Option<String>,
    options: BTreeMap<String, bool>,
    session: ToolSession,
    run: Option<MaintRun>,
    /// Run requested; starts as soon as the session is open.
    pending: bool,
    error: Option<String>,
    /// Highlighting of the statements (the connection's dialect).
    lang: Option<SqlLanguage>,
}

fn message_text(m: &ServerMessage) -> String {
    let mut s = format!("{}: {}", m.severity, m.message);
    if let Some(d) = &m.detail {
        s.push_str(&format!(" ({d})"));
    }
    s
}

/// Execute one statement, reading every result.
async fn execute_one(session: &SessionHandle, sql: &str) -> DbResult<StatementResult> {
    let started = Instant::now();
    let mut out = StatementResult { sql: sql.to_string(), error: None, affected: None, tag: None, rows: None, messages: Vec::new(), duration: Duration::ZERO };
    let mut guard = session.lock().await;
    let mut cursor = guard.execute(sql, &[], &ExecOptions::default()).await?;
    let read: DbResult<()> = async {
        while let Some(header) = cursor.next_result().await? {
            out.messages.extend(cursor.take_messages().iter().map(message_text));
            match header {
                ResultHeader::Rows(cols) => {
                    let names: Vec<String> = cols.into_iter().map(|c| c.name).collect();
                    let mut data = match out.rows.take() {
                        Some(d) if d.columns == names => d,
                        _ => TabularData::new(names),
                    };
                    loop {
                        let batch = cursor.fetch(512).await?;
                        if batch.is_empty() {
                            break;
                        }
                        let room = MAX_ROWS.saturating_sub(data.rows.len());
                        data.rows.extend(batch.into_iter().take(room));
                    }
                    out.rows = Some(data);
                }
                ResultHeader::Update { rows_affected, tag, .. } => {
                    if let Some(n) = rows_affected {
                        out.affected = Some(out.affected.unwrap_or(0) + n);
                    }
                    if tag.is_some() {
                        out.tag = tag;
                    }
                }
            }
        }
        out.messages.extend(cursor.take_messages().iter().map(message_text));
        Ok(())
    }
    .await;
    let _ = cursor.close().await;
    drop(guard);
    read?;
    out.duration = started.elapsed();
    Ok(out)
}

async fn run_all(
    session: SessionHandle,
    statements: Vec<String>,
    needs_autocommit: bool,
    shared: Arc<Mutex<Shared>>,
    cancel: CancellationToken,
    repaint: Repainter,
    progress: Arc<OpProgress>,
) {
    let push = |level: Level, text: String| {
        shared.lock().log.push((level, text));
        repaint.request();
    };
    if session.tx_state().mode == TransactionMode::Manual {
        match session.set_transaction_mode(TransactionMode::Auto).await {
            Ok(()) => push(
                Level::Info,
                if needs_autocommit {
                    tr("Auto-commit switched on: this operation cannot run inside a transaction.").to_string()
                } else {
                    tr("Auto-commit switched on for this session.").to_string()
                },
            ),
            Err(e) => {
                push(Level::Error, e.full_text());
                return;
            }
        }
    }
    let total = statements.len();
    for (i, sql) in statements.into_iter().enumerate() {
        if cancel.is_cancelled() {
            push(Level::Error, tr("Cancelled").to_string());
            break;
        }
        progress.set(i as u64, Some(total as u64));
        progress.set_message(sql.chars().take(120).collect::<String>());
        push(Level::Info, format!("▶ {sql}"));
        let started = Instant::now();
        match execute_one(&session, &sql).await {
            Ok(r) => {
                for m in &r.messages {
                    push(Level::Notice, m.clone());
                }
                let mut done = trf("Done in {}", &[&crate::widgets::format_duration(r.duration)]);
                if let Some(t) = &r.tag {
                    done = format!("{t} — {done}");
                }
                if let Some(n) = r.affected.filter(|n| *n > 0) {
                    done.push_str(&format!(" — {}", trf("{} row(s)", &[&n])));
                }
                push(Level::Ok, done);
                shared.lock().results.push(r);
            }
            Err(e) => {
                let text = e.full_text();
                push(Level::Error, text.clone());
                shared.lock().results.push(StatementResult { sql, error: Some(text), affected: None, tag: None, rows: None, messages: Vec::new(), duration: started.elapsed() });
                if cancel.is_cancelled() {
                    break;
                }
            }
        }
    }
    progress.set(total as u64, Some(total as u64));
}

impl MaintenanceTab {
    pub fn new(conn: String, objects: Vec<ObjectRef>) -> Self {
        let database = objects.iter().find_map(|o| o.database.clone());
        MaintenanceTab { done: false, conn, objects, op: None, options: BTreeMap::new(), session: ToolSession::new(database), run: None, pending: false, error: None, lang: None }
    }

    /// Opened on the operation `id` (Truncate… in the navigator's menu).
    pub fn with_op(mut self, id: Option<String>) -> Self {
        self.op = id;
        self
    }

    fn is_truncate(&self) -> bool {
        self.op.as_deref() == Some(MaintenanceOp::TRUNCATE)
    }

    /// Dialect of the connection (also when it is not connected).
    fn dialect(&self, cx: &AppContext) -> Option<Arc<dyn Dialect>> {
        cx.conns.live(&self.conn).map(|l| l.dialect.clone()).or_else(|| cx.conns.config(&self.conn).and_then(|c| cx.conns.registry.get(c.kind)).map(|d| d.dialect()))
    }

    /// Objects the statements apply to: the selected objects, or the
    /// current database when none were given.
    fn targets(&self, cx: &AppContext) -> Vec<ObjectRef> {
        if !self.objects.is_empty() {
            return self.objects.clone();
        }
        let db = cx.conns.live(&self.conn).and_then(|l| l.current_database.clone()).or_else(|| cx.conns.config(&self.conn).and_then(|c| c.database.clone())).unwrap_or_default();
        vec![ObjectRef::new(ObjectKind::Database, None, db)]
    }

    fn ops(&self, cx: &AppContext) -> Vec<MaintenanceOp> {
        let Some(d) = self.dialect(cx) else { return Vec::new() };
        let targets = self.targets(cx);
        let mut ops = d.maintenance_ops();
        ops.push(MaintenanceOp::truncate(d.as_ref()));
        ops.into_iter().filter(|op| targets.iter().all(|t| op.applies_to.contains(&t.kind))).collect()
    }

    fn current_op(&self, cx: &AppContext) -> Option<MaintenanceOp> {
        let ops = self.ops(cx);
        let id = self.op.as_deref()?;
        ops.into_iter().find(|o| o.id == id)
    }

    /// Ids of the operations applicable to the objects.
    pub fn available_ops(&self, cx: &AppContext) -> Vec<String> {
        self.ops(cx).iter().map(|o| o.id.to_string()).collect()
    }

    /// Choose an operation; its options get their defaults.
    pub fn select_op(&mut self, cx: &AppContext, id: &str) -> bool {
        let Some(op) = self.ops(cx).into_iter().find(|o| o.id == id) else { return false };
        self.op = Some(op.id.to_string());
        self.options = op.options.iter().map(|(k, _, d)| (k.to_string(), *d)).collect();
        true
    }

    /// Set an option of the selected operation.
    pub fn set_option(&mut self, key: &str, on: bool) {
        self.options.insert(key.to_string(), on);
    }

    /// Statements that will run.
    pub fn preview_sql(&self, cx: &AppContext) -> Vec<String> {
        match (self.dialect(cx), self.op.as_deref()) {
            (Some(d), Some(MaintenanceOp::TRUNCATE)) => d.truncate_sql(&self.targets(cx), &self.options),
            (Some(d), Some(op)) => d.maintenance_sql(op, &self.targets(cx), &self.options),
            _ => Vec::new(),
        }
    }

    /// Run the selected operation (starts once the session is open).
    pub fn start(&mut self, cx: &mut AppContext) {
        if self.is_running() {
            return;
        }
        self.error = None;
        let statements = self.preview_sql(cx);
        if statements.is_empty() {
            self.error = Some(tr("Nothing to run for the selected objects.").to_string());
            return;
        }
        if let (Some(cfg), Some(d)) = (cx.conns.config(&self.conn), self.dialect(cx))
            && cfg.read_only
            && statements.iter().any(|s| ternarygrid_sql::classify(s, d.syntax()).is_write)
        {
            self.error = Some(trf("The connection \"{}\" is read-only.", &[&cfg.name]));
            return;
        }
        self.pending = true;
        self.try_start(cx);
    }

    fn try_start(&mut self, cx: &mut AppContext) {
        if !self.pending {
            return;
        }
        let session = match self.session.state(cx, &self.conn, "maintenance") {
            SessionState::Ready(s) => s,
            SessionState::Opening => return,
            SessionState::Closed => {
                self.pending = false;
                self.error = Some(tr("The connection is closed.").to_string());
                return;
            }
            SessionState::Failed(e) => {
                self.pending = false;
                self.error = Some(e);
                return;
            }
        };
        self.pending = false;
        let Some(op) = self.current_op(cx) else { return };
        let statements = self.preview_sql(cx);
        let title = trf("{} on {}", &[&tr(op.label), &cx.conns.config(&self.conn).map(|c| c.name.clone()).unwrap_or_default()]);
        let handle = cx.ops.start(title);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let cancel = handle.cancel.clone();
        let job = cx.backend.spawn(run_all(session.clone(), statements, op.needs_autocommit, shared.clone(), cancel.clone(), cx.backend.repainter(), handle.progress.clone()));
        self.run = Some(MaintRun { job: Some(job), shared, cancel, op: handle, session, started: Instant::now(), ended: None });
    }

    /// A run is requested or in progress.
    pub fn is_running(&self) -> bool {
        self.pending || self.run.as_ref().is_some_and(|r| r.job.is_some())
    }

    /// Results of the last run.
    pub fn results(&self) -> Vec<StatementResult> {
        self.run.as_ref().map(|r| r.shared.lock().results.clone()).unwrap_or_default()
    }

    /// Log of the last run.
    pub fn log_text(&self) -> String {
        self.run.as_ref().map(|r| r.shared.lock().log.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default()
    }

    /// Error shown above the log (session problems, nothing to run).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Cancel the running operation.
    pub fn cancel(&mut self, cx: &AppContext) {
        self.pending = false;
        if let Some(r) = &self.run
            && r.job.is_some()
        {
            r.cancel.cancel();
            let s = r.session.clone();
            cx.backend.spawn_detached(async move {
                let _ = s.cancel().await;
            });
        }
    }

    fn poll(&mut self, cx: &mut AppContext) {
        self.try_start(cx);
        // All statements ran and nothing to read (no status tables): the pop-up closes.
        let mut quiet_success: Option<usize> = None;
        if let Some(run) = self.run.as_mut()
            && run.job.as_mut().and_then(|j| j.poll()).is_some()
        {
            run.job = None;
            run.ended = Some(Instant::now());
            let shared = run.shared.lock();
            let failed = shared.results.iter().filter(|r| r.error.is_some()).count();
            let state = if run.cancel.is_cancelled() {
                OpState::Cancelled
            } else if failed > 0 || shared.results.is_empty() {
                OpState::Failed(trf("{} statement(s) failed", &[&failed]))
            } else {
                if shared.results.iter().all(|r| r.rows.is_none()) {
                    quiet_success = Some(shared.results.len());
                }
                OpState::Succeeded(trf("{} statement(s) executed", &[&shared.results.len()]))
            };
            drop(shared);
            cx.ops.finish(run.op.id, state);
        }
        if let Some(n) = quiet_success {
            cx.toasts.success(trf("{} statement(s) executed", &[&n]));
            self.done = true;
        }
    }

    /// "Objects:" and a chip for each target.
    fn objects_ui(&self, ui: &mut Ui, cx: &AppContext) {
        let targets = self.targets(cx);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            look::muted(ui, tr("Objects:"));
            for t in targets.iter().take(20) {
                let label = if t.kind == ObjectKind::Database { if t.name.is_empty() { tr("(current database)").to_string() } else { t.name.clone() } } else { t.display_path() };
                look::chip(ui, &label);
            }
            if targets.len() > 20 {
                look::faint(ui, &trf("… {} more", &[&(targets.len() - 20)]));
            }
        });
    }

    fn ops_ui(&mut self, ui: &mut Ui, cx: &AppContext) {
        let ops = self.ops(cx);
        if ops.is_empty() {
            look::muted(ui, tr("No maintenance operations are available for the selected objects."));
            return;
        }
        match ops.iter().find(|o| Some(o.id) == self.op.as_deref()) {
            None => {
                let first = ops[0].id;
                self.select_op(cx, first);
            }
            // Opened on it: its options' defaults.
            Some(op) if self.options.is_empty() && !op.options.is_empty() => {
                let id = op.id;
                self.select_op(cx, id);
            }
            Some(_) => {}
        }
        ui.spacing_mut().item_spacing.y = 10.0;
        look::title(ui, tr("Operation"));
        let mut chosen = None;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            for op in &ops {
                let selected = self.op.as_deref() == Some(op.id);
                if look::radio(ui, selected, tr(op.label)).on_hover_text(tr(op.description)).clicked() && !selected {
                    chosen = Some(op.id);
                }
            }
        });
        if let Some(id) = chosen {
            self.select_op(cx, id);
        }
        let Some(op) = ops.iter().find(|o| Some(o.id) == self.op.as_deref()) else { return };
        look::faint(ui, tr(op.description));
        if !op.options.is_empty() {
            ui.add_space(4.0);
            look::title(ui, tr("Options"));
            self.options_ui(ui, op);
        }
    }

    /// The check boxes of the operation's options.
    fn options_ui(&mut self, ui: &mut Ui, op: &MaintenanceOp) {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            for (key, label, default) in op.options {
                let mut on = self.options.get(*key).copied().unwrap_or(*default);
                if look::checkbox(ui, &mut on, tr(label)).changed() {
                    self.options.insert(key.to_string(), on);
                }
            }
        });
    }

    /// The statements that will run, highlighted (`gutter`: with line numbers).
    fn sql_ui(&mut self, ui: &mut Ui, cx: &AppContext, sql: &[String], gutter: bool) {
        let text = if sql.is_empty() { tr("-- nothing to run").to_string() } else { sql.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n") };
        if self.lang.is_none()
            && let Some(d) = self.dialect(cx)
        {
            self.lang = Some(SqlLanguage::new(d));
        }
        look::code_block(ui, self.lang.as_ref(), &text, gutter);
    }

    /// The result line of the last run (at the right of Run).
    fn status_ui(&self, ui: &mut Ui) {
        let Some(run) = &self.run else { return };
        let p = ternarygrid_ui::pal(ui);
        let results = run.shared.lock().results.clone();
        let elapsed = crate::widgets::format_duration(run.ended.unwrap_or_else(Instant::now).duration_since(run.started));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if run.job.is_some() {
                ui.label(RichText::new(trf("Running… {}", &[&elapsed])).size(12.5).color(p.text_muted));
                ui.spinner();
                ui.ctx().request_repaint_after(Duration::from_millis(250));
            } else {
                let failed = results.iter().filter(|r| r.error.is_some()).count();
                if failed == 0 && !results.is_empty() {
                    ui.label(RichText::new(format!("{} · {elapsed}", trf("{} statement(s) executed", &[&results.len()]))).size(12.5).color(p.ok));
                } else {
                    ui.label(RichText::new(format!("{} · {elapsed}", trf("{} statement(s) failed", &[&failed]))).size(12.5).color(p.err));
                }
            }
        });
    }

    /// The log and the result tables of the last run.
    fn log_ui(&self, ui: &mut Ui, cx: &AppContext) {
        let Some(run) = &self.run else { return };
        let p = ternarygrid_ui::pal(ui);
        let (log, results) = {
            let s = run.shared.lock();
            (s.log.clone(), s.results.clone())
        };
        let lines: Vec<(String, egui::Color32)> = log
            .iter()
            .map(|(level, line)| {
                let c = match level {
                    Level::Info => p.text_muted,
                    Level::Notice => p.text_faint,
                    Level::Ok => p.ok,
                    Level::Error => p.err,
                };
                (line.clone(), c)
            })
            .collect();
        let h = (lines.len() as f32 * 18.0 + 4.0).clamp(54.0, 200.0);
        look::log_box(ui, ("maint_log", &self.conn), &lines, Some(h));
        let fmt = cx.settings.results.format.clone();
        for (i, r) in results.iter().enumerate() {
            let Some(data) = &r.rows else { continue };
            ui.label(RichText::new(r.sql.as_str()).font(look::mono(12.0)).color(p.text));
            let headers = look::headers(&data.columns.iter().map(String::as_str).collect::<Vec<_>>());
            let rows: Vec<Vec<look::Cell>> = data.rows.iter().map(|row| row.iter().map(|v| look::Cell::new(v.display(&fmt).into_owned())).collect()).collect();
            let h = (rows.len() as f32 + 1.5) * (ui.text_style_height(&egui::TextStyle::Body) + 13.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), h.min(300.0)), |ui| {
                look::table(ui, ("maint_rows", &self.conn, i), &headers, &rows, None);
            });
        }
    }

    fn connect_note(&self, ui: &mut Ui, cx: &mut AppContext) {
        if !cx.conns.is_connected(&self.conn) {
            look::note_with(ui, parts::Tone::Info, None, tr("The connection is closed."), |ui| {
                if kit::button_ex(ui, kit::Kind::Secondary, Some(icon::PLUGS_CONNECTED), tr("Connect"), true).clicked() {
                    cx.push(AppCommand::Connect(self.conn.clone()));
                }
            });
        }
    }

    fn open_in_editor(&self, cx: &mut AppContext, sql: &[String]) {
        let text = sql.iter().map(|s| format!("{s};\n")).collect::<String>();
        cx.push(AppCommand::NewSqlEditor { connection: Some(self.conn.clone()), schema: None, text: Some(text) });
    }

    fn run_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let sql = self.preview_sql(cx);
        ui.spacing_mut().item_spacing.y = 10.0;
        let mut open = false;
        parts::section_header(ui, tr("SQL"), |ui| {
            if !sql.is_empty() && kit::ghost(ui, Some(icon::FILE_SQL), tr("Open in SQL editor")).clicked() {
                open = true;
            }
        });
        if open {
            self.open_in_editor(cx, &sql);
        }
        self.sql_ui(ui, cx, &sql, true);
        if self.current_op(cx).is_some_and(|o| o.needs_autocommit) {
            look::faint(ui, tr("Runs outside of a transaction (auto-commit)."));
        }
        let running = self.is_running();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if ui.add_enabled_ui(!running && !sql.is_empty(), |ui| kit::primary_icon(ui, icon::PLAY, tr("Run"))).inner.clicked() {
                self.start(cx);
            }
            if running && kit::secondary(ui, Some(icon::STOP), tr("Cancel")).clicked() {
                self.cancel(cx);
            }
            if self.pending {
                parts::spinner_label(ui, tr("Opening a session…"));
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
            self.status_ui(ui);
        });
        self.connect_note(ui, cx);
        if let Some(e) = &self.error {
            look::error(ui, e);
        }
        self.log_ui(ui, cx);
    }

    /// Truncate: the options, the statement, the warning; its buttons are in the footer.
    fn truncate_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        self.objects_ui(ui, cx);
        let ops = self.ops(cx);
        match ops.iter().find(|o| o.id == MaintenanceOp::TRUNCATE) {
            Some(op) => {
                if self.options.is_empty() && !op.options.is_empty() {
                    self.select_op(cx, MaintenanceOp::TRUNCATE);
                }
                ui.add_enabled_ui(!self.is_running(), |ui| self.options_ui(ui, op));
            }
            None => {
                look::muted(ui, tr("No maintenance operations are available for the selected objects."));
            }
        }
        let sql = self.preview_sql(cx);
        self.sql_ui(ui, cx, &sql, false);
        look::note(ui, parts::Tone::Error, None, tr("Every row of the table(s) is deleted at once, without a transaction to roll back."));
        self.connect_note(ui, cx);
        if self.pending {
            parts::spinner_label(ui, tr("Opening a session…"));
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        if self.run.is_some() {
            ui.horizontal(|ui| self.status_ui(ui));
        }
        if let Some(e) = &self.error {
            look::error(ui, e);
        }
        self.log_ui(ui, cx);
    }

    fn truncate_footer(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let sql = self.preview_sql(cx);
        let running = self.is_running();
        if ui.add_enabled_ui(!sql.is_empty(), |ui| kit::button_ex(ui, kit::Kind::Ghost, Some(icon::FILE_SQL), tr("Open in SQL editor"), false)).inner.clicked() {
            self.open_in_editor(cx, &sql);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add_enabled_ui(!running && !sql.is_empty(), |ui| kit::button_ex(ui, kit::Kind::DangerFilled, Some(icon::TRASH), tr("Truncate"), false)).inner.clicked() {
                self.start(cx);
            }
            if kit::button_ex(ui, kit::Kind::Ghost, None, tr("Cancel"), false).clicked() {
                if running {
                    self.cancel(cx);
                } else {
                    // Closes the pop-up.
                    self.done = true;
                }
            }
        });
    }
}

impl TabView for MaintenanceTab {
    fn take_done(&mut self) -> bool {
        std::mem::take(&mut self.done)
    }

    fn popup_size(&self) -> Option<egui::Vec2> {
        // The design's 622 × 357 and 862 × 511 with their border (the pop-up adds 30).
        Some(if self.is_truncate() { egui::vec2(622.0, 327.0) } else { egui::vec2(862.0, 481.0) })
    }

    fn title(&self) -> String {
        match (self.objects.as_slice(), self.is_truncate()) {
            ([one], true) => trf("Truncate: {}", &[&one.name]),
            (_, true) => tr("Truncate").to_string(),
            ([one], false) => trf("Maintenance: {}", &[&one.name]),
            _ => tr("Maintenance").to_string(),
        }
    }
    fn icon(&self) -> &'static str {
        if self.is_truncate() { icon::TRASH } else { icon::WRENCH }
    }
    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        self.poll(cx);
        let conn = self.conn.clone();
        if self.is_truncate() {
            look::footer(ui, ("truncate_footer", &conn), |ui| self.truncate_footer(ui, cx));
            look::body(ui, ("truncate_body", &conn), |ui| {
                if !cx.popup {
                    look::title(ui, &self.title());
                }
                self.truncate_ui(ui, cx);
            });
            return;
        }
        look::body(ui, ("maint_body", &conn), |ui| {
            if !cx.popup {
                let name = cx.conns.config(&self.conn).map(|c| c.name.clone()).unwrap_or_else(|| self.conn.clone());
                look::title(ui, tr("Maintenance"));
                look::conn_title(ui, &name, None);
            }
            self.objects_ui(ui, cx);
            let p = ternarygrid_ui::pal(ui);
            // As in the design: the operations 300 wide, a hairline 18 to their right, the SQL 18
            // further.
            let row = ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let left = ui
                    .vertical(|ui| {
                        ui.set_width(300.0);
                        ui.add_enabled_ui(!self.is_running(), |ui| self.ops_ui(ui, cx));
                    })
                    .response
                    .rect;
                ui.add_space(37.0);
                ui.vertical(|ui| self.run_ui(ui, cx));
                left
            });
            let x = row.inner.right() + 18.5;
            ui.painter().vline(x, row.response.rect.y_range(), egui::Stroke::new(1.0, p.border));
        });
    }

    fn on_close(&mut self, cx: &mut AppContext) -> bool {
        self.cancel(cx);
        // nothing polls the run once the tab is gone
        if let Some(r) = &self.run
            && r.job.is_some()
        {
            cx.ops.finish(r.op.id, OpState::Cancelled);
        }
        true
    }

    crate::impl_tab_any!();
}
