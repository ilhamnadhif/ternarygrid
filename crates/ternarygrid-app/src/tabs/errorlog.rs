//! Error log view: the application log (errors and warnings of every
//! component, connection failures, info messages) with a level filter,
//! search, copy and the log file location.

use crate::applog::{self, LogEntry};
use crate::cx::AppContext;
use crate::tabs::TabView;
use egui::{Color32, RichText, Ui};
use egui_phosphor::regular as icon;
use ternarygrid_core::tr;
use tracing::Level;

pub struct ErrorLogTab {
    /// Least severe level shown.
    level: Level,
    search: String,
    entries: Vec<LogEntry>,
    version: Option<u64>,
}

impl Default for ErrorLogTab {
    fn default() -> Self {
        ErrorLogTab { level: Level::WARN, search: String::new(), entries: Vec::new(), version: None }
    }
}

impl ErrorLogTab {
    pub fn new() -> Self {
        Self::default()
    }

    /// The entries shown with the current filter, newest first.
    pub fn visible(&self) -> Vec<&LogEntry> {
        let needle = self.search.to_lowercase();
        self.entries
            .iter()
            .rev()
            .filter(|e| e.level <= self.level && (needle.is_empty() || e.message.to_lowercase().contains(&needle) || e.target.to_lowercase().contains(&needle)))
            .collect()
    }

    pub fn set_level(&mut self, level: Level) {
        self.level = level;
    }
}

fn level_color(level: Level, p: &ternarygrid_ui::Palette) -> Color32 {
    match level {
        Level::ERROR => p.err,
        Level::WARN => p.warn,
        _ => p.text_muted,
    }
}

fn level_label(level: Level) -> &'static str {
    match level {
        Level::ERROR => "Errors",
        Level::WARN => "Errors and warnings",
        _ => "Everything",
    }
}

impl TabView for ErrorLogTab {
    fn title(&self) -> String {
        tr("Error log").to_string()
    }

    fn icon(&self) -> &'static str {
        icon::WARNING_OCTAGON
    }

    fn dedupe_key(&self) -> Option<String> {
        Some("error-log".into())
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let _ = cx;
        let v = applog::version();
        if self.version != Some(v) {
            self.entries = applog::entries();
            self.version = Some(v);
        }
        // new entries appear while the tab is open
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        let pal = ternarygrid_ui::pal(ui);
        ui.spacing_mut().item_spacing.y = 0.0;
        ternarygrid_ui::parts::toolbar(ui, |ui| {
            egui::ComboBox::from_id_salt("error_log_level").icon(ternarygrid_ui::kit::combo_caret).width(180.0).selected_text(tr(level_label(self.level))).show_ui(ui, |ui| {
                for l in [Level::ERROR, Level::WARN, Level::INFO] {
                    ui.selectable_value(&mut self.level, l, tr(level_label(l)));
                }
            });
            ternarygrid_ui::kit::search_field(ui, egui::Id::new("error_log_search"), &mut self.search, tr("Search"), 250.0);
            if ternarygrid_ui::kit::ghost(ui, Some(icon::COPY), tr("Copy")).on_hover_text(tr("Copy the shown entries")).clicked() {
                let text: Vec<String> = self.visible().iter().rev().map(|e| e.line()).collect();
                ui.ctx().copy_text(text.join("\n"));
            }
            if ternarygrid_ui::kit::ghost(ui, Some(icon::TRASH), tr("Clear")).clicked() {
                applog::clear();
            }
            if let Some(file) = applog::log_file()
                && ternarygrid_ui::kit::ghost(ui, Some(icon::FOLDER_OPEN), tr("Show log file")).on_hover_text(file.display().to_string()).clicked()
            {
                let _ = open::that_detached(file.parent().unwrap_or(&file));
            }
        });
        let shown = self.visible();
        if shown.is_empty() {
            ternarygrid_ui::kit::empty_state(ui, icon::CHECK_CIRCLE, tr("No entries."), tr("Errors and warnings of this session appear here."), |_| {});
            return;
        }
        let row_h = 30.0;
        let mono = egui::FontId::monospace(12.0);
        ui.painter().rect_filled(ui.available_rect_before_wrap(), 0.0, pal.surface);
        ternarygrid_ui::kit::show_rows(egui::ScrollArea::both().auto_shrink([false, false]), ui, row_h, shown.len(), |ui, range| {
            for e in &shown[range] {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
                let painter = ui.painter();
                painter.hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, ternarygrid_ui::soft(pal.border, 0.75)));
                let cy = rect.center().y;
                let t = painter.text(egui::pos2(rect.left() + 16.0, cy), egui::Align2::LEFT_CENTER, e.time.format("%H:%M:%S").to_string(), mono.clone(), pal.text_faint);
                let level_font = egui::FontId::new(12.0, egui::FontFamily::Monospace);
                painter.text(egui::pos2(t.right() + 14.0, cy), egui::Align2::LEFT_CENTER, format!("{}", e.level), level_font, level_color(e.level, &pal));
                let x = t.right() + 14.0 + 62.0;
                let first = e.message.lines().next().unwrap_or("");
                let size = ternarygrid_ui::style::body_size(ui.ctx());
                let g = egui::WidgetText::from(RichText::new(first).size(size).color(pal.text)).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Truncate),
                    (rect.right() - x - 16.0).max(40.0),
                    egui::TextStyle::Body,
                );
                let truncated = g.elided;
                ui.painter().galley(egui::pos2(x, cy - g.size().y / 2.0), g, pal.text);
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, first));
                // The full time, and for long entries where they come from and the whole message.
                let when = e.time.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
                if e.message.contains('\n') || truncated {
                    resp.on_hover_text(format!("{when}  {}\n\n{}", e.target, e.message));
                } else {
                    resp.on_hover_text(when);
                }
            }
        });
    }

    crate::impl_tab_any!();
}
