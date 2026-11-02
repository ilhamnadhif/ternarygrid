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
    // When the rows were read: are they still current?
    if let Some(at) = model.fetch.fetched_at {
        s.push_str(" · ");
        s.push_str(&fetched_at_text(at, chrono::Local::now()));
    }
    (rows, s)
}

/// `09:35:51` for today, `2026-10-21 09:35` for an earlier day.
fn fetched_at_text(at: chrono::DateTime<chrono::Local>, now: chrono::DateTime<chrono::Local>) -> String {
    if at.date_naive() == now.date_naive() { at.format("%H:%M:%S").to_string() } else { at.format("%Y-%m-%d %H:%M").to_string() }
}

/// The filter bar: the WHERE (or client filter) field and the ORDER BY field, each with
/// its keyword inside, column suggestions and history; Apply; clear; and a ⋯ menu with
/// where filters run (the database or the fetched rows) and the table's default filter.
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
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // The two fields share what the buttons leave: Apply, clear and ⋯.
            let gap = ui.spacing().item_spacing.x;
            let apply_w = kit::button_width(ui, Some(icon::PLAY), tr("Apply"), false);
            let flexible = (ui.available_width() - apply_w - 2.0 * 28.0 - 4.0 * gap).max(200.0);
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
            const WHERE_WORDS: [&str; 10] = ["AND", "OR", "NOT", "IS NULL", "IS NOT NULL", "LIKE", "ILIKE", "IN", "BETWEEN", "NULL"];
            let label = if server { tr("Filter rows in the database") } else { tr("Filter the fetched rows") };
            let field = Field { salt: "filter_where", prefix, label, prefix_tip, hint, width: flexible * 0.62, keywords: &WHERE_WORDS, history: &state.filter_history };
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
            let field = Field {
                salt: "filter_order",
                prefix: "ORDER BY",
                label: tr("Order rows by"),
                prefix_tip: if server { tr("ORDER BY sent to the database") } else { tr("Sorts the fetched rows") },
                hint: tr("e.g. created_at DESC, name"),
                width: flexible * 0.38,
                // The client sort knows no NULLS FIRST / LAST (NULLs sort first).
                keywords: if server { &["ASC", "DESC", "NULLS FIRST", "NULLS LAST"] } else { &["ASC", "DESC"] },
                history: &state.order_history,
            };
            let order_resp = prefixed_field(ui, field, &mut state.order_text, &columns, &quote, &mut state.rt.order_suggest, &mut apply);
            if order_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                apply = true;
            }
            if kit::button_ex(ui, kit::Kind::Primary, Some(icon::PLAY), tr("Apply"), false).on_hover_text(tr("Apply filter and order (Enter)")).clicked() {
                apply = true;
            }
            let filtered = !state.filter_text.trim().is_empty() || !state.order_text.trim().is_empty() || !grid.sort.is_empty() || model.has_client_view();
            ui.add_enabled_ui(filtered, |ui| {
                if kit::icon_button(ui, icon::X, tr("Clear filter and order")).clicked() {
                    clear = true;
                }
            });
            let menu = kit::icon_button(ui, icon::DOTS_THREE, tr("Filter options"));
            Popup::menu(&menu).id(ui.id().with("filter_options")).show(|ui| {
                ui.set_min_width(280.0);
                ui.label(kit::section_title(ui, tr("Filter and sort")));
                // Each choice says what it does: the words alone mean little to a new user.
                let server_hint = tr("The database filters and sorts. The query runs again with your WHERE / ORDER BY, over all rows of the table.");
                let client_hint = tr("TernaryGrid filters and sorts the rows already fetched, without asking the database. Fast, but only those rows.");
                let on_server = ui
                    .add_enabled(!redis, egui::RadioButton::new(server, tr("Server: in the database, over all rows")))
                    .on_hover_text(server_hint)
                    .on_disabled_hover_text(tr("Redis has no server side filter: its read commands take no condition."));
                if on_server.clicked() {
                    grid.mode = DataMode::Server;
                }
                if ui.radio(!server, tr("Client: over the fetched rows")).on_hover_text(client_hint).clicked() {
                    grid.mode = DataMode::Client;
                }
                if crate::model::source_table(model.columns()).is_ok() {
                    ui.separator();
                    let some = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
                    if ui.button((ternarygrid_icons::ic(icon::PUSH_PIN), tr("Save as default filter"))).on_hover_text(tr("Applied whenever this table's data is opened")).clicked()
                    {
                        actions.push(GridAction::DefaultFilter { where_clause: some(&state.filter_text), order_by: some(&state.order_text) });
                        ui.close();
                    }
                    if ui.button((ternarygrid_icons::ic(icon::PUSH_PIN_SLASH), tr("Reset default filter"))).clicked() {
                        actions.push(GridAction::DefaultFilter { where_clause: None, order_by: None });
                        ui.close();
                    }
                }
            });
        });
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
                    grid.sort.clear();
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
    let font = egui::FontId::monospace(11.0);
    let prefix_w = ui.painter().layout_no_wrap(f.prefix.to_string(), font.clone(), p.accent_text).size().x;
    let history = !f.history.is_empty();
    let margin = egui::Margin { left: (prefix_w + 18.0).min(120.0) as i8, right: if history { 28 } else { 8 }, top: 6, bottom: 6 };
    let id = ui.id().with(f.salt);
    let hint = RichText::new(f.hint).color(p.text_faint);
    let resp = crate::suggest::suggest_edit(
        ui,
        id,
        text,
        |t| t.hint_text(hint).desired_width(f.width).font(egui::TextStyle::Monospace).margin(margin),
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
            ui.set_min_width(240.0);
            for item in f.history {
                if ui.button(RichText::new(item).monospace()).clicked() {
                    *text = item.clone();
                    *apply = true;
                }
            }
        });
    }
    resp
}

