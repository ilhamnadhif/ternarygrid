//! Result toolbar and filter bar (FR-D4, FR-D6).

use crate::grid::{self, DataGridState, DataMode, GridAction, PanelKind, ResultViewKind};
use crate::model::ResultModel;
use crate::view::{ClientFilter, SortKey};
use egui::{Button, Key, Popup, PopupCloseBehavior, RichText, Ui};
use egui_phosphor::regular as icon;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use ternarygrid_core::{tr, trf};
use ternarygrid_ui::kit;

const MAX_HISTORY: usize = 30;

/// Auto refresh choices.
pub const AUTO_REFRESH_CHOICES: [Option<u64>; 5] = [None, Some(5), Some(10), Some(30), Some(60)];

/// State of the toolbar and filter bar (persistable).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ToolbarState {
    /// Text of the WHERE / client filter input.
    pub filter_text: String,
    /// Text of the ORDER BY input.
    pub order_text: String,
    /// Recently applied filters, newest first.
    pub filter_history: Vec<String>,
    /// Recently applied orderings, newest first.
    pub order_history: Vec<String>,
    /// Auto refresh interval in seconds (`None` = off).
    pub auto_refresh_secs: Option<u64>,
    /// Value of the "custom" auto refresh input.
    pub custom_refresh_secs: u64,
    /// Current result view.
    pub view: ResultViewKind,
    /// Open side panels.
    pub open_panels: Vec<PanelKind>,
    /// The side panel area is hidden (its panels stay open for the next show).
    pub panels_hidden: bool,
    /// Message of the application shown next to the status: (text, error).
    /// Errors open a pop-up instead (see [`ToolbarState::error_popup`]).
    #[serde(skip)]
    pub notice: Option<(String, bool)>,
    /// The error shown in a pop-up over the result.
    #[serde(skip)]
    pub error_popup: Option<String>,
    /// The fetch error that already popped up (once each).
    #[serde(skip)]
    pub(crate) seen_fetch_error: Option<String>,
    /// The fetch error the user closed (its bar over the result stays hidden).
    #[serde(skip)]
    pub(crate) dismissed_fetch_error: Option<String>,
    #[serde(skip)]
    rt: ToolbarRuntime,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct ToolbarRuntime {
    /// Column suggestions of the WHERE and ORDER BY boxes.
    where_suggest: crate::suggest::Suggest,
    order_suggest: crate::suggest::Suggest,
    filter_error: Option<String>,
    /// Focus the filter box (cursor at the end) on the next frame.
    focus_filter: bool,
    next_refresh: Option<f64>,
}

impl ToolbarState {
    /// Focus the filter box on the next frame, the cursor at the end.
    pub fn focus_filter(&mut self) {
        self.rt.focus_filter = true;
    }

    /// Is a panel open?
    pub fn is_panel_open(&self, p: PanelKind) -> bool {
        self.open_panels.contains(&p)
    }

    /// Open or close a panel.
    pub fn set_panel_open(&mut self, p: PanelKind, open: bool) {
        self.open_panels.retain(|x| *x != p);
        if open {
            self.open_panels.push(p);
        }
    }

    fn push_history(list: &mut Vec<String>, text: &str) {
        let t = text.trim();
        if t.is_empty() {
            return;
        }
        list.retain(|h| h != t);
        list.insert(0, t.to_string());
        list.truncate(MAX_HISTORY);
    }

    /// Error of the last client filter, if any.
    pub fn filter_error(&self) -> Option<&str> {
        self.rt.filter_error.as_deref()
    }
}

/// Parse an ORDER BY list (`name DESC, [order date]`) against model column names.
pub fn parse_order_by(text: &str, columns: &[String]) -> Result<Vec<SortKey>, String> {
    let mut keys = Vec::new();
    for part in text.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let lower = p.to_ascii_lowercase();
        let (name, desc) = if lower.ends_with(" desc") {
            (p[..p.len() - 5].trim(), true)
        } else if lower.ends_with(" asc") {
            (p[..p.len() - 4].trim(), false)
        } else {
            (p, false)
        };
        let unquoted = name.strip_prefix('[').and_then(|n| n.strip_suffix(']')).or_else(|| name.strip_prefix('"').and_then(|n| n.strip_suffix('"'))).unwrap_or(name);
        let col = columns
            .iter()
            .position(|c| c == unquoted)
            .or_else(|| columns.iter().position(|c| c.eq_ignore_ascii_case(unquoted)))
            .or_else(|| unquoted.parse::<usize>().ok().filter(|n| *n >= 1 && *n <= columns.len()).map(|n| n - 1))
            .ok_or_else(|| trf("Unknown column '{}' in ORDER BY", &[&unquoted]))?;
        keys.push(SortKey { column: col, descending: desc });
    }
    Ok(keys)
}

