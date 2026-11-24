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

/// The footer of a result, the same under a table's data and the SQL editor: on the left the
/// status ("400 rows · more available · 286 ms", the time the rows were read in its tooltip)
/// and Fetch next; on the right the pending changes with Discard and Save (only while there
/// are some), then every result action grouped by function:
/// Data/Fetch (Refresh, Auto refresh, Fetch all, Total count) |
/// Row editing (Add, Add before, Duplicate, Copy above, Copy below, Delete, Undo) |
/// Views & Visuals (Grid, Text, Record, JSON, XML, Chart, Spatial) |
/// Export |
/// Panels (Value, Metadata, Grouping, Calc, References, Hide/show).
/// What does not fit in a narrow pane moves cleanly into ⋯ ([`FooterFit`]).
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
        let can_edit = can_edit && model.is_editable();
        let mut actions = Vec::new();
        auto_refresh_tick(ui, model, grid.is_editing() || editing, state, &mut actions);
        let p = ternarygrid_ui::pal(ui);
        let ic = ternarygrid_ui::style::icons(ui);
        ui.horizontal(|ui| {
            ui.set_min_height(FOOTER_H);
            ui.spacing_mut().item_spacing.x = 8.0;
            // --- left: stacked two-line status (row count · elapsed on top; Fetch next / active
            // auto-refresh / stop / error link below) so all five action groups stay comfortable
            // on a single pro-toolbar row.
            let fetching = model.fetch.fetching;
            let fetch_label = if fetch_size > 0 { trf("Fetch next {}", &[&fetch_size]) } else { tr("Fetch more").to_string() };
            let fetch_link_w = ui.painter().layout_no_wrap(fetch_label.clone(), ternarygrid_ui::style::medium(ui.ctx(), 11.0), egui::Color32::PLACEHOLDER).size().x + 6.0;
            // What the right side needs at minimum: the read-only note, the pending changes
            // (shorter when the status would not fit beside them), and the fewest buttons.
            let auto_short = state.auto_refresh_secs.map(auto_label).unwrap_or_else(|| tr("Auto").to_string());
            let min_widths = footer_widths(ui, model.is_editable(), false, &auto_short);
            let locked_w = if model.is_editable() { 0.0 } else { model.read_only_reason().map_or(0.0, |r| read_only_width(ui, r.short_label()) + FOOTER_GAP) };
            let full = ui.available_width();
            let needed = min_widths.minimal + locked_w + FOOTER_SIDE_GAP;
            let pending = model.pending_counts().total();
            let pending_label = if pending == 1 { tr("1 change").to_string() } else { trf("{} changes", &[&pending]) };
            // First check status width to decide whether pending pill should be compact.
            let preview_status = status_job(ui, model, state, &p, false);
            let preview_status_w = ui.painter().layout_job(preview_status).size().x;
            let pill_short = full - needed - preview_status_w < pending_width(ui, &pending_label, false) + FOOTER_GAP;
            let pending_label = if pill_short { pending.to_string() } else { pending_label };
            let pending_w = if pending > 0 { pending_width(ui, &pending_label, pill_short) + FOOTER_GAP } else { 0.0 };
            let free = (full - needed - pending_w).max(60.0);
            let fetch_shown = model.fetch.has_more && free >= fetch_link_w;
            let fetch_overflow = model.fetch.has_more && !fetch_shown;
            let widths = footer_widths(ui, model.is_editable(), fetch_overflow, &auto_short);
            let job = status_job(ui, model, state, &p, fetch_shown);
            let read_at = model.fetch.fetched_at.map(|at| fetched_at_text(at, chrono::Local::now()));
            let dismissed_err = model.fetch.error.as_ref().filter(|e| state.dismissed_fetch_error.as_ref() == Some(*e)).cloned();
            let show_stop = model.fetch.fetching_for().is_some_and(|r| r >= crate::grid::CANCEL_AFTER);
            if let Some(running) = model.fetch.fetching_for()
                && running < crate::grid::CANCEL_AFTER
            {
                ui.ctx().request_repaint_after(crate::grid::CANCEL_AFTER - running);
            }
            // Only show the compact left auto-refresh chip when the right-side Auto button
            // overflowed into More (avoids duplicating the control at full width).
            let line1_w = ui.painter().layout_job(job.clone()).size().x;
            let base_line2_w = (if fetch_shown { fetch_link_w } else { 0.0 }) + if show_stop { 26.0 } else { 0.0 } + if dismissed_err.is_some() { 116.0 } else { 0.0 };
            let base_left_w = line1_w.max(base_line2_w).min(free);
            let pre_fit = FooterFit::choose(full - base_left_w - pending_w - locked_w - FOOTER_SIDE_GAP, &widths);
            let show_left_auto = state.auto_refresh_secs.is_some() && pre_fit < FooterFit::All;
            let has_line2 = fetch_shown || show_left_auto || show_stop || dismissed_err.is_some();
            // Compute the tight width of the left status stack so `room` on the right is accurate.
            let mut line2_w = 0.0_f32;
            if fetch_shown {
                line2_w += fetch_link_w;
            }
            if show_left_auto && let Some(s) = state.auto_refresh_secs {
                let chip_w = ui.painter().layout_no_wrap(auto_label(s), ternarygrid_ui::style::medium(ui.ctx(), 10.5), egui::Color32::PLACEHOLDER).size().x + 24.0;
                line2_w += if line2_w > 0.0 { 6.0 + chip_w } else { chip_w };
            }
            if show_stop {
                line2_w += if line2_w > 0.0 { 6.0 + 20.0 } else { 20.0 };
            }
            if dismissed_err.is_some() {
                line2_w += if line2_w > 0.0 { 6.0 + 110.0 } else { 110.0 };
            }
            let status_col_w = line1_w.max(line2_w).min(free);
            ui.allocate_ui_with_layout(egui::vec2(status_col_w, FOOTER_H), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                if !has_line2 {
                    ui.add_space(((FOOTER_H - 16.0) / 2.0).max(0.0));
                } else {
                    ui.add_space(2.0);
                }
                let status = ui.add(egui::Label::new(job).selectable(false).truncate());
                if let Some(at) = read_at {
                    status.on_hover_text(trf("Read at {}", &[&at]));
                }
                if has_line2 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if fetch_shown {
                            let r = ui.add_enabled_ui(!fetching, |ui| compact_link(ui, &fetch_label)).inner.on_hover_text(tr("Fetch next page"));
                            if r.clicked() {
                                actions.push(GridAction::FetchMore);
                            }
                        }
                        if show_left_auto && let Some(s) = state.auto_refresh_secs {
                            let r = compact_auto_chip(ui, &auto_label(s)).on_hover_text(tr("Auto refresh"));
                            Popup::menu(&r).id(ui.id().with("auto_refresh_chip")).show(|ui| auto_refresh_menu(ui, state, &mut actions));
                        }
                        if show_stop {
                            let stop = Button::new(RichText::new(icon::STOP_CIRCLE).size(12.5).color(ic.red)).frame_when_inactive(false);
                            if ui.add(stop).on_hover_text(tr("Stop the query")).clicked() {
                                actions.push(GridAction::CancelFetch);
                            }
                        }
                        if let Some(e) = &dismissed_err {
                            let label = egui::Label::new(RichText::new(format!("{} {}", icon::WARNING_CIRCLE, tr("The fetch failed"))).size(11.0).color(ic.red))
                                .sense(egui::Sense::click())
                                .truncate();
                            if ui.add(label).on_hover_text(e.as_str()).clicked() {
                                state.dismissed_fetch_error = None;
                            }
                        }
                    });
                }
            });
            // --- right, right to left: grouped by function — Panels, Export, Views & Visuals,
            // Row edits, Refresh & Fetch, and Pending changes. Every button displays its semantic
            // coloured icon on top with a concise label underneath (Opsi 1).
            let room = ui.max_rect().right() - ui.min_rect().right() - pending_w - locked_w - FOOTER_SIDE_GAP;
            let fit = FooterFit::choose(room, &widths);
            let has_more_menu = fit < FooterFit::All || fetch_overflow;
            let footer = MoreContext { fit, can_edit, fetch: fetch_overflow.then_some(fetch_label) };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = FOOTER_GAP;
                let divide = |ui: &mut Ui| {
                    if fit >= FooterFit::Core {
                        footer_divider(ui);
                    }
                };
                // Group 5: Side panels (Value, Meta, Group, Calc, Refs) + Panel toggle (and More if overflow).
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = FOOTER_INNER_GAP;
                    let visible = !state.open_panels.is_empty() && !state.panels_hidden;
                    if panel_toggle(ui, visible).clicked() {
                        if visible {
                            state.panels_hidden = true;
                        } else {
                            if state.open_panels.is_empty() {
                                state.set_panel_open(PanelKind::Value, true);
                            }
                            state.panels_hidden = false;
                        }
                    }
                    if has_more_menu {
                        let w = kit::labeled_icon_width(ui, tr("More"));
                        let more =
                            kit::labeled_icon_button(ui, icon::DOTS_THREE, tr("More"), tr("More result actions"), tr("More result actions"), false, w, FOOTER_H, None, false);
                        Popup::menu(&more).id(ui.id().with("result_more")).align(egui::RectAlign::TOP_END).show(|ui| more_menu(ui, model, grid, state, &footer, &mut actions));
                    }
                    if fit >= FooterFit::All {
                        for (kind, glyph, name, short) in PANELS.iter().rev() {
                            let on = state.is_panel_open(*kind) && !state.panels_hidden;
                            let tip = trf("{} panel", &[&tr(name)]);
                            let w = kit::labeled_icon_width(ui, tr(short));
                            if kit::labeled_icon_button(ui, glyph, tr(short), tr(name), &tip, on, w, FOOTER_H, Some(panel_color(ui, *kind)), false).clicked() {
                                actions.push(GridAction::TogglePanel(*kind));
                            }
                        }
                    }
                });
                divide(ui);
                // Group 4: Export.
                if fit >= FooterFit::Core {
                    let w = kit::labeled_icon_width(ui, tr("Export"));
                    if kit::labeled_icon_button(ui, icon::EXPORT, tr("Export"), tr("Export"), tr("Export…"), false, w, FOOTER_H, Some(ic.teal), false).clicked() {
                        actions.push(GridAction::Export { rows: None, columns: grid.visible_columns() });
                    }
                    divide(ui);
                }
                // Group 3: Views (Grid, Text, Rec, JSON, XML) + Visualizations (Chart, Map).
                if fit >= FooterFit::Essential {
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.x = FOOTER_INNER_GAP;
                        if fit >= FooterFit::All {
                            let map_w = kit::labeled_icon_width(ui, tr("Map"));
                            if kit::labeled_icon_button(ui, icon::MAP_TRIFOLD, tr("Map"), tr("Spatial view"), tr("Spatial view"), false, map_w, FOOTER_H, Some(ic.purple), false)
                                .clicked()
                            {
                                actions.push(GridAction::ShowSpatial);
                            }
                            let chart_w = kit::labeled_icon_width(ui, tr("Chart"));
                            if kit::labeled_icon_button(ui, icon::CHART_BAR, tr("Chart"), tr("Chart"), tr("Chart"), false, chart_w, FOOTER_H, Some(ic.green), false).clicked() {
                                actions.push(GridAction::ShowChart);
                            }
                            ui.add_space(2.0);
                        }
                        if let Some(kind) = view_switch(ui, state.view)
                            && state.view != kind
                        {
                            state.view = kind;
                            actions.push(GridAction::SwitchView(kind));
                        }
                    });
                }
                // Group 2: Row editing (or read-only indicator).
                if model.is_editable() {
                    divide(ui);
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.x = FOOTER_INNER_GAP;
                        row_buttons(ui, fit, can_edit, model, grid, &mut actions);
                    });
                } else if let Some(r) = model.read_only_reason() {
                    // 🔒 No unique key, 5 apart, the reason in the tooltip.
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        ui.label(RichText::new(r.short_label()).size(12.0).color(p.warn)).on_hover_text(r.message());
                        ui.label(RichText::new(icon::LOCK_SIMPLE).font(ternarygrid_ui::style::icon_font(13.0)).color(p.warn));
                    });
                }
                divide(ui);
                // Group 1: Data & Fetch (Refresh, Auto, All, Count).
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = FOOTER_INNER_GAP;
                    if fit >= FooterFit::All {
                        let count_w = kit::labeled_icon_width(ui, tr("Count"));
                        let count_resp = ui
                            .add_enabled_ui(!model.fetch.counting, |ui| {
                                kit::labeled_icon_button(
                                    ui,
                                    icon::HASH,
                                    tr("Count"),
                                    tr("Calculate total row count"),
                                    tr("Calculate total row count"),
                                    false,
                                    count_w,
                                    FOOTER_H,
                                    Some(ic.purple),
                                    false,
                                )
                            })
                            .inner;
                        if count_resp.clicked() {
                            actions.push(GridAction::CountTotal);
                        }
                        let fetch_all_tip = if model.fetch.has_more { tr("Fetch all rows") } else { tr("All rows fetched") };
                        let all_w = kit::labeled_icon_width(ui, tr("All"));
                        let fetch_all_resp = ui
                            .add_enabled_ui(model.fetch.has_more && !fetching, |ui| {
                                kit::labeled_icon_button(ui, icon::DOWNLOAD_SIMPLE, tr("All"), tr("Fetch all rows"), fetch_all_tip, false, all_w, FOOTER_H, Some(ic.teal), false)
                            })
                            .inner;
                        if fetch_all_resp.clicked() {
                            actions.push(GridAction::FetchAll);
                        }
                        let auto_tip = match state.auto_refresh_secs {
                            Some(s) => format!("{} ({})", tr("Auto refresh"), auto_label(s)),
                            None => tr("Auto refresh").to_string(),
                        };
                        let auto_w = kit::labeled_icon_width(ui, &auto_short);
                        let auto_resp = kit::labeled_icon_button(
                            ui,
                            icon::TIMER,
                            &auto_short,
                            tr("Auto refresh"),
                            &auto_tip,
                            state.auto_refresh_secs.is_some(),
                            auto_w,
                            FOOTER_H,
                            Some(ic.amber),
                            false,
                        );
                        Popup::menu(&auto_resp).id(ui.id().with("auto_refresh_btn")).align(egui::RectAlign::TOP_START).show(|ui| auto_refresh_menu(ui, state, &mut actions));
                    }
                    let refresh_tip = format!("{} ({})", tr("Refresh"), keys("⌘R", "Ctrl+R"));
                    let ref_w = kit::labeled_icon_width(ui, tr("Refresh"));
                    if kit::labeled_icon_button(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), tr("Refresh"), &refresh_tip, false, ref_w, FOOTER_H, Some(ic.blue), false).clicked() {
                        actions.push(GridAction::Refresh);
                    }
                });
                // --- pending changes: only while there are some (Cmd+S saves, as always)
                if pending > 0 {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(pending_w - FOOTER_GAP, 32.0), egui::Sense::hover());
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
                    let saving = model.is_saving();
                    match child.add_enabled_ui(!saving, |ui| pending_pill(ui, &pending_label, pending, pill_short)).inner {
                        Some(true) => actions.push(grid::save_action(model)),
                        Some(false) => {
                            // Discard drops them at once: they are only pending.
                            model.discard_changes();
                            actions.push(GridAction::ChangesDiscarded);
                        }
                        None => {}
                    }
                }
                // Errors open a pop-up (the result viewer); other messages show here.
                if let Some((message, _)) = state.notice.clone().filter(|(_, error)| !error) {
                    if kit::icon_button_ex(ui, icon::X, tr("Dismiss"), false, 20.0).clicked() {
                        state.notice = None;
                    }
                    ui.add(egui::Label::new(RichText::new(format!("{} {message}", icon::INFO)).size(12.0).color(p.text)).truncate()).on_hover_text(message);
                }
            });
        });
        actions
    }
}

