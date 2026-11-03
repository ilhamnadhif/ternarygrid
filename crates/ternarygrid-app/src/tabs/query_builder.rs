//! Visual query builder tab (FR-S10): drop tables on a canvas, join them,
//! pick columns, conditions, grouping and ordering; the SQL is generated live.

use crate::backend::Loadable;
use crate::command::AppCommand;
use crate::cx::AppContext;
use crate::tabs::TabView;
use egui::{RichText, Sense, Ui, vec2};
use egui_phosphor::regular as icon;
use ternarygrid_core::meta::{ObjectKind, ObjectRef, SchemaRef};
use ternarygrid_core::tr;
use ternarygrid_ui::{kit, parts, style};
use ternarygrid_visual::builder::{QbEvent, QbTab, QbViewState, QueryModel, show_query_builder};

pub struct QueryBuilderTab {
    conn: String,
    schema: Option<SchemaRef>,
    model: QueryModel,
    state: Option<QbViewState>,
    picker_open: bool,
    picker_filter: String,
    /// Tables chosen in the picker whose definition is still loading.
    pending: Vec<ObjectRef>,
}

/// One row of the table picker: the kind's icon and the name.
fn picker_row(ui: &mut Ui, kind: ObjectKind, name: &str) -> egui::Response {
    let p = style::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, false, name));
    if resp.hovered() {
        ui.painter().rect_filled(rect, style::RADIUS, p.hover);
    }
    let cy = rect.center().y;
    ui.painter().text(egui::pos2(rect.left() + 16.0, cy), egui::Align2::CENTER_CENTER, crate::nav::kind_icon(kind), style::icon_font(14.0), p.text_muted);
    ui.painter().text(egui::pos2(rect.left() + 32.0, cy), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(style::body_size(ui.ctx())), p.text);
    resp
}

impl QueryBuilderTab {
    pub fn new(conn: String, schema: Option<SchemaRef>) -> Self {
        QueryBuilderTab { conn, schema, model: QueryModel::default(), state: None, picker_open: false, picker_filter: String::new(), pending: Vec::new() }
    }

    /// Add these tables as if chosen in the picker (their definitions load first).
    #[doc(hidden)]
    pub fn add_tables(&mut self, tables: &[ObjectRef]) {
        self.pending.extend(tables.iter().cloned());
    }

    /// The model (tables, joins, columns, conditions).
    #[doc(hidden)]
    pub fn model_mut(&mut self) -> &mut QueryModel {
        &mut self.model
    }

    /// Show the bottom tab `tab`, and the table picker filtered by `filter`.
    #[doc(hidden)]
    pub fn show_tab(&mut self, tab: QbTab, picker: Option<&str>) {
        if let Some(s) = self.state.as_mut() {
            s.tab = tab;
        }
        if let Some(f) = picker {
            self.picker_open = true;
            self.picker_filter = f.to_string();
        }
    }