/// Human readable duration (`120 ms`, `1.25 s`, `2 min 3 s`).
pub fn duration_text(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.2} s", d.as_secs_f64())
    } else {
        format!("{} min {} s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Status line text: fetched rows, more available, total, elapsed.
pub fn status_text(model: &ResultModel) -> String {
    let (rows, rest) = status_parts(model);
    rows + &rest
}

/// The status in two parts: the row count (`1,234 rows`) and the rest (` · more available · 42 ms`).
fn status_parts(model: &ResultModel) -> (String, String) {
    let rows = crate::format::group_thousands(&model.row_count().to_string());
    let rows = trf("{} rows", &[&rows]);
    let mut s = String::new();
    if model.has_client_view() && model.row_count() != model.stored_row_count() {
        s.push_str(&trf(" (of {} fetched)", &[&crate::format::group_thousands(&model.stored_row_count().to_string())]));
    }
    if model.fetch.fetching {
        s.push_str(" · ");
        s.push_str(tr("fetching…"));
    } else if model.fetch.has_more {
        s.push_str(" · ");
        s.push_str(tr("more available"));
    }
    if model.fetch.counting {
        s.push_str(" · ");
        s.push_str(tr("counting…"));
    } else if let Some(t) = model.fetch.total_count {
        s.push_str(&trf(" · total {}", &[&crate::format::group_thousands(&t.to_string())]));
    }
    if let Some(e) = model.fetch.elapsed {
        s.push_str(" · ");
        s.push_str(&duration_text(e));
    }
    (rows, s)
}

/// `09:35:51` for today, `2026-10-21 09:35` for an earlier day.
fn fetched_at_text(at: chrono::DateTime<chrono::Local>, now: chrono::DateTime<chrono::Local>) -> String {
    if at.date_naive() == now.date_naive() { at.format("%H:%M:%S").to_string() } else { at.format("%Y-%m-%d %H:%M").to_string() }
}

/// The filter bar: the WHERE (or client filter) field with its keyword inside, column
/// suggestions and history; the ordering; Apply; and a ⋯ menu with where filters run (the
/// database or the fetched rows) and the table's default filter. The ordering is a chip
/// ("Sort ↓ created_at ×", a click edits the ORDER BY, × drops it); the same on a table's
/// data page and under the SQL editor.
pub struct FilterBar<'a> {
    model: &'a mut ResultModel,
    grid: &'a mut DataGridState,
    state: &'a mut ToolbarState,
    /// Quotes suggested column names that need it (else plain, or "…").
    dialect: Option<std::sync::Arc<dyn ternarygrid_core::Dialect>>,
}

impl<'a> FilterBar<'a> {
    /// New filter bar.
    pub fn new(model: &'a mut ResultModel, grid: &'a mut DataGridState, state: &'a mut ToolbarState) -> Self {
        FilterBar { model, grid, state, dialect: None }
    }

    /// The database's dialect, for quoting suggested column names.
    pub fn dialect(mut self, dialect: Option<std::sync::Arc<dyn ternarygrid_core::Dialect>>) -> Self {
        self.dialect = dialect;
        self
    }

    /// Show it. Returns actions for the application (server side filters).
    pub fn show(self, ui: &mut Ui) -> Vec<GridAction> {
        let FilterBar { model, grid, state, dialect } = self;
        let kind = dialect.as_ref().map(|d| d.kind());
        // Redis has no server side filter: the read commands take no condition.
        let redis = kind == Some(ternarygrid_core::DbKind::Redis);
        if redis {
            grid.mode = DataMode::Client;
        }
        // Column names for the suggestions, written as SQL (server) or as typed (client).
        let columns: Vec<String> = model.columns().iter().map(|c| c.name.clone()).collect();
        let simple = |n: &str| n.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_') && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        let server = grid.mode == DataMode::Server;
        let quote = move |n: &str| -> String {
            match &dialect {
                Some(d) if server => d.quote_ident(n),
                _ if simple(n) => n.to_string(),
                _ => format!("\"{}\"", n.replace('"', "\"\"")),
            }
        };
        let p = ternarygrid_ui::pal(ui);
        let mut actions = Vec::new();
        let mut apply = false;
        let mut clear = false;
        let mut clear_sort = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // The keyword says where the filter runs: the database (WHERE) or the fetched rows.
            let (prefix, prefix_tip, hint) = match kind {
                _ if !server => (
                    "FILTER",
                    tr("Filters the rows already fetched, in TernaryGrid, without asking the database. Fast, but only those rows. Switch in ⋯."),
                    tr("e.g. amount > 100 and city = 'Bandung'"),
                ),
                Some(ternarygrid_core::DbKind::MongoDb) => ("QUERY", tr("Query document sent to the database. Switch in ⋯."), tr("e.g. {status: \"open\"}")),
                _ => {
                    ("WHERE", tr("WHERE condition sent to the database: the query runs again over all rows of the table. Switch in ⋯."), tr("e.g. status = 'open' and price > 100"))
                }
            };
            // The ordering (chip or field), Apply, × and ⋯ keep their widths; the WHERE field takes the rest.
            let gap = ui.spacing().item_spacing.x;
            let sort = sort_summary(model, grid, state);
            let order_w = sort_chip_width(ui, sort.as_ref()) + 30.0;
            let apply_w = kit::button_width(ui, Some(icon::PLAY), tr("Apply"), false);
            let flexible = (ui.available_width() - order_w - apply_w - 3.0 * gap).max(160.0);
            const WHERE_WORDS: [&str; 10] = ["AND", "OR", "NOT", "IS NULL", "IS NOT NULL", "LIKE", "ILIKE", "IN", "BETWEEN", "NULL"];
            let label = if server { tr("Filter rows in the database") } else { tr("Filter the fetched rows") };
            let field = Field { salt: "filter_where", prefix, label, prefix_tip, hint, width: flexible, keywords: &WHERE_WORDS, history: &state.filter_history };
            let filter_resp = prefixed_field(ui, field, &mut state.filter_text, &columns, &quote, &mut state.rt.where_suggest, &mut apply);
            if filter_resp.changed() {
                state.rt.filter_error = None;
            }
            if std::mem::take(&mut state.rt.focus_filter) {
                let end = egui::text::CCursor::new(state.filter_text.chars().count());
                let mut s = egui::text_edit::TextEditState::load(ui.ctx(), filter_resp.id).unwrap_or_default();
                s.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
                s.store(ui.ctx(), filter_resp.id);
                filter_resp.request_focus();
            }
            if filter_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                apply = true;
            }
            if !server {
                filter_resp.clone().on_hover_text(crate::expr::help_text());
            }
            let order_field = |ui: &mut Ui, width: f32, text: &mut String, suggest: &mut crate::suggest::Suggest, history: &[String], apply: &mut bool| {
                let field = Field {
                    salt: "filter_order",
                    prefix: "ORDER BY",
                    label: tr("Order rows by"),
                    prefix_tip: if server { tr("ORDER BY sent to the database") } else { tr("Sorts the fetched rows") },
                    hint: tr("e.g. created_at DESC, name"),
                    width,
                    // The client sort knows no NULLS FIRST / LAST (NULLs sort first).
                    keywords: if server { &["ASC", "DESC", "NULLS FIRST", "NULLS LAST"] } else { &["ASC", "DESC"] },
                    history,
                };
                let order_resp = prefixed_field(ui, field, text, &columns, &quote, suggest, apply);
                if order_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    *apply = true;
                }
            };
            let order_popup = ui.id().with("order_by_popup");
            // ORDER BY as a chip ("Sort ↓ created_at ×"): a click edits the ORDER BY text, × drops it.
            let (chip, remove) = sort_chip(ui, sort.as_ref(), sort_chip_width(ui, sort.as_ref()));
            clear_sort = remove;
            Popup::menu(&chip).id(order_popup).close_behavior(PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                ui.set_min_width(380.0);
                ui.label(kit::section_title(ui, tr("Order rows by")));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    order_field(ui, 300.0, &mut state.order_text, &mut state.rt.order_suggest, &state.order_history, &mut apply);
                    if kit::button_ex(ui, kit::Kind::Primary, None, tr("Apply"), false).clicked() {
                        apply = true;
                    }
                });
                if apply {
                    ui.close();
                }
            });
            if kit::button_ex(ui, kit::Kind::Primary, Some(icon::PLAY), tr("Apply"), false).on_hover_text(tr("Apply filter and order (Enter)")).clicked() {
                apply = true;
            }
            let filtered = !state.filter_text.trim().is_empty() || !state.order_text.trim().is_empty() || !grid.sort.is_empty() || model.has_client_view();
            let menu = kit::icon_button_ex(ui, icon::DOTS_THREE, tr("Filter options"), false, 30.0);
            Popup::menu(&menu).id(ui.id().with("filter_options")).show(|ui| {
                kit::menu_look(ui, 300.0);
                kit::menu_caption(ui, tr("Filter and sort"));
                // Each choice says what it does: the words alone mean little to a new user.
                let server_hint = tr("The database filters and sorts. The query runs again with your WHERE / ORDER BY, over all rows of the table.");
                let client_hint = tr("TernaryGrid filters and sorts the rows already fetched, without asking the database. Fast, but only those rows.");
                let mut on = server;
                let on_server = ui
                    .add_enabled_ui(!redis, |ui| kit::menu_check(ui, &mut on, tr("Server: in the database, over all rows")))
                    .inner
                    .on_hover_text(server_hint)
                    .on_disabled_hover_text(tr("Redis has no server side filter: its read commands take no condition."));
                if on_server.clicked() {
                    grid.mode = DataMode::Server;
                }
                let mut on = !server;
                if kit::menu_check(ui, &mut on, tr("Client: over the fetched rows")).on_hover_text(client_hint).clicked() {
                    grid.mode = DataMode::Client;
                }
                // The chip's ORDER BY editor and ×.
                kit::menu_separator(ui);
                if kit::menu_item(ui, Some(icon::ARROWS_DOWN_UP), tr("Edit ORDER BY…"), "").clicked() {
                    ui.close();
                    Popup::open_id(ui.ctx(), order_popup);
                }
                if ui.add_enabled_ui(filtered, |ui| kit::menu_item(ui, Some(icon::X), tr("Clear filter and order"), "")).inner.clicked() {
                    clear = true;
                    ui.close();
                }
                if crate::model::source_table(model.columns()).is_ok() {
                    kit::menu_separator(ui);
                    let some = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
                    if kit::menu_item(ui, Some(icon::PUSH_PIN), tr("Save as default filter"), "").on_hover_text(tr("Applied whenever this table's data is opened")).clicked() {
                        actions.push(GridAction::DefaultFilter { where_clause: some(&state.filter_text), order_by: some(&state.order_text) });
                        ui.close();
                    }
                    if kit::menu_item(ui, Some(icon::PUSH_PIN_SLASH), tr("Reset default filter"), "").clicked() {
                        actions.push(GridAction::DefaultFilter { where_clause: None, order_by: None });
                        ui.close();
                    }
                }
            });
        });
        if clear_sort {
            state.order_text.clear();
            grid.sort.clear();
            apply = true;
        }
        if let Some(e) = &state.rt.filter_error {
            ui.label(RichText::new(e).color(p.err));
        }
        if clear {
            state.filter_text.clear();
            state.order_text.clear();
            state.rt.filter_error = None;
            grid.sort.clear();
            match grid.mode {
                DataMode::Client => model.clear_view(),
                DataMode::Server => actions.push(GridAction::ServerQueryChanged { where_clause: Some(String::new()), order_by: Some(String::new()) }),
            }
        }
        if apply {
            ToolbarState::push_history(&mut state.filter_history, &state.filter_text.clone());
            ToolbarState::push_history(&mut state.order_history, &state.order_text.clone());
            match grid.mode {
                DataMode::Server => {
                    actions.push(GridAction::ServerQueryChanged { where_clause: Some(state.filter_text.trim().to_string()), order_by: Some(state.order_text.trim().to_string()) });
                    // The header's arrows show the ORDER BY when it names columns (none for expressions).
                    grid.sort = parse_order_by(&state.order_text, &model.column_names()).unwrap_or_default();
                }
                DataMode::Client => {
                    let names = model.column_names();
                    let filter = if state.filter_text.trim().is_empty() {
                        Ok(None)
                    } else {
                        ClientFilter::parse(&state.filter_text, &names).map(Some).map_err(|e| e.display_with_position(&state.filter_text))
                    };
                    let sort = parse_order_by(&state.order_text, &names);
                    match (filter, sort) {
                        (Ok(filter), Ok(sort)) => {
                            state.rt.filter_error = None;
                            if !state.order_text.trim().is_empty() || !sort.is_empty() {
                                grid.sort = sort.clone();
                            }
                            let spec = crate::view::ViewSpec { sort: if state.order_text.trim().is_empty() { grid.sort.clone() } else { sort }, filter };
                            let ctx = ui.ctx().clone();
                            model.start_client_view(spec, Box::new(move || ctx.request_repaint()));
                        }
                        (Err(e), _) | (_, Err(e)) => state.rt.filter_error = Some(e),
                    }
                }
            }
        }
        actions
    }
}

