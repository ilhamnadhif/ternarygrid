//! Data transfer tabs: the export wizard (FR-T1), the import wizard (FR-T2),
//! table migration between connections (FR-T3) and mock data generation.
//!
//! The tabs only collect a `ternarygrid-transfer` job, run it on the backend with
//! dedicated sessions opened by [`AppSessionFactory`](crate::transferexec::AppSessionFactory),
//! mirror its progress into the operations panel and show the summary.
//! Export, import and migration configurations can be saved as tasks and run
//! headless through [`crate::transferexec`].

// `DbError` (the error type of ternarygrid-core) is 144 bytes, so every `DbResult`
// trips this lint; boxing it is a decision for ternarygrid-core.
#![allow(clippy::result_large_err)]

pub mod export;
pub mod import;
pub mod migration;
pub mod mock;
pub(crate) mod wizard;

pub use export::{ExportTab, ResultSource, grid_store_rows};
pub use import::{ImportFileFormat, ImportTab, ImportTarget};
pub use migration::MigrationTab;
pub use mock::MockDataTab;

use crate::backend::Job;
use crate::command::AppCommand;
use crate::cx::AppContext;
use crate::ops::{OpProgress, OpState};
use egui::{Margin, RichText, Ui};
use egui_phosphor::regular as icon;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use ternarygrid_core::ddl::TableDef;
use ternarygrid_core::meta::{ObjectInfo, ObjectKind, ObjectRef, SchemaRef};
use ternarygrid_core::{DataKind, DbError, DbResult, ErrorKind, tr, trf};
use ternarygrid_tasks::{TaskDef, TaskKind};
use ternarygrid_transfer::{CancellationToken, MessageLevel, OnError, Progress, ProgressMessage, TransactionScope};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts;
use wizard::{Form, line};

pub(crate) use wizard::section;

/// Messages kept by a tab's log (older ones are dropped).
const MAX_LOG_MESSAGES: usize = 2000;

/// Unique id for the egui ids of one tab instance.
pub(crate) fn next_uid() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// ------------------------------------------------------------ running jobs

/// A transfer running on the backend, registered in the operations panel.
pub(crate) struct Running<T> {
    op: u64,
    pub progress: Arc<Progress>,
    cancel: CancellationToken,
    job: Job<DbResult<T>>,
}

impl<T: Send + 'static> Running<T> {
    /// Register an operation called `title` and run `make(progress, cancel)`
    /// on the backend. The operations panel's cancel button cancels it too.
    pub fn spawn<F, Fut>(cx: &mut AppContext, title: String, make: F) -> Self
    where
        F: FnOnce(Arc<Progress>, CancellationToken) -> Fut,
        Fut: Future<Output = DbResult<T>> + Send + 'static,
    {
        let handle = cx.ops.start(title);
        let progress = Progress::shared();
        let fut = make(progress.clone(), handle.cancel.clone());
        let job = cx.backend.spawn(mirror_progress(fut, progress.clone(), handle.progress.clone()));
        Running { op: handle.id, progress, cancel: handle.cancel, job }
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// The result once the job has finished; the operation is marked
    /// finished with `describe(result)` as its message.
    pub fn poll(&mut self, cx: &mut AppContext, describe: impl FnOnce(&T) -> String) -> Option<DbResult<T>> {
        let result = self.job.poll()?;
        let state = match &result {
            Ok(v) => OpState::Succeeded(describe(v)),
            Err(e) if e.kind == ErrorKind::Cancelled => OpState::Cancelled,
            Err(e) => OpState::Failed(e.full_text()),
        };
        cx.ops.finish(self.op, state);
        Some(result)
    }

    /// Cancel and mark the operation cancelled (the tab is being closed).
    pub fn abandon(&mut self, cx: &mut AppContext) {
        self.cancel.cancel();
        cx.ops.finish(self.op, OpState::Cancelled);
    }
}

