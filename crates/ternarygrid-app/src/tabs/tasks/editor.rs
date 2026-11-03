//! The task editor dialog: general settings, the form of the task type, the
//! schedule with validation and a preview of the next runs, time limit,
//! notifications and the system scheduler option.

use super::draft::{IntervalUnit, KindDraft, ScheduleDraft, ScheduleMode, TaskDraft, kind_icon, kind_label, weekday_names};
use super::look;
use super::system;
use crate::cx::AppContext;
use crate::tabs::dbtools::{ToolFlavor, connection_combo, dialog_near, schema_field};
use chrono::{Datelike, Local};
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use std::path::PathBuf;
use ternarygrid_core::config::AuthMethod;
use ternarygrid_core::{tr, trf};
use ternarygrid_tasks::{Schedule, TaskDef, TaskKind};
use ternarygrid_ui::kit::{self, Kind};

/// What the user did in the editor this frame.
pub(super) enum EditorAction {
    None,
    Cancel,
    Save(Box<TaskDef>),
}

pub(super) struct TaskEditor {
    pub draft: TaskDraft,
    /// Error of the last save attempt.
    pub error: Option<String>,
    /// A save is in progress.
    pub saving: bool,
    add_step: Option<String>,
    exe: Option<PathBuf>,
}

const CRON_PRESETS: &[&str] = &["*/15 * * * *", "0 * * * *", "0 2 * * *", "0 9 * * 1-5", "0 6 * * 1", "0 0 1 * *"];

fn preset_label(expr: &str) -> &'static str {
    match expr {
        "*/15 * * * *" => tr("Every 15 minutes"),
        "0 * * * *" => tr("Every hour"),
        "0 2 * * *" => tr("Every day at 02:00"),
        "0 9 * * 1-5" => tr("Weekdays at 09:00"),
        "0 6 * * 1" => tr("Every Monday at 06:00"),
        _ => tr("First day of the month at 00:00"),
    }
}

/// Warning when a connection cannot be opened without the user.
pub(super) fn credential_warning(cx: &AppContext, conn: &str) -> Option<&'static str> {
    let cfg = cx.conns.config(conn)?;
    match cfg.auth {
        AuthMethod::Prompt => Some(tr("This connection asks for its password at every connect; tasks can only use saved passwords.")),
        AuthMethod::Password if !cfg.save_password && !cfg.kind.is_file_based() => Some(tr("This connection does not save its password; the task cannot connect without it.")),
        _ => None,
    }
}

/// Width of the label column of the editor.
const LABEL_W: f32 = 120.0;

/// A section of the editor: a hairline above (except the first), its title in semibold.
fn section(ui: &mut Ui, title: &str, first: bool) {
    // The design's sections are 16 apart, the hairline between them.
    if !first {
        ui.add_space(16.0 - ui.spacing().item_spacing.y);
        ternarygrid_ui::parts::hline(ui);
        ui.add_space(16.0);
    }
    let p = ternarygrid_ui::pal(ui);
    kit::semibold_label(ui, title, 13.0, p.text);
}

/// A form row of the editor: the label in a column of 120, the controls 16 points further on a
/// 30 point line, the label centred on it.
fn row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    row_h(ui, label, look::FIELD_H, add)
}

/// [`row`] with controls `h` high (radio buttons 16, day toggles 26, a check box 17).
fn row_h<R>(ui: &mut Ui, label: &str, h: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut add = Some(add);
    let mut out = None;
    row_dyn(ui, label, h, &mut |ui| out = add.take().map(|a| a(ui)));
    out.expect("a row's controls are drawn once")
}

fn row_dyn(ui: &mut Ui, label: &str, h: f32, add: &mut dyn FnMut(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        // The label centred on the controls' line (on the first line of a taller box).
        let line = h.min(look::FIELD_H);
        let line = if label.is_empty() {
            ui.allocate_exact_size(egui::vec2(LABEL_W, line), egui::Sense::hover());
            h
        } else {
            h.max(look::form_label(ui, label, LABEL_W, line))
        };
        let w = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(w, line), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            add(ui);
        });
    });
}