/// Build the first-line status job (`400 rows · 171 ms` — omitting the redundant "more available"
/// when the clickable "Fetch next …" link is rendered directly underneath on line 2).
fn status_job(ui: &Ui, model: &ResultModel, state: &ToolbarState, p: &ternarygrid_ui::Palette, fetch_below: bool) -> egui::text::LayoutJob {
    let rows = crate::format::group_thousands(&model.row_count().to_string());
    let rows = trf("{} rows", &[&rows]);
    let mut rest = String::new();
    if model.has_client_view() && model.row_count() != model.stored_row_count() {
        rest.push_str(&trf(" (of {} fetched)", &[&crate::format::group_thousands(&model.stored_row_count().to_string())]));
    }
    if model.fetch.fetching {
        rest.push_str(" · ");
        rest.push_str(tr("fetching…"));
    } else if model.fetch.has_more && !fetch_below {
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
        rest.push_str(tr("auto refresh paused"));
    }
    let mut job = egui::text::LayoutJob::default();
    let size = 12.0;
    job.append(&rows, 0.0, egui::TextFormat::simple(ternarygrid_ui::style::medium(ui.ctx(), size), p.text));
    job.append(&rest, 0.0, egui::TextFormat::simple(egui::FontId::proportional(size), p.text_muted));
    job
}

