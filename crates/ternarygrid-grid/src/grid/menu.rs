//! Context menus of the body and the header.

use super::frame::GridFrame;
use super::state::VirtualColumnDialog;
use super::{GridAction, SqlKind, apply_sort, apply_value_filter, sql_table_name};
use crate::copy::{CopyFormat, CopyOptions};
use crate::format::ColumnFormat;
use crate::model::ReadOnlyReason;
use crate::toolbar::keys;
use crate::view::{FilterOp, SortKey};
use egui::{Popup, PopupCloseBehavior, PopupKind, Response, SetOpenCommand, Ui};
use egui_phosphor::regular as icon;
use ternarygrid_core::dialect::GenericDialect;
use ternarygrid_core::{DataKind, Dialect, Value, tr, trf};
use ternarygrid_ui::kit;

/// A menu item in the kit's look; `keys` its shortcut as the menus write it (see [`keys`]).
fn item(ui: &mut Ui, icon: Option<&str>, text: &str, keys: Option<&str>) -> Response {
    kit::menu_item(ui, icon, text, keys.unwrap_or(""))
}

/// A submenu (230 points wide, as the mockups' "Filter by value").
fn submenu(ui: &mut Ui, icon: Option<&str>, text: &str, add: &mut dyn FnMut(&mut Ui)) {
    kit::submenu(ui, icon, text, SUBMENU_W, add);
}

/// Widths of the menus, as in the mockups: the body's menu 280, the header's 300, the
/// submenus 230.
const CELL_MENU_W: f32 = 280.0;
const HEAD_MENU_W: f32 = 300.0;
const SUBMENU_W: f32 = 230.0;

/// The filters offered for a cell's value (the cell menu's "Filter by value", the column
/// menu's "Cell value"), by the column's type: comparisons for numbers and dates, "contains"
/// for text (`>` on text, a `LIKE` without wildcards are not what one means).
fn value_filter_ops(kind: DataKind, value: &Value) -> &'static [FilterOp] {
    if value.is_null() {
        return &[FilterOp::IsNull, FilterOp::IsNotNull];
    }
    if kind.is_numeric() || kind.is_temporal() || matches!(value, Value::Int(_) | Value::UInt(_) | Value::Float(_) | Value::Decimal(_)) {
        &[FilterOp::Eq, FilterOp::NotEq, FilterOp::Gt, FilterOp::GtEq, FilterOp::Lt, FilterOp::LtEq, FilterOp::IsNull, FilterOp::IsNotNull]
    } else if kind == DataKind::Bool || matches!(value, Value::Bool(_)) {
        &[FilterOp::Eq, FilterOp::NotEq, FilterOp::IsNull, FilterOp::IsNotNull]
    } else {
        &[FilterOp::Eq, FilterOp::NotEq, FilterOp::Contains, FilterOp::IsNull, FilterOp::IsNotNull]
    }
}

/// A value filter's menu label: `status = 'open'`, `status contains 'open'`, `note IS NULL`.
fn value_filter_label(name: &str, op: FilterOp, v: &str) -> String {
    match op {
        FilterOp::IsNull | FilterOp::IsNotNull => format!("{name} {}", op.sql()),
        FilterOp::Contains => trf("{} contains '{}'", &[&name, &v]),
        _ => format!("{name} {} '{v}'", op.sql()),
    }
}

/// "Delete row" / "Delete 3 rows" (`one` and `many` with `{}` for the count).
fn rows_label(n: usize, one: &str, many: &str) -> String {
    if n <= 1 { one.to_string() } else { trf(many, &[&crate::format::display_count(n)]) }
}

fn short_value(v: &Value) -> String {
    if v.is_null() {
        return "NULL".into();
    }
    let text = v.to_text();
    let t = ternarygrid_core::value::single_line(&text);
    if t.chars().count() > 32 { format!("{}…", t.chars().take(32).collect::<String>()) } else { t.into_owned() }
}