/// The schema: a 200 point field; with the connection connected, a caret inside it lists
/// the schemas.
fn schema_input(ui: &mut Ui, cx: &mut AppContext, conn: &str, value: &mut String) {
    let p = ternarygrid_ui::pal(ui);
    let field = look::text_field(ui, value, 200.0, tr("Default schema"), false);
    if !cx.conns.is_connected(conn) {
        return;
    }
    let backend = cx.backend;
    let Some(live) = cx.conns.live_mut(conn) else { return };
    let caret = egui::Rect::from_center_size(egui::pos2(field.rect.right() - 15.0, field.rect.center().y), egui::vec2(22.0, 22.0));
    let r = ui.interact(caret, field.id.with("schemas"), egui::Sense::click()).on_hover_text(tr("Choose a schema"));
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Choose a schema")));
    ui.painter().text(caret.center(), egui::Align2::CENTER_CENTER, icon::CARET_DOWN, ternarygrid_ui::style::icon_font(13.0), if r.hovered() { p.text } else { p.text_muted });
    egui::Popup::menu(&r).show(|ui| {
        kit::menu_look(ui, 200.0);
        let main = live.main.clone();
        live.meta.request_schemas(backend, &main, None);
        match live.meta.schemas_of(None).get() {
            Some(list) => {
                egui::ScrollArea::vertical().id_salt("task_schema_list").max_height(280.0).show(ui, |ui| {
                    for s in list.iter().filter(|s| !s.is_system) {
                        let mut on = *value == s.name;
                        if kit::menu_check(ui, &mut on, &s.name).clicked() {
                            *value = s.name.clone();
                            ui.close();
                        }
                    }
                });
            }
            None => {
                ui.spinner();
            }
        }
    });
}

/// SQL as the design's code block: bordered (radius 8), line numbers in a 46 point gutter,
/// 20 points a line, as tall as the text (two lines at least), highlighted for `conn`.
fn code_area(ui: &mut Ui, cx: &AppContext, conn: &str, text: &mut String, hint: &str) -> egui::Response {
    use ternarygrid_editor::EditorLanguage;
    let pal = ternarygrid_ui::pal(ui);
    let lines = text.split('\n').count().max(1);
    let dialect = cx.conns.live(conn).map(|l| l.dialect.clone());
    let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
    let font = egui::FontId::monospace(12.5);
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
        let tokens = dialect.as_ref().map(|d| crate::sqllang::SqlLanguage::new(d.clone()).highlight(buf.as_str())).unwrap_or_default();
        let mut job = crate::sqllang::highlight_job(buf.as_str(), &tokens, &theme, font.clone());
        for s in &mut job.sections {
            s.format.line_height = Some(20.0);
        }
        ui.fonts_mut(|f| f.layout_job(job))
    };
    ternarygrid_ui::parts::bordered(ui)
        .show(ui, |ui| {
            let r = ui.add(
                egui::TextEdit::multiline(text)
                    .code_editor()
                    // A custom frame's margin is the text's margin (`margin` is ignored with it).
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: 46, right: 12, top: 10, bottom: 10 }))
                    .desired_rows(lines.max(2))
                    // Lines of 20 (egui counts the font's own line height).
                    .min_size(egui::vec2(0.0, lines.max(2) as f32 * 20.0 + 20.0))
                    .desired_width(f32::INFINITY)
                    .hint_text(hint)
                    .layouter(&mut layouter),
            );
            let gutter = egui::FontId::monospace(12.5);
            for i in 0..lines {
                let y = r.rect.top() + 10.0 + i as f32 * 20.0 + 10.0;
                ui.painter().text(egui::pos2(r.rect.left() + 32.0, y), egui::Align2::RIGHT_CENTER, (i + 1).to_string(), gutter.clone(), pal.text_faint);
            }
            r
        })
        .inner
}

fn warn(ui: &mut Ui, text: &str) {
    ternarygrid_ui::parts::status(ui, ternarygrid_ui::parts::Tone::Warning, text);
}

