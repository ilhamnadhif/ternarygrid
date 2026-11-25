//! Result toolbar and filter bar (FR-D4, FR-D6).

use crate::grid::{self, DataGridState, DataMode, GridAction, PanelKind, ResultViewKind};
use crate::model::ResultModel;
use crate::view::{ClientFilter, SortKey};
use egui::{Key, Popup, PopupCloseBehavior, RichText, Ui};
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

/// Strip a trailing SQL keyword sequence (`NULLS FIRST`, `DESC`, …) preceded by whitespace.
fn strip_trailing_words<'a>(s: &'a str, words: &[&str]) -> Option<&'a str> {
    let mut rest = s.trim_end();
    for &w in words.iter().rev() {
        let len = w.len();
        if rest.len() <= len || !rest.is_char_boundary(rest.len() - len) {
            return None;
        }
        let split = rest.len() - len;
        if !rest[split..].eq_ignore_ascii_case(w) {
            return None;
        }
        let before = &rest[..split];
        if !before.chars().next_back().is_some_and(char::is_whitespace) {
            return None;
        }
        rest = before.trim_end();
    }
    Some(rest)
}

/// Parse an ORDER BY list (`name DESC NULLS LAST, [order date]`) against model column names.
pub fn parse_order_by(text: &str, columns: &[String]) -> Result<Vec<SortKey>, String> {
    let mut keys = Vec::new();
    for part in text.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // Strip optional NULLS FIRST / NULLS LAST suffix before checking ASC / DESC.
        let p = strip_trailing_words(p, &["NULLS", "FIRST"]).or_else(|| strip_trailing_words(p, &["NULLS", "LAST"])).unwrap_or(p);
        let (name, desc) = if let Some(n) = strip_trailing_words(p, &["DESC"]) {
            (n, true)
        } else if let Some(n) = strip_trailing_words(p, &["ASC"]) {
            (n, false)
        } else {
            (p, false)
        };
        let unquoted = name
            .strip_prefix('[')
            .and_then(|n| n.strip_suffix(']'))
            .or_else(|| name.strip_prefix('"').and_then(|n| n.strip_suffix('"')))
            .or_else(|| name.strip_prefix('`').and_then(|n| n.strip_suffix('`')))
            .unwrap_or(name);
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
            let flexible = (ui.available_width() - order_w - apply_w - 3.0 * gap).max(110.0);
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

/// The ordering in effect: the ORDER BY text, else the grid's own sort (the headers' sort buttons).
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

/// The footer of a result, shared by table data tabs and the SQL editor (Option A · Pola A1):
/// - **Left**: Refresh split-button (`↻ ▾` with Auto refresh menu & active amber badge),
///   Fetch next pill (`↓ +200`) or red `Stop` button, Fetch all & Total count (at full width),
///   single-line tabular status (`400 rows · 162 ms`, read time in tooltip), and error chip.
/// - **Center**: Row editing (`+ Add ▾` split-button, Duplicate, Delete, Undo) and fixed-anchor
///   `Cancel` / `Save` commit cluster (warm-amber pill when dirty, spinner when saving, no delta
///   counter clutter), or a warm-amber read-only pill with optional `Virtual key…` shortcut.
/// - **Right**: Compact segmented view switcher (`Grid | Rec | Text | JSON ▾` with Chart & Spatial
///   inside the menu), `Export`, `Panels ▾` dropdown with active count badge, and `⋯` overflow
///   containing strictly the actions that did not fit on the bar ([`FooterFit`]).
pub struct ResultToolbar<'a> {
    model: &'a mut ResultModel,
    grid: &'a mut DataGridState,
    state: &'a mut ToolbarState,
    fetch_size: usize,
    can_edit: bool,
    editing: bool,
}

impl<'a> ResultToolbar<'a> {
    /// New toolbar.
    pub fn new(model: &'a mut ResultModel, grid: &'a mut DataGridState, state: &'a mut ToolbarState) -> Self {
        ResultToolbar { model, grid, state, fetch_size: 0, can_edit: true, editing: false }
    }

    /// The rows one "Fetch next" reads (its label says how many; 0: "Fetch more").
    pub fn fetch_size(mut self, rows: usize) -> Self {
        self.fetch_size = rows;
        self
    }

    /// Whether the grid takes edits now (not on a read-only connection or during a save): the
    /// row buttons stay in place, disabled.
    pub fn can_edit(mut self, can_edit: bool) -> Self {
        self.can_edit = can_edit;
        self
    }

    /// True while another view (such as Record view) has an active editor or dropdown open,
    /// pausing auto-refresh so in-progress edits are not discarded.
    pub fn editing(mut self, editing: bool) -> Self {
        self.editing = editing;
        self
    }

    /// Show it. Returns actions for the application.
    pub fn show(self, ui: &mut Ui) -> Vec<GridAction> {
        let ResultToolbar { model, grid, state, fetch_size, can_edit, editing } = self;
        let editable = model.is_editable();
        let saving = model.is_saving();
        let can_edit = can_edit && editable && !saving;
        let mut actions = Vec::new();
        auto_refresh_tick(ui, model, grid.is_editing() || editing, state, &mut actions);
        let p = ternarygrid_ui::pal(ui);
        let ic = ternarygrid_ui::style::icons(ui);
        let fetching = model.fetch.fetching;
        let has_more = model.fetch.has_more;
        let pending = model.pending_counts().total();
        let show_stop = model.fetch.fetching_for().is_some_and(|r| r >= crate::grid::CANCEL_AFTER);
        if let Some(running) = model.fetch.fetching_for()
            && running < crate::grid::CANCEL_AFTER
        {
            ui.ctx().request_repaint_after(crate::grid::CANCEL_AFTER - running);
        }
        let auto_text = state.auto_refresh_secs.map(auto_label);
        let fetch_label = if fetch_size > 0 { trf("Fetch next {}", &[&fetch_size]) } else { tr("Fetch more").to_string() };
        let fetch_short = if fetch_size > 0 { format!("+{fetch_size}") } else { tr("More").to_string() };
        let read_only = if editable { None } else { model.read_only_reason().cloned() };
        let dismissed_err = model.fetch.error.as_ref().filter(|e| state.dismissed_fetch_error.as_ref() == Some(*e)).cloned();
        let read_at = model.fetch.fetched_at.map(|at| fetched_at_text(at, chrono::Local::now()));
        let panels_visible = !state.open_panels.is_empty() && !state.panels_hidden;
        let panel_badge = panels_visible.then_some(state.open_panels.len()).filter(|n| *n > 0);
        let metrics = FooterMetrics {
            editable,
            saving,
            pending,
            show_stop,
            has_more,
            auto_text: auto_text.as_deref(),
            fetch_short: &fetch_short,
            read_only_label: read_only.as_ref().map(|r| r.short_label()),
            read_only_vkey: read_only.as_ref().is_some_and(|r| r.virtual_key_helps()),
            has_err_chip: dismissed_err.is_some(),
            view: state.view,
            panel_badge,
        };
        ui.horizontal(|ui| {
            ui.set_min_height(FOOTER_H);
            ui.spacing_mut().item_spacing.x = 0.0;
            let full_avail = ui.available_width();
            let (fit, fetch_shown) = choose_footer_plan(ui, full_avail, &metrics);
            let fetch_overflow = has_more && !show_stop && !fetch_shown;
            let has_more_menu = fit < FooterFit::Full || fetch_overflow;
            let job = status_job(ui, model, state, &p, fetch_shown);
            let status_desired_w = ui.painter().layout_job(job.clone()).size().x.ceil() + 2.0;
            let layout = measure_plan(ui, fit, fetch_shown, &metrics);
            let fixed_non_status = layout.left_ctrl_w + layout.err_chip_w + layout.center_w + layout.right_w + layout.zone_gaps;
            let status_avail = (full_avail - fixed_non_status).max(20.0);
            let status_w = status_desired_w.min(status_avail);
            let extra = (full_avail - fixed_non_status - status_w).max(0.0);
            let (left_spring, right_spring) = if layout.center_w > 0.0 {
                let l = (extra * 0.45).floor();
                (l, extra - l)
            } else {
                (0.0, extra)
            };

            // -----------------------------------------------------------------
            // Zone 1 (Left): Refresh ▾ | Stop / ↓ +200 | ⤓ # | Status | Error
            // -----------------------------------------------------------------
            let (refresh_resp, auto_resp) = refresh_split_button(ui, auto_text.as_deref(), model.has_changes() || fetching);
            if refresh_resp.clicked() {
                actions.push(GridAction::Refresh);
            }
            Popup::menu(&auto_resp).id(ui.id().with("auto_refresh_btn")).align(egui::RectAlign::TOP_START).show(|ui| auto_refresh_menu(ui, state, &mut actions));

            if show_stop {
                ui.add_space(CTRL_GAP);
                if stop_button(ui).clicked() {
                    actions.push(GridAction::CancelFetch);
                }
            } else if fetch_shown {
                ui.add_space(CTRL_GAP);
                let resp = ui.add_enabled_ui(!fetching, |ui| fetch_pill(ui, &fetch_short, &fetch_label, tr("Fetch next page"))).inner;
                if resp.clicked() {
                    actions.push(GridAction::FetchMore);
                }
            }

            if fit >= FooterFit::Full {
                ui.add_space(CTRL_GAP);
                let fetch_all_tip = if has_more { tr("Fetch all rows") } else { tr("All rows fetched") };
                let fetch_all =
                    ui.add_enabled_ui(has_more && !fetching, |ui| toolbar_icon_btn(ui, icon::DOWNLOAD_SIMPLE, tr("Fetch all rows"), fetch_all_tip, false, None, false)).inner;
                if fetch_all.clicked() {
                    actions.push(GridAction::FetchAll);
                }
                ui.add_space(INNER_GAP);
                let count = ui
                    .add_enabled_ui(!model.fetch.counting, |ui| {
                        toolbar_icon_btn(ui, icon::HASH, tr("Calculate total row count"), tr("Calculate total row count"), false, None, false)
                    })
                    .inner;
                if count.clicked() {
                    actions.push(GridAction::CountTotal);
                }
            }

            ui.add_space(STATUS_GAP);
            ui.allocate_ui_with_layout(egui::vec2(status_w, FOOTER_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let status = ui.add(egui::Label::new(job).selectable(false).truncate());
                if let Some(at) = read_at {
                    status.on_hover_text(trf("Read at {}", &[&at]));
                }
            });

            if let Some(e) = &dismissed_err {
                ui.add_space(CTRL_GAP);
                if fetch_error_chip(ui, e).clicked() {
                    state.dismissed_fetch_error = None;
                }
            }

            // -----------------------------------------------------------------
            // Zone 2 (Center): Row editing + Fixed Cancel/Save, or Read-only
            // -----------------------------------------------------------------
            if layout.center_w > 0.0 {
                ui.add_space(ZONE_GAP + left_spring);
                if editable {
                    row_edit_and_commit_zone(ui, fit, can_edit, saving, pending, model, grid, &mut actions);
                } else if let Some(r) = &read_only {
                    let show_vkey = r.virtual_key_helps() && fit >= FooterFit::Essential;
                    if read_only_pill(ui, r.short_label(), &r.message(), show_vkey) {
                        actions.push(GridAction::DefineVirtualKey { table: crate::model::source_table(model.columns()).ok() });
                    }
                }
            }

            // -----------------------------------------------------------------
            // Zone 3 (Right): View switcher | Export | Panels ▾ | ⋯
            // -----------------------------------------------------------------
            ui.add_space(ZONE_GAP + right_spring);
            let mut right_started = false;
            if fit >= FooterFit::Essential {
                right_started = true;
                view_switcher(ui, fit, state, &mut actions);
            }
            if fit >= FooterFit::Core {
                if right_started {
                    ui.add_space(GROUP_GAP);
                }
                right_started = true;
                let clicked = if fit >= FooterFit::Full {
                    toolbar_text_btn(ui, Some(icon::EXPORT), tr("Export"), tr("Export"), tr("Export…"), false, None).clicked()
                } else {
                    toolbar_icon_btn(ui, icon::EXPORT, tr("Export"), tr("Export…"), false, None, false).clicked()
                };
                if clicked {
                    actions.push(GridAction::Export { rows: None, columns: grid.visible_columns() });
                }
            }
            if right_started {
                ui.add_space(GROUP_GAP);
            }
            let panels_resp = panels_dropdown_button(ui, fit >= FooterFit::Core, panels_visible, panel_badge);
            Popup::menu(&panels_resp).id(ui.id().with("panels_menu")).align(egui::RectAlign::TOP_END).show(|ui| panels_dropdown_menu(ui, state, panels_visible, &mut actions));

            if has_more_menu {
                ui.add_space(CTRL_GAP);
                let footer = MoreContext { fit, can_edit, fetch: fetch_overflow.then_some(fetch_label) };
                let more = toolbar_icon_btn(ui, icon::DOTS_THREE, tr("More result actions"), tr("More result actions"), false, None, false);
                Popup::menu(&more).id(ui.id().with("result_more")).align(egui::RectAlign::TOP_END).show(|ui| more_menu(ui, model, grid, state, &footer, &mut actions));
            }
            let _ = ic;
        });
        actions
    }
}