/// Compact clickable link shown on line 2 under the status (`Fetch next 200`).
fn compact_link(ui: &mut Ui, text: &str) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let font = ternarygrid_ui::style::medium(ui.ctx(), 11.0);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x + 4.0, 16.0), egui::Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let color = if enabled { p.accent_text } else { p.accent_text.gamma_multiply(0.45) };
        if hovered {
            ui.painter().rect_filled(rect.expand2(egui::vec2(2.0, 0.0)), 4, p.accent_soft);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        ui.painter().galley(egui::pos2(rect.left() + 1.0, rect.center().y - galley.size().y / 2.0), galley, color);
    }
    resp
}

/// Compact auto-refresh interval pill on line 2 (`⏱ 10 s`).
fn compact_auto_chip(ui: &mut Ui, text: &str) -> egui::Response {
    let ic = ternarygrid_ui::style::icons(ui);
    let dark = ui.visuals().dark_mode;
    let font = ternarygrid_ui::style::medium(ui.ctx(), 10.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x + 22.0, 16.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    if ui.is_rect_visible(rect) {
        let fill = ternarygrid_ui::style::soft(
            ic.amber,
            if resp.hovered() {
                0.24
            } else if dark {
                0.16
            } else {
                0.12
            },
        );
        ui.painter().rect_filled(rect, 4, fill);
        ui.painter().text(egui::pos2(rect.left() + 9.0, rect.center().y), egui::Align2::CENTER_CENTER, icon::TIMER, ternarygrid_ui::style::icon_font(11.0), ic.amber);
        ui.painter().galley(egui::pos2(rect.left() + 17.0, rect.center().y - galley.size().y / 2.0), galley, ic.amber);
    }
    resp
}

/// How much of the footer's buttons fit beside the status and the pending changes, fewest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FooterFit {
    /// Refresh, Add row, More and Panel toggle; views and the rest in More.
    Minimal,
    /// Refresh, Add row, all 5 views, More and Panel toggle; the rest in More.
    Essential,
    /// Core groups (Refresh, core row edits, 5 views, Export, More and Panel toggle).
    Core,
    /// Every functional group surfaced directly on the single-row footer bar.
    All,
}