fn path_row(ui: &mut Ui, value: &mut String, hint: &str, save: bool) {
    ui.horizontal(|ui| {
        look::text_field(ui, value, 300.0, hint, false);
        if kit::button_ex(ui, Kind::Secondary, Some(icon::FOLDER_OPEN), tr("Browse…"), true).clicked() {
            let d = dialog_near(value);
            let picked = if save { d.save_file() } else { d.pick_file() };
            if let Some(p) = picked {
                *value = p.display().to_string();
            }
        }
    });
}

/// Schedule form with validation, description and the next five runs.
pub(super) fn schedule_ui(ui: &mut Ui, s: &mut ScheduleDraft) {
    let p = ternarygrid_ui::pal(ui);
    // The modes' pill group spans the row (only its first part holds the modes).
    let at = ui.cursor().min;
    ui.painter().rect_filled(egui::Rect::from_min_size(at, egui::vec2(ui.available_width(), ternarygrid_ui::style::CONTROL_H)), 7, p.subtle);
    ternarygrid_ui::parts::segmented(
        ui,
        &mut s.mode,
        &[
            (ScheduleMode::None, tr("Manual only")),
            (ScheduleMode::Cron, tr("Cron expression")),
            (ScheduleMode::Interval, tr("Interval")),
            (ScheduleMode::Daily, tr("Daily")),
            (ScheduleMode::Once, tr("Once")),
        ],
    );
    match s.mode {
        ScheduleMode::None => {
            ui.label(RichText::new(tr("The task runs only when started manually.")).color(p.text_muted));
        }
        ScheduleMode::Cron => {
            ui.horizontal(|ui| {
                // The syntax is explained on the field's hover (the design shows only the field).
                look::text_field(ui, &mut s.cron, 200.0, "m h dom mon dow", true)
                    .on_hover_text(tr("minute hour day-of-month month day-of-week, e.g. 30 9 * * 1-5 (also @hourly, @daily, @weekly, @monthly)"));
                let b = kit::button_ex(ui, Kind::Ghost, Some(icon::CARET_DOWN), tr("Examples"), true);
                egui::Popup::menu(&b).show(|ui| {
                    kit::menu_look(ui, 260.0);
                    for expr in CRON_PRESETS {
                        if kit::menu_item(ui, None, preset_label(expr), expr).clicked() {
                            s.cron = expr.to_string();
                            ui.close();
                        }
                    }
                });
            });
        }
        ScheduleMode::Interval => {
            row(ui, tr("Every"), |ui| {
                ui.add_sized(egui::vec2(80.0, ternarygrid_ui::style::CONTROL_H), egui::DragValue::new(&mut s.every).range(1..=100_000).speed(1));
                egui::ComboBox::from_id_salt("sched_unit").icon(kit::combo_caret).popup_style(egui::containers::menu::menu_style.into()).selected_text(s.unit.label()).show_ui(
                    ui,
                    |ui| {
                        kit::menu_look(ui, 140.0);
                        for u in IntervalUnit::ALL {
                            let mut on = s.unit == u;
                            if kit::menu_check(ui, &mut on, u.label()).clicked() {
                                s.unit = u;
                            }
                        }
                    },
                );
            });
        }
        ScheduleMode::Daily => {
            row(ui, tr("At"), |ui| {
                look::text_field(ui, &mut s.daily_at, 70.0, "HH:MM", false);
                ui.label(RichText::new(tr("local time")).color(p.text_muted));
            });
            row_h(ui, tr("Days"), 26.0, |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (i, name) in weekday_names().iter().enumerate() {
                    look::day_toggle(ui, &mut s.weekdays[i], name);
                }
                if !s.weekdays.iter().any(|d| *d) {
                    ui.add_space(6.0);
                    ui.label(RichText::new(tr("every day")).color(p.text_muted));
                }
            });
        }
        ScheduleMode::Once => {
            row(ui, tr("At"), |ui| {
                look::text_field(ui, &mut s.once_at, 150.0, "YYYY-MM-DD HH:MM", false);
                ui.label(RichText::new(tr("local time")).color(p.text_muted));
            });
        }
    }
    if s.mode == ScheduleMode::None {
        return;
    }
    match s.preview(Local::now(), 5) {
        Err(e) => {
            ternarygrid_ui::parts::status(ui, ternarygrid_ui::parts::Tone::Error, &e);
        }
        Ok(times) => {
            if let Ok(Some(sched)) = s.to_schedule() {
                ternarygrid_ui::parts::status(ui, ternarygrid_ui::parts::Tone::Success, &sched.describe());
            }
            if times.is_empty() {
                ui.label(RichText::new(tr("No upcoming runs.")).color(p.text_muted));
            } else {
                ui.label(RichText::new(tr("Next runs")).size(12.0).color(p.text_muted));
                let names = weekday_names();
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for t in times {
                        let line = format!("{} {}", names[t.weekday().num_days_from_monday() as usize], t.format("%Y-%m-%d %H:%M:%S"));
                        ui.label(look::mono(ui, &line, p.text_muted).size(11.5));
                    }
                });
            }
        }
    }
}