impl GridFrame<'_> {
    /// Context menu of the body (and row numbers).
    pub fn context_menu(&mut self, resp: &Response) {
        Popup::context_menu(resp).show(|ui| {
            kit::menu_look(ui, CELL_MENU_W);
            self.menu_items(ui);
        });
    }

    /// Context menu of the header.
    pub fn header_menu(&mut self, resp: &Response) {
        if resp.secondary_clicked()
            && let Some(p) = resp.interact_pointer_pos()
            && let Some(pos) = self.geo.col_at(p.x, self.state.rt.scroll_x, false)
            && !self.state.rt.selection.is_column_selected(pos)
        {
            self.state.rt.selection.select_columns(pos, pos, false);
        }
        Popup::context_menu(resp).show(|ui| {
            kit::menu_look(ui, CELL_MENU_W);
            let Some(col) = self.active().map(|(_, c)| c).or_else(|| self.geo.cols.first().copied()) else {
                ui.label(tr("No columns"));
                return;
            };
            self.column_items(ui, col);
            self.more_menu(ui, col, None);
            kit::menu_separator(ui);
            if item(ui, Some(icon::COPY), tr("Copy column name"), None).clicked() {
                self.ctx.copy_text(self.model.column_name(col).to_string());
                ui.close();
            }
            if item(ui, Some(icon::COPY), tr("Copy column values"), None).clicked() {
                if let Some(pos) = self.pos_of(col) {
                    self.state.rt.selection.select_columns(pos, pos, false);
                    self.copy_selection(&CopyOptions::plain());
                }
                ui.close();
            }
        });
    }

    /// The column menu of the header's ▾ button: ordering, filters by the
    /// current cell's value, filter templates, default filter.
    pub fn header_popup(&mut self, ui: &Ui) {
        let Some((col, anchor, open)) = self.state.rt.header_popup else { return };
        self.state.rt.header_popup = Some((col, anchor, false));
        let set = open.then_some(SetOpenCommand::Bool(true));
        let shown = Popup::new(self.id.with("header_popup"), self.ctx.clone(), anchor, ui.layer_id())
            .kind(PopupKind::Menu)
            .layout(egui::Layout::top_down_justified(egui::Align::Min))
            .style(egui::containers::menu::menu_style)
            .gap(0.0)
            .open_memory(set)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                kit::menu_look(ui, HEAD_MENU_W);
                self.column_popup_items(ui, col);
            });
        if shown.is_none() {
            self.state.rt.header_popup = None;
        }
    }

    fn column_popup_items(&mut self, ui: &mut Ui, col: usize) {
        let name = self.model.column_name(col).to_string();
        let current = self.state.sort.iter().find(|k| k.column == col).copied();
        // --- ordering: the one in effect has a check mark
        for desc in [false, true] {
            let on = current.is_some_and(|k| k.descending == desc);
            let label = if desc { format!("↓  {}", trf("Order by {} DESC", &[&name])) } else { format!("↑  {}", trf("Order by {} ASC", &[&name])) };
            let mut on = on;
            if kit::menu_check(ui, &mut on, &label).clicked() {
                let key = if desc { SortKey::desc(col) } else { SortKey::asc(col) };
                // A column already in the ordering keeps its place.
                let keys = if current.is_some() { self.state.sort.iter().map(|k| if k.column == col { key } else { *k }).collect() } else { vec![key] };
                apply_sort(self.model, self.state, self.options, keys, &self.ctx, &mut self.actions);
                ui.close();
            }
        }
        if ui.add_enabled_ui(current.is_some(), |ui| item(ui, None, &trf("Disable order by {}", &[&name]), None)).inner.clicked() {
            let keys = self.state.sort.iter().copied().filter(|k| k.column != col).collect();
            apply_sort(self.model, self.state, self.options, keys, &self.ctx, &mut self.actions);
            ui.close();
        }
        if !self.state.sort.is_empty() && current.is_none() {
            submenu(ui, Some(icon::LIST_PLUS), tr("Add to the ordering"), &mut |ui| {
                for (desc, label) in [(false, "ASC"), (true, "DESC")] {
                    if item(ui, None, &format!("{name} {label}"), None).clicked() {
                        let mut keys = self.state.sort.clone();
                        keys.push(if desc { SortKey::desc(col) } else { SortKey::asc(col) });
                        apply_sort(self.model, self.state, self.options, keys, &self.ctx, &mut self.actions);
                        ui.close();
                    }
                }
            });
        }
        // --- filters
        let server = self.state.mode == super::DataMode::Server;
        let filterable = !(self.model.is_virtual_column(col) && server);
        let row = self.active().map(|(r, _)| r).or((self.geo.nrows > 0).then_some(0));
        if filterable
            && let Some(row) = row
            && let Some(value) = self.menu_value(row, col)
        {
            // The cell menu's list, without the NULL tests (under Custom); nothing for a NULL.
            let v = short_value(&value);
            let ops: Vec<FilterOp> = value_filter_ops(self.model.column_kind(col), &value).iter().copied().filter(|op| !op.is_unary()).collect();
            if !ops.is_empty() {
                kit::menu_separator(ui);
                kit::menu_caption(ui, tr("Cell value"));
            }
            for op in ops {
                if item(ui, None, &value_filter_label(&name, op, &v), None).clicked() {
                    apply_value_filter(self.model, self.state, self.options, (col, op, &value), &self.ctx, &mut self.actions);
                    ui.close();
                }
            }
        }
        if filterable {
            kit::menu_separator(ui);
            kit::menu_caption(ui, tr("Custom"));
            let column = if server { super::server_column(self.model, self.options, col) } else { name.clone() };
            for op in [FilterOp::Eq, FilterOp::NotEq, FilterOp::Gt, FilterOp::Lt] {
                if item(ui, None, &format!("{name} {} …", op.sql()), None).on_hover_text(tr("Type the value in the filter box")).clicked() {
                    self.actions.push(GridAction::EditFilter(format!("{column} {} ", op.sql())));
                    ui.close();
                }
            }
            for op in [FilterOp::IsNull, FilterOp::IsNotNull] {
                if item(ui, None, &format!("{name} {}", op.sql()), None).clicked() {
                    apply_value_filter(self.model, self.state, self.options, (col, op, &Value::Null), &self.ctx, &mut self.actions);
                    ui.close();
                }
            }
        }
        // --- default filter, reset
        kit::menu_separator(ui);
        if crate::model::source_table(self.model.columns()).is_ok() {
            if item(ui, Some(icon::PUSH_PIN), tr("Save as default filter"), None).on_hover_text(tr("Applied whenever this table's data is opened")).clicked() {
                self.actions.push(GridAction::SaveDefaultFilter);
                ui.close();
            }
            if item(ui, None, tr("Reset default filter"), None).clicked() {
                self.actions.push(GridAction::DefaultFilter { where_clause: None, order_by: None });
                ui.close();
            }
        }
        if item(ui, Some(icon::X), tr("Remove all filters and orderings"), None).clicked() {
            self.state.sort.clear();
            match self.state.mode {
                super::DataMode::Client => self.model.clear_view(),
                super::DataMode::Server => self.actions.push(GridAction::ServerQueryChanged { where_clause: Some(String::new()), order_by: Some(String::new()) }),
            }
            self.actions.push(GridAction::ClearFilterText);
            ui.close();
        }
        if item(ui, Some(icon::FUNNEL), tr("Edit filter…"), None).clicked() {
            self.actions.push(GridAction::EditFilter(String::new()));
            ui.close();
        }
    }

    pub(super) fn menu_items(&mut self, ui: &mut Ui) {
        let active = self.active();
        let editable = self.model.is_editable() && self.options.allow_editing;
        // --- clipboard (a detached menu has its own Copy)
        if !self.detached && item(ui, Some(icon::COPY), tr("Copy"), Some(keys("⌘C", "Ctrl+C"))).clicked() {
            self.copy_selection(&CopyOptions::plain());
            ui.close();
        }
        if item(ui, None, tr("Advanced copy…"), Some(keys("⌘⇧C", "Ctrl+Shift+C"))).clicked() {
            self.state.rt.copy_dialog = true;
            ui.close();
        }
        submenu(ui, None, tr("Copy as"), &mut |ui| {
            for f in CopyFormat::ALL {
                if item(ui, None, f.label(), None).clicked() {
                    let opts = CopyOptions { format: f, include_header: true, ..self.state.copy_options.clone() };
                    self.copy_selection(&opts);
                    ui.close();
                }
            }
        });
        if editable && item(ui, None, tr("Paste"), Some(keys("⌘V", "Ctrl+V"))).clicked() {
            self.actions.push(GridAction::PasteRequested);
            ui.close();
        }
        kit::menu_separator(ui);
        // --- editing
        if let Some((row, col)) = active {
            let cell_ok = editable && self.model.can_edit_cell(row, col).is_ok();
            // A new UUID only where one fits: a uuid column, or text holding UUIDs.
            let kind = self.model.column_kind(col);
            let uuid_ok = kind == DataKind::Uuid
                || kind == DataKind::Text
                    && self.menu_value(row, col).is_some_and(|v| matches!(v, Value::Uuid(_)) || v.as_str().is_some_and(|s| uuid::Uuid::parse_str(s.trim()).is_ok()));
            ui.add_enabled_ui(cell_ok, |ui| {
                if item(ui, Some(icon::PENCIL_SIMPLE), tr("Edit cell"), Some(keys("↩", "Enter"))).clicked() {
                    self.start_edit(row, col, None);
                    ui.close();
                }
                if item(ui, None, tr("Set NULL"), Some(keys("⇧⌫", "Shift+Delete"))).clicked() {
                    self.set_selected_null();
                    ui.close();
                }
                if item(ui, None, tr("Set DEFAULT"), None).clicked() {
                    self.set_selected_default();
                    ui.close();
                }
                if uuid_ok && item(ui, None, tr("Generate UUID"), None).clicked() {
                    self.generate_uuid();
                    ui.close();
                }
            });
        }
        // The rows the items act on: "Delete row" / "Delete 3 rows".
        let selected_rows = self.state.rt.selection.rows(self.geo.nrows, 10_000).len();
        ui.add_enabled_ui(editable, |ui| {
            // Insert (Shift+Insert: above); Macs have no Insert key: the menu or the footer's + only.
            if item(ui, Some(icon::PLUS), tr("Add row"), if cfg!(target_os = "macos") { None } else { Some("Insert") }).clicked() {
                self.add_row_after_active();
                ui.close();
            }
            if item(ui, Some(icon::ROWS_PLUS_BOTTOM), &rows_label(selected_rows, tr("Duplicate row"), tr("Duplicate {} rows")), Some(keys("⌘D", "Ctrl+D"))).clicked() {
                self.duplicate_selected();
                ui.close();
            }
            if kit::menu_item_danger(ui, Some(icon::TRASH), &rows_label(selected_rows, tr("Delete row"), tr("Delete {} rows")), keys("⌫", "Delete")).clicked() {
                self.delete_selected();
                ui.close();
            }
        });
        if self.model.can_undo() && item(ui, Some(icon::ARROW_U_UP_LEFT), tr("Undo"), Some(keys("⌘Z", "Ctrl+Z"))).clicked() {
            self.model.undo();
            ui.close();
        }
        if !editable && let Some(reason) = self.model.read_only_reason().cloned() {
            ui.add_enabled_ui(false, |ui| item(ui, Some(icon::LOCK_SIMPLE), reason.short_label(), None)).inner.on_disabled_hover_text(reason.message());
            if reason == ReadOnlyReason::NoKey && item(ui, Some(icon::KEY), tr("Define virtual key…"), None).clicked() {
                let table = crate::model::source_table(self.model.columns()).ok();
                self.actions.push(GridAction::DefineVirtualKey { table });
                ui.close();
            }
        }
        kit::menu_separator(ui);
        // --- filtering / ordering / columns
        if let Some((row, col)) = active {
            // While the store is busy and the value was not read before: its items are disabled.
            let known = self.menu_value(row, col);
            let value = known.clone().unwrap_or_default();
            let name = self.model.column_name(col).to_string();
            let virtual_server = self.model.is_virtual_column(col) && self.state.mode == super::DataMode::Server;
            let kind = self.model.column_kind(col);
            ui.add_enabled_ui(!virtual_server && known.is_some(), |ui| {
                submenu(ui, Some(icon::FUNNEL), tr("Filter by value"), &mut |ui| {
                    let v = short_value(&value);
                    for &op in value_filter_ops(kind, &value) {
                        // The comparisons, then a line and the NULL tests.
                        if op == FilterOp::IsNull && !value.is_null() {
                            kit::menu_separator(ui);
                        }
                        if item(ui, None, &value_filter_label(&name, op, &v), None).clicked() {
                            let value = if op.is_unary() { Value::Null } else { value.clone() };
                            apply_value_filter(self.model, self.state, self.options, (col, op, &value), &self.ctx, &mut self.actions);
                            ui.close();
                        }
                    }
                });
            });
            let has_filter = self.model.requested_view_spec().filter.is_some();
            if has_filter && item(ui, Some(icon::FUNNEL_X), tr("Clear client filter"), None).clicked() {
                crate::grid::apply_client_filter(self.model, None, &self.ctx);
                ui.close();
            }
            self.column_items(ui, col);
            self.more_menu(ui, col, known.map(|v| (name.clone(), v)));
            kit::menu_separator(ui);
            // --- navigation / values
            if let Some(i) = self.model.foreign_key_for_column(col) {
                let target = self.model.foreign_keys()[i].ref_table.display_path();
                if item(ui, Some(icon::ARROW_SQUARE_OUT), &trf("Navigate to {}", &[&target]), None).on_hover_text(tr("Cmd+click a link cell to navigate")).clicked() {
                    self.navigate_fk(row, col);
                    ui.close();
                }
            }
            if item(ui, Some(icon::EYE), tr("View value"), Some(keys("⇧↩", "Shift+Enter"))).clicked() {
                self.actions.push(GridAction::OpenValuePanel { row, col });
                ui.close();
            }
            submenu(ui, Some(icon::FILE_SQL), tr("Generate SQL"), &mut |ui| {
                for (kind, label) in [(SqlKind::Select, "SELECT"), (SqlKind::Insert, "INSERT"), (SqlKind::Update, "UPDATE"), (SqlKind::Delete, "DELETE")] {
                    if item(ui, None, label, None).clicked() {
                        self.generate_sql(kind);
                        ui.close();
                    }
                }
            });
        }
        kit::menu_separator(ui);
        if item(ui, Some(icon::EXPORT), tr("Export selection…"), None).clicked() {
            let rows = self.state.rt.selection.rows(self.geo.nrows, usize::MAX);
            let columns = self.state.selected_columns();
            let all = self.state.rt.selection.is_all();
            self.actions.push(GridAction::Export { rows: (!all).then_some(rows), columns });
            ui.close();
        }
        if item(ui, Some(icon::ARROW_CLOCKWISE), tr("Refresh"), Some(keys("⌘R", "Ctrl+R"))).clicked() {
            self.actions.push(GridAction::Refresh);
            ui.close();
        }
    }

    /// Column related items: ordering, hiding, statistics, the columns (visibility, sizes,
    /// layout, virtual columns, zoom) and the format.
    fn column_items(&mut self, ui: &mut Ui, col: usize) {
        submenu(ui, Some(icon::SORT_ASCENDING), tr("Order by"), &mut |ui| {
            let name = self.model.column_name(col).to_string();
            if item(ui, None, &format!("{name} ASC"), None).clicked() {
                apply_sort(self.model, self.state, self.options, vec![SortKey::asc(col)], &self.ctx, &mut self.actions);
                ui.close();
            }
            if item(ui, None, &format!("{name} DESC"), None).clicked() {
                apply_sort(self.model, self.state, self.options, vec![SortKey::desc(col)], &self.ctx, &mut self.actions);
                ui.close();
            }
            if !self.state.sort.is_empty() {
                if item(ui, None, tr("Add as secondary ASC"), None).clicked() {
                    let mut keys = self.state.sort.clone();
                    keys.retain(|k| k.column != col);
                    keys.push(SortKey::asc(col));
                    apply_sort(self.model, self.state, self.options, keys, &self.ctx, &mut self.actions);
                    ui.close();
                }
                if item(ui, None, tr("Clear sorting"), None).clicked() {
                    apply_sort(self.model, self.state, self.options, Vec::new(), &self.ctx, &mut self.actions);
                    ui.close();
                }
            }
        });
        if item(ui, Some(icon::EYE_SLASH), tr("Hide column"), None).clicked() {
            self.state.set_column_visible(col, false);
            ui.close();
        }
        let active = self.active();
        submenu(ui, Some(icon::COLUMNS), tr("Columns"), &mut |ui| {
            if item(ui, None, tr("Show all"), None).clicked() {
                self.state.show_all_columns();
            }
            if !self.detached {
                if item(ui, Some(icon::ARROWS_OUT_LINE_HORIZONTAL), tr("Auto-size column"), None).clicked() {
                    let ctx = self.ctx.clone();
                    let painter = egui::Painter::new(ctx, ui.layer_id(), ui.clip_rect());
                    self.autosize_with(&painter, col);
                    ui.close();
                }
                if item(ui, Some(icon::ARROW_LEFT), tr("Move column left"), Some(keys("⌘⌥←", "Ctrl+Alt+Left"))).clicked() {
                    self.move_active_column(-1);
                    ui.close();
                }
                if item(ui, Some(icon::ARROW_RIGHT), tr("Move column right"), Some(keys("⌘⌥→", "Ctrl+Alt+Right"))).clicked() {
                    self.move_active_column(1);
                    ui.close();
                }
                if item(ui, Some(icon::ARROWS_OUT_LINE_HORIZONTAL), tr("Fit columns to values"), None).clicked() {
                    let ctx = self.ctx.clone();
                    let painter = egui::Painter::new(ctx, ui.layer_id(), ui.clip_rect());
                    self.fit_columns_to_values(&painter);
                    ui.close();
                }
                if item(ui, Some(icon::ARROWS_HORIZONTAL), tr("Fit columns to screen"), None).clicked() {
                    self.fit_columns_to_screen();
                    ui.close();
                }
                if item(ui, Some(icon::EYE_SLASH), tr("Hide columns without data"), None).clicked() {
                    self.hide_empty_columns();
                    ui.close();
                }
            }
            kit::menu_separator(ui);
            // Virtual columns: computed on the client from the row's values.
            if item(ui, Some(icon::FUNCTION), tr("Add virtual column…"), None).clicked() {
                self.state.rt.virtual_dialog = Some(VirtualColumnDialog::default());
                ui.close();
            }
            if let Some((_, c)) = active
                && self.model.is_virtual_column(c)
            {
                let idx = c - self.model.base_column_count();
                if item(ui, None, tr("Edit virtual column…"), None).clicked() {
                    let v = &self.model.virtual_columns()[idx];
                    self.state.rt.virtual_dialog =
                        Some(VirtualColumnDialog { name: v.name.clone(), expression: v.expression.source().to_string(), error: None, editing: Some(idx) });
                    ui.close();
                }
                if item(ui, None, tr("Remove virtual column"), None).clicked() {
                    self.model.remove_virtual_column(idx);
                    // The columns after it move: widths, order and sort follow them.
                    self.state.sync_columns(self.model);
                    ui.close();
                }
            }
            // Zoom is the grid's own.
            if !self.detached {
                kit::menu_separator(ui);
                if item(ui, Some(icon::MAGNIFYING_GLASS_PLUS), tr("Zoom in"), None).clicked() {
                    self.state.zoom_by(Some(0.1));
                }
                if item(ui, Some(icon::MAGNIFYING_GLASS_MINUS), tr("Zoom out"), None).clicked() {
                    self.state.zoom_by(Some(-0.1));
                }
                if item(ui, None, tr("Reset zoom"), None).clicked() {
                    self.state.zoom_by(None);
                    ui.close();
                }
            }
            kit::menu_separator(ui);
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for i in 0..self.state.order.len() {
                    let c = self.state.order[i];
                    let mut visible = self.state.columns.get(c).is_some_and(|s| s.visible);
                    if kit::menu_check(ui, &mut visible, self.model.column_name(c)).changed() {
                        self.state.set_column_visible(c, visible);
                    }
                }
            });
        });
    }

    /// "More ▸": the column's format, its statistics over all rows and (for a cell's `value`
    /// of the column `name`) highlighting the rows with it.
    fn more_menu(&mut self, ui: &mut Ui, col: usize, value: Option<(String, Value)>) {
        submenu(ui, Some(icon::DOTS_THREE), tr("More"), &mut |ui| {
            if let Some((name, value)) = &value {
                submenu(ui, Some(icon::HIGHLIGHTER), tr("Highlight rows with this value"), &mut |ui| {
                    let text = (!value.is_null()).then(|| value.to_text().into_owned());
                    for (label, rgb) in super::state::HIGHLIGHT_COLORS {
                        let swatch = egui::RichText::new("■").color(egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                        if ui.button(egui::WidgetText::from(swatch)).on_hover_text(tr(label)).clicked() {
                            self.state.set_highlight(name, text.clone(), Some(rgb));
                            ui.close();
                        }
                    }
                    kit::menu_separator(ui);
                    if item(ui, None, tr("Remove this highlight"), None).clicked() {
                        self.state.set_highlight(name, text.clone(), None);
                        ui.close();
                    }
                    if !self.state.highlights.is_empty() && item(ui, None, tr("Remove all highlights"), None).clicked() {
                        self.state.highlights.clear();
                        ui.close();
                    }
                });
            }
            self.format_menu(ui, col);
            // The header's tooltip summarises the fetched rows; this asks the server.
            if !self.model.is_virtual_column(col)
                && item(ui, Some(icon::CHART_BAR), tr("Statistics of the whole column"), None)
                    .on_hover_text(tr("Rows, NULLs, distinct values, the smallest and the largest, over all rows of the query"))
                    .clicked()
            {
                self.actions.push(GridAction::ColumnStatistics { column: col });
                ui.close();
            }
        });
    }

    fn format_menu(&mut self, ui: &mut Ui, col: usize) {
        submenu(ui, None, tr("Format"), &mut |ui| {
            let mut f = self.state.column_format(col).cloned().unwrap_or_default();
            let mut changed = false;
            if self.model.column_kind(col).is_numeric() || self.model.is_virtual_column(col) {
                kit::menu_caption(ui, tr("Decimals"));
                ui.horizontal(|ui| {
                    if ui.selectable_label(f.decimals.is_none(), tr("Auto")).clicked() {
                        f.decimals = None;
                        changed = true;
                    }
                    for d in 0..=4usize {
                        if ui.selectable_label(f.decimals == Some(d), d.to_string()).clicked() {
                            f.decimals = Some(d);
                            changed = true;
                        }
                    }
                });
                let mut grouped = f.thousands_separator.unwrap_or(self.options.format.thousands_separator);
                if kit::menu_check(ui, &mut grouped, tr("Thousands separator")).changed() {
                    f.thousands_separator = Some(grouped);
                    changed = true;
                }
            }
            if self.model.column_kind(col).is_temporal() {
                kit::menu_caption(ui, tr("Date pattern"));
                for p in ["%Y-%m-%d", "%d.%m.%Y", "%d/%m/%Y", "%m/%d/%Y", "%e %b %Y"] {
                    if kit::menu_check(ui, &mut (f.date_format.as_deref() == Some(p)), p).clicked() {
                        f.date_format = Some(p.to_string());
                        changed = true;
                    }
                }
                kit::menu_caption(ui, tr("Timestamp pattern"));
                for p in ["%Y-%m-%d %H:%M:%S", "%d.%m.%Y %H:%M", "%Y-%m-%dT%H:%M:%S%.3f"] {
                    if kit::menu_check(ui, &mut (f.datetime_format.as_deref() == Some(p)), p).clicked() {
                        f.datetime_format = Some(p.to_string());
                        changed = true;
                    }
                }
            }
            kit::menu_separator(ui);
            if item(ui, None, tr("Reset format"), None).clicked() {
                f = ColumnFormat::default();
                changed = true;
            }
            if changed {
                self.state.set_column_format(col, Some(f));
            }
        });
    }

    /// The value of a cell for a menu, shown every frame while it is open: never
    /// waits long for the row store; while the fetch task holds it, the value
    /// read before (`None` when there is none: the items using it are disabled).
    fn menu_value(&mut self, row: usize, col: usize) -> Option<Value> {
        let key = (self.model.content_generation(), row, col);
        match self.model.try_cell(row, col, super::STORE_WAIT) {
            Some(v) => {
                let v = v.unwrap_or_default();
                self.state.rt.menu_value = Some((key, v.clone()));
                Some(v)
            }
            None => {
                self.ctx.request_repaint_after(std::time::Duration::from_millis(10));
                self.state.rt.menu_value.as_ref().filter(|(k, _)| *k == key).map(|(_, v)| v.clone())
            }
        }
    }

    /// Auto-size `col` using a painter for measurement: it fits the longest
    /// shown value, up to the width of the grid (a longer text is read in the
    /// value panel, not by scrolling sideways).
    pub fn autosize_with(&mut self, painter: &egui::Painter, col: usize) {
        let mut w = self.header_width(painter, col);
        let rows = self.geo.visible_rows(self.state.rt.scroll_y);
        let fmt = self.options.format.clone();
        let prepared = crate::format::PreparedFormat::new(&fmt, self.state.column_format(col));
        let mut texts = Vec::new();
        self.model.visit_rows(rows, |_, cells| {
            let v = cells.value_of(col).unwrap_or_default();
            texts.push(if v.is_null() { fmt.null_text.clone() } else { prepared.format(&v).into_owned() });
        });
        for t in &texts {
            w = w.max(self.measure(painter, t) + 2.0 * super::frame::PAD + 2.0);
        }
        let max = (self.geo.body.width() - 2.0).max(super::state::MIN_COLUMN_WIDTH);
        self.state.set_column_width(col, w.clamp(super::state::MIN_COLUMN_WIDTH, max));
    }

    /// Generate SQL for the selected rows and show it.
    pub fn generate_sql(&mut self, kind: SqlKind) {
        let rows = self.state.rt.selection.rows(self.geo.nrows, 10_000);
        if rows.is_empty() {
            return;
        }
        let generic = GenericDialect::new();
        let dialect: &dyn Dialect = self.options.dialect.as_deref().unwrap_or(&generic);
        let table = sql_table_name(self.model, self.options);
        let sql = self.model.generate_rows_sql(kind, &rows, dialect, &table);
        self.state.rt.sql_window = Some((trf("Generated SQL ({} rows)", &[&rows.len()]), sql.clone()));
        self.actions.push(GridAction::GeneratedSql { kind, sql });
    }
}