/// What [`prefixed_field`] shows.
struct Field<'a> {
    salt: &'static str,
    /// The keyword inside the field, on the left (`WHERE`, `ORDER BY`).
    prefix: &'a str,
    /// Its accessible name.
    label: &'a str,
    prefix_tip: &'a str,
    hint: &'a str,
    width: f32,
    keywords: &'a [&'a str],
    history: &'a [String],
}

/// Height of the filter bar's fields, chip and buttons.
const FIELD_H: f32 = 30.0;

/// What the sort chip shows: the first ORDER BY column, its direction, how many follow.
struct SortSummary {
    name: String,
    descending: bool,
    more: usize,
}

/// The ordering in effect: the ORDER BY text, else the grid's own sort (header clicks).
fn sort_summary(model: &ResultModel, grid: &DataGridState, state: &ToolbarState) -> Option<SortSummary> {
    let text = state.order_text.trim();
    if !text.is_empty() {
        let parts: Vec<&str> = text.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
        let mut first = parts.first()?.to_string();
        for suffix in [" nulls first", " nulls last"] {
            if first.to_ascii_lowercase().ends_with(suffix) {
                first.truncate(first.len() - suffix.len());
            }
        }
        let lower = first.to_ascii_lowercase();
        let (name, descending) = if lower.ends_with(" desc") {
            (first[..first.len() - 5].trim(), true)
        } else if lower.ends_with(" asc") {
            (first[..first.len() - 4].trim(), false)
        } else {
            (first.trim(), false)
        };
        return Some(SortSummary { name: name.to_string(), descending, more: parts.len() - 1 });
    }
    let first = grid.sort.first()?;
    Some(SortSummary { name: model.column_name(first.column).to_string(), descending: first.descending, more: grid.sort.len() - 1 })
}

/// The parts of the sort chip: "Sort", the arrow, the column (and "+n"), ×.
struct ChipParts {
    sort: std::sync::Arc<egui::Galley>,
    name: Option<std::sync::Arc<egui::Galley>>,
}

fn chip_parts(ui: &Ui, sort: Option<&SortSummary>) -> ChipParts {
    let sort_g = ui.painter().layout_no_wrap(tr("Sort").to_string(), egui::FontId::proportional(12.5), egui::Color32::PLACEHOLDER);
    let name = sort.map(|s| {
        let text = if s.more > 0 { format!("{} +{}", s.name, s.more) } else { s.name.clone() };
        ui.painter().layout_no_wrap(text, egui::FontId::monospace(12.0), egui::Color32::PLACEHOLDER)
    });
    ChipParts { sort: sort_g, name }
}

/// Width of the sort chip: 10 | Sort 6 ↓ 6 column 6 × | 6, or 10 | ↕ 6 Sort | 10 without an ordering.
fn sort_chip_width(ui: &Ui, sort: Option<&SortSummary>) -> f32 {
    let parts = chip_parts(ui, sort);
    match &parts.name {
        Some(n) => (10.0 + parts.sort.size().x + 6.0 + 13.0 + 6.0 + n.size().x.min(220.0) + 6.0 + 20.0 + 6.0).ceil(),
        None => (10.0 + 13.0 + 6.0 + parts.sort.size().x + 10.0).ceil(),
    }
}