impl TaskEditor {
    pub fn new(draft: TaskDraft) -> Self {
        TaskEditor { draft, error: None, saving: false, add_step: None, exe: system::current_exe().ok() }
    }

    fn kind_title(&self) -> (&'static str, &'static str) {
        match self.draft.kind.to_kind() {
            Ok(k) => (kind_icon(&k), kind_label(&k)),
            Err(_) => match &self.draft.kind {
                KindDraft::SqlScript(_) => (icon::FILE_SQL, tr("SQL script")),
                KindDraft::Shell { .. } => (icon::TERMINAL, tr("Shell command")),
                KindDraft::Backup { .. } => (icon::ARCHIVE, tr("Backup")),
                KindDraft::Restore { .. } => (icon::ARROW_COUNTER_CLOCKWISE, tr("Restore")),
                KindDraft::SchemaCompare(_) => (icon::GIT_DIFF, tr("Schema compare")),
                KindDraft::Composite { .. } => (icon::STACK, tr("Composite task")),
                KindDraft::External(k) => (kind_icon(k), kind_label(k)),
            },
        }
    }

    /// Problem with the system scheduler entry (blocks saving).
    fn os_problem(&self, task: &TaskDef) -> Option<String> {
        if !task.os_scheduler {
            return None;
        }
        let exe = self.exe.as_ref()?;
        system::entry_preview(task, exe).err().map(|e| e.message)
    }