/// Build the single-line status job (`400 rows · 162 ms` — omitting the redundant "more available"
/// when the clickable `↓ +200` fetch pill is shown directly beside it).
fn status_job(ui: &Ui, model: &ResultModel, state: &ToolbarState, p: &ternarygrid_ui::Palette, fetch_shown: bool) -> egui::text::LayoutJob {
    let rows = crate::format::group_thousands(&model.row_count().to_string());
    let rows = trf("{} rows", &[&rows]);
    let mut rest = String::new();
    if model.has_client_view() && model.row_count() != model.stored_row_count() {
        rest.push_str(&trf(" (of {} fetched)", &[&crate::format::group_thousands(&model.stored_row_count().to_string())]));
    }
    if model.fetch.fetching {
        rest.push_str(" · ");
        rest.push_str(tr("fetching…"));
    } else if model.fetch.has_more && !fetch_shown {
        rest.push_str(" · ");
        rest.push_str(tr("more available"));
    }
    if model.fetch.counting {
        rest.push_str(" · ");
        rest.push_str(tr("counting…"));
    } else if let Some(t) = model.fetch.total_count {
        rest.push_str(&trf(" · total {}", &[&crate::format::group_thousands(&t.to_string())]));
    }
    if let Some(e) = model.fetch.elapsed {
        rest.push_str(" · ");
        rest.push_str(&duration_text(e));
    }
    if state.auto_refresh_secs.is_some() && (model.has_changes() || model.fetch.fetching) {
        rest.push_str(" · ");
        rest.push_str(tr("auto paused"));
    }
    let mut job = egui::text::LayoutJob::default();
    let size = 12.0;
    job.append(&rows, 0.0, egui::TextFormat::simple(ternarygrid_ui::style::medium(ui.ctx(), size), p.text));
    job.append(&rest, 0.0, egui::TextFormat::simple(egui::FontId::proportional(size), p.text_muted));
    job
}

/// Responsive tiers of the single-row Option A footer, narrowest to widest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FooterFit {
    /// Ultra-narrow: Refresh split, status, compact `+ ▾` (+ active Cancel/Save if dirty), compact Panels, `⋯`.
    Minimal,
    /// Narrow: adds compact 3-segment view switcher (`Grid | Rec | ▾`) and `+ Add ▾` label.
    Essential,
    /// Medium: adds Dup/Del/Undo, fixed Cancel/Save anchor, Export icon, full `Panels ▾` label, Text segment.
    Core,
    /// Wide: adds Fetch all, Total count, Export label, `JSON ▾` label, Save shortcut hint; no `⋯` unless fetch overflows.
    Full,
}

/// What the footer needs at each [`FooterFit`] (used for testing monotonic tier selection).
#[cfg(test)]
struct FooterWidths {
    essential: f32,
    core: f32,
    full: f32,
}

impl FooterFit {
    /// The widest tier that fits in `room`.
    #[cfg(test)]
    fn choose(room: f32, widths: &FooterWidths) -> Self {
        if room >= widths.full {
            FooterFit::Full
        } else if room >= widths.core {
            FooterFit::Core
        } else if room >= widths.essential {
            FooterFit::Essential
        } else {
            FooterFit::Minimal
        }
    }
}

/// Inputs that determine exact widths across all tiers.
struct FooterMetrics<'a> {
    editable: bool,
    saving: bool,
    pending: usize,
    show_stop: bool,
    has_more: bool,
    auto_text: Option<&'a str>,
    fetch_short: &'a str,
    read_only_label: Option<&'a str>,
    read_only_vkey: bool,
    has_err_chip: bool,
    view: ResultViewKind,
    panel_badge: Option<usize>,
}

/// Exact pixel budget of a concrete `(FooterFit, fetch_shown)` plan.
struct FooterPlanLayout {
    left_ctrl_w: f32,
    err_chip_w: f32,
    center_w: f32,
    right_w: f32,
    zone_gaps: f32,
}

/// What `⋯` overflow adds, strictly limited to what is not currently on the bar.
struct MoreContext {
    fit: FooterFit,
    can_edit: bool,
    /// `"Fetch next 200"` / `"Fetch more"` when it did not fit on the bar.
    fetch: Option<String>,
}

/// Height of the single-row Option A footer content area (`28 + 4 + 4 = 36 px` total bar height).
const FOOTER_H: f32 = 28.0;
/// Standard control height inside the footer.
const CTRL_H: f32 = 26.0;
/// Width of square icon controls inside the footer.
const ICON_BTN_W: f32 = 26.0;
/// Gap between adjacent icons inside one tight group.
const INNER_GAP: f32 = 2.0;
/// Gap between related controls inside a zone.
const CTRL_GAP: f32 = 4.0;
/// Gap between the left controls and the status label.
const STATUS_GAP: f32 = 8.0;
/// Gap between separate groups inside the right zone.
const GROUP_GAP: f32 = 6.0;
/// Minimum horizontal separation between Left, Center and Right zones.
const ZONE_GAP: f32 = 10.0;
/// Width of [`footer_divider`].
const DIVIDER_W: f32 = 9.0;
/// Minimum width reserved for the status text when picking a [`FooterFit`] tier.
const STATUS_MIN_W: f32 = 76.0;

fn measure_text(ui: &Ui, text: &str, font: egui::FontId) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER).size().x
}

fn measure_plan(ui: &Ui, fit: FooterFit, fetch_shown: bool, m: &FooterMetrics<'_>) -> FooterPlanLayout {
    let mut left_ctrl_w = refresh_split_width(ui, m.auto_text);
    if m.show_stop {
        left_ctrl_w += CTRL_GAP + stop_button_width(ui);
    } else if fetch_shown {
        left_ctrl_w += CTRL_GAP + fetch_pill_width(ui, m.fetch_short);
    }
    if fit >= FooterFit::Full {
        left_ctrl_w += CTRL_GAP + ICON_BTN_W + INNER_GAP + ICON_BTN_W;
    }
    left_ctrl_w += STATUS_GAP;

    let err_chip_w = if m.has_err_chip { CTRL_GAP + fetch_error_chip_width(ui) } else { 0.0 };

    let center_w = if m.editable {
        let mut w = add_split_width(ui, fit >= FooterFit::Essential);
        if fit >= FooterFit::Core {
            w += CTRL_GAP + ICON_BTN_W + INNER_GAP + ICON_BTN_W + INNER_GAP + ICON_BTN_W;
        }
        let show_commit = fit >= FooterFit::Core || m.pending > 0 || m.saving;
        if show_commit {
            let show_keys = fit >= FooterFit::Full;
            w += CTRL_GAP + DIVIDER_W + CTRL_GAP + commit_cluster_width(ui, m.saving, show_keys);
        }
        w
    } else if let Some(lbl) = m.read_only_label {
        read_only_pill_width(ui, lbl, m.read_only_vkey && fit >= FooterFit::Essential)
    } else {
        0.0
    };

    let fetch_overflow = m.has_more && !m.show_stop && !fetch_shown;
    let has_more_menu = fit < FooterFit::Full || fetch_overflow;
    let mut right_w = 0.0;
    let mut right_groups = 0usize;
    if fit >= FooterFit::Essential {
        right_w += view_switcher_width(ui, fit, m.view);
        right_groups += 1;
    }
    if fit >= FooterFit::Core {
        if right_groups > 0 {
            right_w += GROUP_GAP;
        }
        right_w += if fit >= FooterFit::Full { export_label_btn_width(ui) } else { ICON_BTN_W };
        right_groups += 1;
    }
    if right_groups > 0 {
        right_w += GROUP_GAP;
    }
    right_w += panels_dropdown_width(ui, fit >= FooterFit::Core, m.panel_badge);
    if has_more_menu {
        right_w += CTRL_GAP + ICON_BTN_W;
    }

    let zone_gaps = if center_w > 0.0 { 2.0 * ZONE_GAP } else { ZONE_GAP };
    FooterPlanLayout { left_ctrl_w, err_chip_w, center_w, right_w, zone_gaps }
}

