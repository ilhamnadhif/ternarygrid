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
use ternarygrid_core::{DataKind, DbError, DbResult, ErrorKind, tr, trf, trn};
use ternarygrid_tasks::{TaskDef, TaskKind};
use ternarygrid_transfer::{CancellationToken, MessageLevel, OnError, Progress, ProgressMessage, TransactionScope};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts;
use wizard::{Form, fact, line};

pub(crate) use wizard::section;

/// Messages kept by a tab's log (older ones are dropped).
const MAX_LOG_MESSAGES: usize = 2000;

/// `48210` as `48,210` (the counts of the wizards).
pub(crate) fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

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

/// What a running transfer goes through: the label of its objects' counter ("Tables") and
/// their noun ("table", "tables"), and whether it writes files (its "Written" counter).
#[derive(Clone, Copy)]
pub(crate) struct ProgressKind {
    pub label: &'static str,
    pub items: (&'static str, &'static str),
    pub files: bool,
}

/// Live counters of a running transfer: the bar with the row count and the counters in two
/// columns (Cancel is the footer's).
pub(crate) fn progress_ui(ui: &mut Ui, progress: &Progress, kind: ProgressKind) {
    let p = ternarygrid_ui::pal(ui);
    let s = progress.snapshot();
    let items = |done: u64, total: u64| {
        let noun = if total == 1 { kind.items.0 } else { kind.items.1 };
        format!("{} {noun}", trf("{} of {}", &[&done, &total]))
    };
    // The bar fills by rows when their total is known, else by objects: the label says which.
    let rows_label = match s.total_rows {
        Some(t) if t > 0 => trf("{} of {} rows", &[&group(s.rows), &group(t)]),
        _ if s.items_total > 0 => format!("{} · {}", items(s.items_done, s.items_total), rows_text(s.rows)),
        _ => rows_text(s.rows),
    };
    let mut facts = vec![fact(tr("Speed"), trf("{} rows/s", &[&group(s.rows_per_second.max(0.0).round() as u64)]))];
    if kind.files {
        let written = if s.bytes > 0 { crate::widgets::format_bytes(s.bytes.min(i64::MAX as u64) as i64) } else { "—".to_string() };
        facts.push(fact(tr("Written"), written));
    }
    if s.items_total > 0 {
        facts.push(fact(kind.label, items(s.items_done, s.items_total)));
    }
    if !s.active_items.is_empty() {
        facts.push(fact(tr("Current"), s.active_items.join(", ")));
    }
    facts.push(fact(tr("Errors"), s.errors.to_string()).color((s.errors > 0).then_some(p.err)));
    facts.push(fact(tr("Elapsed"), crate::widgets::format_duration(Duration::from_secs_f64(s.elapsed_secs.max(0.0)))));
    // The bar and the counters, 12 apart.
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 12.0;
        wizard::tight_row(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            match s.fraction {
                Some(f) => {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&rows_label).size(12.5).color(p.text_muted));
                        wizard::bar(ui, f as f32, 8.0);
                    });
                }
                None => {
                    ui.add(egui::Spinner::new().size(14.0).color(p.accent_text));
                    ui.label(RichText::new(&rows_label).size(12.5).color(p.text_muted));
                }
            }
        });
        wizard::fact_grid(ui, &facts);
    });
    ui.ctx().request_repaint_after(Duration::from_millis(100));
}