/// Copy the transfer counters into the operations panel while `fut` runs,
/// so the panel stays current even when the tab is not visible.
async fn mirror_progress<T>(fut: impl Future<Output = DbResult<T>>, progress: Arc<Progress>, op: Arc<OpProgress>) -> DbResult<T> {
    tokio::pin!(fut);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            r = &mut fut => {
                copy_progress(&progress, &op);
                return r;
            }
            _ = tick.tick() => copy_progress(&progress, &op),
        }
    }
}

fn copy_progress(progress: &Progress, op: &OpProgress) {
    let rows = progress.rows();
    op.set(rows, progress.total_rows());
    op.set_message(match progress.current_item() {
        Some(item) => trf("{} rows · {}", &[&rows, &item]),
        None => trf("{} rows", &[&rows]),
    });
}

// ----------------------------------------------------------------- progress

/// Live counters of a running transfer: the bar with the row count, the counters in two
/// columns and Cancel. Returns true when Cancel was clicked.
pub(crate) fn progress_ui(ui: &mut Ui, progress: &Progress, cancelling: bool) -> bool {
    let p = ternarygrid_ui::pal(ui);
    let s = progress.snapshot();
    let rows_text = match s.total_rows {
        Some(t) => trf("{} of {} rows", &[&s.rows, &t]),
        None => trf("{} rows", &[&s.rows]),
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        match s.fraction {
            Some(f) => {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(&rows_text).size(12.5).color(p.text_muted));
                    parts::progress(ui, f as f32, f32::INFINITY);
                });
            }
            None => {
                ui.add(egui::Spinner::new().size(14.0).color(p.accent_text));
                ui.label(RichText::new(&rows_text).size(12.5).color(p.text_muted));
            }
        }
    });
    let value = |ui: &mut Ui, text: String, color: Option<egui::Color32>| {
        ui.add(egui::Label::new(RichText::new(text).color(color.unwrap_or(p.text))).truncate());
    };
    ui.spacing_mut().interact_size.y = 20.0;
    wizard::form(ui, "progress", 2, 100.0, |f| {
        f.row(tr("Speed"), |ui| value(ui, trf("{} rows/s", &[&format!("{:.0}", s.rows_per_second)]), None));
        let written = if s.bytes > 0 { crate::widgets::format_bytes(s.bytes.min(i64::MAX as u64) as i64) } else { "—".to_string() };
        f.row(tr("Written"), |ui| value(ui, written, None));
        if s.items_total > 0 {
            f.row(tr("Objects"), |ui| value(ui, trf("{} of {}", &[&s.items_done, &s.items_total]), None));
        }
        if !s.active_items.is_empty() {
            f.row(tr("Current"), |ui| value(ui, s.active_items.join(", "), None));
        }
        f.row(tr("Errors"), |ui| value(ui, s.errors.to_string(), (s.errors > 0).then_some(p.err)));
        f.row(tr("Elapsed"), |ui| value(ui, crate::widgets::format_duration(Duration::from_secs_f64(s.elapsed_secs.max(0.0))), None));
    });
    ui.spacing_mut().interact_size.y = ternarygrid_ui::style::CONTROL_H;
    let label = if cancelling { tr("Cancelling…") } else { tr("Cancel") };
    let clicked = ui.add_enabled_ui(!cancelling, |ui| kit::button_ex(ui, Kind::Danger, Some(icon::STOP), label, true)).inner.clicked();
    ui.ctx().request_repaint_after(Duration::from_millis(100));
    clicked
}

/// Messages of a transfer, drained from its [`Progress`] every frame.
#[derive(Default)]
pub(crate) struct MessageLog {
    messages: Vec<ProgressMessage>,
    dropped: usize,
}

impl MessageLog {
    pub fn clear(&mut self) {
        self.messages.clear();
        self.dropped = 0;
    }

    pub fn drain(&mut self, progress: &Progress) {
        self.push(progress.take_messages());
    }

    /// Append messages taken from a progress by the caller.
    pub fn push(&mut self, messages: Vec<ProgressMessage>) {
        self.messages.extend(messages);
        if self.messages.len() > MAX_LOG_MESSAGES {
            let extra = self.messages.len() - MAX_LOG_MESSAGES;
            self.messages.drain(..extra);
            self.dropped += extra;
        }
    }