fn choose_footer_plan(ui: &Ui, avail: f32, m: &FooterMetrics<'_>) -> (FooterFit, bool) {
    let want_fetch = m.has_more && !m.show_stop;
    let fits = |fit: FooterFit, fetch_shown: bool| {
        let l = measure_plan(ui, fit, fetch_shown, m);
        l.left_ctrl_w + STATUS_MIN_W + l.err_chip_w + l.center_w + l.right_w + l.zone_gaps <= avail
    };
    if fits(FooterFit::Full, want_fetch) {
        return (FooterFit::Full, want_fetch);
    }
    if fits(FooterFit::Core, want_fetch) {
        return (FooterFit::Core, want_fetch);
    }
    if fits(FooterFit::Essential, want_fetch) {
        return (FooterFit::Essential, want_fetch);
    }
    if want_fetch && fits(FooterFit::Essential, false) {
        return (FooterFit::Essential, false);
    }
    (FooterFit::Minimal, false)
}

/// Subtle vertical divider inside the footer.
fn footer_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(DIVIDER_W, 18.0), egui::Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), egui::Stroke::new(1.0, ternarygrid_ui::pal(ui).border_strong));
}

/// Return a painter that undoes egui's disabled opacity fade so custom disabled states stay legible.
fn crisp_painter(ui: &Ui, enabled: bool) -> egui::Painter {
    let mut painter = ui.painter().clone();
    if !enabled {
        painter.set_opacity((painter.opacity() / ui.visuals().disabled_alpha()).min(1.0));
    }
    painter
}

/// Quiet neutral 26×26 icon button for the footer (with optional semantic icon color & danger hover).
fn toolbar_icon_btn(ui: &mut Ui, glyph: &str, name: &str, tip: &str, on: bool, color: Option<egui::Color32>, danger_hover: bool) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let ic = ternarygrid_ui::style::icons(ui);
    let dark = ui.visuals().dark_mode;
    let enabled = ui.is_enabled();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ICON_BTN_W, CTRL_H), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, on, name));
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let pressed = enabled && resp.is_pointer_button_down_on();
        let painter = crisp_painter(ui, enabled);
        let radius = egui::CornerRadius::same(ternarygrid_ui::style::RADIUS);
        if on {
            painter.rect_filled(rect, radius, p.accent_soft);
            painter.rect_stroke(rect, radius, egui::Stroke::new(1.0, ternarygrid_ui::style::soft(p.accent_text, if dark { 0.35 } else { 0.25 })), egui::StrokeKind::Inside);
        } else if pressed {
            painter.rect_filled(rect, radius, p.subtle);
        } else if hovered {
            let hover_fill = if danger_hover { ternarygrid_ui::style::soft(ic.red, if dark { 0.16 } else { 0.11 }) } else { p.hover };
            painter.rect_filled(rect, radius, hover_fill);
        }
        if resp.has_focus() {
            painter.rect_stroke(rect, radius, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
        }
        let icon_color = if !enabled {
            p.text_faint.gamma_multiply(if dark { 0.55 } else { 0.65 })
        } else if on {
            p.accent_text
        } else if hovered && danger_hover {
            ic.red
        } else if let Some(c) = color {
            c
        } else if hovered {
            p.text
        } else {
            p.text_muted
        };
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, glyph, ternarygrid_ui::style::icon_font(14.0), icon_color);
    }
    resp
}

fn export_label_btn_width(ui: &Ui) -> f32 {
    (8.0 + 14.0 + 5.0 + measure_text(ui, tr("Export"), ternarygrid_ui::style::medium(ui.ctx(), 12.0)) + 8.0).ceil()
}

/// Quiet icon+text button for the footer (e.g. `Export`).
fn toolbar_text_btn(ui: &mut Ui, glyph: Option<&str>, text: &str, name: &str, tip: &str, on: bool, icon_tint: Option<egui::Color32>) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let dark = ui.visuals().dark_mode;
    let enabled = ui.is_enabled();
    let galley = ui.painter().layout_no_wrap(text.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 12.0), egui::Color32::PLACEHOLDER);
    let icon_w = if glyph.is_some() { 14.0 + 5.0 } else { 0.0 };
    let width = (8.0 + icon_w + galley.size().x + 8.0).ceil();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, CTRL_H), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, on, name));
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let pressed = enabled && resp.is_pointer_button_down_on();
        let painter = crisp_painter(ui, enabled);
        let radius = egui::CornerRadius::same(ternarygrid_ui::style::RADIUS);
        if on {
            painter.rect_filled(rect, radius, p.accent_soft);
            painter.rect_stroke(rect, radius, egui::Stroke::new(1.0, ternarygrid_ui::style::soft(p.accent_text, if dark { 0.35 } else { 0.25 })), egui::StrokeKind::Inside);
        } else if pressed {
            painter.rect_filled(rect, radius, p.subtle);
        } else if hovered {
            painter.rect_filled(rect, radius, p.hover);
        }
        let fg = if !enabled {
            p.text_faint
        } else if on {
            p.accent_text
        } else if hovered {
            p.text
        } else {
            p.text_muted
        };
        let mut x = rect.left() + 8.0;
        if let Some(g) = glyph {
            let ic_col = if !enabled { p.text_faint } else { icon_tint.unwrap_or(fg) };
            painter.text(egui::pos2(x + 7.0, rect.center().y), egui::Align2::CENTER_CENTER, g, ternarygrid_ui::style::icon_font(14.0), ic_col);
            x += 14.0 + 5.0;
        }
        painter.galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, fg);
    }
    resp
}

// -----------------------------------------------------------------------------
// Left Zone Widgets: Refresh ▾, Stop, ↓ +200, Error chip
// -----------------------------------------------------------------------------

fn refresh_split_width(ui: &Ui, auto_text: Option<&str>) -> f32 {
    let main_w = match auto_text {
        Some(t) => (8.0 + 13.0 + 4.0 + measure_text(ui, t, ternarygrid_ui::style::medium(ui.ctx(), 11.0)) + 6.0).ceil(),
        None => ICON_BTN_W,
    };
    main_w + 16.0
}

/// Split button for Refresh (`↻`) + Auto refresh menu (`▾`). Turns amber when auto-refresh is active.
fn refresh_split_button(ui: &mut Ui, auto_text: Option<&str>, paused: bool) -> (egui::Response, egui::Response) {
    let p = ternarygrid_ui::pal(ui);
    let ic = ternarygrid_ui::style::icons(ui);
    let dark = ui.visuals().dark_mode;
    let active = auto_text.is_some();
    let total_w = refresh_split_width(ui, auto_text);
    let main_w = total_w - 16.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total_w, CTRL_H), egui::Sense::hover());
    let main_rect = egui::Rect::from_min_size(rect.min, egui::vec2(main_w, CTRL_H));
    let drop_rect = egui::Rect::from_min_size(egui::pos2(main_rect.right(), rect.top()), egui::vec2(16.0, CTRL_H));
    let main_resp = ui.interact(main_rect, ui.id().with("refresh_main"), egui::Sense::click());
    main_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Refresh")));
    let refresh_tip = format!("{} ({})", tr("Refresh"), keys("⌘R", "Ctrl+R"));
    let main_resp = main_resp.on_hover_text(refresh_tip);

    let drop_resp = ui.interact(drop_rect, ui.id().with("refresh_auto_drop"), egui::Sense::click());
    drop_resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, active, tr("Auto refresh")));
    let auto_tip = match auto_text {
        Some(t) if paused => format!("{} ({t} — {})", tr("Auto refresh"), tr("paused while editing or fetching")),
        Some(t) => format!("{} ({t})", tr("Auto refresh")),
        None => tr("Auto refresh").to_string(),
    };
    let drop_resp = drop_resp.on_hover_text(auto_tip);

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let r = ternarygrid_ui::style::RADIUS;
        let radius = egui::CornerRadius::same(r);
        let any_hover = main_resp.hovered() || drop_resp.hovered();
        if active {
            let fill = ternarygrid_ui::style::soft(ic.amber, if dark { 0.16 } else { 0.12 });
            let stroke = ternarygrid_ui::style::soft(ic.amber, if dark { 0.36 } else { 0.28 });
            painter.rect(rect, radius, fill, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
            let hover_fill = ternarygrid_ui::style::soft(ic.amber, if dark { 0.26 } else { 0.20 });
            if main_resp.hovered() {
                painter.rect_filled(main_rect, egui::CornerRadius { nw: r, sw: r, ne: 0, se: 0 }, hover_fill);
            } else if drop_resp.hovered() {
                painter.rect_filled(drop_rect, egui::CornerRadius { nw: 0, sw: 0, ne: r, se: r }, hover_fill);
            }
            painter.vline(main_rect.right(), (rect.top() + 4.0)..=(rect.bottom() - 4.0), egui::Stroke::new(1.0, stroke));
        } else {
            if main_resp.is_pointer_button_down_on() {
                painter.rect_filled(main_rect, egui::CornerRadius { nw: r, sw: r, ne: 0, se: 0 }, p.subtle);
            } else if main_resp.hovered() {
                painter.rect_filled(main_rect, egui::CornerRadius { nw: r, sw: r, ne: 0, se: 0 }, p.hover);
            }
            if drop_resp.is_pointer_button_down_on() {
                painter.rect_filled(drop_rect, egui::CornerRadius { nw: 0, sw: 0, ne: r, se: r }, p.subtle);
            } else if drop_resp.hovered() {
                painter.rect_filled(drop_rect, egui::CornerRadius { nw: 0, sw: 0, ne: r, se: r }, p.hover);
            }
            if any_hover {
                painter.rect_stroke(rect, radius, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
                painter.vline(main_rect.right(), (rect.top() + 4.0)..=(rect.bottom() - 4.0), egui::Stroke::new(1.0, p.border));
            }
        }
        let main_color = if active {
            ic.amber
        } else if main_resp.hovered() {
            p.text
        } else {
            p.text_muted
        };
        if let Some(t) = auto_text {
            let mut x = main_rect.left() + 8.0;
            painter.text(egui::pos2(x + 6.5, main_rect.center().y), egui::Align2::CENTER_CENTER, icon::ARROW_CLOCKWISE, ternarygrid_ui::style::icon_font(13.5), main_color);
            x += 13.0 + 4.0;
            let g = painter.layout_no_wrap(t.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 11.0), main_color);
            painter.galley(egui::pos2(x, main_rect.center().y - g.size().y / 2.0), g, main_color);
        } else {
            painter.text(main_rect.center(), egui::Align2::CENTER_CENTER, icon::ARROW_CLOCKWISE, ternarygrid_ui::style::icon_font(14.0), main_color);
        }
        let drop_color = if active {
            ic.amber
        } else if drop_resp.hovered() {
            p.text
        } else {
            p.text_faint
        };
        painter.text(drop_rect.center(), egui::Align2::CENTER_CENTER, icon::CARET_DOWN, ternarygrid_ui::style::icon_font(9.5), drop_color);
    }
    (main_resp, drop_resp)
}