/// The ORDER BY as a chip: "Sort ↓ created_at ×". Returns the chip (a click opens the ORDER BY
/// editor) and whether × was clicked (the ordering is removed).
fn sort_chip(ui: &mut Ui, sort: Option<&SortSummary>, width: f32) -> (egui::Response, bool) {
    let p = ternarygrid_ui::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, FIELD_H), egui::Sense::click());
    let label = match sort {
        Some(s) => format!("{} {} {}", tr("Sort"), s.name, if s.descending { "DESC" } else { "ASC" }),
        None => tr("Sort").to_string(),
    };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    let resp = resp.on_hover_text(tr("Order rows by (ORDER BY)"));
    let x_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 6.0 - 10.0, rect.center().y), egui::vec2(20.0, 20.0));
    let remove = sort.is_some().then(|| {
        let r = ui.interact(x_rect, resp.id.with("remove"), egui::Sense::click()).on_hover_text(tr("Remove sort"));
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Remove sort")));
        r
    });
    if !ui.is_rect_visible(rect) {
        return (resp, remove.is_some_and(|r| r.clicked()));
    }
    let painter = ui.painter();
    let hovered = resp.hovered() && !remove.as_ref().is_some_and(|r| r.hovered());
    painter.rect(rect, ternarygrid_ui::style::RADIUS, if hovered { p.hover } else { p.surface }, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    let parts = chip_parts(ui, sort);
    let cy = rect.center().y;
    let mut x = rect.left() + 10.0;
    let icon_font = ternarygrid_ui::style::icon_font(13.0);
    match (sort, parts.name) {
        (Some(s), Some(name)) => {
            painter.galley(egui::pos2(x, cy - parts.sort.size().y / 2.0), parts.sort.clone(), p.text_muted);
            x += parts.sort.size().x + 6.0;
            let arrow = if s.descending { icon::ARROW_DOWN } else { icon::ARROW_UP };
            painter.text(egui::pos2(x + 6.5, cy), egui::Align2::CENTER_CENTER, arrow, icon_font, p.accent_text);
            x += 13.0 + 6.0;
            let clip = egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x_rect.left() - 4.0, rect.bottom()));
            ui.painter().with_clip_rect(clip.intersect(ui.clip_rect())).galley(egui::pos2(x, cy - name.size().y / 2.0), name, p.text);
            if let Some(r) = &remove {
                if r.hovered() {
                    painter.rect_filled(x_rect, ternarygrid_ui::style::RADIUS, p.hover);
                }
                painter.text(x_rect.center(), egui::Align2::CENTER_CENTER, icon::X, ternarygrid_ui::style::icon_font(12.0), if r.hovered() { p.text } else { p.text_muted });
            }
        }
        _ => {
            painter.text(egui::pos2(x + 6.5, cy), egui::Align2::CENTER_CENTER, icon::ARROWS_DOWN_UP, icon_font, p.text_muted);
            x += 13.0 + 6.0;
            painter.galley(egui::pos2(x, cy - parts.sort.size().y / 2.0), parts.sort, p.text_muted);
        }
    }
    (resp, remove.is_some_and(|r| r.clicked()))
}

/// A monospace field with its keyword inside on the left, column suggestions and, once
/// something was applied, a clock on the right that lists the history (a pick applies it).
fn prefixed_field(
    ui: &mut Ui,
    f: Field<'_>,
    text: &mut String,
    columns: &[String],
    quote: &dyn Fn(&str) -> String,
    suggest: &mut crate::suggest::Suggest,
    apply: &mut bool,
) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let font = egui::FontId::monospace(11.5);
    let prefix_w = ui.painter().layout_no_wrap(f.prefix.to_string(), font.clone(), p.accent_text).size().x;
    let history = !f.history.is_empty();
    // 30 points high (the mockups' fields): the text's row in the middle.
    let text_font = egui::FontId::monospace(12.5);
    let row_h = ui.fonts_mut(|fonts| fonts.row_height(&text_font));
    let pad_y = ((FIELD_H - row_h) / 2.0).round().max(2.0);
    let margin = egui::Margin { left: (prefix_w + 18.0).min(120.0) as i8, right: if history { 28 } else { 8 }, top: pad_y as i8, bottom: pad_y as i8 };
    let id = ui.id().with(f.salt);
    let hint = RichText::new(f.hint).font(egui::FontId::monospace(12.5)).color(p.text_faint);
    let resp = crate::suggest::suggest_edit(
        ui,
        id,
        text,
        |t| t.hint_text(hint).desired_width(f.width).font(text_font).margin(margin).min_size(egui::vec2(0.0, FIELD_H)),
        columns,
        f.keywords,
        quote,
        suggest,
    );
    // The keyword is painted, not a label: give the field its name for screen readers.
    ui.ctx().accesskit_node_builder(resp.id, |node| node.set_label(f.label));
    let r = resp.rect;
    if resp.has_focus() {
        ternarygrid_ui::style::focus_halo(ui, r, ternarygrid_ui::style::RADIUS);
    }
    ui.painter().text(egui::pos2(r.left() + 10.0, r.center().y), egui::Align2::LEFT_CENTER, f.prefix, font, p.accent_text);
    let prefix_rect = egui::Rect::from_min_max(egui::pos2(r.left() + 4.0, r.top()), egui::pos2(r.left() + 14.0 + prefix_w, r.bottom()));
    ui.interact(prefix_rect, id.with("prefix"), egui::Sense::hover()).on_hover_text(f.prefix_tip);
    if history {
        let hr = egui::Rect::from_center_size(egui::pos2(r.right() - 15.0, r.center().y), egui::vec2(22.0, 22.0));
        let h = ui.interact(hr, id.with("history"), egui::Sense::click()).on_hover_text(tr("History"));
        h.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("History")));
        if h.hovered() {
            ui.painter().rect_filled(hr, 4, p.hover);
        }
        let color = if h.hovered() { p.text } else { p.text_faint };
        ui.painter().text(hr.center(), egui::Align2::CENTER_CENTER, icon::CLOCK_COUNTER_CLOCKWISE, ternarygrid_ui::style::icon_font(14.0), color);
        Popup::menu(&h).id(id.with("history_popup")).close_behavior(PopupCloseBehavior::CloseOnClick).show(|ui| {
            kit::menu_look(ui, 260.0);
            kit::menu_caption(ui, tr("History"));
            for item in f.history {
                if ui.button(RichText::new(item).font(egui::FontId::monospace(12.5))).clicked() {
                    *text = item.clone();
                    *apply = true;
                }
            }
        });
    }
    resp
}

/// The footer of a result, the same under a table's data and the SQL editor: on the left the
/// status ("400 rows · more available · 286 ms", the time the rows were read in its tooltip),
/// Fetch next and ⋯ (refresh, auto refresh, fetch all, the total count, row operations, Chart,
/// Spatial view, the XML view, the panels); in the middle the pending changes with Discard
/// and Save (only while there are some); on the right the views, Export and the panels.
pub struct ResultToolbar<'a> {
    model: &'a mut ResultModel,
    grid: &'a mut DataGridState,
    state: &'a mut ToolbarState,
    fetch_size: usize,
}

