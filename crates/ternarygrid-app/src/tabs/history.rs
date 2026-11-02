//! Query manager (FR-S7): search the history of executed statements and
//! reopen them in an editor.

use crate::command::AppCommand;
use crate::cx::AppContext;
use crate::tabs::TabView;
use crate::tabs::tasks::look;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use std::time::{Duration, Instant};
use ternarygrid_config::{HistoryEntry, HistoryQuery, QueryStatus};
use ternarygrid_core::{tr, trf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Period {
    Day,
    Week,
    Month,
    All,
}

pub struct HistoryTab {
    text: String,
    connection: Option<String>,
    status: Option<QueryStatus>,
    period: Period,
    entries: Vec<HistoryEntry>,
    total: u64,
    selected: Option<usize>,
    dirty_at: Option<Instant>,
    loaded: bool,
    error: Option<String>,
    limit: usize,
}

impl Default for HistoryTab {
    fn default() -> Self {
        HistoryTab {
            text: String::new(),
            connection: None,
            status: None,
            period: Period::Week,
            entries: Vec::new(),
            total: 0,
            selected: None,
            dirty_at: None,
            loaded: false,
            error: None,
            limit: 500,
        }
    }
}

impl HistoryTab {
    pub fn for_connection(connection: Option<String>) -> Self {
        HistoryTab { connection, ..Default::default() }
    }

    fn query(&self) -> HistoryQuery {
        let now = chrono::Utc::now().timestamp_millis();
        let since = match self.period {
            Period::Day => Some(now - 86_400_000),
            Period::Week => Some(now - 7 * 86_400_000),
            Period::Month => Some(now - 30 * 86_400_000),
            Period::All => None,
        };
        HistoryQuery {
            text: Some(self.text.trim().to_string()).filter(|t| !t.is_empty()),
            connection_id: self.connection.clone(),
            since,
            until: None,
            status: self.status,
            limit: self.limit,
            offset: 0,
        }
    }

    fn reload(&mut self, cx: &mut AppContext) {
        self.loaded = true;
        self.dirty_at = None;
        let Some(store) = cx.services.history.clone() else {
            self.error = Some(tr("The query history is disabled.").to_string());
            return;
        };
        let q = self.query();
        let store = store.lock();
        match store.search(&q) {
            Ok(e) => {
                self.entries = e;
                self.total = store.count(&q).unwrap_or(self.entries.len() as u64);
                self.error = None;
                if self.selected.is_some_and(|s| s >= self.entries.len()) {
                    self.selected = None;
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn open_in_editor(&self, cx: &mut AppContext, e: &HistoryEntry) {
        // Where it ran: a DELETE of database `reports` does not reopen on the default one.
        let schema = (e.database.is_some() || e.schema.is_some()).then(|| ternarygrid_core::meta::SchemaRef::new(e.database.clone(), e.schema.clone()));
        cx.push(AppCommand::NewSqlEditor { connection: e.connection_id.clone(), schema, text: Some(e.sql.clone()) });
    }
}

/// `n` with thousands separators ("2,914").
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn status_color(s: QueryStatus, p: &ternarygrid_ui::Palette) -> egui::Color32 {
    match s {
        QueryStatus::Ok => p.ok,
        QueryStatus::Error => p.err,
        QueryStatus::Cancelled => p.warn,
    }
}

fn status_label(s: QueryStatus) -> &'static str {
    match s {
        QueryStatus::Ok => tr("Success"),
        QueryStatus::Error => tr("Error"),
        QueryStatus::Cancelled => tr("Cancelled"),
    }
}

impl TabView for HistoryTab {
    fn title(&self) -> String {
        tr("Query history").to_string()
    }

    fn icon(&self) -> &'static str {
        icon::CLOCK_COUNTER_CLOCKWISE
    }

    fn dedupe_key(&self) -> Option<String> {
        Some("history".into())
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if !self.loaded || self.dirty_at.is_some_and(|t| t.elapsed() > Duration::from_millis(250)) {
            self.reload(cx);
        }
        if let Some(t) = self.dirty_at {
            ui.ctx().request_repaint_after(Duration::from_millis(260).saturating_sub(t.elapsed()));
        }
        let mut changed = false;
        let pal = ternarygrid_ui::pal(ui);
        ui.spacing_mut().item_spacing.y = 0.0;
        ternarygrid_ui::parts::toolbar(ui, |ui| {
            // The selects at the height of the design's fields.
            ui.spacing_mut().interact_size.y = look::FIELD_H;
            changed |= ternarygrid_ui::kit::search_field(ui, egui::Id::new("hist_search"), &mut self.text, tr("Search SQL text…"), 320.0).changed();
            let conn_name = self.connection.as_deref().and_then(|c| cx.conns.config(c)).map(|c| c.name.clone()).unwrap_or_else(|| tr("All connections").to_string());
            egui::ComboBox::from_id_salt("hist_conn").icon(ternarygrid_ui::kit::combo_caret).width(180.0).selected_text(conn_name).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut self.connection, None, tr("All connections")).changed();
                for c in cx.conns.configs() {
                    changed |= ui.selectable_value(&mut self.connection, Some(c.id.clone()), &c.name).changed();
                }
            });
            let st = self.status.map(status_label).unwrap_or(tr("Any status"));
            egui::ComboBox::from_id_salt("hist_status").icon(ternarygrid_ui::kit::combo_caret).width(140.0).selected_text(st).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut self.status, None, tr("Any status")).changed();
                for s in [QueryStatus::Ok, QueryStatus::Error, QueryStatus::Cancelled] {
                    changed |= ui.selectable_value(&mut self.status, Some(s), status_label(s)).changed();
                }
            });
            changed |= ternarygrid_ui::parts::segmented(
                ui,
                &mut self.period,
                &[(Period::Day, tr("24 hours")), (Period::Week, tr("7 days")), (Period::Month, tr("30 days")), (Period::All, tr("All"))],
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ternarygrid_ui::kit::icon_button(ui, icon::ARROW_CLOCKWISE, tr("Refresh")).clicked() {
                    self.reload(cx);
                }
            });
        });
        if changed {
            self.dirty_at = Some(Instant::now());
        }
        egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 8)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            if let Some(e) = &self.error {
                look::error(ui, e);
            }
            let (shown, total) = (grouped(self.entries.len() as u64), grouped(self.total));
            ui.label(RichText::new(trf("{} of {} entries", &[&shown, &total])).size(12.0).color(pal.text_faint));
        });
        let sel_entry = self.selected.and_then(|i| self.entries.get(i).cloned());
        let names: std::collections::HashMap<String, String> = cx.conns.configs().iter().map(|c| (c.id.clone(), c.name.clone())).collect();
        // The details: 12 of margin, 12 of padding, the buttons, 10, the statement's lines.
        let detail_h = sel_entry.as_ref().map_or(0.0, |e| 12.0 * 2.0 + 12.0 * 2.0 + 2.0 + 26.0 + 10.0 + e.sql.lines().count().clamp(1, 6) as f32 * 20.0 + 8.0);
        let avail = (ui.available_height() - detail_h).max(80.0);
        let cols = [
            look::Col::new(tr("Time"), look::Width::Initial(170.0)),
            look::Col::new(tr("Connection"), look::Width::Initial(130.0)),
            look::Col::new(tr("Duration"), look::Width::Initial(90.0)).right(),
            look::Col::new(tr("Rows"), look::Width::Initial(80.0)).right(),
            look::Col::new(tr("Status"), look::Width::Initial(100.0)),
            look::Col::new(tr("SQL"), look::Width::Rest(160.0)),
        ];
        let mut clicked = None;
        let mut open = None;
        let selected = self.selected;
        let entries = &self.entries;
        ui.allocate_ui(egui::vec2(ui.available_width(), avail), |ui| {
            // The table takes the room above the details: they stay at the bottom.
            ui.set_min_height(avail);
            egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 0, bottom: 0 }).show(ui, |ui| {
                look::table(ui, "history_table", &cols, entries.len(), |row, i| {
                    let e = &entries[i];
                    row.set_selected(selected == Some(i));
                    let when = chrono::DateTime::from_timestamp_millis(e.timestamp_ms)
                        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_default();
                    let conn = e.connection_id.as_ref().and_then(|c| names.get(c).cloned()).or_else(|| e.connection_name.clone()).unwrap_or_default();
                    look::cell(row, false, |ui| {
                        look::cell_text(ui, look::mono(ui, &when, pal.text_muted));
                    });
                    look::cell(row, false, |ui| {
                        look::cell_text(ui, conn);
                    });
                    look::cell(row, true, |ui| {
                        look::cell_text(ui, e.duration_ms.map(|d| crate::widgets::format_duration(Duration::from_millis(d))).unwrap_or_default());
                    });
                    look::cell(row, true, |ui| {
                        look::cell_text(ui, e.rows.map(|r| r.to_string()).unwrap_or_else(|| "—".into()));
                    });
                    look::cell(row, false, |ui| {
                        look::cell_text(ui, RichText::new(status_label(e.status)).color(status_color(e.status, &pal)));
                    });
                    look::cell(row, false, |ui| {
                        let sql = e.sql.split_whitespace().collect::<Vec<_>>().join(" ");
                        let r = look::cell_text(ui, look::mono(ui, &sql, pal.text));
                        if e.sql.len() > 120 {
                            r.on_hover_text(e.sql.chars().take(4000).collect::<String>());
                        }
                    });
                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() {
                        clicked = Some(i);
                    }
                    if resp.double_clicked() {
                        open = Some(i);
                    }
                });
            });
        });
        if let Some(i) = clicked {
            self.selected = Some(i);
        }
        if let Some(i) = open
            && let Some(e) = self.entries.get(i).cloned()
        {
            self.open_in_editor(cx, &e);
        }
        if let Some(e) = sel_entry {
            let frame = egui::Frame::new()
                .fill(pal.surface)
                .stroke(egui::Stroke::new(1.0, pal.border))
                .corner_radius(ternarygrid_ui::style::RADIUS_LG)
                .inner_margin(12)
                .outer_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 12 });
            frame.show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if ternarygrid_ui::kit::button_ex(ui, ternarygrid_ui::kit::Kind::Secondary, Some(icon::FILE_CODE), tr("Open in SQL editor"), true).clicked() {
                        self.open_in_editor(cx, &e);
                    }
                    if ternarygrid_ui::kit::ghost(ui, Some(icon::COPY), tr("Copy")).clicked() {
                        ui.ctx().copy_text(e.sql.clone());
                    }
                    if ternarygrid_ui::kit::ghost(ui, Some(icon::TRASH), tr("Delete")).clicked() {
                        if let Some(store) = cx.services.history.clone() {
                            let _ = store.lock().delete(&[e.id]);
                        }
                        self.selected = None;
                        self.reload(cx);
                    }
                    if let Some(err) = &e.error_message {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(err.lines().next().unwrap_or("")).size(12.5).color(pal.err)).truncate());
                        });
                    }
                });
                // The statement as plain highlighted text (20 points a line, at most six shown).
                let conn = e.connection_id.clone().filter(|c| cx.conns.is_connected(c));
                let lines = e.sql.lines().count().clamp(1, 6) as f32;
                ui.allocate_ui(egui::vec2(ui.available_width(), lines * 20.0 + 8.0), |ui| {
                    ui.set_height(lines * 20.0 + 8.0);
                    ui.style_mut().text_styles.insert(egui::TextStyle::Monospace, egui::FontId::monospace(12.5));
                    let v = ui.visuals_mut();
                    v.extreme_bg_color = pal.surface;
                    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
                        w.bg_stroke = egui::Stroke::NONE;
                    }
                    v.selection.stroke = egui::Stroke::NONE;
                    match conn {
                        Some(conn) => crate::tabs::code(ui, cx, &conn, ("hist_sql", e.id), &e.sql),
                        None => crate::widgets::code_view(ui, ("hist_sql", e.id), &e.sql, &[]),
                    }
                });
            });
        }
    }

    crate::impl_tab_any!();
}