    /// Draw the dialog.
    pub fn show(&mut self, ctx: &egui::Context, cx: &mut AppContext, tasks: &[TaskDef], folders: &[String]) -> EditorAction {
        let screen = ctx.content_rect();
        // 720 points wide (a composite task's list: 680).
        let full = if matches!(self.draft.kind, KindDraft::Composite { .. }) { 680.0 } else { 720.0 };
        let width = (screen.width() - 80.0).clamp(420.0, full);
        // The design's body: at most 690 high.
        let max_h = (screen.height() - 210.0).max(200.0);
        let (glyph, kind) = self.kind_title();
        let title = if self.draft.is_new() { tr("New task") } else { tr("Edit task") };
        let saving = self.saving;
        let (save, cancel) = (std::cell::Cell::new(false), std::cell::Cell::new(false));
        let state: std::cell::RefCell<Option<(Result<TaskDef, String>, Option<String>)>> = std::cell::RefCell::new(None);
        // The scroll bar in the right padding: the fields end 20 from the edge, as designed.
        let pad = egui::Margin { left: 20, right: 6, top: 16, bottom: 16 };
        let dialog = ternarygrid_ui::parts::Dialog::new(title).icon(glyph).subtitle(kind).width(width).body_padding(pad);
        let mut footer = |ui: &mut Ui| {
            let st = state.borrow();
            let problem = st.as_ref().and_then(|(_, p)| p.clone());
            let can_save = problem.is_none() && !saving;
            if saving {
                ternarygrid_ui::parts::spinner_label(ui, tr("Saving…"));
            } else if let Some(p) = &problem {
                let pal = ternarygrid_ui::pal(ui);
                ui.add(egui::Label::new(RichText::new(p).color(pal.warn)).truncate());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled_ui(can_save, |ui| kit::button_ex(ui, Kind::Primary, Some(icon::CHECK), tr("Save"), false)).inner.clicked() {
                    save.set(true);
                }
                if kit::button_ex(ui, Kind::Ghost, None, tr("Cancel"), false).clicked() {
                    cancel.set(true);
                }
            });
        };
        let (closed, ()) = look::modal_keep(
            ctx,
            egui::Id::new("task_editor_modal"),
            &dialog,
            |ui| {
                egui::ScrollArea::vertical().id_salt("task_editor_scroll").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    // Room for the scroll bar at the right.
                    ui.set_width(ui.available_width() - 14.0);
                    ternarygrid_ui::style::form_spacing(ui);
                    ui.spacing_mut().item_spacing.y = 10.0;
                    section(ui, tr("General"), true);
                    self.general_ui(ui, folders);
                    section(ui, kind, false);
                    self.kind_ui(ui, cx, tasks);
                    section(ui, tr("Schedule"), false);
                    schedule_ui(ui, &mut self.draft.schedule);
                    section(ui, tr("Options"), false);
                    self.options_ui(ui);
                });
                if let Some(e) = &self.error {
                    ui.add_space(8.0);
                    look::error(ui, e);
                }
                let built = self.draft.to_task();
                let problem = match &built {
                    Ok(t) => self.os_problem(t),
                    Err(e) => Some(e.clone()),
                };
                *state.borrow_mut() = Some((built, problem));
            },
            &mut footer,
        );
        if closed || cancel.get() {
            return EditorAction::Cancel;
        }
        if save.get()
            && let Some((Ok(t), None)) = state.into_inner()
        {
            return EditorAction::Save(Box::new(t));
        }
        EditorAction::None
    }

    fn general_ui(&mut self, ui: &mut Ui, folders: &[String]) {
        let d = &mut self.draft;
        row(ui, tr("Name"), |ui| look::text_field(ui, &mut d.name, ui.available_width(), "", false));
        row(ui, tr("Folder"), |ui| {
            let w = ui.available_width() - if folders.is_empty() { 0.0 } else { 52.0 };
            look::text_field(ui, &mut d.folder, w, tr("Top level"), false);
            if !folders.is_empty() {
                // A select box of 44 × 30 with its chevron.
                let b = ui
                    .add_sized(egui::vec2(44.0, look::FIELD_H), egui::Button::new(RichText::new(icon::CARET_DOWN).font(ternarygrid_ui::style::icon_font(13.0))))
                    .on_hover_text(tr("Folders"));
                egui::Popup::menu(&b).show(|ui| {
                    kit::menu_look(ui, 200.0);
                    if kit::menu_item(ui, Some(icon::LIST_CHECKS), tr("Top level"), "").clicked() {
                        d.folder.clear();
                        ui.close();
                    }
                    for f in folders {
                        if kit::menu_item(ui, Some(icon::FOLDER), f, "").clicked() {
                            d.folder = f.clone();
                            ui.close();
                        }
                    }
                });
            }
        });
        // A 48 point box, its first line in the middle (it grows with more lines).
        row_h(ui, tr("Description"), 48.0, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut d.description)
                    .desired_rows(1)
                    .desired_width(ui.available_width())
                    .margin(egui::Margin { left: 13, right: 10, top: 15, bottom: 15 })
                    .min_size(egui::vec2(0.0, 48.0)),
            );
        });
        row_h(ui, "", 17.0, |ui| look::checkbox_sub(ui, &mut d.enabled, tr("Enabled"), Some(tr("Disabled tasks are not run on their schedule."))));
    }

    fn kind_ui(&mut self, ui: &mut Ui, cx: &mut AppContext, tasks: &[TaskDef]) {
        let own_id = self.draft.original.as_ref().map(|t| t.id.clone());
        match &mut self.draft.kind {
            KindDraft::SqlScript(d) => {
                row(ui, tr("Connection"), |ui| {
                    // The database's logo, then the select filling the row.
                    if let Some(kind) = cx.conns.config(&d.connection_id).map(|c| c.kind)
                        && let Some(img) = crate::dblogo::image(ui.ctx(), kind, 15.0, crate::theme::logo_color(kind, ui.visuals().dark_mode))
                    {
                        ui.add(img);
                    }
                    let w = ui.available_width();
                    connection_combo(ui, cx, "task_sql_conn", &mut d.connection_id, w)
                });
                if let Some(w) = credential_warning(cx, &d.connection_id) {
                    row(ui, "", |ui| warn(ui, w));
                }
                row(ui, tr("Database"), |ui| look::text_field(ui, &mut d.database, 200.0, tr("The connection's database"), false));
                row(ui, tr("Schema"), |ui| {
                    let conn = d.connection_id.clone();
                    schema_input(ui, cx, &conn, &mut d.schema);
                });
                // What tasks leave out of the SQL is said on the source's hover.
                row_h(ui, tr("Source"), 16.0, |ui| look::radios(ui, &mut d.from_file, &[(false, tr("SQL text")), (true, tr("Script file"))]));
                if d.from_file {
                    row(ui, "", |ui| path_row(ui, &mut d.script_path, tr("Script file to run"), false));
                } else {
                    let conn = d.connection_id.clone();
                    code_area(ui, cx, &conn, &mut d.sql, "SELECT …;").on_hover_text(tr("Client commands such as @set or @include are not run by tasks."));
                }
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 20.0;
                    look::checkbox(ui, &mut d.stop_on_error, tr("Stop at the first failing statement"));
                    look::checkbox(ui, &mut d.commit_at_end, tr("Commit at the end (connections in manual commit mode)"));
                });
            }
            KindDraft::Shell { command } => {
                ternarygrid_ui::parts::bordered(ui).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(command)
                            .code_editor()
                            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 10)))
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text(tr("Command line")),
                    );
                });
                ui.label(
                    RichText::new(tr("Runs with the system shell (sh -c, cmd /C on Windows); the output goes to the run log."))
                        .size(12.0)
                        .color(ternarygrid_ui::pal(ui).text_muted),
                );
            }
            KindDraft::Backup { connection_id, form } => {
                row(ui, tr("Connection"), |ui| connection_combo(ui, cx, "task_backup_conn", connection_id, 260.0));
                if let Some(w) = credential_warning(cx, connection_id) {
                    warn(ui, w);
                }
                let flavor = cx.conns.config(connection_id).map(|c| ToolFlavor::of(c.kind)).unwrap_or(ToolFlavor::Other);
                form.ui(ui, flavor, "task_backup");
            }
            KindDraft::Restore { connection_id, form } => {
                row(ui, tr("Connection"), |ui| connection_combo(ui, cx, "task_restore_conn", connection_id, 260.0));
                if let Some(w) = credential_warning(cx, connection_id) {
                    warn(ui, w);
                }
                let flavor = cx.conns.config(connection_id).map(|c| ToolFlavor::of(c.kind)).unwrap_or(ToolFlavor::Other);
                form.ui(ui, flavor, "task_restore");
            }
            KindDraft::SchemaCompare(d) => {
                row(ui, tr("Source"), |ui| {
                    connection_combo(ui, cx, "task_cmp_src", &mut d.source_connection, 200.0);
                    let conn = d.source_connection.clone();
                    schema_field(ui, cx, "task_cmp_src_schema", &conn, &mut d.source_schema);
                });
                row(ui, tr("Target"), |ui| {
                    connection_combo(ui, cx, "task_cmp_dst", &mut d.target_connection, 200.0);
                    let conn = d.target_connection.clone();
                    schema_field(ui, cx, "task_cmp_dst_schema", &conn, &mut d.target_schema);
                });
                row(ui, tr("Migration script"), |ui| path_row(ui, &mut d.script_path, tr("optional: write the script to this file"), true));
                row(ui, tr("HTML report"), |ui| path_row(ui, &mut d.report_path, tr("optional: write a report to this file"), true));
                for conn in [d.source_connection.clone(), d.target_connection.clone()] {
                    if let Some(w) = credential_warning(cx, &conn) {
                        warn(ui, w);
                        break;
                    }
                }
                look::checkbox(ui, &mut d.drop_removed, tr("Drop objects missing in the source"));
                look::checkbox(ui, &mut d.apply, tr("Apply the migration script to the target"));
                if d.apply {
                    warn(ui, tr("Every run changes the target schema without asking."));
                }
            }
            KindDraft::Composite { steps, stop_on_error, parallel } => {
                let name_of = |id: &str| tasks.iter().find(|t| t.id == id).map(|t| (kind_icon(&t.kind), t.path()));
                let mut action: Option<(usize, i32)> = None;
                let pal = ternarygrid_ui::pal(ui);
                if steps.is_empty() {
                    ui.label(RichText::new(tr("Add the tasks to run.")).color(pal.text_muted));
                } else {
                    let n = steps.len();
                    ternarygrid_ui::parts::bordered(ui).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (i, id) in steps.iter().enumerate() {
                            // Rows of 32 and their hairline, the parts 8 apart, as designed.
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 33.0), egui::Sense::hover());
                            if i + 1 < n {
                                ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, pal.line));
                            }
                            let mut child =
                                ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(8.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
                            let ui = &mut child;
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.label(RichText::new(format!("{}.", i + 1)).color(pal.text_faint));
                            let found = name_of(id);
                            let (glyph, color) = found.as_ref().map_or((icon::X_CIRCLE, pal.err), |(g, _)| (*g, pal.text_muted));
                            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, glyph, ternarygrid_ui::style::icon_font(14.0), color);
                            match found {
                                Some((_, name)) => {
                                    ui.add(egui::Label::new(name).truncate());
                                }
                                None => {
                                    ui.label(RichText::new(tr("Missing task")).color(pal.err));
                                }
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if kit::icon_button_sized(ui, icon::X, tr("Remove"), false, 24.0, 12.0).clicked() {
                                    action = Some((i, 0));
                                }
                                if ui.add_enabled_ui(i + 1 < n, |ui| kit::icon_button_sized(ui, icon::ARROW_DOWN, tr("Move down"), false, 24.0, 12.0)).inner.clicked() {
                                    action = Some((i, 1));
                                }
                                if ui.add_enabled_ui(i > 0, |ui| kit::icon_button_sized(ui, icon::ARROW_UP, tr("Move up"), false, 24.0, 12.0)).inner.clicked() {
                                    action = Some((i, -1));
                                }
                            });
                        }
                    });
                }
                match action {
                    Some((i, 0)) => {
                        steps.remove(i);
                    }
                    Some((i, d)) => {
                        let j = (i as i32 + d) as usize;
                        steps.swap(i, j);
                    }
                    None => {}
                }
                ui.horizontal(|ui| {
                    let candidates: Vec<&TaskDef> = tasks.iter().filter(|t| Some(&t.id) != own_id.as_ref()).collect();
                    let label = self.add_step.as_ref().and_then(|id| candidates.iter().find(|t| &t.id == id)).map(|t| t.path()).unwrap_or_else(|| tr("Choose a task…").to_string());
                    egui::ComboBox::from_id_salt("task_add_step")
                        .icon(kit::combo_caret)
                        .popup_style(egui::containers::menu::menu_style.into())
                        .selected_text(label)
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            kit::menu_look(ui, 260.0);
                            for t in &candidates {
                                if kit::menu_item(ui, Some(kind_icon(&t.kind)), &t.path(), "").clicked() {
                                    self.add_step = Some(t.id.clone());
                                }
                            }
                        });
                    if ui.add_enabled_ui(self.add_step.is_some(), |ui| kit::button_ex(ui, Kind::Secondary, Some(icon::PLUS), tr("Add step"), true)).inner.clicked()
                        && let Some(id) = self.add_step.take()
                    {
                        steps.push(id);
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 20.0;
                    look::checkbox(ui, parallel, tr("Run the steps in parallel"));
                    look::checkbox(ui, stop_on_error, if *parallel { tr("Cancel the other steps when a step fails") } else { tr("Stop at the first failing step") });
                });
            }
            KindDraft::External(kind) => {
                look::note(
                    ui,
                    ternarygrid_ui::parts::Tone::Info,
                    tr("This task was created by a transfer wizard; its settings are shown read-only. Create it again from the wizard to change them."),
                );
                let conns: Vec<String> = kind.connection_ids().iter().map(|c| cx.conns.config(c).map(|c| c.name.clone()).unwrap_or_else(|| c.to_string())).collect();
                if !conns.is_empty() {
                    ui.label(trf("Connections: {}", &[&conns.join(", ")]));
                }
                let json = match kind {
                    TaskKind::Export { config, .. } | TaskKind::Import { config, .. } | TaskKind::Migration { config, .. } | TaskKind::SchemaCompare { config } => {
                        serde_json::to_string_pretty(config).unwrap_or_default()
                    }
                    other => serde_json::to_string_pretty(other).unwrap_or_default(),
                };
                ternarygrid_ui::parts::bordered(ui).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("task_external_json").max_height(220.0).show(ui, |ui| {
                        let mut t = json.as_str();
                        ui.add(egui::TextEdit::multiline(&mut t).code_editor().frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 10))).desired_width(f32::INFINITY));
                    });
                });
            }
        }
    }

    fn options_ui(&mut self, ui: &mut Ui) {
        let d = &mut self.draft;
        let pal = ternarygrid_ui::pal(ui);
        ui.horizontal(|ui| {
            look::checkbox(ui, &mut d.timeout, tr("Time limit"));
            ui.add_enabled_ui(d.timeout, |ui| look::number(ui, &mut d.timeout_secs, 1..=7 * 86_400, "s"));
            if d.timeout {
                ui.label(RichText::new(crate::widgets::format_duration(std::time::Duration::from_secs(d.timeout_secs))).color(pal.text_faint));
            }
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.label(RichText::new(tr("Notify")).color(pal.text_muted));
            look::checkbox(ui, &mut d.notify_success, tr("when a run succeeds"));
            look::checkbox(ui, &mut d.notify_failure, tr("when a run fails"));
        });
        let schedule = d.schedule.to_schedule().ok().flatten();
        let r = ui.add_enabled_ui(schedule.is_some(), |ui| look::checkbox(ui, &mut d.os_scheduler, tr("Also run when TernaryGrid is closed (system scheduler)")));
        r.inner.on_disabled_hover_text(tr("Needs a schedule"));
        if !(d.os_scheduler && schedule.is_some()) {
            return;
        }
        ui.indent("task_os", |ui| {
            let text = trf(
                "The schedule is registered with {} when you save. It starts TernaryGrid without a window (--run-task) and uses saved passwords only.",
                &[&system::scheduler_name()],
            );
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(pal.text_muted)).wrap());
            if matches!(schedule, Some(Schedule::Once { .. })) {
                warn(ui, tr("The system scheduler repeats a one-time schedule every year on that date; remove the option after the run."));
            }
            let task = self.draft.to_task();
            match (&task, &self.exe) {
                (Ok(t), Some(exe)) => match system::entry_preview(t, exe) {
                    Ok(text) => {
                        egui::CollapsingHeader::new(tr("System scheduler entry")).id_salt("task_os_entry").show(ui, |ui| {
                            egui::ScrollArea::vertical().id_salt("task_os_entry_scroll").max_height(180.0).show(ui, |ui| {
                                let mut s = text.as_str();
                                ui.add(egui::TextEdit::multiline(&mut s).code_editor().desired_width(f32::INFINITY));
                            });
                        });
                    }
                    Err(e) => {
                        ternarygrid_ui::parts::status(ui, ternarygrid_ui::parts::Tone::Error, &e.message);
                    }
                },
                (_, None) => warn(ui, tr("The TernaryGrid program could not be located.")),
                _ => {}
            }
        });
    }
}