/// What the footer's right side needs at each [`FooterFit`].
struct FooterWidths {
    minimal: f32,
    essential: f32,
    core: f32,
    all: f32,
}

impl FooterFit {
    /// The widest that fits in `room`.
    fn choose(room: f32, widths: &FooterWidths) -> Self {
        if room >= widths.all {
            FooterFit::All
        } else if room >= widths.core {
            FooterFit::Core
        } else if room >= widths.essential {
            FooterFit::Essential
        } else {
            FooterFit::Minimal
        }
    }
}

/// What More adds to its own actions, depending on what the bar shows.
struct MoreContext {
    fit: FooterFit,
    can_edit: bool,
    /// "Fetch next 200", when it is not on the bar.
    fetch: Option<String>,
}

/// Gap between the footer's functional groups.
const FOOTER_GAP: f32 = 3.0;
/// Gap between the status and what follows it.
const FOOTER_SIDE_GAP: f32 = 10.0;
/// Gap between buttons within the same functional group.
const FOOTER_INNER_GAP: f32 = 1.0;
/// Width of [`footer_divider`].
const DIVIDER_W: f32 = 5.0;
/// Width of each view segment inside [`view_switch`].
const VIEW_SEG_W: f32 = 29.0;
/// Width of the 5-view segmented pill: five segments 1 apart, 2 padding around them.
const VIEW_SWITCH_W: f32 = 5.0 * VIEW_SEG_W + 4.0 * 1.0 + 2.0 * 2.0;

