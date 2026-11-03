//! Complete result tab: filter bar, view (grid / text / record / JSON / XML),
//! toolbar with status, and side panels. Use this for the common case, or
//! compose the individual widgets yourself.

use crate::grid::{DataGrid, DataGridState, DataMode, GridAction, GridOptions, GridResponse, PanelKind, ResultViewKind};
use crate::model::ResultModel;
use crate::panels::{
    CalcPanel, CalcPanelState, GroupingAction, GroupingPanel, GroupingPanelState, MetadataPanel, ReferencesPanel, ReferencesPanelState, ValuePanel, ValuePanelState,
};
use crate::toolbar::{FilterBar, ResultToolbar, ToolbarState};
use crate::views::{DocumentKind, DocumentView, DocumentViewState, RecordView, RecordViewState, TextView, TextViewState};
use egui::{Frame, Id, Panel, Ui};
use serde::{Deserialize, Serialize};
use ternarygrid_core::tr;

/// All state of a result tab. The grid layout and toolbar settings are
/// serialisable; panel caches are transient.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ResultViewState {
    /// Grid layout, selection, sort.
    pub grid: DataGridState,
    /// Filter bar / toolbar settings.
    pub toolbar: ToolbarState,
    /// Panel tab shown in the side area.
    pub active_panel: Option<PanelKind>,
    /// Width of the side panel area.
    pub side_width: Option<f32>,
    /// Record view state.
    #[serde(skip)]
    pub record: RecordViewState,
    /// Text view cache.
    #[serde(skip)]
    pub text: TextViewState,
    /// JSON / XML view cache.
    #[serde(skip)]
    pub documents: DocumentViewState,
    /// Value panel state (editor text, image preview cache).
    #[serde(skip)]
    pub value: ValuePanelState,
    /// Calc panel cache.
    #[serde(skip)]
    pub calc: CalcPanelState,
    /// Grouping panel state.
    #[serde(skip)]
    pub grouping: GroupingPanelState,
    /// References panel state; feed it with
    /// [`ReferencesPanelState::set_relations`] and
    /// [`ReferencesPanelState::set_related_rows`].
    #[serde(skip)]
    pub references: ReferencesPanelState,
    #[serde(skip)]
    last_client_filter: Option<String>,
}

impl ResultViewState {
    /// State for a model.
    pub fn new(model: &ResultModel) -> Self {
        ResultViewState { grid: DataGridState::for_model(model), ..Default::default() }
    }

    /// Open a side panel and make it the active tab.
    pub fn open_panel(&mut self, p: PanelKind) {
        self.toolbar.set_panel_open(p, true);
        self.toolbar.panels_hidden = false;
        self.active_panel = Some(p);
    }

    /// The side panel area is on screen.
    pub fn panels_visible(&self) -> bool {
        !self.toolbar.open_panels.is_empty() && !self.toolbar.panels_hidden
    }
}

/// Response of [`ResultViewer::show`].
#[derive(Debug)]
pub struct ResultViewResponse {
    /// Actions for the application (see [`GridAction`]).
    pub actions: Vec<GridAction>,
    /// The grid response when the grid view was shown.
    pub grid: Option<GridResponse>,
}

/// A complete result tab.
pub struct ResultViewer<'a> {
    model: &'a mut ResultModel,
    state: &'a mut ResultViewState,
    options: &'a GridOptions,
    id_salt: Id,
}

