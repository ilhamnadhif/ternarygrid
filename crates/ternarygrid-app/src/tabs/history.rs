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
use ternarygrid_ui::kit;

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
    /// The highlighting of the selected entry's statement (its id).
    tokens: Option<(i64, Vec<(std::ops::Range<usize>, ternarygrid_editor::TokenStyle)>)>,
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
            tokens: None,
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
            // The selects at the height of the design's fields, their text 11 points in.
            ui.spacing_mut().interact_size.y = look::FIELD_H;
            ui.spacing_mut().button_padding = egui::vec2(11.0, 5.0);
            changed |= kit::search_field(ui, egui::Id::new("hist_search"), &mut self.text, tr("Search SQL text…"), 320.0).changed();
            let conn_name = self.connection.as_deref().and_then(|c| cx.conns.config(c)).map(|c| c.name.clone()).unwrap_or_else(|| tr("All connections").to_string());
            egui::ComboBox::from_id_salt("hist_conn").icon(kit::combo_caret).popup_style(egui::containers::menu::menu_style.into()).width(180.0).selected_text(conn_name).show_ui(
                ui,
                |ui| {
                    kit::menu_look(ui, 180.0);
                    let mut on = self.connection.is_none();
                    if kit::menu_check(ui, &mut on, tr("All connections")).clicked() && self.connection.is_some() {
                        self.connection = None;
                        changed = true;
                    }
                    for c in cx.conns.configs() {
                        let mut on = self.connection.as_deref() == Some(c.id.as_str());
                        if kit::menu_check(ui, &mut on, &c.name).clicked() && on {
                            self.connection = Some(c.id.clone());
                            changed = true;
                        }
                    }
                },
            );
            let st = self.status.map(status_label).unwrap_or(tr("Any status"));
            egui::ComboBox::from_id_salt("hist_status").icon(kit::combo_caret).popup_style(egui::containers::menu::menu_style.into()).width(140.0).selected_text(st).show_ui(
                ui,
                |ui| {
                    kit::menu_look(ui, 140.0);
                    for s in [None, Some(QueryStatus::Ok), Some(QueryStatus::Error), Some(QueryStatus::Cancelled)] {
                        let mut on = self.status == s;
                        if kit::menu_check(ui, &mut on, s.map(status_label).unwrap_or(tr("Any status"))).clicked() && on {
                            self.status = s;
                            changed = true;
                        }
                    }
                },
            );
            changed |= ternarygrid_ui::parts::segmented(
                ui,
                &mut self.period,
                &[(Period::Day, tr("24 hours")), (Period::Week, tr("7 days")), (Period::Month, tr("30 days")), (Period::All, tr("All"))],
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::icon_button_sized(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), false, 28.0, 15.0).clicked() {
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
            let (shown, total) = (look::grouped_number(self.entries.len() as f64, true), look::grouped_number(self.total as f64, true));
            let count = if self.total == 1 { trf("{} of 1 entry", &[&shown]) } else { trf("{} of {} entries", &[&shown, &total]) };
            // On the design's line of 19.5 (its text on the baseline of a 16 point line box).
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 19.5), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_min_height(19.5);
                ui.add_space(3.5);
                ui.label(RichText::new(count).size(12.0).color(pal.text_muted));
            });
        });
        let sel_entry = self.selected.and_then(|i| self.entries.get(i).cloned());
        let names: std::collections::HashMap<String, String> = cx.conns.configs().iter().map(|c| (c.id.clone(), c.name.clone())).collect();
        let conn_of = |e: &HistoryEntry| e.connection_id.as_ref().and_then(|c| names.get(c).cloned()).or_else(|| e.connection_name.clone()).unwrap_or_default();
        // The details: 12 of margin, 12 of padding, the buttons, 10, the statement's lines,
        // and the error under them (its height of the last frame).
        let error_id = egui::Id::new("history_detail_error_h");
        let error_h = if sel_entry.as_ref().is_some_and(|e| e.error_message.is_some()) { ui.data(|d| d.get_temp::<f32>(error_id)).unwrap_or(40.0) + 10.0 } else { 0.0 };
        let detail_h = sel_entry.as_ref().map_or(0.0, |e| 12.0 * 2.0 + 12.0 * 2.0 + 2.0 + 26.0 + 10.0 + e.sql.lines().count().clamp(1, 6) as f32 * 20.0 + 8.0 + error_h);
        let avail = (ui.available_height() - detail_h).max(80.0);
        // The connection as wide as its longest name (at most 220).
        let conn_names: Vec<String> = self.entries.iter().map(&conn_of).collect();
        let conn_w = look::fit(ui, conn_names.iter().map(String::as_str), 0.0, 110.0, 220.0);
        let cols = [
            look::Col::new(tr("Time"), look::Width::Initial(170.0)),
            look::Col::new(tr("Connection"), look::Width::Fit(conn_w)),
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
                    let conn = conn_of(e);
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
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if kit::button_ex(ui, kit::Kind::Secondary, Some(icon::FILE_CODE), tr("Open in SQL editor"), true).clicked() {
                        self.open_in_editor(cx, &e);
                    }
                    if kit::ghost(ui, Some(icon::COPY), tr("Copy")).clicked() {
                        ui.ctx().copy_text(e.sql.clone());
                    }
                    if kit::ghost(ui, Some(icon::TRASH), tr("Delete")).clicked() {
                        if let Some(store) = cx.services.history.clone() {
                            let _ = store.lock().delete(&[e.id]);
                        }
                        self.selected = None;
                        self.reload(cx);
                    }
                });
                // The statement highlighted as the design's code block: lines of 20, 4 above and
                // below, at most six lines shown.
                if self.tokens.as_ref().is_none_or(|(id, _)| *id != e.id) {
                    use ternarygrid_editor::EditorLanguage;
                    let kind = e.connection_id.as_deref().and_then(|c| cx.conns.config(c)).map(|c| c.kind);
                    let dialect = match kind {
                        Some(k) => cx.conns.registry.dialect(k),
                        None => std::sync::Arc::new(ternarygrid_core::dialect::GenericDialect::new()),
                    };
                    self.tokens = Some((e.id, crate::sqllang::SqlLanguage::new(dialect).highlight(&e.sql)));
                }
                let tokens = self.tokens.as_ref().map(|(_, t)| t.as_slice()).unwrap_or_default();
                let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
                let shown: String = e.sql.lines().take(6).collect::<Vec<_>>().join("\n");
                let mut job = crate::sqllang::highlight_job(&shown, tokens, &theme, egui::FontId::monospace(12.5));
                for s in &mut job.sections {
                    s.format.line_height = Some(20.0);
                }
                job.wrap.max_width = ui.available_width();
                job.wrap.max_rows = 6;
                let galley = ui.fonts_mut(|f| f.layout_job(job));
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(4.0);
                ui.add(egui::Label::new(egui::WidgetText::Galley(galley)));
                ui.add_space(4.0);
                // What went wrong, in full, under the statement.
                if let Some(err) = &e.error_message {
                    ui.add_space(10.0);
                    let top = ui.cursor().top();
                    look::error(ui, err.trim());
                    let h = ui.cursor().top() - top;
                    if (ui.data(|d| d.get_temp::<f32>(error_id)).unwrap_or(0.0) - h).abs() > 0.5 {
                        ui.data_mut(|d| d.insert_temp(error_id, h));
                        ui.ctx().request_repaint();
                    }
                }
            });
        }
    }

    crate::impl_tab_any!();
}