fn stop_button_width(ui: &Ui) -> f32 {
    (8.0 + 13.0 + 5.0 + measure_text(ui, tr("Stop"), ternarygrid_ui::style::medium(ui.ctx(), 11.5)) + 8.0).ceil()
}

/// Red Stop pill shown when a fetch has been running longer than `CANCEL_AFTER`.
fn stop_button(ui: &mut Ui) -> egui::Response {
    let ic = ternarygrid_ui::style::icons(ui);
    let dark = ui.visuals().dark_mode;
    let w = stop_button_width(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, CTRL_H), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Stop the query")));
    let resp = resp.on_hover_text(tr("Stop the query"));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = ternarygrid_ui::style::soft(
            ic.red,
            if resp.hovered() {
                if dark { 0.28 } else { 0.20 }
            } else if dark {
                0.17
            } else {
                0.11
            },
        );
        let stroke = ternarygrid_ui::style::soft(ic.red, if dark { 0.42 } else { 0.30 });
        painter.rect(rect, ternarygrid_ui::style::RADIUS, fill, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
        let mut x = rect.left() + 8.0;
        painter.text(egui::pos2(x + 6.5, rect.center().y), egui::Align2::CENTER_CENTER, icon::STOP_CIRCLE, ternarygrid_ui::style::icon_font(13.0), ic.red);
        x += 13.0 + 5.0;
        let g = painter.layout_no_wrap(tr("Stop").to_string(), ternarygrid_ui::style::medium(ui.ctx(), 11.5), ic.red);
        painter.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, ic.red);
    }
    resp
}

fn fetch_pill_width(ui: &Ui, short_text: &str) -> f32 {
    (7.0 + 12.0 + 4.0 + measure_text(ui, short_text, ternarygrid_ui::style::medium(ui.ctx(), 11.5)) + 8.0).ceil()
}

/// Teal accent-soft fetch pill (`↓ +200` / `↓ More`). Accessible label stays `"Fetch next 200"` / `"Fetch more"`.
fn fetch_pill(ui: &mut Ui, short_text: &str, accessible_name: &str, tip: &str) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let dark = ui.visuals().dark_mode;
    let enabled = ui.is_enabled();
    let w = fetch_pill_width(ui, short_text);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, accessible_name));
    let resp = resp.on_hover_text(tip);
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let painter = crisp_painter(ui, enabled);
        let fill = if !enabled {
            p.subtle
        } else if hovered {
            ternarygrid_ui::style::soft(p.accent, if dark { 0.30 } else { 0.20 })
        } else {
            ternarygrid_ui::style::soft(p.accent, if dark { 0.18 } else { 0.11 })
        };
        let stroke = if enabled { egui::Stroke::new(1.0, ternarygrid_ui::style::soft(p.accent_text, if dark { 0.34 } else { 0.24 })) } else { egui::Stroke::NONE };
        painter.rect(rect, ternarygrid_ui::style::RADIUS, fill, stroke, egui::StrokeKind::Inside);
        let fg = if enabled { p.accent_text } else { p.text_faint };
        let mut x = rect.left() + 7.0;
        painter.text(egui::pos2(x + 6.0, rect.center().y), egui::Align2::CENTER_CENTER, icon::ARROW_DOWN, ternarygrid_ui::style::icon_font(12.0), fg);
        x += 12.0 + 4.0;
        let g = painter.layout_no_wrap(short_text.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 11.5), fg);
        painter.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, fg);
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    resp
}

fn fetch_error_chip_width(ui: &Ui) -> f32 {
    (7.0 + 12.0 + 4.0 + measure_text(ui, tr("Fetch failed"), ternarygrid_ui::style::medium(ui.ctx(), 11.0)) + 7.0).ceil()
}

/// Compact red clickable chip to restore a dismissed fetch error banner.
fn fetch_error_chip(ui: &mut Ui, full_error: &str) -> egui::Response {
    let ic = ternarygrid_ui::style::icons(ui);
    let dark = ui.visuals().dark_mode;
    let w = fetch_error_chip_width(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("The fetch failed")));
    let resp = resp.on_hover_text(full_error);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = ternarygrid_ui::style::soft(
            ic.red,
            if resp.hovered() {
                if dark { 0.26 } else { 0.18 }
            } else if dark {
                0.15
            } else {
                0.10
            },
        );
        let stroke = ternarygrid_ui::style::soft(ic.red, if dark { 0.35 } else { 0.25 });
        painter.rect(rect, ternarygrid_ui::style::RADIUS, fill, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
        let mut x = rect.left() + 7.0;
        painter.text(egui::pos2(x + 6.0, rect.center().y), egui::Align2::CENTER_CENTER, icon::WARNING_CIRCLE, ternarygrid_ui::style::icon_font(12.0), ic.red);
        x += 12.0 + 4.0;
        let g = painter.layout_no_wrap(tr("Fetch failed").to_string(), ternarygrid_ui::style::medium(ui.ctx(), 11.0), ic.red);
        painter.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, ic.red);
    }
    resp
}

// -----------------------------------------------------------------------------
// Center Zone Widgets: + Add ▾, Dup/Del/Undo, Fixed Cancel/Save, Read-only Pill
// -----------------------------------------------------------------------------

fn add_split_width(ui: &Ui, show_label: bool) -> f32 {
    let main_w = if show_label { (8.0 + 13.0 + 5.0 + measure_text(ui, tr("Add"), ternarygrid_ui::style::medium(ui.ctx(), 12.0)) + 8.0).ceil() } else { ICON_BTN_W };
    main_w + 16.0
}

/// `+ Add ▾` split button: main click inserts row after selection; chevron opens all row actions.
fn add_split_button(ui: &mut Ui, show_label: bool, enabled: bool) -> (egui::Response, egui::Response) {
    let p = ternarygrid_ui::pal(ui);
    let ic = ternarygrid_ui::style::icons(ui);
    let dark = ui.visuals().dark_mode;
    let total_w = add_split_width(ui, show_label);
    let main_w = total_w - 16.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total_w, CTRL_H), egui::Sense::hover());
    let main_rect = egui::Rect::from_min_size(rect.min, egui::vec2(main_w, CTRL_H));
    let drop_rect = egui::Rect::from_min_size(egui::pos2(main_rect.right(), rect.top()), egui::vec2(16.0, CTRL_H));

    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let main_resp = ui.interact(main_rect, ui.id().with("add_row_main"), sense);
    main_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tr("Add row")));
    let add_tip = if cfg!(target_os = "macos") { tr("Add a row after the selected one") } else { tr("Add a row after the selected one (Insert)") };
    let main_resp = main_resp.on_hover_text(add_tip);

    let drop_resp = ui.interact(drop_rect, ui.id().with("add_row_drop"), sense);
    drop_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tr("More row actions")));
    let drop_resp = drop_resp.on_hover_text(tr("More row actions"));

    if ui.is_rect_visible(rect) {
        let painter = crisp_painter(ui, enabled);
        let r = ternarygrid_ui::style::RADIUS;
        let radius = egui::CornerRadius::same(r);
        let main_hover = enabled && main_resp.hovered();
        let drop_hover = enabled && drop_resp.hovered();
        let any_hover = main_hover || drop_hover;
        if main_hover && main_resp.is_pointer_button_down_on() {
            painter.rect_filled(main_rect, egui::CornerRadius { nw: r, sw: r, ne: 0, se: 0 }, p.subtle);
        } else if main_hover {
            painter.rect_filled(main_rect, egui::CornerRadius { nw: r, sw: r, ne: 0, se: 0 }, p.hover);
        }
        if drop_hover && drop_resp.is_pointer_button_down_on() {
            painter.rect_filled(drop_rect, egui::CornerRadius { nw: 0, sw: 0, ne: r, se: r }, p.subtle);
        } else if drop_hover {
            painter.rect_filled(drop_rect, egui::CornerRadius { nw: 0, sw: 0, ne: r, se: r }, p.hover);
        }
        if any_hover {
            painter.rect_stroke(rect, radius, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
            painter.vline(main_rect.right(), (rect.top() + 4.0)..=(rect.bottom() - 4.0), egui::Stroke::new(1.0, p.border));
        }
        let plus_color = if !enabled { p.text_faint.gamma_multiply(if dark { 0.55 } else { 0.65 }) } else { ic.green };
        let text_color = if !enabled {
            p.text_faint.gamma_multiply(if dark { 0.55 } else { 0.65 })
        } else if main_hover {
            p.text
        } else {
            p.text_muted
        };
        if show_label {
            let mut x = main_rect.left() + 8.0;
            painter.text(egui::pos2(x + 6.5, main_rect.center().y), egui::Align2::CENTER_CENTER, icon::PLUS, ternarygrid_ui::style::icon_font(13.5), plus_color);
            x += 13.0 + 5.0;
            let g = painter.layout_no_wrap(tr("Add").to_string(), ternarygrid_ui::style::medium(ui.ctx(), 12.0), text_color);
            painter.galley(egui::pos2(x, main_rect.center().y - g.size().y / 2.0), g, text_color);
        } else {
            painter.text(main_rect.center(), egui::Align2::CENTER_CENTER, icon::PLUS, ternarygrid_ui::style::icon_font(14.0), plus_color);
        }
        let caret_color = if !enabled {
            p.text_faint.gamma_multiply(if dark { 0.55 } else { 0.65 })
        } else if drop_hover {
            p.text
        } else {
            p.text_faint
        };
        painter.text(drop_rect.center(), egui::Align2::CENTER_CENTER, icon::CARET_DOWN, ternarygrid_ui::style::icon_font(9.5), caret_color);
    }
    (main_resp, drop_resp)
}