    fn picker(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let backend = cx.backend;
        let Some(live) = cx.conns.live_mut(&self.conn) else { return };
        let mut schema = self.schema.clone().unwrap_or_else(|| SchemaRef::new(live.current_database.clone(), live.current_schema.clone()));
        // A database chosen without a schema: its default one.
        if schema.schema.is_none() && schema.database.is_some() {
            schema.schema = live.current_schema.clone();
        }
        // The schema's own database: another database of the server has its own session.
        let Some(main) = live.ensure_session(backend, schema.database.as_deref()) else {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            return;
        };
        live.meta.request_objects(backend, &main, &schema, ObjectKind::Table);
        live.meta.request_objects(backend, &main, &schema, ObjectKind::View);
        let mut chosen: Option<ObjectRef> = None;
        let mut open = self.picker_open;
        let content = ui.ctx().content_rect();
        let at = egui::pos2((content.right() - 360.0).max(content.left()), content.top() + 120.0);
        let filter_id = egui::Id::new(("qb_picker_filter", &self.conn));
        let filter_text = &mut self.picker_filter;
        parts::Dialog::new(tr("Add table")).icon(icon::TABLE).width(320.0).body_padding(egui::Margin::symmetric(14, 12)).show_window(
            ui.ctx(),
            egui::Id::new(("qb_picker", &self.conn)),
            &mut open,
            Some(at),
            |ui| {
                let p = style::pal(ui);
                ui.spacing_mut().item_spacing.y = 8.0;
                kit::search_field(ui, filter_id, filter_text, tr("Filter…"), ui.available_width());
                let filter = filter_text.to_lowercase();
                egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for kind in [ObjectKind::Table, ObjectKind::View] {
                        match live.meta.objects.get(&(schema.clone(), kind)) {
                            Some(Loadable::Loaded(list)) => {
                                for o in list.iter().filter(|o| filter.is_empty() || o.obj.name.to_lowercase().contains(&filter)) {
                                    if picker_row(ui, kind, &o.obj.name).double_clicked() {
                                        chosen = Some(o.obj.clone());
                                    }
                                }
                            }
                            Some(Loadable::Failed(e)) => {
                                ui.label(RichText::new(e.message.clone()).color(p.err));
                            }
                            _ => {
                                ui.spinner();
                            }
                        }
                    }
                });
                ui.label(RichText::new(tr("Double-click a table to add it.")).size(12.0).color(p.text_faint));
            },
            None,
        );
        self.picker_open = open;
        if let Some(obj) = chosen {
            live.meta.request_table_def(backend, &main, &obj);
            self.pending.push(obj);
        }
    }
}

impl TabView for QueryBuilderTab {
    fn title(&self) -> String {
        tr("Query builder").to_string()
    }
    fn icon(&self) -> &'static str {
        icon::GRAPH
    }
    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let Some(live) = cx.conns.live_mut(&self.conn) else {
            kit::empty_state(ui, icon::PLUGS, tr("The connection is closed."), tr("Connect to add its tables to the query."), |ui| {
                if kit::primary_icon(ui, icon::PLUGS_CONNECTED, tr("Connect")).clicked() {
                    cx.commands.push(AppCommand::Connect(self.conn.clone()));
                }
            });
            return;
        };
        let dialect = live.dialect.clone();
        let backend = cx.backend;
        // Tables whose definition arrived.
        let mut still = Vec::new();
        for obj in std::mem::take(&mut self.pending) {
            if !live.meta.table_defs.contains_key(&obj)
                && let Some(main) = live.ensure_session(backend, obj.database.as_deref())
            {
                live.meta.request_table_def(backend, &main, &obj);
            }
            match live.meta.table_defs.get(&obj) {
                Some(Loadable::Loaded(def)) => {
                    let def = def.clone();
                    self.model.add_table(&def);
                }
                Some(Loadable::Failed(e)) => cx.toasts.error(tr("Cannot read the table"), Some(e.full_text())),
                _ => still.push(obj),
            }
        }
        self.pending = still;
        if !self.pending.is_empty() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        let state = self.state.get_or_insert_with(|| QbViewState::new(dialect));
        let resp = show_query_builder(ui, &mut self.model, state);
        for ev in resp.events {
            match ev {
                QbEvent::AddTableRequested => self.picker_open = true,
                QbEvent::OpenInEditor(sql) => {
                    cx.push(AppCommand::NewSqlEditor { connection: Some(self.conn.clone()), schema: self.schema.clone(), text: Some(sql) });
                }
                // Run: an editor that executes it as soon as its session is open,
                // with the confirmations of the connection type.
                QbEvent::RunQuery(sql) => {
                    let mut tab = crate::tabs::sql_editor::SqlEditorTab::new(Some(self.conn.clone()), self.schema.clone(), sql, None, false, tr("Query builder").to_string());
                    tab.run_on_open = true;
                    cx.push(AppCommand::OpenTab(Box::new(tab)));
                }
                QbEvent::Changed => {}
            }
        }
        if self.picker_open {
            self.picker(ui, cx);
        }
    }

    crate::impl_tab_any!();
}