/// An error in a pop-up, in full (selectable, scrolls when long), with what
/// can be done about it: define a virtual key for a table without a key.
fn error_popup(ui: &Ui, id: Id, message: &str, model: &ResultModel, state: &mut ResultViewState, actions: &mut Vec<GridAction>) {
    use egui::{RichText, ScrollArea};
    use egui_phosphor::regular as icon;
    use ternarygrid_ui::{kit, parts::Dialog};
    let no_key = model.read_only_reason().is_some_and(|r| r.virtual_key_helps() && r.message() == message);
    let mut close = false;
    let title = if no_key { tr("This table is read-only") } else { tr("Error") };
    let dialog = Dialog::new(title).icon(if no_key { icon::LOCK_SIMPLE } else { icon::WARNING_CIRCLE }).width(520.0);
    let (resp, ()) = dialog.show_modal(
        ui.ctx(),
        id.with("error_popup"),
        |ui| {
            let pal = ternarygrid_ui::pal(ui);
            if no_key {
                ui.add(egui::Label::new(tr("TernaryGrid saves a change with UPDATE … WHERE <key> = …, so it needs a primary key, a unique key or a unique index to find exactly that row. This table has none in the result.")).wrap());
                ui.add(egui::Label::new(RichText::new(tr("Define a virtual key (columns whose values identify a row) to edit it here, or add a primary key to the table.")).color(pal.text_muted)).wrap());
            } else {
                ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.add(egui::Label::new(message).wrap().selectable(true));
                });
            }
        },
        |ui| {
            if kit::button_ex(ui, kit::Kind::Ghost, Some(icon::COPY), tr("Copy"), false).clicked() {
                ui.ctx().copy_text(message.to_string());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if no_key {
                    if kit::primary_icon(ui, icon::KEY, tr("Define virtual key…")).clicked() {
                        actions.push(GridAction::DefineVirtualKey { table: crate::model::source_table(model.columns()).ok() });
                        close = true;
                    }
                    if kit::button_ex(ui, kit::Kind::Ghost, None, tr("Close"), false).clicked() {
                        close = true;
                    }
                } else if kit::primary(ui, tr("OK")).clicked() {
                    close = true;
                }
            });
        },
    );
    if close || resp.close {
        state.toolbar.error_popup = None;
    }
}

/// The error of a failed fetch above the result: the message (selectable, scrolls
/// when long), Copy and ×. Returns true when it was closed.
fn fetch_error_bar(ui: &mut Ui, message: &str) -> bool {
    use egui::{Label, Margin, RichText, ScrollArea};
    use egui_phosphor::regular as icon;
    use ternarygrid_ui::parts::Tone;
    let pal = ternarygrid_ui::pal(ui);
    let mut close = false;
    Frame::new().fill(Tone::Error.fill(ui)).inner_margin(Margin::symmetric(16, 9)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label(RichText::new(icon::X_CIRCLE).font(ternarygrid_ui::style::icon_font(16.0)).color(pal.err));
            let text_width = (ui.available_width() - 70.0).max(80.0);
            ui.allocate_ui(egui::vec2(text_width, 0.0), |ui| {
                ScrollArea::vertical().max_height(72.0).id_salt("fetch_error").show(ui, |ui| {
                    ui.add(Label::new(RichText::new(message).color(pal.text)).wrap().selectable(true));
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ternarygrid_ui::kit::icon_button_ex(ui, icon::X, tr("Close"), false, 24.0).clicked() {
                    close = true;
                }
                if ternarygrid_ui::kit::icon_button_ex(ui, icon::COPY, tr("Copy"), false, 24.0).clicked() {
                    ui.ctx().copy_text(message.to_string());
                }
            });
        });
    });
    close
}

fn panel_icon(p: PanelKind) -> &'static str {
    crate::toolbar::PANELS.iter().find(|(k, ..)| *k == p).map_or("", |(_, icon, _)| icon)
}

fn panel_name(p: PanelKind) -> &'static str {
    crate::toolbar::PANELS.iter().find(|(k, ..)| *k == p).map_or("", |(.., name)| tr(name))
}