/// Center zone for editable models: `+ Add ▾`, core row icons (`Dup`, `Del`, `Undo`), and fixed-anchor `Cancel` / `Save`.
#[allow(clippy::too_many_arguments)]
fn row_edit_and_commit_zone(
    ui: &mut Ui,
    fit: FooterFit,
    can_edit: bool,
    saving: bool,
    pending: usize,
    model: &mut ResultModel,
    grid: &mut DataGridState,
    actions: &mut Vec<GridAction>,
) {
    let notify_error = |r: Result<usize, String>, actions: &mut Vec<GridAction>| {
        if let Err(message) = r {
            actions.push(GridAction::Notify { message, error: true });
        }
    };
    let selected = !grid.selection().is_empty();
    let (add_resp, drop_resp) = add_split_button(ui, fit >= FooterFit::Essential, can_edit);
    if can_edit && add_resp.clicked() {
        notify_error(grid::add_row(model, grid).map(|_| 0), actions);
    }
    Popup::menu(&drop_resp).id(ui.id().with("row_actions_menu")).align(egui::RectAlign::TOP_START).show(|ui| {
        kit::menu_look(ui, 240.0);
        ui.add_enabled_ui(can_edit, |ui| {
            if kit::menu_item(ui, Some(icon::PLUS), tr("Add row after"), keys("", "Insert")).clicked() {
                notify_error(grid::add_row(model, grid).map(|_| 0), actions);
                ui.close();
            }
            if kit::menu_item(ui, Some(icon::ROWS_PLUS_TOP), tr("Add row before"), keys("", "Shift+Insert")).clicked() {
                notify_error(grid::add_row_before(model, grid), actions);
                ui.close();
            }
            kit::menu_separator(ui);
            if ui.add_enabled_ui(selected, |ui| kit::menu_item(ui, Some(icon::COPY), tr("Duplicate selected rows"), keys("⌘D", "Ctrl+D"))).inner.clicked() {
                notify_error(grid::duplicate_selected(model, grid), actions);
                ui.close();
            }
            if ui.add_enabled_ui(selected, |ui| kit::menu_item(ui, Some(icon::ARROW_FAT_LINE_DOWN), tr("Copy from row above"), keys("⌘⌥↑", "Ctrl+Alt+Up"))).inner.clicked() {
                notify_error(grid::copy_from_adjacent(model, grid, true), actions);
                ui.close();
            }
            if ui.add_enabled_ui(selected, |ui| kit::menu_item(ui, Some(icon::ARROW_FAT_LINE_UP), tr("Copy from row below"), keys("⌘⌥↓", "Ctrl+Alt+Down"))).inner.clicked() {
                notify_error(grid::copy_from_adjacent(model, grid, false), actions);
                ui.close();
            }
            if fit < FooterFit::Core {
                kit::menu_separator(ui);
                if ui.add_enabled_ui(model.can_undo(), |ui| kit::menu_item(ui, Some(icon::ARROW_U_UP_LEFT), tr("Undo"), keys("⌘Z", "Ctrl+Z"))).inner.clicked() {
                    model.undo();
                    ui.close();
                }
                if ui.add_enabled_ui(selected, |ui| kit::menu_item_danger(ui, Some(icon::TRASH), tr("Delete selected rows"), keys("fn⌫", "Delete"))).inner.clicked() {
                    delete_rows(model, grid, actions);
                    ui.close();
                }
            }
        });
    });

    if fit >= FooterFit::Core {
        ui.add_space(CTRL_GAP);
        let dup_tip = format!("{} ({})", tr("Duplicate selected rows"), keys("⌘D", "Ctrl+D"));
        if ui.add_enabled_ui(can_edit && selected, |ui| toolbar_icon_btn(ui, icon::COPY, tr("Duplicate"), &dup_tip, false, None, false)).inner.clicked() {
            notify_error(grid::duplicate_selected(model, grid), actions);
        }
        ui.add_space(INNER_GAP);
        let del_tip = format!("{} ({})", tr("Delete selected rows"), keys("fn⌫", "Delete"));
        if ui.add_enabled_ui(can_edit && selected, |ui| toolbar_icon_btn(ui, icon::TRASH, tr("Delete"), &del_tip, false, None, true)).inner.clicked() {
            delete_rows(model, grid, actions);
        }
        ui.add_space(INNER_GAP);
        let undo_tip = format!("{} ({})", tr("Undo"), keys("⌘Z", "Ctrl+Z"));
        if ui.add_enabled_ui(can_edit && model.can_undo(), |ui| toolbar_icon_btn(ui, icon::ARROW_U_UP_LEFT, tr("Undo"), &undo_tip, false, None, false)).inner.clicked() {
            model.undo();
        }
    }

    let show_commit = fit >= FooterFit::Core || pending > 0 || saving;
    if show_commit {
        ui.add_space(CTRL_GAP);
        footer_divider(ui);
        ui.add_space(CTRL_GAP);
        let show_keys = fit >= FooterFit::Full;
        match commit_cluster(ui, pending, saving, show_keys) {
            Some(true) => actions.push(grid::save_action(model)),
            Some(false) => {
                model.discard_changes();
                actions.push(GridAction::ChangesDiscarded);
            }
            None => {}
        }
    }
}

/// Width of the fixed-anchor Cancel / Save cluster (no `+1 ~2 -1` counters).
fn commit_cluster_width(ui: &Ui, saving: bool, show_keys: bool) -> f32 {
    let cancel_w = commit_cancel_width(ui);
    let save_w = commit_save_width(ui, saving, show_keys);
    3.0 + cancel_w + 4.0 + save_w + 3.0
}

fn commit_cancel_width(ui: &Ui) -> f32 {
    (9.0 + measure_text(ui, tr("Cancel"), egui::FontId::proportional(12.0)) + 9.0).ceil()
}

fn commit_save_width(ui: &Ui, saving: bool, show_keys: bool) -> f32 {
    let medium = ternarygrid_ui::style::medium(ui.ctx(), 12.0);
    if saving {
        (9.0 + 12.0 + 6.0 + measure_text(ui, tr("Saving…"), medium) + 10.0).ceil()
    } else {
        let keys_w = if show_keys { 6.0 + measure_text(ui, SAVE_KEYS, egui::FontId::proportional(10.5)) } else { 0.0 };
        (9.0 + 13.0 + 5.0 + measure_text(ui, tr("Save"), medium) + keys_w + 10.0).ceil()
    }
}