/// Tall subtle vertical hairline separating functional groups on the Opsi 1 footer.
fn footer_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(DIVIDER_W, 24.0), egui::Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), egui::Stroke::new(1.0, ternarygrid_ui::pal(ui).border_strong));
}

/// Total width of a list of labeled buttons inside a single group.
fn labeled_group_width(ui: &Ui, labels: &[&str]) -> f32 {
    if labels.is_empty() {
        return 0.0;
    }
    let sum: f32 = labels.iter().map(|l| kit::labeled_icon_width(ui, tr(l))).sum();
    sum + (labels.len() - 1) as f32 * FOOTER_INNER_GAP
}

/// The read-only note's width: 🔒 and the short reason, 5 apart.
fn read_only_width(ui: &Ui, text: &str) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(12.0), egui::Color32::PLACEHOLDER).size().x + 5.0 + 13.0
}

/// The width of a row of `items`, each followed by the gap.
fn row_width(items: &[f32]) -> f32 {
    items.iter().map(|w| w + FOOTER_GAP).sum()
}

/// What the right side of the footer needs at each [`FooterFit`].
fn footer_widths(ui: &Ui, editable: bool, fetch_overflow: bool, auto_short: &str) -> FooterWidths {
    let d = DIVIDER_W;
    let w_refresh = kit::labeled_icon_width(ui, tr("Refresh"));
    let w_add = kit::labeled_icon_width(ui, tr("Add"));
    let w_export = kit::labeled_icon_width(ui, tr("Export"));
    let right_collapsed = labeled_group_width(ui, &["More", "Panel"]);
    let minimal = if editable { row_width(&[right_collapsed, w_add, w_refresh]) } else { row_width(&[right_collapsed, w_refresh]) };
    let essential = minimal + row_width(&[VIEW_SWITCH_W]);
    let rows_core = labeled_group_width(ui, &["Add", "Dup", "Del", "Undo"]);
    let core_common = row_width(&[right_collapsed, d, w_export, d, VIEW_SWITCH_W, d, w_refresh]);
    let core = if editable { core_common + row_width(&[rows_core, d]) } else { core_common };
    let mut panel_labels: Vec<&str> = PANELS.iter().map(|(_, _, _, short)| *short).collect();
    if fetch_overflow {
        panel_labels.push("More");
    }
    panel_labels.push("Panel");
    let panels_all = labeled_group_width(ui, &panel_labels);
    let visuals_w = labeled_group_width(ui, &["Chart", "Map"]);
    let views_all = VIEW_SWITCH_W + 2.0 + FOOTER_INNER_GAP + visuals_w;
    let data_all = w_refresh + kit::labeled_icon_width(ui, auto_short) + kit::labeled_icon_width(ui, tr("All")) + kit::labeled_icon_width(ui, tr("Count")) + 3.0 * FOOTER_INNER_GAP;
    let rows_all = labeled_group_width(ui, &["Add", "Before", "Dup", "Above", "Below", "Del", "Undo"]);
    let all_common = row_width(&[panels_all, d, w_export, d, views_all, d, data_all]);
    let all = if editable { all_common + row_width(&[rows_all, d]) } else { all_common };
    FooterWidths { minimal, essential, core, all }
}

/// A labeled icon button on the footer with enabled/disabled state and semantic icon colour.
#[allow(clippy::too_many_arguments)]
fn footer_btn(ui: &mut Ui, glyph: &str, short: &str, name: &str, tip: &str, color: egui::Color32, danger_hover: bool, enabled: bool) -> egui::Response {
    let w = kit::labeled_icon_width(ui, short);
    ui.add_enabled_ui(enabled, |ui| kit::labeled_icon_button(ui, glyph, short, name, tip, false, w, FOOTER_H, Some(color), danger_hover)).inner
}