/// A panel's tab in the panel strip: the shown one with its name, the others their icon (the
/// name in the tooltip). Every panel has its tab, so each is one click away.
fn panel_tab(ui: &mut Ui, p: PanelKind, selected: bool) -> egui::Response {
    use egui::{Align2, Sense, vec2};
    let pal = ternarygrid_ui::pal(ui);
    let name = panel_name(p);
    let galley = selected.then(|| ui.painter().layout_no_wrap(name.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 12.5), egui::Color32::PLACEHOLDER));
    // 8 | icon 14 | 6 name | 10 (named), 6 | icon 14 | 6 (icon only).
    let width = match &galley {
        Some(g) => 8.0 + 14.0 + 6.0 + g.size().x + 10.0,
        None => 26.0,
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 26.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, name));
    let resp = if selected { resp } else { resp.on_hover_text(name) };
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, ternarygrid_ui::style::RADIUS, pal.accent_soft);
    } else if resp.hovered() {
        painter.rect_filled(rect, ternarygrid_ui::style::RADIUS, pal.hover);
    }
    let icon_color = if selected {
        pal.accent_text
    } else if resp.hovered() {
        pal.text
    } else {
        pal.text_muted
    };
    let icon_x = if selected { rect.left() + 8.0 + 7.0 } else { rect.center().x };
    painter.text(egui::pos2(icon_x, rect.center().y), Align2::CENTER_CENTER, panel_icon(p), ternarygrid_ui::style::icon_font(14.0), icon_color);
    if let Some(g) = galley {
        painter.galley(egui::pos2(rect.left() + 8.0 + 14.0 + 6.0, rect.center().y - g.size().y / 2.0), g, pal.accent_text);
    }
    resp
}

impl<'a> ResultViewer<'a> {
    /// New result viewer.
    pub fn new(model: &'a mut ResultModel, state: &'a mut ResultViewState, options: &'a GridOptions) -> Self {
        ResultViewer { model, state, options, id_salt: Id::new("ternarygrid_result_viewer") }
    }

    /// Distinguish several viewers in one `Ui`.
    pub fn id_salt(mut self, salt: impl egui::AsId) -> Self {
        self.id_salt = Id::new(salt);
        self
    }

    /// Show everything in the available space.
    pub fn show(self, ui: &mut Ui) -> ResultViewResponse {
        // A row above that did not fit (cut at the clip rect) widened `ui`: lay out
        // in the visible part only, or the bars and side panels below would run past
        // the tab (over the panel next to it).
        let rect = ui.available_rect_before_wrap().intersect(ui.clip_rect());
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
        let response = self.show_inside(&mut inner);
        ui.advance_cursor_after_rect(rect);
        response
    }

