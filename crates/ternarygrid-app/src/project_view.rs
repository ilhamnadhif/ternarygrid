//! Project panel next to the database navigator: scripts (with folders),
//! bookmarks and saved ER diagrams of the default project, in collapsible
//! sections of tree rows.

use crate::command::{AppCommand, ObjectPage};
use crate::cx::AppContext;
use crate::tabs::tasks::look;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use ternarygrid_config::Bookmark;
use ternarygrid_config::project::ScriptFolder;
use ternarygrid_core::tr;

#[derive(Default)]
pub struct ProjectView {
    scripts: Option<ScriptFolder>,
    bookmarks: Vec<Bookmark>,
    diagrams: Vec<String>,
    loaded: Option<Instant>,
    filter: String,
    error: Option<String>,
    /// Name typed for a new project.
    new_project: Option<String>,
    /// Folded sections ("scripts", "bookmarks", "diagrams").
    folded: HashSet<&'static str>,
    /// Unfolded script folders (relative paths).
    open_folders: HashSet<PathBuf>,
    /// The script, bookmark or diagram clicked last.
    selected: Option<String>,
}

/// A section's header: a caret, its icon and its name in small semibold; a click folds it.
fn section(ui: &mut Ui, folded: &mut HashSet<&'static str>, key: &'static str, glyph: &str, title: &str) -> bool {
    let p = ternarygrid_ui::pal(ui);
    let open = !folded.contains(key);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    if resp.clicked() {
        if open {
            folded.insert(key);
        } else {
            folded.remove(key);
        }
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let cy = rect.center().y;
        let color = if resp.hovered() { p.text } else { p.text_muted };
        painter.text(
            egui::pos2(rect.left() + 19.0, cy),
            egui::Align2::CENTER_CENTER,
            if open { icon::CARET_DOWN } else { icon::CARET_RIGHT },
            ternarygrid_ui::style::icon_font(11.0),
            p.text_faint,
        );
        painter.text(egui::pos2(rect.left() + 34.0, cy), egui::Align2::CENTER_CENTER, glyph, ternarygrid_ui::style::icon_font(13.0), color);
        painter.text(egui::pos2(rect.left() + 46.0, cy), egui::Align2::LEFT_CENTER, title, ternarygrid_ui::style::semibold(ui.ctx(), 12.0), color);
        if resp.has_focus() {
            painter.rect_stroke(rect.shrink2(egui::vec2(6.0, 0.0)), ternarygrid_ui::style::RADIUS, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
        }
    }
    open
}

/// A faint line in a section (its empty state).
fn hint(ui: &mut Ui, text: &str) {
    let p = ternarygrid_ui::pal(ui);
    ui.horizontal(|ui| {
        ui.add_space(36.0);
        ui.add(egui::Label::new(RichText::new(text).size(12.0).color(p.text_faint)).wrap());
    });
}

impl ProjectView {
    fn reload(&mut self, cx: &AppContext) {
        self.loaded = Some(Instant::now());
        let Some(p) = cx.services.projects.as_ref().and_then(|m| m.default_project().ok()) else {
            self.error = Some(tr("The project folder is not available.").to_string());
            return;
        };
        self.error = None;
        self.scripts = p.script_tree().ok();
        self.bookmarks = p.bookmarks().unwrap_or_default();
        self.diagrams = p.list_diagrams().unwrap_or_default();
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if self.loaded.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
            self.reload(cx);
        }
        let pal = ternarygrid_ui::pal(ui);
        ui.add_space(4.0);
        ui.spacing_mut().interact_size.y = ternarygrid_ui::style::CONTROL_H;
        if let Some(pm) = cx.services.projects.clone() {
            let active = pm.active_name();
            egui::ComboBox::from_id_salt("project_switch").icon(ternarygrid_ui::kit::combo_caret).selected_text(active.clone()).width(ui.available_width() - 4.0).show_ui(
                ui,
                |ui| {
                    for p in pm.list().unwrap_or_default() {
                        if ui.selectable_label(p.name() == active, p.name()).clicked() && p.name() != active {
                            cx.push(AppCommand::SetActiveProject(p.name().to_string()));
                            self.loaded = None;
                        }
                    }
                    ui.separator();
                    if ui.button((ternarygrid_icons::ic(icon::PLUS), tr("New project…"))).clicked() {
                        self.new_project = Some(String::new());
                    }
                },
            );
            if let Some(name) = &mut self.new_project {
                let mut done = false;
                ui.horizontal(|ui| {
                    let w = ui.available_width() - 90.0;
                    let r = look::text_field(ui, name, w, tr("Project name"), false);
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ternarygrid_ui::kit::button_ex(ui, ternarygrid_ui::kit::Kind::Primary, None, tr("Create"), true).clicked() || enter) && !name.trim().is_empty() {
                        match pm.create(name.trim()) {
                            Ok(p) => {
                                cx.push(AppCommand::SetActiveProject(p.name().to_string()));
                                self.loaded = None;
                                done = true;
                            }
                            Err(e) => cx.toasts.error(tr("Cannot create the project"), Some(e.to_string())),
                        }
                    }
                    if ternarygrid_ui::kit::icon_button_ex(ui, icon::X, tr("Cancel"), false, 24.0).clicked() {
                        done = true;
                    }
                });
                if done {
                    self.new_project = None;
                }
            }
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if ternarygrid_ui::kit::icon_button_ex(ui, icon::FILE_PLUS, tr("New SQL editor"), false, 26.0).clicked() {
                cx.push(AppCommand::NewSqlEditor { connection: None, schema: None, text: None });
            }
            if ternarygrid_ui::kit::icon_button_ex(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), false, 26.0).clicked() {
                self.reload(cx);
            }
            if let Some(p) = cx.services.projects.as_ref().and_then(|m| m.default_project().ok())
                && ternarygrid_ui::kit::icon_button_ex(ui, icon::FOLDER_OPEN, tr("Show in Finder"), false, 26.0).clicked()
            {
                let _ = open::that(p.path());
            }
        });
        ternarygrid_ui::kit::search_field(ui, egui::Id::new("project_filter"), &mut self.filter, tr("Filter"), ui.available_width());
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(pal.text_muted));
        }
        ui.add_space(2.0);
        let filter = self.filter.to_lowercase();
        // The rows reach the panel's edges (their own margins inset them).
        let full = ui.available_rect_before_wrap().expand2(egui::vec2(6.0, 0.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(full).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut child;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            if section(ui, &mut self.folded, "scripts", icon::FILE_SQL, tr("Scripts"))
                && let Some(root) = self.scripts.clone()
            {
                self.folder(ui, cx, &root, &filter, 1);
            }
            if section(ui, &mut self.folded, "bookmarks", icon::PUSH_PIN, tr("Bookmarks")) {
                if self.bookmarks.is_empty() {
                    hint(ui, tr("Right-click an object in the navigator to bookmark it."));
                }
                let mut remove: Option<String> = None;
                for b in &self.bookmarks {
                    if !filter.is_empty() && !b.label.to_lowercase().contains(&filter) {
                        continue;
                    }
                    let conn_name = cx.conns.config(&b.connection_id).map(|c| c.name.clone()).unwrap_or_default();
                    let key = format!("bookmark:{}", b.id);
                    let row = look::tree_row(ui, 1, None, crate::nav::kind_icon(b.object.kind), &b.label, None, self.selected.as_deref() == Some(key.as_str()), false);
                    let r = row.resp.on_hover_text(&conn_name);
                    if r.clicked() {
                        self.selected = Some(key);
                    }
                    if r.double_clicked() {
                        if !cx.conns.is_connected(&b.connection_id) {
                            cx.push(AppCommand::Connect(b.connection_id.clone()));
                        }
                        let page = if b.object.kind.has_data() { ObjectPage::Data } else { ObjectPage::Properties };
                        cx.push(AppCommand::OpenObject { connection: b.connection_id.clone(), object: b.object.clone(), page });
                    }
                    r.context_menu(|ui| {
                        if ui.button(tr("Remove bookmark")).clicked() {
                            remove = Some(b.id.clone());
                            ui.close();
                        }
                    });
                }
                if let Some(id) = remove {
                    if let Some(p) = cx.services.projects.as_ref().and_then(|m| m.default_project().ok()) {
                        let _ = p.remove_bookmark(&id);
                    }
                    self.loaded = None;
                }
            }
            if section(ui, &mut self.folded, "diagrams", icon::GRAPH, tr("Diagrams")) {
                if self.diagrams.is_empty() {
                    hint(ui, tr("Saved ER diagrams appear here."));
                }
                let dir = cx.services.projects.as_ref().and_then(|m| m.default_project().ok()).map(|p| p.diagrams_dir());
                for d in &self.diagrams {
                    if !filter.is_empty() && !d.to_lowercase().contains(&filter) {
                        continue;
                    }
                    let key = format!("diagram:{d}");
                    let row = look::tree_row(ui, 1, None, icon::GRAPH, d, None, self.selected.as_deref() == Some(key.as_str()), false);
                    if row.resp.clicked() {
                        self.selected = Some(key);
                    }
                    if row.resp.double_clicked()
                        && let Some(dir) = &dir
                    {
                        cx.push(AppCommand::OpenSavedDiagram(dir.join(format!("{d}.json"))));
                    }
                }
            }
        });
    }

    fn folder(&mut self, ui: &mut Ui, cx: &mut AppContext, f: &ScriptFolder, filter: &str, depth: usize) {
        for sub in &f.folders {
            let key = PathBuf::from(&sub.relative);
            // A filter shows the scripts inside folders too.
            let open = !filter.is_empty() || self.open_folders.contains(&key);
            let row = look::tree_row(ui, depth, Some(open), if open { icon::FOLDER_OPEN } else { icon::FOLDER }, &sub.name, None, false, false);
            if row.caret || (row.resp.clicked() && filter.is_empty()) {
                if self.open_folders.contains(&key) {
                    self.open_folders.remove(&key);
                } else {
                    self.open_folders.insert(key);
                }
            }
            if open {
                self.folder(ui, cx, sub, filter, depth + 1);
            }
        }
        for s in &f.scripts {
            if !filter.is_empty() && !s.name.to_lowercase().contains(filter) {
                continue;
            }
            let key = format!("script:{}", s.path.display());
            let row = look::tree_row(ui, depth, None, icon::FILE_SQL, &s.name, None, self.selected.as_deref() == Some(key.as_str()), false);
            let r = row.resp.on_hover_text(s.path.display().to_string());
            if r.clicked() {
                self.selected = Some(key);
            }
            if r.double_clicked() {
                cx.push(AppCommand::OpenScript(s.path.clone()));
            }
            r.context_menu(|ui| {
                if ui.button(tr("Open")).clicked() {
                    cx.push(AppCommand::OpenScript(s.path.clone()));
                    ui.close();
                }
                if ui.button(tr("Show in Finder")).clicked() {
                    let _ = open::that(s.path.parent().unwrap_or(&s.path));
                    ui.close();
                }
            });
        }
    }
}