    pub fn text(&self) -> String {
        self.messages.iter().map(format_message).collect::<Vec<_>>().join("\n")
    }

    pub fn ui(&self, ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, max_height: f32) {
        if self.messages.is_empty() {
            return;
        }
        let p = ternarygrid_ui::pal(ui);
        wizard::section_with(ui, tr("Messages"), |ui| {
            if wizard::ghost(ui, Some(icon::COPY), tr("Copy")).clicked() {
                ui.ctx().copy_text(self.text());
            }
            if self.dropped > 0 {
                wizard::faint(ui, &trf("({} older messages not shown)", &[&self.dropped]));
            }
        });
        wizard::boxed(ui, Margin::symmetric(12, 8), |ui| {
            egui::ScrollArea::vertical().id_salt(id).max_height(max_height).auto_shrink([false, true]).stick_to_bottom(true).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for m in &self.messages {
                    let (glyph, color) = match m.level {
                        MessageLevel::Info => (icon::INFO, p.text_muted),
                        MessageLevel::Warning => (icon::WARNING, p.warn),
                        MessageLevel::Error => (icon::X_CIRCLE, p.err),
                    };
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(RichText::new(glyph).font(ternarygrid_ui::style::icon_font(13.0)).color(color));
                        ui.add(egui::Label::new(RichText::new(format_message(m)).monospace().size(11.5).color(p.text)).wrap().selectable(true));
                    });
                }
            });
        });
    }
}

fn format_message(m: &ProgressMessage) -> String {
    match &m.item {
        Some(item) => format!("[{:.1}s] {item}: {}", m.elapsed_secs, m.text),
        None => format!("[{:.1}s] {}", m.elapsed_secs, m.text),
    }
}

// -------------------------------------------------------------- save tasks

/// "Save as task" dialog plus the background write to the task store.
#[derive(Default)]
pub(crate) struct TaskSaver {
    open: bool,
    name: String,
    folder: String,
    error: Option<String>,
    job: Option<Job<DbResult<TaskDef>>>,
    /// Id of the task saved last.
    pub saved: Option<String>,
}

impl TaskSaver {
    pub fn open(&mut self, default_name: String) {
        self.open = true;
        self.name = default_name;
        self.error = None;
    }

    pub fn is_saving(&self) -> bool {
        self.job.is_some()
    }

    /// Draw the dialog; returns the confirmed name and folder.
    pub fn ui(&mut self, ctx: &egui::Context, id: egui::Id) -> Option<(String, Option<String>)> {
        if !self.open {
            return None;
        }
        let mut save = false;
        let mut cancel = false;
        // Not while a save runs: a second click would save the task twice.
        let can_save = std::cell::Cell::new(false);
        let saving = self.job.is_some();
        let (name, folder, error) = (&mut self.name, &mut self.folder, &self.error);
        let (resp, _) = parts::Dialog::new(tr("Save as task")).icon(icon::FLOPPY_DISK).width(480.0).show_modal(
            ctx,
            id,
            |ui| {
                let p = ternarygrid_ui::pal(ui);
                ternarygrid_ui::style::form_spacing(ui);
                ui.spacing_mut().item_spacing.y = 12.0;
                ui.add(
                    egui::Label::new(RichText::new(tr("The task runs the same transfer from the Tasks view, on a schedule or with ternarygrid --run-task.")).color(p.text_muted))
                        .wrap(),
                );
                wizard::form(ui, id.with("grid"), 1, 64.0, |f| {
                    f.row(tr("Name"), |ui| ui.add(line(name).desired_width(f32::INFINITY)));
                    f.row(tr("Folder"), |ui| ui.add(line(folder).hint_text(tr("optional, e.g. exports/nightly")).desired_width(f32::INFINITY)));
                });
                if let Some(e) = error {
                    crate::widgets::error_box(ui, e);
                }
                can_save.set(!name.trim().is_empty() && !saving);
            },
            |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled_ui(can_save.get(), |ui| kit::primary_icon(ui, icon::FLOPPY_DISK, tr("Save"))).inner.clicked() {
                        save = true;
                    }
                    if kit::button_ex(ui, Kind::Ghost, None, tr("Cancel"), false).clicked() {
                        cancel = true;
                    }
                });
            },
        );
        if cancel || resp.close {
            self.open = false;
        }
        save.then(|| {
            let folder = Some(self.folder.trim().to_string()).filter(|f| !f.is_empty());
            (self.name.trim().to_string(), folder)
        })
    }

    /// Store a new task in the background.
    pub fn save(&mut self, cx: &AppContext, name: String, folder: Option<String>, kind: TaskKind) {
        let Some(store) = cx.services.tasks.clone() else {
            self.error = Some(tr("The task store is not available.").to_string());
            return;
        };
        let mut task = TaskDef::new(name, kind);
        task.folder = folder;
        self.error = None;
        self.job = Some(cx.backend.spawn(async move { store.create(task) }));
    }

    /// Show the error of a failed attempt in the dialog.
    pub fn fail(&mut self, error: String) {
        self.open = true;
        self.error = Some(error);
    }

    pub fn poll(&mut self, cx: &mut AppContext) {
        let Some(result) = self.job.as_mut().and_then(|j| j.poll()) else { return };
        self.job = None;
        match result {
            Ok(task) => {
                self.open = false;
                cx.toasts.success(trf("Task \"{}\" saved", &[&task.name]));
                self.saved = Some(task.id);
            }
            Err(e) => {
                self.open = true;
                self.error = Some(e.full_text());
            }
        }
    }
}