/// The footer of a result: refresh and auto refresh, the status with the fetch buttons
/// next to the row count they change, pending changes (Save / Discard, only while there
/// are some), row operations, the view switcher, Chart / Spatial, Export and the panels.
pub struct ResultToolbar<'a> {
    model: &'a mut ResultModel,
    grid: &'a mut DataGridState,
    state: &'a mut ToolbarState,
}

impl<'a> ResultToolbar<'a> {
    /// New toolbar.
    pub fn new(model: &'a mut ResultModel, grid: &'a mut DataGridState, state: &'a mut ToolbarState) -> Self {
        ResultToolbar { model, grid, state }
    }

    /// Show it. Returns actions for the application.
    pub fn show(self, ui: &mut Ui) -> Vec<GridAction> {
        let ResultToolbar { model, grid, state } = self;
        let mut actions = Vec::new();
        auto_refresh_tick(ui, model, grid.is_editing(), state, &mut actions);
        let p = ternarygrid_ui::pal(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // --- refresh, auto refresh
            if kit::icon_button_ex(ui, icon::ARROWS_CLOCKWISE, tr("Refresh (Cmd+R)"), false, 26.0).clicked() {
                actions.push(GridAction::Refresh);
            }
            let resp = match state.auto_refresh_secs {
                None => kit::icon_button_ex(ui, icon::TIMER, tr("Auto refresh"), false, 26.0),
                Some(s) => kit::button_full(ui, kit::Kind::Ghost, Some(icon::TIMER), &auto_label(s), true, true).on_hover_text(tr("Auto refresh")),
            };
            Popup::menu(&resp).show(|ui| {
                for choice in AUTO_REFRESH_CHOICES {
                    let text = match choice {
                        None => tr("Off").to_string(),
                        Some(s) => auto_label(s),
                    };
                    if ui.selectable_label(state.auto_refresh_secs == choice, text).clicked() {
                        set_auto_refresh(ui, state, choice, &mut actions);
                        ui.close();
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(tr("Custom (s)"));
                    if state.custom_refresh_secs == 0 {
                        state.custom_refresh_secs = 15;
                    }
                    ui.add(egui::DragValue::new(&mut state.custom_refresh_secs).range(1..=86_400));
                    if ui.button(tr("Set")).clicked() {
                        let s = state.custom_refresh_secs.max(1);
                        set_auto_refresh(ui, state, Some(s), &mut actions);
                        ui.close();
                    }
                });
            });
            kit::vdivider(ui, 16.0);
            // --- status: the row count stands out; the fetch buttons follow it
            let (rows, mut rest) = status_parts(model);
            if state.auto_refresh_secs.is_some() && (model.has_changes() || model.fetch.fetching) {
                rest.push_str(" · ");
                rest.push_str(tr("auto refresh paused"));
            }
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add(egui::Label::new(RichText::new(rows).font(ternarygrid_ui::style::medium(ui.ctx(), ternarygrid_ui::style::body_size(ui.ctx()))).color(p.text)));
                ui.add(egui::Label::new(RichText::new(rest).color(p.text_muted)));
            });
            let fetching = model.fetch.fetching;
            if model.fetch.has_more {
                ui.add_enabled_ui(!fetching, |ui| {
                    if kit::button_full(ui, kit::Kind::Ghost, Some(icon::CARET_DOUBLE_DOWN), tr("Fetch more"), true, false).on_hover_text(tr("Fetch next page")).clicked() {
                        actions.push(GridAction::FetchMore);
                    }
                    if kit::icon_button_ex(ui, icon::ARROW_LINE_DOWN, tr("Fetch all rows"), false, 26.0).clicked() {
                        actions.push(GridAction::FetchAll);
                    }
                });
            }
            if !model.fetch.counting && kit::icon_button_ex(ui, icon::HASH, tr("Calculate total row count"), false, 26.0).clicked() {
                actions.push(GridAction::CountTotal);
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
            // --- right, right to left: panels, export, views, rows, pending changes, messages
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let visible = !state.open_panels.is_empty() && !state.panels_hidden;
                let panels = kit::icon_button_ex(ui, icon::CARET_DOWN, tr("Choose panels"), false, 20.0);
                Popup::menu(&panels).id(ui.id().with("panels_menu")).show(|ui| {
                    for (kind, ic, name) in PANELS {
                        let open = state.is_panel_open(kind);
                        if ui.add(Button::selectable(open, kit::with_icon(ic, tr(name)))).clicked() {
                            actions.push(GridAction::TogglePanel(kind));
                            ui.close();
                        }
                    }
                });
                if kit::icon_toggle(ui, icon::SIDEBAR_SIMPLE, if visible { tr("Hide panels") } else { tr("Show panels") }, visible).clicked() {
                    if visible {
                        state.panels_hidden = true;
                    } else {
                        if state.open_panels.is_empty() {
                            state.set_panel_open(PanelKind::Value, true);
                        }
                        state.panels_hidden = false;
                    }
                }
                if kit::ghost(ui, Some(icon::EXPORT), tr("Export")).on_hover_text(tr("Export…")).clicked() {
                    actions.push(GridAction::Export { rows: None, columns: grid.visible_columns() });
                }
                kit::vdivider(ui, 16.0);
                if kit::icon_button_ex(ui, icon::MAP_TRIFOLD, tr("Spatial view"), false, 26.0).clicked() {
                    actions.push(GridAction::ShowSpatial);
                }
                if kit::icon_button_ex(ui, icon::CHART_BAR, tr("Chart"), false, 26.0).clicked() {
                    actions.push(GridAction::ShowChart);
                }
                // --- views: one segmented control (laid out right to left: added reversed)
                let views = [
                    (ResultViewKind::Grid, icon::TABLE, tr("Grid")),
                    (ResultViewKind::Text, icon::TEXT_AA, tr("Text")),
                    (ResultViewKind::Record, icon::ROWS, tr("Record")),
                    (ResultViewKind::Json, icon::BRACKETS_CURLY, tr("JSON")),
                    (ResultViewKind::Xml, icon::BRACKETS_ANGLE, tr("XML")),
                ];
                let segments: Vec<kit::Segment> = views.iter().map(|(k, ic, tip)| kit::Segment { icon: ic, tooltip: tip, on: state.view == *k }).collect();
                let clicked = ui.scope(|ui| kit::button_group(ui, &segments).iter().position(|r| r.clicked())).inner;
                if let Some(i) = clicked {
                    let kind = views[i].0;
                    if state.view != kind {
                        state.view = kind;
                        actions.push(GridAction::SwitchView(kind));
                    }
                }
                kit::vdivider(ui, 16.0);
                // --- rows (right to left: delete, duplicate, add)
                let notify = |r: Result<usize, String>, actions: &mut Vec<GridAction>| {
                    if let Err(e) = r {
                        actions.push(GridAction::Notify { message: e, error: true });
                    }
                };
                if model.is_editable() {
                    if kit::icon_button_ex(ui, icon::TRASH, tr("Delete selected rows"), false, 26.0).clicked() {
                        notify(grid::delete_selected(model, grid), &mut actions);
                    }
                    if kit::icon_button_ex(ui, icon::COPY_SIMPLE, tr("Duplicate selected rows"), false, 26.0).clicked() {
                        notify(grid::duplicate_selected(model, grid), &mut actions);
                    }
                    if kit::icon_button_ex(ui, icon::ROWS_PLUS_BOTTOM, tr("Add row"), false, 26.0).clicked() {
                        notify(grid::add_row(model, grid), &mut actions);
                    }
                } else if let Some(r) = model.read_only_reason() {
                    ui.label(RichText::new(format!("{} {}", icon::LOCK_SIMPLE, r.short_label())).color(p.warn)).on_hover_text(r.message());
                }
                // --- pending changes: only while there are some (Cmd+S saves, as always)
                let pending = model.pending_counts().total();
                if pending > 0 {
                    ui.add_space(4.0);
                    let bg = ui.painter().add(egui::Shape::Noop);
                    let save = kit::button_full(ui, kit::Kind::Primary, Some(icon::FLOPPY_DISK), tr("Save"), true, false)
                        .on_hover_text(trf("Save {} pending change(s) (Cmd+S)", &[&pending]));
                    if save.clicked() {
                        actions.push(grid::save_action(model));
                    }
                    // Discard drops them at once: they are only pending.
                    let discard = kit::button_full(ui, kit::Kind::Ghost, None, tr("Discard"), true, false).on_hover_text(tr("Discard pending changes"));
                    if discard.clicked() {
                        model.discard_changes();
                        actions.push(GridAction::ChangesDiscarded);
                    }
                    let count = if pending == 1 { tr("1 change").to_string() } else { trf("{} changes", &[&pending]) };
                    let label = ui.label(RichText::new(count).font(ternarygrid_ui::style::medium(ui.ctx(), ternarygrid_ui::style::body_size(ui.ctx()))).color(p.text));
                    let dot = kit::dot(ui, p.warn, 3.5);
                    let rect = save.rect.union(discard.rect).union(label.rect).union(dot.rect).expand2(egui::vec2(6.0, 2.0));
                    ui.painter().set(bg, egui::Shape::rect_filled(rect, 8, ternarygrid_ui::soft(p.warn, if ui.visuals().dark_mode { 0.16 } else { 0.12 })));
                    ui.add_space(8.0);
                }
                // A failed fetch popped up once; a click shows it again.
                if let Some(e) = &model.fetch.error {
                    let label = egui::Label::new(RichText::new(format!("{} {}", icon::WARNING_CIRCLE, e)).color(p.err)).truncate().sense(egui::Sense::click());
                    if ui.add(label).on_hover_text(tr("Show the error")).clicked() {
                        state.error_popup = Some(e.clone());
                    }
                }
                // Errors open a pop-up (the result viewer); other messages show here.
                if let Some((message, _)) = state.notice.clone().filter(|(_, error)| !error) {
                    if kit::icon_button_ex(ui, icon::X, tr("Dismiss"), false, 20.0).clicked() {
                        state.notice = None;
                    }
                    ui.add(egui::Label::new(RichText::new(format!("{} {message}", icon::INFO)).color(p.text)).truncate()).on_hover_text(message);
                }
            });
        });
        actions
    }
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
        assert!(status_text(&m).ends_with(&format!("42 ms · {}", at.format("%H:%M:%S"))));
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