/// Fixed-anchor DBeaver-style Cancel / Save commit cluster (Pola A1, without `+1 ~2 -1` badges).
/// Returns `Some(true)` on Save, `Some(false)` on Cancel/Discard.
fn commit_cluster(ui: &mut Ui, pending: usize, saving: bool, show_keys: bool) -> Option<bool> {
    let p = ternarygrid_ui::pal(ui);
    let dark = ui.visuals().dark_mode;
    let dirty = pending > 0 && !saving;
    let total_w = commit_cluster_width(ui, saving, show_keys);
    let cancel_w = commit_cancel_width(ui);
    let save_w = commit_save_width(ui, saving, show_keys);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total_w, FOOTER_H), egui::Sense::hover());

    if ui.is_rect_visible(rect) {
        if dirty {
            let modified = crate::theme::GridTheme::from_visuals(ui.visuals()).modified;
            let border = ternarygrid_ui::style::soft(p.warn, if dark { 0.42 } else { 0.35 });
            ui.painter().rect(rect, 7, modified, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
        } else if saving {
            ui.painter().rect(rect, 7, p.subtle, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
        }
    }

    let btn_h = 22.0;
    let top = rect.center().y - btn_h / 2.0;
    let cancel_rect = egui::Rect::from_min_size(egui::pos2(rect.left() + 3.0, top), egui::vec2(cancel_w, btn_h));
    let save_rect = egui::Rect::from_min_size(egui::pos2(cancel_rect.right() + 4.0, top), egui::vec2(save_w, btn_h));

    let mut out = None;

    // Cancel button (visual text "Cancel", accessible name "Discard" to avoid colliding with modal "Cancel").
    let cancel_sense = if dirty { egui::Sense::click() } else { egui::Sense::hover() };
    let cancel_resp = ui.interact(cancel_rect, ui.id().with("commit_cancel"), cancel_sense);
    cancel_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, dirty, tr("Discard")));
    let cancel_tip =
        if dirty { ternarygrid_core::trn(pending, tr("Discard 1 pending change"), tr("Discard {} pending changes")) } else { tr("No pending changes to discard").to_string() };
    let cancel_resp = cancel_resp.on_hover_text(cancel_tip);
    if ui.is_rect_visible(cancel_rect) {
        let hovered = dirty && cancel_resp.hovered();
        let pressed = dirty && cancel_resp.is_pointer_button_down_on();
        let fill = if pressed {
            p.subtle
        } else if hovered {
            p.hover
        } else {
            egui::Color32::TRANSPARENT
        };
        if fill != egui::Color32::TRANSPARENT {
            ui.painter().rect_filled(cancel_rect, 5, fill);
        }
        let fg = if dirty { p.text } else { p.text_faint.gamma_multiply(if dark { 0.58 } else { 0.68 }) };
        let g = ui.painter().layout_no_wrap(tr("Cancel").to_string(), egui::FontId::proportional(12.0), fg);
        ui.painter().galley(egui::pos2(cancel_rect.center().x - g.size().x / 2.0, cancel_rect.center().y - g.size().y / 2.0), g, fg);
    }
    if dirty && cancel_resp.clicked() {
        out = Some(false);
    }

    // Save / Saving… button.
    let save_sense = if dirty { egui::Sense::click() } else { egui::Sense::hover() };
    let save_resp = ui.interact(save_rect, ui.id().with("commit_save"), save_sense);
    let save_a11y = if saving { tr("Saving…") } else { tr("Save") };
    save_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, dirty, save_a11y));
    let save_tip = if saving {
        tr("Saving pending changes…").to_string()
    } else if dirty {
        format!("{} ({SAVE_KEYS})", ternarygrid_core::trn(pending, tr("Save 1 pending change"), tr("Save {} pending changes")))
    } else {
        format!("{} ({SAVE_KEYS})", tr("No pending changes to save"))
    };
    let save_resp = save_resp.on_hover_text(save_tip);
    if ui.is_rect_visible(save_rect) {
        let hovered = dirty && save_resp.hovered();
        let (fill, stroke, fg) = if saving {
            (p.accent_soft, egui::Stroke::new(1.0, ternarygrid_ui::style::soft(p.accent_text, 0.30)), p.accent_text)
        } else if dirty {
            (if hovered { p.accent_hover } else { p.accent }, egui::Stroke::NONE, p.on_accent)
        } else {
            (p.subtle, egui::Stroke::new(1.0, p.border), p.text_faint.gamma_multiply(if dark { 0.65 } else { 0.72 }))
        };
        ui.painter().rect(save_rect, 5, fill, stroke, egui::StrokeKind::Inside);
        if save_resp.has_focus() {
            ui.painter().rect_stroke(save_rect.expand(1.5), 6, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Outside);
        }
        let mut x = save_rect.left() + 9.0;
        if saving {
            paint_mini_spinner(ui, egui::pos2(x + 6.0, save_rect.center().y), 5.0, fg);
            x += 12.0 + 6.0;
            let g = ui.painter().layout_no_wrap(tr("Saving…").to_string(), ternarygrid_ui::style::medium(ui.ctx(), 12.0), fg);
            ui.painter().galley(egui::pos2(x, save_rect.center().y - g.size().y / 2.0), g, fg);
        } else {
            ui.painter().text(egui::pos2(x + 6.5, save_rect.center().y), egui::Align2::CENTER_CENTER, icon::FLOPPY_DISK, ternarygrid_ui::style::icon_font(13.0), fg);
            x += 13.0 + 5.0;
            let g = ui.painter().layout_no_wrap(tr("Save").to_string(), ternarygrid_ui::style::medium(ui.ctx(), 12.0), fg);
            let gw = g.size().x;
            ui.painter().galley(egui::pos2(x, save_rect.center().y - g.size().y / 2.0), g, fg);
            if show_keys {
                let kc = if dirty { fg.gamma_multiply(0.80) } else { fg.gamma_multiply(0.75) };
                let kg = ui.painter().layout_no_wrap(SAVE_KEYS.to_string(), egui::FontId::proportional(10.5), kc);
                ui.painter().galley(egui::pos2(x + gw + 6.0, save_rect.center().y - kg.size().y / 2.0), kg, kc);
            }
        }
    }
    if dirty && save_resp.clicked() {
        out = Some(true);
    }
    out
}

/// Tiny rotating arc spinner painted only while a save is running (zero idle repaints).
fn paint_mini_spinner(ui: &Ui, center: egui::Pos2, radius: f32, color: egui::Color32) {
    let t = ui.input(|i| i.time) as f32;
    let start = t * std::f32::consts::TAU * 1.15;
    let sweep = std::f32::consts::TAU * 0.68;
    let steps = 14usize;
    let pts: Vec<egui::Pos2> = (0..=steps)
        .map(|i| {
            let a = start + sweep * (i as f32 / steps as f32);
            egui::pos2(center.x + a.cos() * radius, center.y + a.sin() * radius)
        })
        .collect();
    ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.6, color)));
    ui.ctx().request_repaint();
}

fn read_only_pill_width(ui: &Ui, short_label: &str, show_vkey: bool) -> f32 {
    let base = (8.0 + 13.0 + 5.0 + measure_text(ui, short_label, ternarygrid_ui::style::medium(ui.ctx(), 11.5)) + 8.0).ceil();
    if show_vkey { base + 4.0 + 6.0 + 4.0 + measure_text(ui, tr("Virtual key…"), ternarygrid_ui::style::medium(ui.ctx(), 11.5)).ceil() + 2.0 } else { base }
}

/// Warm-amber read-only pill with optional clickable `Virtual key…` action. Returns true if clicked.
fn read_only_pill(ui: &mut Ui, short_label: &str, reason_msg: &str, show_vkey: bool) -> bool {
    let p = ternarygrid_ui::pal(ui);
    let dark = ui.visuals().dark_mode;
    let total_w = read_only_pill_width(ui, short_label, show_vkey);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(total_w, 24.0), egui::Sense::hover());
    let _ = resp.on_hover_text(reason_msg);
    let painter = ui.painter();
    let fill = ternarygrid_ui::style::soft(p.warn, if dark { 0.14 } else { 0.10 });
    let stroke = ternarygrid_ui::style::soft(p.warn, if dark { 0.30 } else { 0.24 });
    painter.rect(rect, ternarygrid_ui::style::RADIUS, fill, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let mut x = rect.left() + 8.0;
    painter.text(egui::pos2(x + 6.5, rect.center().y), egui::Align2::CENTER_CENTER, icon::LOCK_SIMPLE, ternarygrid_ui::style::icon_font(12.5), p.warn);
    x += 13.0 + 5.0;
    let lbl = painter.layout_no_wrap(short_label.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 11.5), p.warn);
    let lbl_w = lbl.size().x;
    painter.galley(egui::pos2(x, rect.center().y - lbl.size().y / 2.0), lbl, p.warn);
    if !show_vkey {
        return false;
    }
    x += lbl_w + 5.0;
    painter.text(egui::pos2(x + 2.0, rect.center().y), egui::Align2::CENTER_CENTER, "·", egui::FontId::proportional(12.0), p.warn.gamma_multiply(0.7));
    x += 8.0;
    let vkey_text = tr("Virtual key…");
    let vkey_g = painter.layout_no_wrap(vkey_text.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 11.5), p.accent_text);
    let vkey_rect = egui::Rect::from_min_size(egui::pos2(x - 2.0, rect.top() + 2.0), egui::vec2(vkey_g.size().x + 4.0, rect.height() - 4.0));
    let vkey_resp = ui.interact(vkey_rect, ui.id().with("ro_virtual_key"), egui::Sense::click());
    vkey_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, vkey_text));
    let vkey_resp = vkey_resp.on_hover_text(tr("Define a virtual key to edit this table"));
    if vkey_resp.hovered() {
        painter.rect_filled(vkey_rect, 4, p.accent_soft);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    painter.galley(egui::pos2(x, rect.center().y - vkey_g.size().y / 2.0), vkey_g, p.accent_text);
    vkey_resp.clicked()
}

/// Mark the selected rows deleted and say how many (as the keys and the context menu do).
fn delete_rows(model: &mut ResultModel, grid: &mut DataGridState, actions: &mut Vec<GridAction>) {
    match grid::delete_selected(model, grid) {
        Ok(n) => actions.push(GridAction::Notify { message: ternarygrid_core::trn(n, tr("1 row marked for deletion"), tr("{} rows marked for deletion")), error: false }),
        Err(message) => actions.push(GridAction::Notify { message, error: true }),
    }
}

// -----------------------------------------------------------------------------
// Right Zone Widgets: Segmented View Switcher & Panels Dropdown
// -----------------------------------------------------------------------------

/// The views of the rows: kind, icon, full accessible name, concise label.
const VIEWS: [(ResultViewKind, &str, &str, &str); 5] = [
    (ResultViewKind::Grid, icon::TABLE, "Grid", "Grid"),
    (ResultViewKind::Text, icon::TEXT_AA, "Text", "Text"),
    (ResultViewKind::Record, icon::LIST_DASHES, "Record", "Rec"),
    (ResultViewKind::Json, icon::BRACKETS_CURLY, "JSON", "JSON"),
    (ResultViewKind::Xml, icon::FILE_CODE, "XML", "XML"),
];

fn view_seg_labeled_w(ui: &Ui, label: &str) -> f32 {
    (7.0 + 13.0 + 4.0 + measure_text(ui, label, ternarygrid_ui::style::medium(ui.ctx(), 11.5)) + 7.0).ceil()
}

fn view_extra_seg_w(ui: &Ui, fit: FooterFit) -> f32 {
    if fit >= FooterFit::Full {
        let max_lbl = measure_text(ui, "JSON", ternarygrid_ui::style::medium(ui.ctx(), 11.5)).max(measure_text(ui, "XML", ternarygrid_ui::style::medium(ui.ctx(), 11.5)));
        (7.0 + 13.0 + 4.0 + max_lbl + 4.0 + 9.0 + 6.0).ceil()
    } else if fit >= FooterFit::Core {
        34.0
    } else {
        24.0
    }
}

fn view_switcher_width(ui: &Ui, fit: FooterFit, _current: ResultViewKind) -> f32 {
    if fit < FooterFit::Essential {
        return 0.0;
    }
    if fit == FooterFit::Essential {
        // [▦ | ☰ | ▾]
        return 2.0 + 26.0 + 1.0 + 26.0 + 1.0 + view_extra_seg_w(ui, fit) + 2.0;
    }
    let grid_w = view_seg_labeled_w(ui, tr("Grid"));
    let rec_w = view_seg_labeled_w(ui, tr("Rec"));
    let text_w = 26.0;
    let extra_w = view_extra_seg_w(ui, fit);
    2.0 + grid_w + 1.0 + rec_w + 1.0 + text_w + 1.0 + extra_w + 2.0
}