// ------------------------------------------------------------------ widgets

/// Combo box over a fixed list of (value, label) pairs.
pub(crate) fn combo<T: PartialEq + Clone>(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, value: &mut T, options: &[(T, String)]) -> bool {
    let current = options.iter().find(|(v, _)| v == value).map(|(_, l)| l.clone()).unwrap_or_default();
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).icon(ternarygrid_ui::kit::combo_caret).selected_text(current).show_ui(ui, |ui| {
        for (v, label) in options {
            if ui.selectable_label(v == value, label).clicked() && v != value {
                *value = v.clone();
                changed = true;
            }
        }
    });
    changed
}

/// Single line editor of an optional text (empty = `None`).
pub(crate) fn opt_text(ui: &mut Ui, value: &mut Option<String>, hint: &str, width: f32) -> bool {
    let mut s = value.clone().unwrap_or_default();
    let changed = ui.add(line(&mut s).hint_text(hint).desired_width(width)).changed();
    if changed {
        *value = Some(s).filter(|s| !s.is_empty());
    }
    changed
}

/// Text field over a value that is parsed from text. The typed text is kept
/// while the field has focus, so intermediate (invalid) input such as `-` or
/// an unfinished date can be typed; `apply` receives the text after every change.
pub(crate) fn parsed_edit(ui: &mut Ui, shown: String, hint: &str, width: f32, multiline: bool, apply: impl FnOnce(&str)) -> bool {
    parsed_edit_ex(ui, shown, hint, width, multiline, false, apply)
}

/// [`parsed_edit`], in the monospace font with `mono`.
fn parsed_edit_ex(ui: &mut Ui, shown: String, hint: &str, width: f32, multiline: bool, mono: bool, apply: impl FnOnce(&str)) -> bool {
    let key = ui.next_auto_id().with("ternarygrid_parsed_edit");
    let mut text = ui.data(|d| d.get_temp::<String>(key)).unwrap_or(shown);
    let mut edit = if multiline { egui::TextEdit::multiline(&mut text).desired_rows(2) } else { egui::TextEdit::singleline(&mut text) };
    if mono {
        edit = edit.font(egui::TextStyle::Monospace);
    }
    let r = ui.add(edit.margin(wizard::INPUT_MARGIN).hint_text(hint).desired_width(width));
    let changed = r.changed();
    if changed {
        apply(&text);
    }
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(key, text));
    } else {
        ui.data_mut(|d| d.remove::<String>(key));
    }
    changed
}