/// The rows count of a summary ("1 row", "2,464 rows").
pub(crate) fn rows_text(n: u64) -> String {
    if n == 1 { tr("1 row").to_string() } else { trf("{} rows", &[&group(n)]) }
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

    /// The log under a section title (while running); with `folded` a card that is closed until
    /// clicked (result pages, whose table already lists what happened).
    pub fn ui(&self, ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, max_height: f32, folded: bool) {
        if self.messages.is_empty() {
            return;
        }
        let id = egui::Id::new(id);
        if !folded {
            wizard::section_with(ui, tr("Messages"), |ui| self.copy_ui(ui));
            self.lines_ui(ui, id, max_height);
            return;
        }
        let key = id.with("fold");
        let mut open = wizard::remembered_open(ui, key, false);
        let errors = self.messages.iter().filter(|m| m.level == MessageLevel::Error).count();
        let warnings = self.messages.iter().filter(|m| m.level == MessageLevel::Warning).count();
        let mut summary = vec![trn(self.messages.len() + self.dropped, "1 message", "{} messages")];
        if errors > 0 {
            summary.push(trn(errors, "1 error", "{} errors"));
        }
        if warnings > 0 {
            summary.push(trn(warnings, "1 warning", "{} warnings"));
        }
        ui.add_space(16.0 - wizard::GAP);
        if wizard::fold_card(ui, key, &mut open, tr("Messages"), Some(&summary.join(" · ")), true, |ui| {
            wizard::tight_row(ui, |ui| self.copy_ui(ui));
            self.lines_ui(ui, id, max_height);
        }) {
            wizard::remember_open(ui, key, open);
        }
    }

    fn copy_ui(&self, ui: &mut Ui) {
        if wizard::action(ui, Some(icon::COPY), tr("Copy")).clicked() {
            ui.ctx().copy_text(self.text());
        }
        if self.dropped > 0 {
            wizard::faint(ui, &trf("({} older messages not shown)", &[&self.dropped]));
        }
    }

    fn lines_ui(&self, ui: &mut Ui, id: egui::Id, max_height: f32) {
        let p = ternarygrid_ui::pal(ui);
        // Lines 22 high, each on one line (the whole text in its tooltip).
        wizard::boxed(ui, Margin::symmetric(12, 8), |ui| {
            egui::ScrollArea::vertical().id_salt(id).max_height(max_height).auto_shrink([false, true]).stick_to_bottom(true).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for m in &self.messages {
                    let (glyph, color) = match m.level {
                        MessageLevel::Info => (icon::INFO, p.text_muted),
                        MessageLevel::Warning => (icon::WARNING, p.warn),
                        MessageLevel::Error => (icon::X_CIRCLE, p.err),
                    };
                    let text = format_message(m);
                    wizard::row_dyn(ui, 22.0, 8.0, &mut |ui| {
                        ui.label(RichText::new(glyph).font(ternarygrid_ui::style::icon_font(13.0)).color(color));
                        ui.add(egui::Label::new(RichText::new(&text).monospace().size(11.5).color(p.text)).truncate()).on_hover_text(&text);
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
        let (resp, _) = parts::Dialog::new(tr("Save as task")).icon(icon::FLOPPY_DISK).width(480.0).body_padding(egui::Margin::symmetric(20, 18)).show_modal(
            ctx,
            id,
            |ui| {
                let p = ternarygrid_ui::pal(ui);
                ternarygrid_ui::style::form_spacing(ui);
                ui.spacing_mut().item_spacing.y = 12.0;
                // The command in the monospace font.
                let size = ternarygrid_ui::style::body_size(ui.ctx());
                let format = |font: egui::FontId| egui::TextFormat { font_id: font, color: p.text_muted, line_height: Some(19.5), ..Default::default() };
                let mut text = egui::text::LayoutJob::default();
                text.append(tr("The task runs the same transfer from the Tasks view, on a schedule or with "), 0.0, format(egui::FontId::proportional(size)));
                text.append("ternarygrid --run-task", 0.0, format(egui::FontId::monospace(size - 1.0)));
                text.append(".", 0.0, format(egui::FontId::proportional(size)));
                ui.add(egui::Label::new(text).wrap());
                wizard::form_gap(ui, id.with("grid"), 1, 90.0, 12.0, |f| {
                    f.row(tr("Name"), |ui| ui.add(line(name).desired_width(f32::INFINITY)));
                    // The task's folder in the Tasks view (not where an export writes its files).
                    f.row(tr("Task folder"), |ui| ui.add(line(folder).hint_text(tr("optional, e.g. exports/nightly")).desired_width(f32::INFINITY)));
                });
                if let Some(e) = error {
                    crate::widgets::error_box(ui, e);
                }
                can_save.set(!name.trim().is_empty() && !saving);
            },
            |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled_ui(can_save.get(), |ui| kit::primary_icon(ui, icon::CHECK, tr("Save task"))).inner.clicked() {
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
    combo_w(ui, id, value, options, 0.0)
}

/// [`combo`] `width` wide (0: as wide as the chosen label, as the design's selects in a form).
pub(crate) fn combo_w<T: PartialEq + Clone>(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, value: &mut T, options: &[(T, String)], width: f32) -> bool {
    let labels: Vec<&str> = options.iter().map(|(_, l)| l.as_str()).collect();
    let sel = options.iter().position(|(v, _)| v == value);
    match combo_idx(ui, egui::Id::new(id), sel, &labels, width) {
        Some(i) => {
            *value = options[i].0.clone();
            true
        }
        None => false,
    }
}

/// The combo box of [`combo_w`] over labels; returns the label picked (another one).
fn combo_idx(ui: &mut Ui, id: egui::Id, sel: Option<usize>, labels: &[&str], width: f32) -> Option<usize> {
    let current = sel.and_then(|i| labels.get(i)).copied().unwrap_or_default();
    let mut picked = None;
    // The text, 10 before it, 8 to the 13 pt chevron and 8 after it.
    let text_w =
        |ui: &Ui, s: &str| ui.painter().layout_no_wrap(s.to_string(), egui::FontId::proportional(ternarygrid_ui::style::body_size(ui.ctx())), egui::Color32::PLACEHOLDER).size().x;
    let width = if width > 0.0 { width } else { text_w(ui, current) + 39.0 };
    // The list as wide as its longest choice (with the check mark's room).
    let menu_w = labels.iter().map(|l| text_w(ui, l) + 54.0).fold(width, f32::max);
    egui::ComboBox::from_id_salt(id).icon(kit::combo_caret).popup_style(egui::containers::menu::menu_style.into()).selected_text(current).width(width).show_ui(ui, |ui| {
        kit::menu_look(ui, menu_w);
        for (i, label) in labels.iter().enumerate() {
            if wizard::choice(ui, sel == Some(i), label) && sel != Some(i) {
                picked = Some(i);
            }
        }
    });
    picked
}

/// Single line editor of an optional text (empty = `None`).
pub(crate) fn opt_text(ui: &mut Ui, value: &mut Option<String>, hint: &str, width: f32) -> bool {
    opt_text_h(ui, value, hint, width, ternarygrid_ui::style::CONTROL_H)
}

/// [`opt_text`] `h` high.
pub(crate) fn opt_text_h(ui: &mut Ui, value: &mut Option<String>, hint: &str, width: f32, h: f32) -> bool {
    opt_text_ex(ui, value, hint, width, h, false)
}

/// [`opt_text`] in the monospace font (format patterns).
pub(crate) fn opt_mono(ui: &mut Ui, value: &mut Option<String>, hint: &str, width: f32) -> bool {
    opt_text_ex(ui, value, hint, width, ternarygrid_ui::style::CONTROL_H, true)
}

fn opt_text_ex(ui: &mut Ui, value: &mut Option<String>, hint: &str, width: f32, h: f32, mono: bool) -> bool {
    let mut s = value.clone().unwrap_or_default();
    let mut edit = wizard::fit(egui::TextEdit::singleline(&mut s), h).hint_text(hint).desired_width(width);
    if mono {
        edit = edit.font(egui::TextStyle::Monospace);
    }
    let changed = ui.add(edit).changed();
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

/// A single-line [`parsed_edit`] `h` high (the toolbars' 28 pt fields).
pub(crate) fn parsed_line(ui: &mut Ui, shown: String, width: f32, h: f32, apply: impl FnOnce(&str)) -> bool {
    let h0 = ui.spacing().interact_size.y;
    ui.spacing_mut().interact_size.y = h;
    let changed = parsed_edit_ex(ui, shown, "", width, false, false, apply);
    ui.spacing_mut().interact_size.y = h0;
    changed
}

/// [`parsed_edit`], in the monospace font with `mono`; single-line fields are as high as the
/// ui's controls.
fn parsed_edit_ex(ui: &mut Ui, shown: String, hint: &str, width: f32, multiline: bool, mono: bool, apply: impl FnOnce(&str)) -> bool {
    let key = ui.next_auto_id().with("ternarygrid_parsed_edit");
    let mut text = ui.data(|d| d.get_temp::<String>(key)).unwrap_or(shown);
    let mut edit = if multiline { egui::TextEdit::multiline(&mut text).desired_rows(2) } else { egui::TextEdit::singleline(&mut text) };
    if mono {
        edit = edit.font(egui::TextStyle::Monospace);
    }
    let edit = if multiline { edit.margin(wizard::INPUT_MARGIN) } else { wizard::fit(edit, ui.spacing().interact_size.y.max(ternarygrid_ui::style::SMALL_H)) };
    let r = ui.add(edit.hint_text(hint).desired_width(width));
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

/// The delimiter of CSV files: the common ones, or "Other…" with a field for any other.
pub(crate) fn delimiter_choice(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, delimiter: &mut String) {
    const PRESETS: [&str; 4] = [",", ";", "\t", "|"];
    let other_key = ui.id().with(("delimiter_other", id));
    let preset = PRESETS.iter().copied().find(|p| *p == delimiter.as_str());
    let other = ui.data(|d| d.get_temp::<bool>(other_key)).unwrap_or(false) || preset.is_none();
    let mut current: &'static str = if other { "" } else { preset.unwrap_or("") };
    let items = [(",", ","), (";", ";"), ("\t", tr("Tab")), ("|", tr("Pipe (|)")), ("", tr("Other…"))];
    if parts::segmented(ui, &mut current, &items) {
        if !current.is_empty() {
            *delimiter = current.to_string();
        }
        ui.data_mut(|d| d.insert_temp(other_key, current.is_empty()));
    }
    if current.is_empty() {
        ui.add(wizard::mono_line(delimiter, 60.0)).on_hover_text(tr("Any other delimiter"));
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

/// Schema picker `width` wide; returns true when the selection changed.
pub(crate) fn schema_combo(ui: &mut Ui, cx: &mut AppContext, conn: &str, id: impl std::hash::Hash + std::fmt::Debug, value: &mut Option<SchemaRef>, width: f32) -> bool {
    let names = schema_names(cx, conn);
    let label = value.as_ref().and_then(|s| s.schema.clone()).unwrap_or_else(|| tr("(default)").to_string());
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).icon(kit::combo_caret).popup_style(egui::containers::menu::menu_style.into()).selected_text(label).width(width).show_ui(ui, |ui| {
        kit::menu_look(ui, width);
        if wizard::choice(ui, value.as_ref().is_none_or(|v| v.schema.is_none()), tr("(default)")) {
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
                    if wizard::choice(ui, selected, n) && !selected {
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

/// Connection picker over all configured connections; the chosen one's database logo leads its
/// text inside the box ("Choose…" in the faint colour until one is chosen).
pub(crate) fn connection_combo(ui: &mut Ui, cx: &AppContext, id: impl std::hash::Hash + std::fmt::Debug, value: &mut Option<String>, width: f32) -> bool {
    let p = ternarygrid_ui::pal(ui);
    let conns: Vec<(String, String)> = cx.conns.configs().iter().map(|c| (c.id.clone(), c.name.clone())).collect();
    let chosen = value.as_ref().and_then(|v| conns.iter().find(|(i, _)| i == v)).map(|(_, n)| n.clone());
    let kind = value.as_ref().and_then(|v| cx.conns.config(v)).map(|c| c.kind);
    const LOGO: f32 = 16.0;
    let size = ternarygrid_ui::style::body_size(ui.ctx());
    let mut text = egui::text::LayoutJob::default();
    match &chosen {
        Some(name) => {
            if kind.is_some() {
                // Room for the logo, painted over it below.
                text.append(" ", LOGO + 4.0, egui::TextFormat::simple(egui::FontId::proportional(size), egui::Color32::TRANSPARENT));
            }
            text.append(name, 0.0, egui::TextFormat::simple(egui::FontId::proportional(size), p.text));
        }
        None => text.append(tr("Choose…"), 0.0, egui::TextFormat::simple(egui::FontId::proportional(size), p.text_faint)),
    }
    let mut changed = false;
    let shown = egui::ComboBox::from_id_salt(id).icon(kit::combo_caret).popup_style(egui::containers::menu::menu_style.into()).selected_text(text).width(width).show_ui(ui, |ui| {
        kit::menu_look(ui, width);
        for (cid, name) in &conns {
            let glyph = if cx.conns.is_connected(cid) { icon::PLUGS_CONNECTED } else { icon::PLUGS };
            let selected = value.as_deref() == Some(cid.as_str());
            if wizard::choice(ui, selected, &format!("{glyph} {name}")) && !selected {
                *value = Some(cid.clone());
                changed = true;
            }
        }
    });
    if let (Some(kind), Some(_)) = (kind, &chosen) {
        let r = shown.response.rect;
        let at = egui::Rect::from_min_size(egui::pos2(r.left() + ui.spacing().button_padding.x, r.center().y - LOGO / 2.0), egui::vec2(LOGO, LOGO));
        let mut logo = ui.new_child(egui::UiBuilder::new().max_rect(at));
        crate::tabs::dbtools::look::logo(&mut logo, kind, LOGO);
    }
    changed
}

/// Close the wizard (its Close button after a run): a pop-up through `done`, a tab by
/// closing it.
pub(crate) fn close_wizard(cx: &mut AppContext, done: &mut bool) {
    if cx.popup {
        *done = true;
    } else if let Some(id) = cx.current_tab {
        cx.push(AppCommand::CloseTab(id));
    }
}

/// The tone of a run's outcome: an error when nothing was written though rows failed, a
/// warning when some failed (or nothing was written), else a success.
pub(crate) fn outcome_tone(written: u64, failed: u64) -> parts::Tone {
    use parts::Tone;
    match (written, failed) {
        (0, f) if f > 0 => Tone::Error,
        (_, f) if f > 0 => Tone::Warning,
        (0, _) => Tone::Warning,
        _ => Tone::Success,
    }
}

/// The error of a failed run, for the summary page.
pub(crate) fn error_text(e: &DbError) -> String {
    if e.kind == ErrorKind::Cancelled { tr("Cancelled by the user.").to_string() } else { e.full_text() }
}