/// Compact segmented view switcher:
/// - `Full`: `[▦ Grid | ☰ Rec | Aa | {} JSON ▾]`
/// - `Core`: `[▦ Grid | ☰ Rec | Aa | {} ▾]`
/// - `Essential`: `[▦ | ☰ | ▾]`
///
/// Clicking the last segment opens a popover with JSON, XML, Chart, Spatial view (plus Text at Essential).
fn view_switcher(ui: &mut Ui, fit: FooterFit, state: &mut ToolbarState, actions: &mut Vec<GridAction>) {
    let p = ternarygrid_ui::pal(ui);
    let total_w = view_switcher_width(ui, fit, state.view);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total_w, CTRL_H), egui::Sense::hover());
    ui.painter().rect_filled(rect, 7, p.subtle);

    let bh = CTRL_H - 4.0;
    let top = rect.top() + 2.0;
    let mut x = rect.left() + 2.0;

    let draw_seg = |ui: &mut Ui, x: &mut f32, w: f32, id_salt: &str, glyph: &str, label: Option<&str>, name: &str, tip: &str, on: bool| -> egui::Response {
        let r = egui::Rect::from_min_size(egui::pos2(*x, top), egui::vec2(w, bh));
        *x += w + 1.0;
        let resp = ui.interact(r, ui.id().with(("view_seg", id_salt)), egui::Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, name));
        let resp = resp.on_hover_text(tip);
        if ui.is_rect_visible(r) {
            if on {
                kit::raised(ui.painter(), r, egui::CornerRadius::same(5), p.thumb);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 5, p.hover);
            }
            let icon_color = if on {
                p.accent_text
            } else if resp.hovered() {
                p.text
            } else {
                p.text_muted
            };
            let text_color = if on || resp.hovered() { p.text } else { p.text_muted };
            if let Some(lbl) = label {
                let lx = r.left() + 7.0;
                ui.painter().text(egui::pos2(lx + 6.5, r.center().y), egui::Align2::CENTER_CENTER, glyph, ternarygrid_ui::style::icon_font(13.0), icon_color);
                let g = ui.painter().layout_no_wrap(lbl.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 11.5), text_color);
                ui.painter().galley(egui::pos2(lx + 13.0 + 4.0, r.center().y - g.size().y / 2.0), g, text_color);
            } else {
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, glyph, ternarygrid_ui::style::icon_font(13.5), icon_color);
            }
        }
        resp
    };

    let labeled = fit >= FooterFit::Core;
    let grid_w = if labeled { view_seg_labeled_w(ui, tr("Grid")) } else { 26.0 };
    if draw_seg(ui, &mut x, grid_w, "grid", icon::TABLE, labeled.then_some(tr("Grid")), tr("Grid"), &trf("{} view", &[&tr("Grid")]), state.view == ResultViewKind::Grid).clicked()
        && state.view != ResultViewKind::Grid
    {
        state.view = ResultViewKind::Grid;
        actions.push(GridAction::SwitchView(ResultViewKind::Grid));
    }

    let rec_w = if labeled { view_seg_labeled_w(ui, tr("Rec")) } else { 26.0 };
    if draw_seg(ui, &mut x, rec_w, "record", icon::LIST_DASHES, labeled.then_some(tr("Rec")), tr("Record"), &trf("{} view", &[&tr("Record")]), state.view == ResultViewKind::Record)
        .clicked()
        && state.view != ResultViewKind::Record
    {
        state.view = ResultViewKind::Record;
        actions.push(GridAction::SwitchView(ResultViewKind::Record));
    }

    if labeled
        && draw_seg(ui, &mut x, 26.0, "text", icon::TEXT_AA, None, tr("Text"), &trf("{} view", &[&tr("Text")]), state.view == ResultViewKind::Text).clicked()
        && state.view != ResultViewKind::Text
    {
        state.view = ResultViewKind::Text;
        actions.push(GridAction::SwitchView(ResultViewKind::Text));
    }

    // Extra views & visualizations dropdown segment.
    let extra_w = view_extra_seg_w(ui, fit);
    let extra_rect = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(extra_w, bh));
    let extra_active = matches!(state.view, ResultViewKind::Json | ResultViewKind::Xml) || (!labeled && state.view == ResultViewKind::Text);
    let extra_resp = ui.interact(extra_rect, ui.id().with("view_extra_drop"), egui::Sense::click());
    extra_resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, extra_active, tr("More views")));
    let extra_resp = extra_resp.on_hover_text(tr("JSON, XML, Chart and Spatial views"));
    if ui.is_rect_visible(extra_rect) {
        if extra_active {
            kit::raised(ui.painter(), extra_rect, egui::CornerRadius::same(5), p.thumb);
        } else if extra_resp.hovered() {
            ui.painter().rect_filled(extra_rect, 5, p.hover);
        }
        let icon_color = if extra_active {
            p.accent_text
        } else if extra_resp.hovered() {
            p.text
        } else {
            p.text_muted
        };
        let text_color = if extra_active || extra_resp.hovered() { p.text } else { p.text_muted };
        let (glyph, lbl) = match state.view {
            ResultViewKind::Xml => (icon::FILE_CODE, "XML"),
            ResultViewKind::Text if !labeled => (icon::TEXT_AA, "Text"),
            _ => (icon::BRACKETS_CURLY, "JSON"),
        };
        if fit >= FooterFit::Full {
            let lx = extra_rect.left() + 7.0;
            ui.painter().text(egui::pos2(lx + 6.5, extra_rect.center().y), egui::Align2::CENTER_CENTER, glyph, ternarygrid_ui::style::icon_font(13.0), icon_color);
            let g = ui.painter().layout_no_wrap(lbl.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 11.5), text_color);
            ui.painter().galley(egui::pos2(lx + 13.0 + 4.0, extra_rect.center().y - g.size().y / 2.0), g, text_color);
            ui.painter().text(
                egui::pos2(extra_rect.right() - 10.5, extra_rect.center().y),
                egui::Align2::CENTER_CENTER,
                icon::CARET_DOWN,
                ternarygrid_ui::style::icon_font(9.0),
                if extra_active || extra_resp.hovered() { p.text_muted } else { p.text_faint },
            );
        } else if fit >= FooterFit::Core {
            ui.painter().text(egui::pos2(extra_rect.left() + 11.0, extra_rect.center().y), egui::Align2::CENTER_CENTER, glyph, ternarygrid_ui::style::icon_font(13.0), icon_color);
            ui.painter().text(
                egui::pos2(extra_rect.right() - 8.0, extra_rect.center().y),
                egui::Align2::CENTER_CENTER,
                icon::CARET_DOWN,
                ternarygrid_ui::style::icon_font(9.0),
                if extra_active || extra_resp.hovered() { p.text_muted } else { p.text_faint },
            );
        } else {
            ui.painter().text(extra_rect.center(), egui::Align2::CENTER_CENTER, icon::CARET_DOWN, ternarygrid_ui::style::icon_font(10.0), icon_color);
        }
    }
    Popup::menu(&extra_resp).id(ui.id().with("view_extra_menu")).align(egui::RectAlign::TOP_END).show(|ui| {
        kit::menu_look(ui, 190.0);
        kit::menu_caption(ui, tr("View"));
        if !labeled {
            let mut on = state.view == ResultViewKind::Text;
            if kit::menu_check(ui, &mut on, tr("Text")).clicked() {
                state.view = ResultViewKind::Text;
                actions.push(GridAction::SwitchView(ResultViewKind::Text));
                ui.close();
            }
        }
        for (kind, name) in [(ResultViewKind::Json, "JSON"), (ResultViewKind::Xml, "XML")] {
            let mut on = state.view == kind;
            if kit::menu_check(ui, &mut on, tr(name)).clicked() {
                state.view = kind;
                actions.push(GridAction::SwitchView(kind));
                ui.close();
            }
        }
        kit::menu_separator(ui);
        kit::menu_caption(ui, tr("Visualizations"));
        if kit::menu_item(ui, Some(icon::CHART_BAR), tr("Chart"), "").clicked() {
            actions.push(GridAction::ShowChart);
            ui.close();
        }
        if kit::menu_item(ui, Some(icon::MAP_TRIFOLD), tr("Spatial view"), "").clicked() {
            actions.push(GridAction::ShowSpatial);
            ui.close();
        }
    });
}

fn panels_dropdown_width(ui: &Ui, show_label: bool, badge: Option<usize>) -> f32 {
    let badge_w = if badge.is_some() { 5.0 + 16.0 } else { 0.0 };
    if show_label {
        (8.0 + 14.0 + 5.0 + measure_text(ui, tr("Panels"), ternarygrid_ui::style::medium(ui.ctx(), 12.0)) + badge_w + 4.0 + 9.0 + 7.0).ceil()
    } else {
        (7.0 + 14.0 + badge_w + 3.0 + 9.0 + 6.0).ceil()
    }
}

/// Consolidated `Panels ▾` dropdown button with geometric sidebar glyph & active-panel count pill.
fn panels_dropdown_button(ui: &mut Ui, show_label: bool, visible: bool, badge: Option<usize>) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let dark = ui.visuals().dark_mode;
    let w = panels_dropdown_width(ui, show_label, badge);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, CTRL_H), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, visible, tr("Panels")));
    let resp = resp.on_hover_text(tr("Value, metadata and statistics panels"));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = egui::CornerRadius::same(ternarygrid_ui::style::RADIUS);
        if visible {
            painter.rect_filled(rect, radius, p.accent_soft);
            painter.rect_stroke(rect, radius, egui::Stroke::new(1.0, ternarygrid_ui::style::soft(p.accent_text, if dark { 0.36 } else { 0.25 })), egui::StrokeKind::Inside);
        } else if resp.is_pointer_button_down_on() {
            painter.rect_filled(rect, radius, p.subtle);
        } else if resp.hovered() {
            painter.rect_filled(rect, radius, p.hover);
        }
        let fg = if visible {
            p.accent_text
        } else if resp.hovered() {
            p.text
        } else {
            p.text_muted
        };
        let mut x = rect.left() + if show_label { 8.0 } else { 7.0 };
        // Crisp geometric right-sidebar glyph.
        let s = 14.0 / 24.0;
        let box_rect = egui::Rect::from_center_size(egui::pos2(x + 7.0, rect.center().y), egui::vec2(18.0 * s, 15.0 * s));
        let stroke = egui::Stroke::new(1.2, fg);
        painter.rect_stroke(box_rect, 2.0 * s, stroke, egui::StrokeKind::Middle);
        painter.vline(box_rect.left() + 11.5 * s, box_rect.y_range(), stroke);
        x += 14.0;
        if show_label {
            x += 5.0;
            let g = painter.layout_no_wrap(tr("Panels").to_string(), ternarygrid_ui::style::medium(ui.ctx(), 12.0), fg);
            let gw = g.size().x;
            painter.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, fg);
            x += gw;
        }
        if let Some(n) = badge {
            x += 5.0;
            let pill = egui::Rect::from_center_size(egui::pos2(x + 8.0, rect.center().y), egui::vec2(16.0, 15.0));
            painter.rect_filled(pill, 7, ternarygrid_ui::style::soft(p.accent, if dark { 0.35 } else { 0.18 }));
            painter.text(pill.center(), egui::Align2::CENTER_CENTER, n.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 10.0), p.accent_text);
            x += 16.0;
        }
        let caret_x = x + if show_label { 4.0 + 4.5 } else { 3.0 + 4.5 };
        painter.text(
            egui::pos2(caret_x, rect.center().y),
            egui::Align2::CENTER_CENTER,
            icon::CARET_DOWN,
            ternarygrid_ui::style::icon_font(9.0),
            if visible {
                p.accent_text
            } else if resp.hovered() {
                p.text
            } else {
                p.text_faint
            },
        );
    }
    resp
}