impl<'a> ResultToolbar<'a> {
    /// New toolbar.
    pub fn new(model: &'a mut ResultModel, grid: &'a mut DataGridState, state: &'a mut ToolbarState) -> Self {
        ResultToolbar { model, grid, state, fetch_size: 0 }
    }

    /// The rows one "Fetch next" reads (its label says how many; 0: "Fetch more").
    pub fn fetch_size(mut self, rows: usize) -> Self {
        self.fetch_size = rows;
        self
    }

    /// Show it. Returns actions for the application.
    pub fn show(self, ui: &mut Ui) -> Vec<GridAction> {
        let ResultToolbar { model, grid, state, fetch_size } = self;
        let mut actions = Vec::new();
        auto_refresh_tick(ui, model, grid.is_editing(), state, &mut actions);
        let p = ternarygrid_ui::pal(ui);
        // A narrow footer (a side panel open, a split editor) keeps the status, the views and
        // ⋯; Add row and Export move into ⋯ instead of running into the status.
        let compact = ui.available_width() < COMPACT_W;
        let row = ui.horizontal(|ui| {
            ui.set_min_height(FOOTER_H);
            ui.spacing_mut().item_spacing.x = 10.0;
            // --- status: the row count stands out; Fetch next follows it
            let (rows, mut rest) = status_parts(model);
            if state.auto_refresh_secs.is_some() && (model.has_changes() || model.fetch.fetching) {
                rest.push_str(" · ");
                rest.push_str(tr("auto refresh paused"));
            }
            // When the rows were read: in the tooltip.
            let read_at = model.fetch.fetched_at.map(|at| fetched_at_text(at, chrono::Local::now()));
            let mut job = egui::text::LayoutJob::default();
            let size = 12.5;
            job.append(&rows, 0.0, egui::TextFormat::simple(ternarygrid_ui::style::medium(ui.ctx(), size), p.text));
            job.append(&rest, 0.0, egui::TextFormat::simple(egui::FontId::proportional(size), p.text_muted));
            let status = ui.add(egui::Label::new(job).selectable(false));
            if let Some(at) = read_at {
                status.on_hover_text(trf("Read at {}", &[&at]));
            }
            let fetching = model.fetch.fetching;
            if model.fetch.has_more {
                let label = if fetch_size > 0 { trf("Fetch next {}", &[&fetch_size]) } else { tr("Fetch more").to_string() };
                let r = ui.add_enabled_ui(!fetching, |ui| flat_button(ui, None, &label, Flat::Link)).inner.on_hover_text(tr("Fetch next page"));
                if r.clicked() {
                    actions.push(GridAction::FetchMore);
                }
            }
            // Auto refresh on: its interval, a click changes it.
            if let Some(s) = state.auto_refresh_secs {
                let r = flat_button(ui, Some(icon::TIMER), &auto_label(s), Flat::Accent).on_hover_text(tr("Auto refresh"));
                Popup::menu(&r).id(ui.id().with("auto_refresh_chip")).show(|ui| auto_refresh_menu(ui, state, &mut actions));
            }
            // A long fetch (fetch all, a slow query) stops here, next to "fetching…".
            if let Some(running) = model.fetch.fetching_for() {
                if running >= crate::grid::CANCEL_AFTER {
                    let stop = Button::new(RichText::new(icon::STOP_CIRCLE).color(p.err)).frame_when_inactive(false);
                    if ui.add(stop).on_hover_text(tr("Stop the query")).clicked() {
                        actions.push(GridAction::CancelFetch);
                    }
                } else {
                    ui.ctx().request_repaint_after(crate::grid::CANCEL_AFTER - running);
                }
            }
            // A failed fetch shows in its bar over the rows; closed, a short note here brings it back
            // (the message once on screen, not twice).
            if let Some(e) = &model.fetch.error
                && state.dismissed_fetch_error.as_ref() == Some(e)
            {
                let label = egui::Label::new(RichText::new(format!("{} {}", icon::WARNING_CIRCLE, tr("The fetch failed"))).size(12.5).color(p.err)).sense(egui::Sense::click());
                if ui.add(label).on_hover_text(e.as_str()).clicked() {
                    state.dismissed_fetch_error = None;
                }
            }
            let left_end = ui.min_rect().right();
            // --- right, right to left: the panel toggle at the edge, ⋯ (the less used actions,
            // the last of the result's actions as in every bar), Export, the views, Add row
            let right_start = ui
                .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let visible = !state.open_panels.is_empty() && !state.panels_hidden;
                    if panel_toggle(ui, visible, 28.0).clicked() {
                        if visible {
                            state.panels_hidden = true;
                        } else {
                            if state.open_panels.is_empty() {
                                state.set_panel_open(PanelKind::Value, true);
                            }
                            state.panels_hidden = false;
                        }
                    }
                    let more = kit::icon_button_ex(ui, icon::DOTS_THREE, tr("More result actions"), false, 28.0);
                    Popup::menu(&more).id(ui.id().with("result_more")).show(|ui| more_menu(ui, model, grid, state, compact, &mut actions));
                    if !compact {
                        let export = flat_button(ui, Some(icon::EXPORT), tr("Export"), Flat::Plain);
                        if export.on_hover_text(tr("Export…")).clicked() {
                            actions.push(GridAction::Export { rows: None, columns: grid.visible_columns() });
                        }
                    }
                    if let Some(kind) = view_switch(ui, state.view)
                        && state.view != kind
                    {
                        state.view = kind;
                        actions.push(GridAction::SwitchView(kind));
                    }
                    // Adding a row is the edit people look for under the rows (as in DBeaver's
                    // footer); the other row operations are in ⋯, the context menu and the keys.
                    if model.is_editable() && !compact {
                        let add = flat_button(ui, Some(icon::PLUS), tr("Add row"), Flat::Plain).on_hover_text(tr("Add a row after the selected one (Insert)"));
                        if add.clicked()
                            && let Err(e) = grid::add_row(model, grid)
                        {
                            actions.push(GridAction::Notify { message: e, error: true });
                        }
                    }
                    if !model.is_editable()
                        && let Some(r) = model.read_only_reason()
                    {
                        // 🔒 No unique key, 5 apart, the reason in the tooltip.
                        ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            ui.label(RichText::new(r.short_label()).size(12.0).color(p.warn)).on_hover_text(r.message());
                            ui.label(RichText::new(icon::LOCK_SIMPLE).font(ternarygrid_ui::style::icon_font(13.0)).color(p.warn));
                        });
                    }
                    // Errors open a pop-up (the result viewer); other messages show here.
                    if let Some((message, _)) = state.notice.clone().filter(|(_, error)| !error) {
                        if kit::icon_button_ex(ui, icon::X, tr("Dismiss"), false, 20.0).clicked() {
                            state.notice = None;
                        }
                        ui.add(egui::Label::new(RichText::new(format!("{} {message}", icon::INFO)).size(12.5).color(p.text)).truncate()).on_hover_text(message);
                    }
                    ui.min_rect().left()
                })
                .inner;
            (left_end, right_start)
        });
        // --- pending changes, between the two sides: only while there are some (Cmd+S saves, as always)
        let pending = model.pending_counts().total();
        if pending > 0 {
            let (left_end, right_start) = row.inner;
            let count = if pending == 1 { tr("1 change").to_string() } else { trf("{} changes", &[&pending]) };
            let width = pending_width(ui, &count);
            let x = (left_end + (right_start - left_end - width) / 2.0).max(left_end + 8.0);
            let rect = egui::Rect::from_center_size(egui::pos2(x + width / 2.0, row.response.rect.center().y), egui::vec2(width, 32.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
            match pending_pill(&mut child, &count, pending) {
                Some(true) => actions.push(grid::save_action(model)),
                Some(false) => {
                    // Discard drops them at once: they are only pending.
                    model.discard_changes();
                    actions.push(GridAction::ChangesDiscarded);
                }
                None => {}
            }
        }
        actions
    }
}