/// Editor of a single character (keeps the old value while empty).
pub(crate) fn char_edit(ui: &mut Ui, value: &mut char) -> bool {
    parsed_edit_ex(ui, value.to_string(), "", 60.0, false, true, |t| {
        if let Some(c) = t.chars().next() {
            *value = c;
        }
    })
}

/// Editor of an optional character (empty = `None`).
pub(crate) fn opt_char_edit(ui: &mut Ui, value: &mut Option<char>, hint: &str) -> bool {
    parsed_edit_ex(ui, value.map(String::from).unwrap_or_default(), hint, 60.0, false, true, |t| *value = t.chars().next())
}

/// Editor of a list of texts, one per line.
pub(crate) fn lines_edit(ui: &mut Ui, value: &mut Vec<String>, hint: &str, width: f32) -> bool {
    parsed_edit_ex(ui, value.join("\n"), hint, width, true, true, |t| {
        *value = t.lines().map(str::to_string).filter(|l| !l.trim().is_empty()).collect();
    })
}

/// Editor of an optional number (empty = `None`).
pub(crate) fn opt_number<T: std::str::FromStr + ToString>(ui: &mut Ui, value: &mut Option<T>, hint: &str) -> bool {
    let shown = value.as_ref().map(T::to_string).unwrap_or_default();
    parsed_edit(ui, shown, hint, 120.0, false, |t| {
        let t = t.trim();
        if t.is_empty() {
            *value = None;
        } else if let Ok(v) = t.parse::<T>() {
            *value = Some(v);
        }
    })
}

/// Editor of a number (invalid input keeps the old value).
pub(crate) fn number_edit<T: std::str::FromStr + ToString>(ui: &mut Ui, value: &mut T, width: f32) -> bool {
    parsed_edit(ui, value.to_string(), "", width, false, |t| {
        if let Ok(v) = t.trim().parse::<T>() {
            *value = v;
        }
    })
}

/// The delimiter choices of CSV files next to the delimiter field.
pub(crate) fn delimiter_choice(ui: &mut Ui, delimiter: &mut String) {
    let mut current: &str = match delimiter.as_str() {
        "," => ",",
        ";" => ";",
        "\t" => "\t",
        "|" => "|",
        _ => "",
    };
    if parts::segmented(ui, &mut current, &[(",", ","), (";", ";"), ("\t", tr("Tab")), ("|", "|")]) {
        *delimiter = current.to_string();
    }
}

/// Options of [`OnError`].
pub(crate) fn on_error_options() -> Vec<(OnError, String)> {
    vec![
        (OnError::Abort, tr("Stop at the first error").to_string()),
        (OnError::SkipRow, tr("Skip failing rows").to_string()),
        (OnError::LogAndContinue, tr("Skip and log every failing row").to_string()),
    ]
}

/// Options of [`TransactionScope`].
pub(crate) fn transaction_options() -> Vec<(TransactionScope, String)> {
    vec![
        (TransactionScope::PerBatch, tr("Commit after every batch").to_string()),
        (TransactionScope::Whole, tr("One transaction for everything").to_string()),
        (TransactionScope::None, tr("Auto-commit every statement").to_string()),
    ]
}

/// Human readable name of a value kind.
pub(crate) fn kind_label(kind: DataKind) -> &'static str {
    match kind {
        DataKind::Bool => tr("Boolean"),
        DataKind::Int => tr("Integer"),
        DataKind::Float => tr("Floating point"),
        DataKind::Decimal => tr("Decimal"),
        DataKind::Text => tr("Text"),
        DataKind::Binary => tr("Binary"),
        DataKind::Date => tr("Date"),
        DataKind::Time => tr("Time"),
        DataKind::TimeTz => tr("Time with time zone"),
        DataKind::DateTime => tr("Timestamp"),
        DataKind::DateTimeTz => tr("Timestamp with time zone"),
        DataKind::Interval => tr("Interval"),
        DataKind::Uuid => tr("UUID"),
        DataKind::Json => tr("JSON"),
        DataKind::Xml => tr("XML"),
        DataKind::Array => tr("Array"),
        DataKind::Geometry => tr("Geometry"),
        DataKind::Enum => tr("Enumeration"),
        DataKind::Network => tr("Network address"),
        DataKind::Bit => tr("Bit string"),
        DataKind::Range => tr("Range"),
        DataKind::Other => tr("Other"),
    }
}