/// The rows' buttons, right to left (rendered Left → Right as:
/// Add, Before, Dup, Above, Below, Del, Undo).
fn row_buttons(ui: &mut Ui, fit: FooterFit, can_edit: bool, model: &mut ResultModel, grid: &mut DataGridState, actions: &mut Vec<GridAction>) {
    let ic = ternarygrid_ui::style::icons(ui);
    let notify_error = |r: Result<usize, String>, actions: &mut Vec<GridAction>| {
        if let Err(message) = r {
            actions.push(GridAction::Notify { message, error: true });
        }
    };
    let selected = !grid.selection().is_empty();
    if fit >= FooterFit::Core {
        let undo_tip = format!("{} ({})", tr("Undo"), keys("⌘Z", "Ctrl+Z"));
        if footer_btn(ui, icon::ARROW_U_UP_LEFT, tr("Undo"), tr("Undo"), &undo_tip, ic.amber, false, can_edit && model.can_undo()).clicked() {
            model.undo();
        }
        let delete_tip = format!("{} ({})", tr("Delete selected rows"), keys("fn⌫", "Delete"));
        if footer_btn(ui, icon::TRASH, tr("Del"), tr("Delete"), &delete_tip, ic.red, true, can_edit && selected).clicked() {
            delete_rows(model, grid, actions);
        }
    }
    if fit >= FooterFit::All {
        let below_tip = format!("{} ({})", tr("Copy from row below"), keys("⌘⌥↓", "Ctrl+Alt+Down"));
        if footer_btn(ui, icon::ARROW_FAT_LINE_UP, tr("Below"), tr("Copy from row below"), &below_tip, ic.amber, false, can_edit && selected).clicked() {
            notify_error(grid::copy_from_adjacent(model, grid, false), actions);
        }
        let above_tip = format!("{} ({})", tr("Copy from row above"), keys("⌘⌥↑", "Ctrl+Alt+Up"));
        if footer_btn(ui, icon::ARROW_FAT_LINE_DOWN, tr("Above"), tr("Copy from row above"), &above_tip, ic.amber, false, can_edit && selected).clicked() {
            notify_error(grid::copy_from_adjacent(model, grid, true), actions);
        }
    }
    if fit >= FooterFit::Core {
        let duplicate_tip = format!("{} ({})", tr("Duplicate selected rows"), keys("⌘D", "Ctrl+D"));
        if footer_btn(ui, icon::COPY, tr("Dup"), tr("Duplicate"), &duplicate_tip, ic.cyan, false, can_edit && selected).clicked() {
            notify_error(grid::duplicate_selected(model, grid), actions);
        }
    }
    if fit >= FooterFit::All {
        let before_tip = if cfg!(target_os = "macos") { tr("Add row before").to_string() } else { format!("{} (Shift+Insert)", tr("Add row before")) };
        if footer_btn(ui, icon::ROWS_PLUS_TOP, tr("Before"), tr("Add row before"), &before_tip, ic.teal, false, can_edit).clicked() {
            notify_error(grid::add_row_before(model, grid), actions);
        }
    }
    let add_tip = if cfg!(target_os = "macos") { tr("Add a row after the selected one") } else { tr("Add a row after the selected one (Insert)") };
    if footer_btn(ui, icon::PLUS, tr("Add"), tr("Add row"), add_tip, ic.green, false, can_edit).clicked() {
        notify_error(grid::add_row(model, grid).map(|_| 0), actions);
    }
}

/// Mark the selected rows deleted and say how many (as the keys and the context menu do).
fn delete_rows(model: &mut ResultModel, grid: &mut DataGridState, actions: &mut Vec<GridAction>) {
    match grid::delete_selected(model, grid) {
        Ok(n) => actions.push(GridAction::Notify { message: ternarygrid_core::trn(n, tr("1 row marked for deletion"), tr("{} rows marked for deletion")), error: false }),
        Err(message) => actions.push(GridAction::Notify { message, error: true }),
    }
}

/// Height of the single-row Opsi 1 footer (icon + micro-label).
const FOOTER_H: f32 = 38.0;

/// The panels' toggle: a crisp right-sidebar glyph over the "Panel" micro-label.
fn panel_toggle(ui: &mut Ui, on: bool) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let dark = ui.visuals().dark_mode;
    let tip = if on { tr("Hide panels") } else { tr("Show panels") };
    let short = tr("Panel");
    let width = kit::labeled_icon_width(ui, short);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, FOOTER_H), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, tip));
    let resp = resp.on_hover_text(tr("Value, metadata and statistics panels"));
    let painter = ui.painter();
    let radius = egui::CornerRadius::same(ternarygrid_ui::style::RADIUS);
    if on {
        painter.rect_filled(rect, radius, p.accent_soft);
        painter.rect_stroke(rect, radius, egui::Stroke::new(1.0, ternarygrid_ui::style::soft(p.accent_text, if dark { 0.36 } else { 0.25 })), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        painter.rect_filled(rect, radius, p.hover);
    }
    let icon_color = if on {
        p.accent_text
    } else if resp.hovered() {
        p.text
    } else {
        p.text_muted
    };
    let label_color = if on {
        p.accent_text
    } else if resp.hovered() {
        p.text
    } else {
        p.text_muted
    };
    let icon_center = egui::pos2(rect.center().x, rect.top() + (FOOTER_H * 0.34).round());
    let s = 14.5 / 24.0;
    let box_rect = egui::Rect::from_center_size(icon_center, egui::vec2(18.0 * s, 15.0 * s));
    let stroke = egui::Stroke::new(1.2, icon_color);
    painter.rect_stroke(box_rect, 2.0 * s, stroke, egui::StrokeKind::Middle);
    painter.vline(box_rect.left() + 11.5 * s, box_rect.y_range(), stroke);
    painter.text(egui::pos2(rect.center().x, rect.bottom() - 8.0), egui::Align2::CENTER_CENTER, short, ternarygrid_ui::style::medium(ui.ctx(), 9.5), label_color);
    resp
}