/// Height of the footer's row.
const FOOTER_H: f32 = 30.0;

/// Below this width the footer moves Add row and Export into ⋯.
const COMPACT_W: f32 = 760.0;

/// The look of a [`flat_button`].
#[derive(Clone, Copy, PartialEq)]
enum Flat {
    /// Text colour, regular weight (Export, Discard).
    Plain,
    /// Accent text, medium weight (Fetch next).
    Link,
    /// Accent text on the soft accent (an active setting).
    Accent,
}

/// A frameless button of the footer: 26 points high, 8 (10 with an icon) at the sides, 12.5 point text.
fn flat_button(ui: &mut Ui, icon: Option<&str>, text: &str, look: Flat) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let font = if look == Flat::Plain { egui::FontId::proportional(12.5) } else { ternarygrid_ui::style::medium(ui.ctx(), 12.5) };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER);
    let (h, pad, gap) = if icon.is_some() { (28.0, 10.0, 6.0) } else { (26.0, 8.0, 0.0) };
    let icon_w = if icon.is_some() { 14.0 + gap } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x + icon_w + 2.0 * pad, h), egui::Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = match look {
            Flat::Accent => p.accent_soft,
            _ if enabled && resp.hovered() => p.hover,
            _ => egui::Color32::TRANSPARENT,
        };
        painter.rect_filled(rect, ternarygrid_ui::style::RADIUS, fill);
        let color = match look {
            Flat::Plain => p.text,
            Flat::Link | Flat::Accent => p.accent_text,
        };
        let fade = |c: egui::Color32| if enabled { c } else { c.gamma_multiply(0.45) };
        let mut x = rect.left() + pad;
        if let Some(i) = icon {
            let icon_color = if look == Flat::Plain { p.text_muted } else { color };
            painter.text(egui::pos2(x + 7.0, rect.center().y), egui::Align2::CENTER_CENTER, i, ternarygrid_ui::style::icon_font(14.0), fade(icon_color));
            x += icon_w;
        }
        painter.galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, fade(color));
    }
    resp
}

/// The panels' toggle: a window with a pane at the right, `side` square (28 with a 15 point
/// icon; 26 with a 14 point one).
fn panel_toggle(ui: &mut Ui, on: bool, side: f32) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let tip = if on { tr("Hide panels") } else { tr("Show panels") };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, tip));
    let resp = resp.on_hover_text(tr("Value, metadata and statistics panels"));
    let painter = ui.painter();
    if on {
        painter.rect_filled(rect, ternarygrid_ui::style::RADIUS, p.accent_soft);
    } else if resp.hovered() {
        painter.rect_filled(rect, ternarygrid_ui::style::RADIUS, p.hover);
    }
    let color = if on {
        p.accent_text
    } else if resp.hovered() {
        p.text
    } else {
        p.text_muted
    };
    // The mockup's 24-unit icon: a 17 × 15 window, the pane from 14.5.
    let s = (side / 2.0 + 1.0) / 24.0;
    let icon = egui::Rect::from_center_size(rect.center(), egui::vec2(17.0 * s, 15.0 * s));
    let stroke = egui::Stroke::new(1.15, color);
    painter.rect_stroke(icon, 1.8 * s, stroke, egui::StrokeKind::Middle);
    let x = icon.left() + 11.0 * s;
    painter.vline(x, icon.y_range(), stroke);
    resp
}

/// The views of the rows (the XML view is in the data page's ⋯).
const VIEWS: [(ResultViewKind, &str, &str); 5] = [
    (ResultViewKind::Grid, icon::TABLE, "Grid"),
    (ResultViewKind::Text, icon::TEXT_AA, "Text"),
    (ResultViewKind::Record, icon::LIST_DASHES, "Record"),
    (ResultViewKind::Json, icon::BRACKETS_CURLY, "JSON"),
    (ResultViewKind::Xml, icon::FILE_CODE, "XML"),
];

/// The view switcher, one pill (laid out right to left: the pill is allocated at once): the
/// first four views, the shown one raised (the XML view is in ⋯). Returns the view clicked.
fn view_switch(ui: &mut Ui, current: ResultViewKind) -> Option<ResultViewKind> {
    let p = ternarygrid_ui::pal(ui);
    let views = &VIEWS[..4];
    let (bw, bh, pad, gap) = (30.0, 24.0, 2.0, 2.0);
    let n = views.len() as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(n * bw + (n - 1.0) * gap + 2.0 * pad, bh + 2.0 * pad), egui::Sense::hover());
    ui.painter().rect_filled(rect, 7, p.subtle);
    let mut clicked = None;
    for (i, (kind, ic, name)) in views.iter().enumerate() {
        let r = egui::Rect::from_min_size(egui::pos2(rect.left() + pad + i as f32 * (bw + gap), rect.top() + pad), egui::vec2(bw, bh));
        let resp = ui.interact(r, ui.id().with(("view", i)), egui::Sense::click());
        let tip = trf("{} view", &[&tr(name)]);
        let on = current == *kind;
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, tr(name)));
        let resp = resp.on_hover_text(tip);
        if on {
            ternarygrid_ui::kit::raised(ui.painter(), r, egui::CornerRadius::same(5), p.thumb);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 5, p.hover);
        }
        let color = if on || resp.hovered() { p.text } else { p.text_muted };
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, *ic, ternarygrid_ui::style::icon_font(15.0), color);
        if resp.clicked() {
            clicked = Some(*kind);
        }
    }
    clicked
}

/// Width of the pending changes pill: 12 | ● 8 n changes 8 Discard 8 Save ⌘S | 4.
fn pending_width(ui: &Ui, count: &str) -> f32 {
    let w = |t: &str, f: egui::FontId| ui.painter().layout_no_wrap(t.to_string(), f, egui::Color32::PLACEHOLDER).size().x;
    let medium = ternarygrid_ui::style::medium(ui.ctx(), 12.5);
    let label = w(count, medium.clone());
    let discard = w(tr("Discard"), egui::FontId::proportional(12.5)) + 20.0;
    let save = w(tr("Save"), medium) + 8.0 + w(SAVE_KEYS, egui::FontId::proportional(11.0)) + 20.0;
    (12.0 + 7.0 + 8.0 + label + 8.0 + discard + 8.0 + save + 4.0).ceil()
}