/// Kinds offered when choosing a column type.
pub(crate) const COMMON_KINDS: [DataKind; 12] = [
    DataKind::Text,
    DataKind::Int,
    DataKind::Decimal,
    DataKind::Float,
    DataKind::Bool,
    DataKind::Date,
    DataKind::Time,
    DataKind::DateTime,
    DataKind::DateTimeTz,
    DataKind::Uuid,
    DataKind::Json,
    DataKind::Binary,
];

/// Folder offered for new exports: `~/Downloads`, else the home folder.
pub(crate) fn default_output_folder() -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    match home {
        Some(h) => {
            let downloads = h.join("Downloads");
            if downloads.is_dir() { downloads } else { h }
        }
        None => PathBuf::from("."),
    }
}

/// Error shown when the connection is closed; asks the app to connect.
pub(crate) fn not_connected(cx: &mut AppContext, conn: &str) -> String {
    cx.push(AppCommand::Connect(conn.to_string()));
    tr("The connection is closed; connecting… try again when it is connected.").to_string()
}

/// The connection's database has transactions (ClickHouse, Cassandra and the NoSQL
/// stores have none: "Replace" without an upsert of their own is refused there).
pub(crate) fn has_transactions(cx: &AppContext, conn: &str) -> bool {
    match cx.conns.live(conn) {
        Some(l) => l.driver.info().capabilities.transactions,
        None => cx.conns.config(conn).and_then(|c| cx.conns.registry.get(c.kind)).is_none_or(|d| d.info().capabilities.transactions),
    }
}

/// Shown under a "Replace" choice for a database without transactions.
pub(crate) fn replace_without_transactions_ui(f: &mut Form<'_>) {
    f.note(|ui| {
        let p = ternarygrid_ui::pal(ui);
        ui.add(
            egui::Label::new(
                RichText::new(tr(
                    "This database has no transactions: replacing rows works only where it has an upsert of its own, otherwise the run is refused before anything is deleted.",
                ))
                .size(12.5)
                .color(p.warn),
            )
            .wrap(),
        );
    });
}

/// Display name of a connection.
pub(crate) fn connection_name(cx: &AppContext, conn: &str) -> String {
    cx.conns.config(conn).map(|c| c.name.clone()).unwrap_or_else(|| conn.to_string())
}

/// Schemas of a connected connection (requested in the background); `None`
/// while loading or when not connected.
pub(crate) fn schema_names(cx: &mut AppContext, conn: &str) -> Option<Vec<String>> {
    let backend = cx.backend;
    let live = cx.conns.live_mut(conn)?;
    let main = live.main.clone();
    live.meta.request_schemas(backend, &main, None);
    let schemas = live.meta.schemas_of(None);
    if schemas.is_loading() || schemas.is_not_loaded() {
        return None;
    }
    Some(schemas.get().map(|v| v.iter().filter(|s| !s.is_system).map(|s| s.name.clone()).collect()).unwrap_or_default())
}

/// Default schema of a connected connection (current schema or database).
pub(crate) fn current_schema(cx: &AppContext, conn: &str) -> Option<SchemaRef> {
    let live = cx.conns.live(conn)?;
    let schema = live.current_schema.clone().or_else(|| live.current_database.clone());
    Some(SchemaRef::new(None, schema))
}