/// The views of the rows: kind, icon, full accessible name, concise label.
const VIEWS: [(ResultViewKind, &str, &str, &str); 5] = [
    (ResultViewKind::Grid, icon::TABLE, "Grid", "Grid"),
    (ResultViewKind::Text, icon::TEXT_AA, "Text", "Text"),
    (ResultViewKind::Record, icon::LIST_DASHES, "Record", "Rec"),
    (ResultViewKind::Json, icon::BRACKETS_CURLY, "JSON", "JSON"),
    (ResultViewKind::Xml, icon::FILE_CODE, "XML", "XML"),
];

fn view_color(ui: &Ui, kind: ResultViewKind) -> egui::Color32 {
    let ic = ternarygrid_ui::style::icons(ui);
    match kind {
        ResultViewKind::Grid => ic.blue,
        ResultViewKind::Text => ic.slate,
        ResultViewKind::Record => ic.teal,
        ResultViewKind::Json => ic.amber,
        ResultViewKind::Xml => ic.orange,
    }
}

/// The view switcher, one pill: all five views (Grid, Text, Rec, JSON, XML) with coloured icons
/// on top and concise micro-labels below, the active view raised. Returns the view clicked.
fn view_switch(ui: &mut Ui, current: ResultViewKind) -> Option<ResultViewKind> {
    let p = ternarygrid_ui::pal(ui);
    let views = &VIEWS;
    let (bw, bh, pad, gap) = (VIEW_SEG_W, FOOTER_H - 4.0, 2.0, 1.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(VIEW_SWITCH_W, FOOTER_H), egui::Sense::hover());
    ui.painter().rect_filled(rect, 8, p.subtle);
    let mut clicked = None;
    for (i, (kind, glyph, name, short)) in views.iter().enumerate() {
        let r = egui::Rect::from_min_size(egui::pos2(rect.left() + pad + i as f32 * (bw + gap), rect.top() + pad), egui::vec2(bw, bh));
        let resp = ui.interact(r, ui.id().with(("view", i)), egui::Sense::click());
        let tip = trf("{} view", &[&tr(name)]);
        let on = current == *kind;
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, tr(name)));
        let resp = resp.on_hover_text(tip);
        if on {
            ternarygrid_ui::kit::raised(ui.painter(), r, egui::CornerRadius::same(6), p.thumb);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 6, p.hover);
        }
        let vc = view_color(ui, *kind);
        let icon_color = if on || resp.hovered() { vc } else { vc.gamma_multiply(0.82) };
        let label_color = if on || resp.hovered() { p.text } else { p.text_muted };
        ui.painter().text(egui::pos2(r.center().x, r.top() + 11.5), egui::Align2::CENTER_CENTER, *glyph, ternarygrid_ui::style::icon_font(14.0), icon_color);
        ui.painter().text(egui::pos2(r.center().x, r.bottom() - 6.5), egui::Align2::CENTER_CENTER, tr(short), ternarygrid_ui::style::medium(ui.ctx(), 9.0), label_color);
        if resp.clicked() {
            clicked = Some(*kind);
        }
    }
    clicked
}

/// Width of the pending changes pill: 12 | ● 8 n changes 8 Discard 8 Save ⌘S | 4 (`short`:
/// without the keys).
fn pending_width(ui: &Ui, count: &str, short: bool) -> f32 {
    let w = |t: &str, f: egui::FontId| ui.painter().layout_no_wrap(t.to_string(), f, egui::Color32::PLACEHOLDER).size().x;
    let medium = ternarygrid_ui::style::medium(ui.ctx(), 12.5);
    let label = w(count, medium.clone());
    let discard = w(tr("Discard"), egui::FontId::proportional(12.5)) + 20.0;
    let keys = if short { 0.0 } else { 8.0 + w(SAVE_KEYS, egui::FontId::proportional(11.0)) };
    let save = w(tr("Save"), medium) + keys + 20.0;
    (12.0 + 7.0 + 8.0 + label + 8.0 + discard + 8.0 + save + 4.0).ceil()
}

/// The keys of Save, shown in its button.
const SAVE_KEYS: &str = if cfg!(target_os = "macos") { "⌘S" } else { "Ctrl+S" };