/// The keys of Save, shown in its button.
const SAVE_KEYS: &str = if cfg!(target_os = "macos") { "⌘S" } else { "Ctrl+S" };

/// "● 2 changes  Discard  Save ⌘S" on the changed cells' tint. `Some(true)`: Save, `Some(false)`: Discard.
fn pending_pill(ui: &mut Ui, count: &str, pending: usize) -> Option<bool> {
    let p = ternarygrid_ui::pal(ui);
    let rect = ui.max_rect();
    let modified = crate::theme::GridTheme::from_visuals(ui.visuals()).modified;
    ui.painter().rect_filled(rect, 8, modified);
    ui.spacing_mut().item_spacing.x = 8.0;
    ui.add_space(12.0);
    kit::dot(ui, p.warn, 3.5);
    ui.label(RichText::new(count).font(ternarygrid_ui::style::medium(ui.ctx(), 12.5)).color(p.text));
    let mut out = None;
    if key_button(ui, None, tr("Discard"), None, false, 26.0).on_hover_text(tr("Discard pending changes")).clicked() {
        out = Some(false);
    }
    if key_button(ui, None, tr("Save"), Some(SAVE_KEYS), true, 26.0)
        .on_hover_text(ternarygrid_core::trn(pending, tr("Save 1 pending change (⌘S)"), tr("Save {} pending changes (⌘S)")))
        .clicked()
    {
        out = Some(true);
    }
    out
}

/// A button with its keys after the text (Save ⌘S, Copy ⌘↩): primary (accent) or quiet,
/// `height` high, 10 (12 when 28 or more) at the sides, an optional icon before the text.
pub(crate) fn key_button(ui: &mut Ui, icon: Option<&str>, text: &str, keys: Option<&str>, primary: bool, height: f32) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let size = if height >= 28.0 { 13.0 } else { 12.5 };
    let font = if primary { ternarygrid_ui::style::medium(ui.ctx(), size) } else { egui::FontId::proportional(size) };
    let g = ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER);
    let k = keys.map(|k| ui.painter().layout_no_wrap(k.to_string(), egui::FontId::proportional(11.0), egui::Color32::PLACEHOLDER));
    let pad = if height >= 28.0 { 12.0 } else { 10.0 };
    let icon_w = if icon.is_some() { 14.0 + 6.0 } else { 0.0 };
    let width = 2.0 * pad + icon_w + g.size().x + k.as_ref().map_or(0.0, |k| 8.0 + k.size().x);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    let painter = ui.painter();
    let hovered = resp.hovered();
    let (fill, color) = if primary { (if hovered { p.accent_hover } else { p.accent }, p.on_accent) } else { (if hovered { p.hover } else { egui::Color32::TRANSPARENT }, p.text) };
    painter.rect_filled(rect, ternarygrid_ui::style::RADIUS, fill);
    let mut x = rect.left() + pad;
    if let Some(i) = icon {
        painter.text(egui::pos2(x + 7.0, rect.center().y), egui::Align2::CENTER_CENTER, i, ternarygrid_ui::style::icon_font(14.0), color);
        x += icon_w;
    }
    let gw = g.size().x;
    painter.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, color);
    if let Some(k) = k {
        painter.galley(egui::pos2(x + gw + 8.0, rect.center().y - k.size().y / 2.0), k, color.gamma_multiply(0.8));
    }
    resp
}

/// The keys of a shortcut as the menus write them: `mac` on macOS (⌘R), else `other` (Ctrl+R).
pub(crate) fn keys(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") { mac } else { other }
}

/// The auto refresh choices (the ⋯ menu's submenu, the interval chip and the clock button).
fn auto_refresh_menu(ui: &mut Ui, state: &mut ToolbarState, actions: &mut Vec<GridAction>) {
    kit::menu_look(ui, 200.0);
    for choice in AUTO_REFRESH_CHOICES {
        let text = match choice {
            None => tr("Off").to_string(),
            Some(s) => auto_label(s),
        };
        let mut on = state.auto_refresh_secs == choice;
        if kit::menu_check(ui, &mut on, &text).clicked() {
            set_auto_refresh(ui, state, choice, actions);
            ui.close();
        }
    }
    kit::menu_separator(ui);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(tr("Custom (s)"));
        if state.custom_refresh_secs == 0 {
            state.custom_refresh_secs = 15;
        }
        ui.add(egui::DragValue::new(&mut state.custom_refresh_secs).range(1..=86_400));
        if ui.button(tr("Set")).clicked() {
            let s = state.custom_refresh_secs.max(1);
            set_auto_refresh(ui, state, Some(s), actions);
            ui.close();
        }
    });
}

/// The panels as check items (the ⋯ menu, the panel strip's +).
fn panel_items(ui: &mut Ui, state: &ToolbarState, actions: &mut Vec<GridAction>) {
    for (kind, _, name) in PANELS {
        let mut open = state.is_panel_open(kind) && !state.panels_hidden;
        if kit::menu_check(ui, &mut open, tr(name)).clicked() {
            actions.push(GridAction::TogglePanel(kind));
            ui.close();
        }
    }
}