fn panels_dropdown_menu(ui: &mut Ui, state: &mut ToolbarState, visible: bool, actions: &mut Vec<GridAction>) {
    kit::menu_look(ui, 195.0);
    kit::menu_caption(ui, tr("Side panels"));
    panel_items(ui, state, actions);
    kit::menu_separator(ui);
    if visible {
        if kit::menu_item(ui, Some(icon::SIDEBAR_SIMPLE), tr("Hide panels"), "").clicked() {
            state.panels_hidden = true;
            ui.close();
        }
    } else if kit::menu_item(ui, Some(icon::SIDEBAR_SIMPLE), tr("Show panels"), "").clicked() {
        if state.open_panels.is_empty() {
            state.set_panel_open(PanelKind::Value, true);
        }
        state.panels_hidden = false;
        ui.close();
    }
}

/// The keys of Save, shown in its button.
const SAVE_KEYS: &str = if cfg!(target_os = "macos") { "⌘S" } else { "Ctrl+S" };

/// A button with its keys after the text (Save ⌘S, Copy ⌘↩): primary (accent) or quiet,
/// `height` high, 10 (12 when 28 or more) at the sides, an optional icon before the text.
pub(crate) fn key_button(ui: &mut Ui, icon: Option<&str>, text: &str, keys: Option<&str>, primary: bool, height: f32) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let enabled = ui.is_enabled();
    let size = if height >= 28.0 { 13.0 } else { 12.5 };
    let font = if primary { ternarygrid_ui::style::medium(ui.ctx(), size) } else { egui::FontId::proportional(size) };
    let g = ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER);
    let k = keys.map(|k| ui.painter().layout_no_wrap(k.to_string(), egui::FontId::proportional(11.0), egui::Color32::PLACEHOLDER));
    let pad = if height >= 28.0 { 12.0 } else { 10.0 };
    let icon_w = if icon.is_some() { 14.0 + 6.0 } else { 0.0 };
    let width = 2.0 * pad + icon_w + g.size().x + k.as_ref().map_or(0.0, |k| 8.0 + k.size().x);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    let painter = ui.painter();
    let hovered = enabled && resp.hovered();
    let (fill, color) = if !enabled {
        (if primary { p.subtle } else { egui::Color32::TRANSPARENT }, p.text_faint)
    } else if primary {
        (if hovered { p.accent_hover } else { p.accent }, p.on_accent)
    } else {
        (if hovered { p.hover } else { egui::Color32::TRANSPARENT }, p.text)
    };
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

/// The auto refresh choices (the Refresh split-button dropdown).
fn auto_refresh_menu(ui: &mut Ui, state: &mut ToolbarState, actions: &mut Vec<GridAction>) {
    kit::menu_look(ui, 200.0);
    kit::menu_caption(ui, tr("Auto refresh"));
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

/// The panels as check items (the Panels dropdown).
fn panel_items(ui: &mut Ui, state: &ToolbarState, actions: &mut Vec<GridAction>) {
    for (kind, _, name, _) in PANELS {
        let mut open = state.is_panel_open(kind) && !state.panels_hidden;
        if kit::menu_check(ui, &mut open, tr(name)).clicked() {
            actions.push(GridAction::TogglePanel(kind));
            ui.close();
        }
    }
}

/// The footer's `⋯` overflow menu: strictly contains ONLY actions that did not fit on the bar.
fn more_menu(ui: &mut Ui, model: &mut ResultModel, grid: &mut DataGridState, state: &mut ToolbarState, footer: &MoreContext, actions: &mut Vec<GridAction>) {
    kit::menu_look(ui, 250.0);
    let narrow = footer.fit < FooterFit::Core;
    let compact = footer.fit < FooterFit::Full;
    let mut has_section = false;
    let sep = |ui: &mut Ui, has_section: &mut bool| {
        if *has_section {
            kit::menu_separator(ui);
        }
        *has_section = true;
    };
    if footer.fit == FooterFit::Minimal {
        sep(ui, &mut has_section);
        kit::menu_caption(ui, tr("View"));
        for (kind, _, name, _) in &VIEWS {
            let mut on = state.view == *kind;
            if kit::menu_check(ui, &mut on, tr(name)).clicked() {
                state.view = *kind;
                actions.push(GridAction::SwitchView(*kind));
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
    }
    if narrow {
        sep(ui, &mut has_section);
        if kit::menu_item(ui, Some(icon::EXPORT), tr("Export…"), "").clicked() {
            actions.push(GridAction::Export { rows: None, columns: grid.visible_columns() });
            ui.close();
        }
    }
    let fetching = model.fetch.fetching;
    if compact || footer.fetch.is_some() {
        sep(ui, &mut has_section);
        if let Some(label) = &footer.fetch
            && ui.add_enabled_ui(!fetching, |ui| kit::menu_item(ui, Some(icon::ARROW_LINE_DOWN), label, "")).inner.clicked()
        {
            actions.push(GridAction::FetchMore);
            ui.close();
        }
        if compact && model.fetch.has_more && ui.add_enabled_ui(!fetching, |ui| kit::menu_item(ui, Some(icon::DOWNLOAD_SIMPLE), tr("Fetch all rows"), "")).inner.clicked() {
            actions.push(GridAction::FetchAll);
            ui.close();
        }
        if compact && ui.add_enabled_ui(!model.fetch.counting, |ui| kit::menu_item(ui, Some(icon::HASH), tr("Calculate total row count"), "")).inner.clicked() {
            actions.push(GridAction::CountTotal);
            ui.close();
        }
    }
    if narrow && model.is_editable() {
        sep(ui, &mut has_section);
        let notify = |r: Result<usize, String>, actions: &mut Vec<GridAction>| {
            if let Err(e) = r {
                actions.push(GridAction::Notify { message: e, error: true });
            }
        };
        let selected = !grid.selection().is_empty();
        ui.add_enabled_ui(footer.can_edit, |ui| {
            if ui.add_enabled_ui(selected, |ui| kit::menu_item(ui, Some(icon::COPY), tr("Duplicate selected rows"), keys("⌘D", "Ctrl+D"))).inner.clicked() {
                notify(grid::duplicate_selected(model, grid), actions);
                ui.close();
            }
            if ui.add_enabled_ui(model.can_undo(), |ui| kit::menu_item(ui, Some(icon::ARROW_U_UP_LEFT), tr("Undo"), keys("⌘Z", "Ctrl+Z"))).inner.clicked() {
                model.undo();
                ui.close();
            }
            if ui.add_enabled_ui(selected, |ui| kit::menu_item_danger(ui, Some(icon::TRASH), tr("Delete selected rows"), keys("fn⌫", "Delete"))).inner.clicked() {
                delete_rows(model, grid, actions);
                ui.close();
            }
        });
    }
}

/// The side panels: kind, icon, full accessible name, and concise label.
pub(crate) const PANELS: [(PanelKind, &str, &str, &str); 5] = [
    (PanelKind::Value, icon::EYE, "Value", "Value"),
    (PanelKind::Metadata, icon::INFO, "Metadata", "Meta"),
    (PanelKind::Grouping, icon::STACK, "Grouping", "Group"),
    (PanelKind::Calc, icon::CALCULATOR, "Calc", "Calc"),
    (PanelKind::References, icon::TREE_STRUCTURE, "References", "Refs"),
];

/// Semantic colour for each side panel icon (used on the panel tab strip).
pub(crate) fn panel_color(ui: &Ui, kind: PanelKind) -> egui::Color32 {
    let ic = ternarygrid_ui::style::icons(ui);
    match kind {
        PanelKind::Value => ic.blue,
        PanelKind::Metadata => ic.cyan,
        PanelKind::Grouping => ic.purple,
        PanelKind::Calc => ic.amber,
        PanelKind::References => ic.green,
    }
}

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
    fn the_footer_shows_the_widest_that_fits() {
        let widths = FooterWidths { essential: 300.0, core: 500.0, full: 800.0 };
        assert_eq!(FooterFit::choose(900.0, &widths), FooterFit::Full);
        assert_eq!(FooterFit::choose(800.0, &widths), FooterFit::Full);
        assert_eq!(FooterFit::choose(799.0, &widths), FooterFit::Core);
        assert_eq!(FooterFit::choose(499.0, &widths), FooterFit::Essential);
        assert_eq!(FooterFit::choose(299.0, &widths), FooterFit::Minimal);
    }

    #[test]
    fn order_by_parsing() {
        let cols: Vec<String> = ["id", "Name", "order date"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_order_by("name desc, id", &cols).unwrap(), vec![SortKey::desc(1), SortKey::asc(0)]);
        assert_eq!(parse_order_by("[order date] ASC", &cols).unwrap(), vec![SortKey::asc(2)]);
        assert_eq!(parse_order_by("\"order date\" DESC", &cols).unwrap(), vec![SortKey::desc(2)]);
        assert_eq!(parse_order_by("`order date`   DESC   NULLS   LAST, id ASC NULLS FIRST", &cols).unwrap(), vec![SortKey::desc(2), SortKey::asc(0)]);
        assert_eq!(parse_order_by("name NULLS LAST", &cols).unwrap(), vec![SortKey::asc(1)]);
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