/// Schema picker; returns true when the selection changed.
pub(crate) fn schema_combo(ui: &mut Ui, cx: &mut AppContext, conn: &str, id: impl std::hash::Hash + std::fmt::Debug, value: &mut Option<SchemaRef>) -> bool {
    let names = schema_names(cx, conn);
    let label = value.as_ref().and_then(|s| s.schema.clone()).unwrap_or_else(|| tr("(default)").to_string());
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).icon(ternarygrid_ui::kit::combo_caret).selected_text(label).width(180.0).show_ui(ui, |ui| {
        if ui.selectable_label(value.as_ref().is_none_or(|v| v.schema.is_none()), tr("(default)")).clicked() {
            *value = None;
            changed = true;
        }
        match &names {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak(tr("Loading…"));
                });
            }
            Some(names) => {
                for n in names {
                    let selected = value.as_ref().and_then(|v| v.schema.as_deref()) == Some(n.as_str());
                    if ui.selectable_label(selected, n).clicked() && !selected {
                        let database = value.as_ref().and_then(|v| v.database.clone());
                        *value = Some(SchemaRef::new(database, Some(n.clone())));
                        changed = true;
                    }
                }
            }
        }
    });
    changed
}

/// Tables (and optionally views) of a schema from the metadata cache;
/// `None` while loading.
pub(crate) fn schema_tables(cx: &mut AppContext, conn: &str, schema: &SchemaRef, views: bool) -> Option<DbResult<Vec<ObjectInfo>>> {
    let backend = cx.backend;
    let live = cx.conns.live_mut(conn)?;
    let session = live.ensure_session(backend, schema.database.as_deref())?;
    let mut kinds = vec![ObjectKind::Table];
    if views {
        kinds.push(ObjectKind::View);
    }
    for kind in &kinds {
        live.meta.request_objects(backend, &session, schema, *kind);
    }
    let mut out = Vec::new();
    for kind in kinds {
        let entry = live.meta.objects.get(&(schema.clone(), kind))?;
        if let Some(e) = entry.error() {
            return Some(Err(e.clone()));
        }
        out.extend(entry.get()?.iter().cloned());
    }
    Some(Ok(out))
}

/// Definition of a table from the metadata cache (requested in the
/// background); `None` while loading or when not connected.
pub(crate) fn table_def_of(cx: &mut AppContext, conn: &str, table: &ObjectRef) -> Option<DbResult<Arc<TableDef>>> {
    let backend = cx.backend;
    let live = cx.conns.live_mut(conn)?;
    let session = live.ensure_session(backend, table.database.as_deref())?;
    live.meta.request_table_def(backend, &session, table);
    let entry = live.meta.table_defs.get(table)?;
    if let Some(e) = entry.error() {
        return Some(Err(e.clone()));
    }
    entry.get().cloned().map(Ok)
}

/// Forget a cached table definition so it is loaded again.
pub(crate) fn reload_table_def(cx: &mut AppContext, conn: &str, table: &ObjectRef) {
    if let Some(live) = cx.conns.live_mut(conn) {
        live.meta.table_defs.remove(table);
    }
}

/// Connection picker over all configured connections.
pub(crate) fn connection_combo(ui: &mut Ui, cx: &AppContext, id: impl std::hash::Hash + std::fmt::Debug, value: &mut Option<String>) -> bool {
    let conns: Vec<(String, String)> = cx.conns.configs().iter().map(|c| (c.id.clone(), c.name.clone())).collect();
    let current = value.as_ref().and_then(|v| conns.iter().find(|(i, _)| i == v)).map(|(_, n)| n.clone()).unwrap_or_else(|| tr("Choose…").to_string());
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).icon(ternarygrid_ui::kit::combo_caret).selected_text(current).width(200.0).show_ui(ui, |ui| {
        for (cid, name) in &conns {
            let glyph = if cx.conns.is_connected(cid) { icon::PLUGS_CONNECTED } else { icon::PLUGS };
            let selected = value.as_deref() == Some(cid.as_str());
            if ui.selectable_label(selected, format!("{glyph} {name}")).clicked() && !selected {
                *value = Some(cid.clone());
                changed = true;
            }
        }
    });
    changed
}

/// The error of a failed run, for the summary page.
pub(crate) fn error_text(e: &DbError) -> String {
    if e.kind == ErrorKind::Cancelled { tr("Cancelled by the user.").to_string() } else { e.full_text() }
}