    fn show_inside(self, ui: &mut Ui) -> ResultViewResponse {
        let ResultViewer { model, state, options, id_salt } = self;
        // Rows fetched since the last frame (the grid syncs itself; the other views do not).
        if !matches!(state.toolbar.view, ResultViewKind::Grid) {
            model.try_sync();
        }
        let id = ui.make_persistent_id(id_salt);
        let mut actions: Vec<GridAction> = Vec::new();
        let mut grid_response = None;
        let pal = ternarygrid_ui::pal(ui);
        // The filter bar: 46 high, 16 / 12 at the sides, a line under it; the footer a line
        // and 42 under it, 16 / 10 at the sides.
        let filter_frame = Frame::new().inner_margin(egui::Margin { left: 16, right: 12, top: 8, bottom: 9 }).fill(pal.canvas);
        let footer_frame = Frame::new().inner_margin(egui::Margin { left: 16, right: 10, top: 7, bottom: 6 }).fill(pal.canvas);
        let line = egui::Stroke::new(1.0, pal.border);
        let bar = Panel::top(id.with("filter_bar")).frame(filter_frame).show_separator_line(false).show(ui, |ui| {
            actions.extend(FilterBar::new(model, &mut state.grid, &mut state.toolbar).dialect(options.dialect.clone()).show(ui));
        });
        ui.painter().hline(bar.response.rect.x_range(), bar.response.rect.bottom() - 0.5, line);
        // "Filter by value" in server mode adds to the WHERE the filter box shows.
        state.grid.set_server_filter(&state.toolbar.filter_text);
        let footer = Panel::bottom(id.with("toolbar")).frame(footer_frame).show_separator_line(false).show(ui, |ui| {
            actions.extend(ResultToolbar::new(model, &mut state.grid, &mut state.toolbar).fetch_size(options.fetch_more_threshold).show(ui));
        });
        ui.painter().hline(footer.response.rect.x_range(), footer.response.rect.top() + 0.5, line);
        // Errors in a pop-up: a refused edit or a failed save each time, a failed fetch once.
        if let Some((message, true)) = state.toolbar.notice.clone() {
            state.toolbar.notice = None;
            state.toolbar.error_popup = Some(message);
        }
        // A failed fetch is shown over the result (a bar), not in a pop-up that stops the work.
        // A new error (or the same one after a retry) shows again.
        if model.fetch.error != state.toolbar.seen_fetch_error {
            state.toolbar.seen_fetch_error = model.fetch.error.clone();
            state.toolbar.dismissed_fetch_error = None;
        }
        if let Some(message) = state.toolbar.error_popup.clone() {
            error_popup(ui, id, &message, model, state, &mut actions);
        }
        if state.panels_visible() {
            if state.active_panel.is_none_or(|p| !state.toolbar.is_panel_open(p)) {
                state.active_panel = state.toolbar.open_panels.last().copied();
            }
            let side = Panel::right(id.with("side"))
                .resizable(true)
                .default_size(state.side_width.unwrap_or(360.0))
                .size_range(220.0..=900.0)
                .show_separator_line(false)
                .frame(Frame::new().fill(ui.visuals().panel_fill))
                .show(ui, |ui| {
                    state.side_width = Some(ui.available_width());
                    // A tab for each panel (the shown one named), and Hide for the whole area.
                    if state.active_panel.is_none_or(|p| !state.toolbar.is_panel_open(p)) {
                        state.active_panel = state.toolbar.open_panels.last().copied();
                    }
                    let spacing = ui.spacing().item_spacing.y;
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let strip = Frame::new().inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let tabs_width = (ui.available_width() - 26.0).max(40.0);
                            ui.allocate_ui_with_layout(egui::vec2(tabs_width, 26.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                egui::ScrollArea::horizontal().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 2.0;
                                        // Every panel, in a fixed order (Value, Metadata, Grouping, Calc, References).
                                        for (p, ..) in crate::toolbar::PANELS {
                                            if panel_tab(ui, p, state.active_panel == Some(p)).clicked() {
                                                state.toolbar.set_panel_open(p, true);
                                                state.active_panel = Some(p);
                                            }
                                        }
                                    });
                                });
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ternarygrid_ui::kit::icon_button_ex(ui, egui_phosphor::regular::X, tr("Hide panels"), false, 24.0).clicked() {
                                    state.toolbar.panels_hidden = true;
                                }
                            });
                        });
                    });
                    ui.painter().hline(strip.response.rect.x_range(), strip.response.rect.bottom() + 0.5, egui::Stroke::new(1.0, pal.border));
                    ui.add_space(1.0);
                    ui.spacing_mut().item_spacing.y = spacing;
                    if state.active_panel.is_none_or(|p| !state.toolbar.is_panel_open(p)) {
                        state.active_panel = state.toolbar.open_panels.last().copied();
                    }
                    let content = Frame::new().inner_margin(egui::Margin::same(14));
                    content.show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        match state.active_panel {
                            Some(PanelKind::Value) => {
                                actions.extend(ValuePanel::new(model, &state.grid, options, &mut state.value).show(ui));
                            }
                            Some(PanelKind::Metadata) => MetadataPanel::new(model, &mut state.grid).show(ui),
                            Some(PanelKind::Calc) => CalcPanel::new(model, &state.grid, &mut state.calc).show(ui),
                            Some(PanelKind::Grouping) => {
                                if let Some(GroupingAction::Filtered(text)) = GroupingPanel::new(model, &mut state.grouping).show(ui) {
                                    state.grid.mode = DataMode::Client;
                                    state.toolbar.filter_text = text;
                                }
                            }
                            Some(PanelKind::References) => {
                                actions.extend(ReferencesPanel::new(model, &state.grid, &mut state.references).show(ui));
                            }
                            None => {}
                        }
                    });
                });
            // The line left of the panels.
            ui.painter().vline(side.response.rect.left() + 0.5, side.response.rect.y_range(), egui::Stroke::new(1.0, pal.border));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if let Some(e) = model.fetch.error.clone().filter(|e| state.toolbar.dismissed_fetch_error.as_ref() != Some(e))
                && fetch_error_bar(ui, &e)
            {
                state.toolbar.dismissed_fetch_error = Some(e);
            }
            // A sort / filter computed in the background is installed by the grid,
            // and here for the other views (they show and copy the same rows).
            if state.toolbar.view != ResultViewKind::Grid
                && let Some(Err(e)) = model.poll_view_job()
            {
                state.grid.show_message(e, true);
            }
            match state.toolbar.view {
                ResultViewKind::Grid => {
                    let r = DataGrid::new(model, &mut state.grid, options).id_salt(id.with("grid")).notices_elsewhere().show(ui);
                    actions.extend(r.actions.iter().cloned());
                    grid_response = Some(r);
                }
                ResultViewKind::Text => TextView::new(model, &state.grid, options, &mut state.text).show(ui),
                ResultViewKind::Record => actions.extend(RecordView::new(model, &mut state.grid, options, &mut state.record).show(ui)),
                ResultViewKind::Json => DocumentView::new(model, &state.grid, options, &mut state.documents, DocumentKind::Json).show(ui),
                ResultViewKind::Xml => DocumentView::new(model, &state.grid, options, &mut state.documents, DocumentKind::Xml).show(ui),
            }
        });
        // Filter box requests of the column menu.
        let some = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        let mut defaults = Vec::new();
        for a in &actions {
            match a {
                GridAction::EditFilter(t) => {
                    let cur = state.toolbar.filter_text.trim().to_string();
                    if !t.is_empty() {
                        state.toolbar.filter_text = if cur.is_empty() { t.clone() } else { format!("{cur} AND {t}") };
                    }
                    state.toolbar.focus_filter();
                }
                GridAction::SaveDefaultFilter => {
                    defaults.push(GridAction::DefaultFilter { where_clause: some(&state.toolbar.filter_text), order_by: some(&state.toolbar.order_text) });
                }
                GridAction::ClearFilterText => {
                    state.toolbar.filter_text.clear();
                    state.toolbar.order_text.clear();
                }
                _ => {}
            }
        }
        actions.extend(defaults);
        // Internal handling of some actions (they are still reported).
        for a in &actions {
            match a {
                // Messages appear once, in the status bar.
                GridAction::Notify { message, error } => state.toolbar.notice = Some((message.clone(), *error)),
                GridAction::OpenValuePanel { row, col } => {
                    state.grid.select_cell(*row, *col);
                    state.open_panel(PanelKind::Value);
                }
                GridAction::ServerQueryChanged { where_clause, order_by } => {
                    // The whole new WHERE (a filter by value includes the one before): the
                    // filter box shows what the application runs.
                    if let Some(w) = where_clause {
                        state.toolbar.filter_text = w.clone();
                    }
                    if let Some(o) = order_by {
                        state.toolbar.order_text = o.clone();
                    }
                }
                // From the toolbar, or Tab in the grid / record view.
                GridAction::SwitchView(kind) => {
                    state.toolbar.view = *kind;
                    match kind {
                        ResultViewKind::Grid => state.grid.request_focus(),
                        ResultViewKind::Record => state.record.request_focus(),
                        _ => {}
                    }
                }
                // From the footer's panel menu: show a panel, or close the one shown.
                GridAction::TogglePanel(p) => {
                    if state.panels_visible() && state.active_panel == Some(*p) {
                        state.toolbar.set_panel_open(*p, false);
                    } else {
                        state.open_panel(*p);
                    }
                    ui.ctx().request_repaint();
                }
                _ => {}
            }
        }
        // Show client filters applied from menus in the filter bar.
        let current = model.client_view_spec().filter.map(|f| f.text().to_string());
        if current != state.last_client_filter {
            if let Some(t) = &current
                && state.grid.mode == DataMode::Client
            {
                state.toolbar.filter_text = t.clone();
            }
            state.last_client_filter = current;
        }
        ResultViewResponse { actions, grid: grid_response }
    }
}