/// The footer's ⋯ menu: refresh, auto refresh, fetching, rows, Chart / Spatial view, the XML
/// view, panels.
fn more_menu(ui: &mut Ui, model: &mut ResultModel, grid: &mut DataGridState, state: &mut ToolbarState, compact: bool, actions: &mut Vec<GridAction>) {
    kit::menu_look(ui, 260.0);
    // What a narrow footer leaves out.
    if compact {
        if kit::menu_item(ui, Some(icon::EXPORT), tr("Export…"), "").clicked() {
            actions.push(GridAction::Export { rows: None, columns: grid.visible_columns() });
            ui.close();
        }
        kit::menu_separator(ui);
    }
    if kit::menu_item(ui, Some(icon::ARROW_CLOCKWISE), tr("Refresh"), keys("⌘R", "Ctrl+R")).clicked() {
        actions.push(GridAction::Refresh);
        ui.close();
    }
    kit::submenu(ui, Some(icon::TIMER), tr("Auto refresh"), 200.0, &mut |ui| auto_refresh_menu(ui, state, actions));
    let fetching = model.fetch.fetching;
    if model.fetch.has_more && ui.add_enabled_ui(!fetching, |ui| kit::menu_item(ui, Some(icon::DOWNLOAD_SIMPLE), tr("Fetch all rows"), "")).inner.clicked() {
        actions.push(GridAction::FetchAll);
        ui.close();
    }
    if ui.add_enabled_ui(!model.fetch.counting, |ui| kit::menu_item(ui, Some(icon::HASH), tr("Calculate total row count"), "")).inner.clicked() {
        actions.push(GridAction::CountTotal);
        ui.close();
    }
    if model.is_editable() {
        kit::menu_separator(ui);
        let notify = |r: Result<usize, String>, actions: &mut Vec<GridAction>| {
            if let Err(e) = r {
                actions.push(GridAction::Notify { message: e, error: true });
            }
        };
        if kit::menu_item(ui, Some(icon::PLUS), tr("Add row"), "Insert").clicked() {
            notify(grid::add_row(model, grid), actions);
            ui.close();
        }
        if kit::menu_item(ui, Some(icon::ROWS_PLUS_TOP), tr("Add row before"), keys("⇧Insert", "Shift+Insert")).clicked() {
            grid.request(crate::grid::GridCommand::AddRowBefore);
            ui.close();
        }
        if kit::menu_item(ui, Some(icon::COPY), tr("Duplicate selected rows"), keys("⌘D", "Ctrl+D")).clicked() {
            notify(grid::duplicate_selected(model, grid), actions);
            ui.close();
        }
        if kit::menu_item(ui, Some(icon::ARROW_FAT_LINE_DOWN), tr("Copy from row above"), keys("⌘⌥↑", "Ctrl+Alt+Up")).clicked() {
            grid.request(crate::grid::GridCommand::CopyFromAbove);
            ui.close();
        }
        if kit::menu_item(ui, Some(icon::ARROW_FAT_LINE_UP), tr("Copy from row below"), keys("⌘⌥↓", "Ctrl+Alt+Down")).clicked() {
            grid.request(crate::grid::GridCommand::CopyFromBelow);
            ui.close();
        }
        if kit::menu_item_danger(ui, Some(icon::TRASH), tr("Delete selected rows"), keys("⌫", "Delete")).clicked() {
            notify(grid::delete_selected(model, grid), actions);
            ui.close();
        }
        if model.can_undo() && kit::menu_item(ui, Some(icon::ARROW_U_UP_LEFT), tr("Undo"), keys("⌘Z", "Ctrl+Z")).clicked() {
            model.undo();
            ui.close();
        }
    }
    kit::menu_separator(ui);
    if kit::menu_item(ui, Some(icon::CHART_BAR), tr("Chart"), "").clicked() {
        actions.push(GridAction::ShowChart);
        ui.close();
    }
    if kit::menu_item(ui, Some(icon::MAP_TRIFOLD), tr("Spatial view"), "").clicked() {
        actions.push(GridAction::ShowSpatial);
        ui.close();
    }
    // The fifth view (the pill has the first four).
    if kit::menu_item(ui, Some(icon::FILE_CODE), tr("XML view"), "").clicked() {
        state.view = ResultViewKind::Xml;
        actions.push(GridAction::SwitchView(ResultViewKind::Xml));
        ui.close();
    }
    kit::menu_separator(ui);
    kit::menu_caption(ui, tr("Panels"));
    panel_items(ui, state, actions);
}

/// The side panels: kind, icon and name.
pub(crate) const PANELS: [(PanelKind, &str, &str); 5] = [
    (PanelKind::Value, icon::EYE, "Value"),
    (PanelKind::Metadata, icon::INFO, "Metadata"),
    (PanelKind::Grouping, icon::STACK, "Grouping"),
    (PanelKind::Calc, icon::CALCULATOR, "Calc"),
    (PanelKind::References, icon::TREE_STRUCTURE, "References"),
];

fn auto_label(secs: u64) -> String {
    if secs.is_multiple_of(60) { trf("{} min", &[&(secs / 60)]) } else { trf("{} s", &[&secs]) }
}

fn set_auto_refresh(ui: &Ui, state: &mut ToolbarState, secs: Option<u64>, actions: &mut Vec<GridAction>) {
    state.auto_refresh_secs = secs;
    let now = ui.input(|i| i.time);
    state.rt.next_refresh = secs.map(|s| now + s as f64);
    actions.push(GridAction::AutoRefreshChanged(secs.map(Duration::from_secs)));
    if let Some(s) = secs {
        ui.ctx().request_repaint_after(Duration::from_secs(s));
    }
}

/// Emit `Refresh` when the auto refresh interval elapsed, unless there are
/// changes or a cell is being edited; schedules the next frame.
fn auto_refresh_tick(ui: &Ui, model: &ResultModel, editing: bool, state: &mut ToolbarState, actions: &mut Vec<GridAction>) {
    let Some(secs) = state.auto_refresh_secs else {
        state.rt.next_refresh = None;
        return;
    };
    let now = ui.input(|i| i.time);
    let next = *state.rt.next_refresh.get_or_insert(now + secs as f64);
    if now >= next {
        if !model.has_changes() && !model.fetch.fetching && !editing {
            actions.push(GridAction::Refresh);
        }
        state.rt.next_refresh = Some(now + secs as f64);
        ui.ctx().request_repaint_after(Duration::from_secs(secs));
    } else {
        ui.ctx().request_repaint_after(Duration::from_secs_f64(next - now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_by_parsing() {
        let cols: Vec<String> = ["id", "Name", "order date"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_order_by("name desc, id", &cols).unwrap(), vec![SortKey::desc(1), SortKey::asc(0)]);
        assert_eq!(parse_order_by("[order date] ASC", &cols).unwrap(), vec![SortKey::asc(2)]);
        assert_eq!(parse_order_by("\"order date\" DESC", &cols).unwrap(), vec![SortKey::desc(2)]);
        assert_eq!(parse_order_by("3 desc", &cols).unwrap(), vec![SortKey::desc(2)]);
        assert!(parse_order_by("nope", &cols).unwrap_err().contains("nope"));
        assert!(parse_order_by("", &cols).unwrap().is_empty());
    }

    #[test]
    fn texts() {
        assert_eq!(duration_text(Duration::from_millis(120)), "120 ms");
        assert_eq!(duration_text(Duration::from_millis(1250)), "1.25 s");
        assert_eq!(duration_text(Duration::from_secs(125)), "2 min 5 s");
        let mut m = ResultModel::from_rows(vec![ternarygrid_core::ColumnMeta::new("a", "int", ternarygrid_core::DataKind::Int)], vec![vec![ternarygrid_core::Value::Int(1)]; 1234]);
        m.fetch.has_more = true;
        m.fetch.total_count = Some(5000);
        m.fetch.elapsed = Some(Duration::from_millis(42));
        assert_eq!(status_text(&m), "1,234 rows · more available · total 5,000 · 42 ms");
        let at = chrono::Local::now();
        m.fetch.fetched_at = Some(at);
        // When they were read is in the status's tooltip, not in its text.
        assert!(status_text(&m).ends_with("42 ms"));
        let earlier = at - chrono::Duration::days(2);
        assert_eq!(fetched_at_text(earlier, at), earlier.format("%Y-%m-%d %H:%M").to_string());
        let mut s = ToolbarState::default();
        ToolbarState::push_history(&mut s.filter_history, " a = 1 ");
        ToolbarState::push_history(&mut s.filter_history, "b = 2");
        ToolbarState::push_history(&mut s.filter_history, "a = 1");
        assert_eq!(s.filter_history, vec!["a = 1".to_string(), "b = 2".to_string()]);
        s.set_panel_open(PanelKind::Calc, true);
        assert!(s.is_panel_open(PanelKind::Calc));
        s.set_panel_open(PanelKind::Calc, false);
        assert!(!s.is_panel_open(PanelKind::Calc));
    }
}