/// "● 2 changes  Discard  Save ⌘S" on the changed cells' tint. `Some(true)`: Save, `Some(false)`: Discard.
fn pending_pill(ui: &mut Ui, count: &str, pending: usize, short: bool) -> Option<bool> {
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
    let save_tip = format!("{} ({SAVE_KEYS})", ternarygrid_core::trn(pending, tr("Save 1 pending change"), tr("Save {} pending changes")));
    if key_button(ui, None, tr("Save"), (!short).then_some(SAVE_KEYS), true, 26.0).on_hover_text(save_tip).clicked() {
        out = Some(true);
    }
    out
}

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
    for (kind, _, name, _) in PANELS {
        let mut open = state.is_panel_open(kind) && !state.panels_hidden;
        if kit::menu_check(ui, &mut open, tr(name)).clicked() {
            actions.push(GridAction::TogglePanel(kind));
            ui.close();
        }
    }
}

/// The footer's ⋯ overflow menu: contains only actions that did not fit on the bar.
fn more_menu(ui: &mut Ui, model: &mut ResultModel, grid: &mut DataGridState, state: &mut ToolbarState, footer: &MoreContext, actions: &mut Vec<GridAction>) {
    kit::menu_look(ui, 260.0);
    let narrow = footer.fit < FooterFit::Core;
    let compact = footer.fit < FooterFit::All;
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
        if compact {
            kit::submenu(ui, Some(icon::TIMER), tr("Auto refresh"), 200.0, &mut |ui| auto_refresh_menu(ui, state, actions));
        }
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
    if compact && model.is_editable() {
        sep(ui, &mut has_section);
        let notify = |r: Result<usize, String>, actions: &mut Vec<GridAction>| {
            if let Err(e) = r {
                actions.push(GridAction::Notify { message: e, error: true });
            }
        };
        let selected = !grid.selection().is_empty();
        ui.add_enabled_ui(footer.can_edit, |ui| {
            // Macs have no Insert key: no keys to show.
            if kit::menu_item(ui, Some(icon::ROWS_PLUS_TOP), tr("Add row before"), keys("", "Shift+Insert")).clicked() {
                notify(grid::add_row_before(model, grid), actions);
                ui.close();
            }
            if narrow && ui.add_enabled_ui(selected, |ui| kit::menu_item(ui, Some(icon::COPY), tr("Duplicate selected rows"), keys("⌘D", "Ctrl+D"))).inner.clicked() {
                notify(grid::duplicate_selected(model, grid), actions);
                ui.close();
            }
            if ui.add_enabled_ui(selected, |ui| kit::menu_item(ui, Some(icon::ARROW_FAT_LINE_DOWN), tr("Copy from row above"), keys("⌘⌥↑", "Ctrl+Alt+Up"))).inner.clicked() {
                notify(grid::copy_from_adjacent(model, grid, true), actions);
                ui.close();
            }
            if ui.add_enabled_ui(selected, |ui| kit::menu_item(ui, Some(icon::ARROW_FAT_LINE_UP), tr("Copy from row below"), keys("⌘⌥↓", "Ctrl+Alt+Down"))).inner.clicked() {
                notify(grid::copy_from_adjacent(model, grid, false), actions);
                ui.close();
            }
            if narrow && model.can_undo() && kit::menu_item(ui, Some(icon::ARROW_U_UP_LEFT), tr("Undo"), keys("⌘Z", "Ctrl+Z")).clicked() {
                model.undo();
                ui.close();
            }
            if narrow && ui.add_enabled_ui(selected, |ui| kit::menu_item_danger(ui, Some(icon::TRASH), tr("Delete selected rows"), keys("fn⌫", "Delete"))).inner.clicked() {
                delete_rows(model, grid, actions);
                ui.close();
            }
        });
    }
    if compact {
        sep(ui, &mut has_section);
        if kit::menu_item(ui, Some(icon::CHART_BAR), tr("Chart"), "").clicked() {
            actions.push(GridAction::ShowChart);
            ui.close();
        }
        if kit::menu_item(ui, Some(icon::MAP_TRIFOLD), tr("Spatial view"), "").clicked() {
            actions.push(GridAction::ShowSpatial);
            ui.close();
        }
        sep(ui, &mut has_section);
        kit::submenu(ui, Some(icon::SIDEBAR_SIMPLE), tr("Panels"), 180.0, &mut |ui| {
            kit::menu_look(ui, 180.0);
            panel_items(ui, state, actions);
        });
    }
}

/// The side panels: kind, icon, full accessible name, and concise footer label.
pub(crate) const PANELS: [(PanelKind, &str, &str, &str); 5] = [
    (PanelKind::Value, icon::EYE, "Value", "Value"),
    (PanelKind::Metadata, icon::INFO, "Metadata", "Meta"),
    (PanelKind::Grouping, icon::STACK, "Grouping", "Group"),
    (PanelKind::Calc, icon::CALCULATOR, "Calc", "Calc"),
    (PanelKind::References, icon::TREE_STRUCTURE, "References", "Refs"),
];

/// Semantic colour for each side panel icon (used on both the footer and the panel tab strip).
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
        let widths = FooterWidths { minimal: 150.0, essential: 300.0, core: 500.0, all: 800.0 };
        assert_eq!(FooterFit::choose(900.0, &widths), FooterFit::All);
        assert_eq!(FooterFit::choose(800.0, &widths), FooterFit::All);
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
