//! Object editor (FR-N4): properties, data, DDL, dependencies and statistics of
//! a database object.

use crate::backend::{Backend, Loadable};
use crate::command::{AppCommand, ObjectPage};
use crate::conn::session::SessionHandle;
use crate::cx::AppContext;
use crate::tabs::{EmbeddedView, TabView};
use crate::widgets;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use std::future::Future;
use std::sync::Arc;
use ternarygrid_core::ddl::TableDef;
use ternarygrid_core::meta::{Dependency, DependencyDirection, ObjectKind, ObjectRef, Property};
use ternarygrid_core::{DbResult, tr};
use ternarygrid_ui::kit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TablePage {
    Columns,
    Constraints,
    ForeignKeys,
    References,
    Indexes,
    Triggers,
    Partitions,
    VirtualModel,
}

pub struct ObjectEditorTab {
    pub conn: String,
    pub obj: ObjectRef,
    pub page: ObjectPage,
    table_page: TablePage,
    props: Loadable<Vec<Property>>,
    table: Loadable<Arc<TableDef>>,
    ddl: Loadable<String>,
    deps: Loadable<Vec<Dependency>>,
    stats: Loadable<Vec<Property>>,
    selected_row: Option<usize>,
    /// Data page, created on first use.
    pub data: Option<crate::results::data_tab::TableDataView>,
    /// Diagram page (the table and its related ones), created on first use.
    diagram: Option<crate::tabs::er::ErTab>,
    /// Virtual model of the table being edited (virtual key, dictionary, virtual columns).
    vmodel: Option<ternarygrid_config::VirtualTable>,
    new_vcol: (String, String),
    /// Metadata clears seen: after another one (designer Apply, Refresh) the pages reload.
    invalidations: Option<u64>,
}

impl ObjectEditorTab {
    pub fn new(conn: String, obj: ObjectRef, page: ObjectPage) -> Self {
        ObjectEditorTab {
            conn,
            obj,
            page,
            table_page: TablePage::Columns,
            props: Loadable::NotLoaded,
            table: Loadable::NotLoaded,
            ddl: Loadable::NotLoaded,
            deps: Loadable::NotLoaded,
            stats: Loadable::NotLoaded,
            selected_row: None,
            data: None,
            diagram: None,
            vmodel: None,
            new_vcol: (String::new(), String::new()),
            invalidations: None,
        }
    }

    pub fn key(conn: &str, obj: &ObjectRef) -> String {
        format!("object:{conn}:{:?}:{}:{}", obj.kind, obj.database.as_deref().unwrap_or(""), obj.display_path())
    }

    fn session(&self, cx: &mut AppContext) -> Option<SessionHandle> {
        let backend = cx.backend;
        cx.conns.live_mut(&self.conn)?.ensure_session(backend, self.obj.database.as_deref())
    }

    fn load<T: Send + 'static, F, Fut>(slot: &mut Loadable<T>, backend: &Backend, session: Option<SessionHandle>, f: F)
    where
        F: FnOnce(SessionHandle) -> Fut + Send + 'static,
        Fut: Future<Output = DbResult<T>> + Send + 'static,
    {
        slot.poll();
        if slot.is_not_loaded()
            && let Some(s) = session
        {
            *slot = Loadable::Loading(backend.spawn(async move { f(s).await }));
        }
    }

    fn refresh(&mut self) {
        self.props = Loadable::NotLoaded;
        self.table = Loadable::NotLoaded;
        self.ddl = Loadable::NotLoaded;
        self.deps = Loadable::NotLoaded;
        self.stats = Loadable::NotLoaded;
    }

    fn properties_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let session = self.session(cx);
        let obj = self.obj.clone();
        Self::load(&mut self.props, cx.backend, session.clone(), move |s| async move { s.lock().await.object_properties(&obj).await });
        let is_table = self.obj.kind.is_table_like() || matches!(self.obj.kind, ObjectKind::View | ObjectKind::MaterializedView);
        if is_table {
            let obj = self.obj.clone();
            Self::load(&mut self.table, cx.backend, session, move |s| async move { s.lock().await.table_definition(&obj).await.map(Arc::new) });
        }
        let mut retry = false;
        let gap = ui.spacing().item_spacing.y;
        if !is_table {
            // 14 below the header's line, as the mockups' function page.
            ui.add_space(14.0);
            if widgets::load_state(ui, &self.props, &mut retry)
                && let Some(p) = self.props.get()
            {
                egui::ScrollArea::vertical().show(ui, |ui| widgets::property_grid(ui, 180.0, p));
            }
            if retry {
                self.props = Loadable::NotLoaded;
            }
            return;
        }
        // Tables: the sub pages 10 below the header's line, each with its count once the table
        // is read; 10 under them the comment and the options, then the page.
        let def = self.table.get();
        let count = |n: Option<usize>| n.filter(|n| *n > 0).map(|n| n.to_string());
        let constraints = def.map(|t| t.primary_key.iter().count() + t.unique_keys.len() + t.checks.len());
        let sub_pages = [
            (TablePage::Columns, tr("Columns"), count(def.map(|t| t.columns.len()))),
            (TablePage::Constraints, tr("Constraints"), count(constraints)),
            (TablePage::ForeignKeys, tr("Foreign keys"), count(def.map(|t| t.foreign_keys.len()))),
            (TablePage::References, tr("References"), count(def.map(|t| t.referenced_by.len()))),
            (TablePage::Indexes, tr("Indexes"), count(def.map(|t| t.indexes.len()))),
            (TablePage::Triggers, tr("Triggers"), count(def.map(|t| t.triggers.len()))),
            (TablePage::Partitions, tr("Partitions"), count(def.map(|t| t.partitions.len()))),
            (TablePage::VirtualModel, tr("Virtual model"), None),
        ];
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (page, label, n) in sub_pages {
                if pill_tab(ui, label, n.as_deref(), self.table_page == page).clicked() {
                    self.table_page = page;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.obj.kind.is_table_like() && kit::button_full(ui, kit::Kind::Secondary, Some(icon::PENCIL), tr("Edit table"), true, false).clicked() {
                    cx.push(AppCommand::EditTable { connection: self.conn.clone(), table: self.obj.clone() });
                }
            });
        });
        ui.add_space(10.0 - gap);
        if let Some(t) = self.table.get().cloned() {
            // Rows and size are in the title.
            let options: Vec<String> = t.options.iter().map(|(k, v)| format!("{k}: {v}")).collect();
            if t.comment.is_some() || !options.is_empty() {
                let p = ternarygrid_ui::pal(ui);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if let Some(c) = &t.comment {
                        ui.label(RichText::new(c).size(12.5).italics().color(p.text_muted));
                        if !options.is_empty() {
                            ui.label(RichText::new("  ·  ").size(12.5).color(p.text_muted));
                        }
                    }
                    ui.label(RichText::new(options.join("  ·  ")).size(12.5).color(p.text_muted));
                });
                ui.add_space(4.0 - gap);
            }
        }
        if !widgets::load_state(ui, &self.table, &mut retry) {
            if retry {
                self.table = Loadable::NotLoaded;
            }
            return;
        }
        let Some(t) = self.table.get().cloned() else { return };
        let fmt_cols = |c: &[String]| c.join(", ");
        // A name some rows lack: "(unnamed)", faint (a page where none has one drops the column).
        let some_names = |names: &[Option<String>]| names.iter().any(|n| n.as_deref().is_some_and(|n| !n.is_empty()));
        let name_or_unset = |n: &Option<String>| n.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| widgets::unset(tr("(unnamed)")));
        match self.table_page {
            TablePage::Columns => {
                let fk_columns: Vec<&str> = t.foreign_keys.iter().flat_map(|fk| fk.columns.iter().map(String::as_str)).collect();
                let rows: Vec<Vec<String>> = t
                    .columns
                    .iter()
                    .map(|c| {
                        vec![
                            c.ordinal.to_string(),
                            // The key of primary key columns, the link of foreign key ones (as the grid's header).
                            if t.is_pk_column(&c.name) {
                                format!("{} {}", icon::KEY, c.name)
                            } else if fk_columns.contains(&c.name.as_str()) {
                                format!("{} {}", icon::LINK_SIMPLE, c.name)
                            } else {
                                c.name.clone()
                            },
                            // In lower case, as the mockups (and the grid's header) write types.
                            c.data_type.to_lowercase(),
                            if c.nullable { String::new() } else { "NOT NULL".into() },
                            c.generated.as_ref().map(|g| format!("AS ({g})")).or_else(|| c.default.clone()).unwrap_or_default(),
                            if c.auto_increment { "✓".into() } else { String::new() },
                            c.collation.clone().unwrap_or_default(),
                            c.comment.clone().unwrap_or_default(),
                        ]
                    })
                    .collect();
                use widgets::CellStyle as S;
                // The columns empty in every row (no collations, no comments) are left out.
                let (headers, rows, styles, widths) = drop_empty_columns(
                    &["#", tr("Name"), tr("Type"), tr("Null"), tr("Default"), tr("Auto"), tr("Collation"), tr("Comment")],
                    rows,
                    &[S::Faint, S::Plain, S::Mono, S::Muted, S::MonoMuted, S::Ok, S::Muted, S::Muted],
                    &[-40.0, 1.2, 1.0, 0.8, 1.4, 0.4, 0.6, 1.6],
                    3,
                );
                let r = widgets::styled_table_widths(ui, "obj_cols", &headers, &rows, self.selected_row, &styles, &widths);
                if let Some(i) = r.clicked {
                    self.selected_row = Some(i);
                }
            }
            TablePage::Constraints => {
                let names: Vec<Option<String>> =
                    t.primary_key.iter().map(|k| k.name.clone()).chain(t.unique_keys.iter().map(|u| u.name.clone())).chain(t.checks.iter().map(|c| c.name.clone())).collect();
                let named = some_names(&names);
                let mut rows = Vec::new();
                if let Some(pk) = &t.primary_key {
                    rows.push(vec![name_or_unset(&pk.name), "PRIMARY KEY".into(), fmt_cols(&pk.columns)]);
                }
                for u in &t.unique_keys {
                    rows.push(vec![name_or_unset(&u.name), "UNIQUE".into(), fmt_cols(&u.columns)]);
                }
                for c in &t.checks {
                    rows.push(vec![name_or_unset(&c.name), "CHECK".into(), c.expression.clone()]);
                }
                use widgets::CellStyle as S;
                if named {
                    widgets::styled_table_widths(ui, "obj_cons", &[tr("Name"), tr("Type"), tr("Definition")], &rows, None, &[S::Plain, S::Chip, S::Mono], &[1.0, 0.7, 3.0]);
                } else {
                    // No constraint has a name (SQLite's): the kind and the definition only.
                    let rows: Vec<Vec<String>> = rows.into_iter().map(|r| r[1..].to_vec()).collect();
                    widgets::styled_table_widths(ui, "obj_cons", &[tr("Type"), tr("Definition")], &rows, None, &[S::Chip, S::Mono], &[-140.0, 1.0]);
                }
            }
            TablePage::ForeignKeys | TablePage::References => {
                let list = if self.table_page == TablePage::ForeignKeys { &t.foreign_keys } else { &t.referenced_by };
                let incoming = self.table_page == TablePage::References;
                let names: Vec<Option<String>> = list.iter().map(|fk| fk.name.clone()).collect();
                let named = some_names(&names);
                let rows: Vec<Vec<String>> = list
                    .iter()
                    .map(|fk| {
                        vec![
                            if named { name_or_unset(&fk.name) } else { String::new() },
                            // The referencing table (References), the referenced one (Foreign keys):
                            // the other one is this table, said once by the page.
                            if incoming {
                                format!("{}{}", fk.schema.as_ref().map(|s| format!("{s}.")).unwrap_or_default(), fk.table)
                            } else {
                                format!("{}{}", fk.ref_schema.as_ref().map(|s| format!("{s}.")).unwrap_or_default(), fk.ref_table)
                            },
                            fmt_cols(&fk.columns),
                            fmt_cols(&fk.ref_columns),
                            fk.on_update.sql().to_string(),
                            fk.on_delete.sql().to_string(),
                        ]
                    })
                    .collect();
                use widgets::CellStyle as S;
                let table_label = if incoming { tr("Table") } else { tr("References") };
                let (headers, rows, styles, widths) = drop_empty_columns(
                    &[tr("Name"), table_label, tr("Columns"), tr("Referenced columns"), tr("On update"), tr("On delete")],
                    rows,
                    &[S::Plain, S::Plain, S::Mono, S::Mono, S::Plain, S::Plain],
                    &[1.6, 1.1, 1.0, 1.0, 0.8, 0.8],
                    0,
                );
                let r = widgets::styled_table_widths(ui, "obj_fks", &headers, &rows, None, &styles, &widths);
                if let Some(i) = r.double_clicked
                    && let Some(fk) = list.get(i)
                {
                    let (schema, table) = if self.table_page == TablePage::References {
                        (fk.schema.clone(), fk.table.clone())
                    } else {
                        (fk.ref_schema.clone().or(self.obj.schema.clone()), fk.ref_table.clone())
                    };
                    let target = ObjectRef::table(schema, table).with_database(self.obj.database.clone());
                    cx.push(AppCommand::OpenObject { connection: self.conn.clone(), object: target, page: ObjectPage::Properties });
                }
            }
            TablePage::Indexes => {
                let rows: Vec<Vec<String>> = t
                    .indexes
                    .iter()
                    .map(|i| {
                        vec![
                            i.name.clone(),
                            if i.primary {
                                "PRIMARY".into()
                            } else if i.unique {
                                "UNIQUE".into()
                            } else {
                                String::new()
                            },
                            i.method.clone().unwrap_or_default(),
                            i.columns.iter().map(|c| format!("{}{}", c.name, if c.descending { " DESC" } else { "" })).collect::<Vec<_>>().join(", "),
                            i.predicate.clone().unwrap_or_default(),
                            i.definition.clone().unwrap_or_default(),
                        ]
                    })
                    .collect();
                use widgets::CellStyle as S;
                // Without the columns no index fills (SQLite's methods).
                let (headers, rows, styles, widths) = drop_empty_columns(
                    &[tr("Name"), tr("Kind"), tr("Method"), tr("Columns"), tr("Predicate"), tr("Definition")],
                    rows,
                    &[S::Plain, S::Chip, S::Plain, S::Mono, S::Mono, S::MonoMuted],
                    &[1.2, 0.7, 0.5, 1.0, 1.0, 2.6],
                    1,
                );
                widgets::styled_table_widths(ui, "obj_idx", &headers, &rows, None, &styles, &widths);
            }
            TablePage::Triggers => {
                let rows: Vec<Vec<String>> = t
                    .triggers
                    .iter()
                    .map(|tr_| {
                        vec![
                            tr_.name.clone(),
                            tr_.timing.clone(),
                            tr_.events.join(" OR "),
                            if tr_.for_each_row { "ROW".into() } else { "STATEMENT".into() },
                            if tr_.enabled { "✓".into() } else { String::new() },
                            tr_.definition.clone(),
                        ]
                    })
                    .collect();
                use widgets::CellStyle as S;
                let (headers, rows, styles, widths) = drop_empty_columns(
                    &[tr("Name"), tr("Timing"), tr("Events"), tr("Level"), tr("Enabled"), tr("Definition")],
                    rows,
                    &[S::Plain, S::Plain, S::Plain, S::Plain, S::Ok, S::MonoMuted],
                    &[1.3, 0.6, 0.8, 0.6, 0.6, 2.4],
                    1,
                );
                widgets::styled_table_widths(ui, "obj_trg", &headers, &rows, None, &styles, &widths);
            }
            TablePage::VirtualModel => {
                let key = ternarygrid_config::project::VirtualTableKey::of(&self.obj);
                let projects = cx.services.projects.as_ref();
                if self.vmodel.is_none() {
                    let vm = projects.and_then(|p| p.virtual_model(&self.conn).ok()).unwrap_or_default();
                    self.vmodel = Some(vm.get(&key).cloned().unwrap_or_default());
                }
                let Some(vt) = self.vmodel.as_mut() else { return };
                use ternarygrid_ui::parts::group;
                ui.label(RichText::new(tr("Client-side additions used by the data editor; stored in the project, not in the database.")).color(ternarygrid_ui::pal(ui).text_muted));
                ui.add_space(4.0);
                group(ui, Some(tr("Virtual key")), Some(tr("Columns that identify a row when the table has no primary or unique key.")), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        // 20 between the check boxes: each one's box with its own name.
                        ui.spacing_mut().item_spacing.x = 20.0;
                        let mut keys = vt.virtual_key.clone().unwrap_or_default();
                        for c in &t.columns {
                            let mut on = keys.contains(&c.name);
                            if ternarygrid_ui::kit::checkbox(ui, &mut on, &c.name).changed() {
                                if on {
                                    keys.push(c.name.clone());
                                } else {
                                    keys.retain(|k| k != &c.name);
                                }
                            }
                        }
                        vt.virtual_key = if keys.is_empty() { None } else { Some(keys) };
                    })
                });
                ui.add_space(8.0);
                group(ui, Some(tr("Dictionary")), Some(tr("When other tables reference this one, show these columns next to the key.")), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 20.0;
                        let mut cols = vt.dictionary.clone().map(|d| d.description_columns).unwrap_or_default();
                        for c in &t.columns {
                            let mut on = cols.contains(&c.name);
                            if ternarygrid_ui::kit::checkbox(ui, &mut on, &c.name).changed() {
                                if on {
                                    cols.push(c.name.clone());
                                } else {
                                    cols.retain(|k| k != &c.name);
                                }
                            }
                        }
                        vt.dictionary = if cols.is_empty() { None } else { Some(ternarygrid_config::project::DictionaryConfig { description_columns: cols }) };
                    })
                });
                ui.add_space(8.0);
                let mut remove = None;
                let new_vcol = &mut self.new_vcol;
                // A numeric column of this table for the expression's example.
                let measure = t.columns.iter().find(|c| is_amount_type(&c.data_type) && !ternarygrid_grid::format::is_identifier_name(&c.name)).map(|c| c.name.clone());
                let hint = match &measure {
                    Some(m) => ternarygrid_core::trf("Expression, e.g. {} * 1.1", &[m]),
                    None => tr("Expression, e.g. upper(name)").to_string(),
                };
                group(ui, Some(tr("Virtual columns")), Some(tr("Computed on the client from each row's values.")), |ui| {
                    // Fields of fixed widths (not egui's grid, whose cells keep the last frame's width).
                    ui.spacing_mut().item_spacing.y = 6.0;
                    let (name_w, expr_w, h) = (180.0, 380.0, ternarygrid_ui::style::SMALL_H);
                    let faint = ternarygrid_ui::pal(ui).text_faint;
                    fn field<'t>(text: &'t mut String, hint: &str, mono: bool, faint: egui::Color32) -> egui::TextEdit<'t> {
                        let te =
                            egui::TextEdit::singleline(text).vertical_align(egui::Align::Center).margin(egui::Margin::symmetric(8, 0)).hint_text(RichText::new(hint).color(faint));
                        if mono { te.font(egui::TextStyle::Monospace) } else { te }
                    }
                    for (i, c) in vt.virtual_columns.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.add_sized([name_w, h], field(&mut c.name, "", false, faint));
                            ui.add_sized([expr_w, h], field(&mut c.expression, "", true, faint));
                            if kit::icon_button_ex(ui, icon::TRASH, tr("Remove"), false, h).clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.add_sized([name_w, h], field(&mut new_vcol.0, tr("Name"), false, faint));
                        ui.add_sized([expr_w, h], field(&mut new_vcol.1, &hint, true, faint));
                        let ready = !new_vcol.0.trim().is_empty() && !new_vcol.1.trim().is_empty();
                        if ui.add_enabled_ui(ready, |ui| kit::button_full(ui, kit::Kind::Secondary, Some(icon::PLUS), tr("Add column"), true, false).clicked()).inner {
                            vt.virtual_columns.push(ternarygrid_config::project::VirtualColumn {
                                name: new_vcol.0.trim().to_string(),
                                expression: new_vcol.1.trim().to_string(),
                                ..Default::default()
                            });
                            *new_vcol = (String::new(), String::new());
                        }
                    });
                });
                if let Some(i) = remove {
                    vt.virtual_columns.remove(i);
                }
                ui.add_space(10.0);
                if kit::primary_icon(ui, icon::CHECK, tr("Save virtual model")).clicked()
                    && let Some(p) = projects
                {
                    let mut vm = p.virtual_model(&self.conn).unwrap_or_default();
                    if vt.is_empty() {
                        vm.remove(&key);
                    } else {
                        vm.set(key.clone(), vt.clone());
                    }
                    match p.save_virtual_model(&self.conn, &vm) {
                        Ok(()) => cx.toasts.success(tr("Virtual model saved")),
                        Err(e) => cx.toasts.error(tr("Cannot save the virtual model"), Some(e.to_string())),
                    }
                }
            }
            TablePage::Partitions => {
                if let Some(p) = &t.partitioning {
                    ui.label(format!("{} {} ({})", tr("Partitioned by"), p.strategy, p.key));
                }
                let rows: Vec<Vec<String>> =
                    t.partitions.iter().map(|p| vec![p.name.clone(), p.bound.clone(), p.row_estimate.map(|r| r.to_string()).unwrap_or_default()]).collect();
                widgets::simple_table(ui, "obj_parts", &[tr("Name"), tr("Bound"), tr("Rows")], &rows, None);
            }
        }
        if retry {
            self.table = Loadable::NotLoaded;
        }
    }

    fn ddl_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let session = self.session(cx);
        let obj = self.obj.clone();
        Self::load(&mut self.ddl, cx.backend, session, move |s| async move { s.lock().await.object_ddl(&obj).await });
        let mut retry = false;
        if widgets::load_state(ui, &self.ddl, &mut retry) {
            // Formatted or as the database returns it (SQLite: as typed), shared by all tabs.
            // Formatted by default (a CREATE TABLE on one line is unreadable); off shows it as stored.
            let format_id = egui::Id::new("ddl_formatted");
            let mut formatted = ui.data(|d| d.get_temp::<bool>(format_id)).unwrap_or(true);
            let raw = self.ddl.get().cloned().unwrap_or_default();
            let text = match cx.conns.live(&self.conn).filter(|_| formatted) {
                Some(live) => {
                    // Formatted once per text, not every frame.
                    let key = egui::Id::new(("ddl_pretty", &self.obj));
                    match ui.data(|d| d.get_temp::<(String, String)>(key)).filter(|(r, _)| *r == raw) {
                        Some((_, pretty)) => pretty,
                        None => {
                            let pretty = ternarygrid_sql::format_sql(&raw, live.dialect.syntax(), &crate::tabs::sql_editor::format_options(&cx.settings.editor.format));
                            ui.data_mut(|d| d.insert_temp(key, (raw, pretty.clone())));
                            pretty
                        }
                    }
                }
                None => raw,
            };
            // A toolbar 16 / 8 around and 30 high, a line under it, the code flush under the line.
            let bar = egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.horizontal(|ui| {
                    ui.set_min_height(30.0);
                    if kit::button_full(ui, kit::Kind::Secondary, Some(icon::FILE_CODE), tr("Open in SQL editor"), true, false).clicked() {
                        cx.push(AppCommand::NewSqlEditor { connection: Some(self.conn.clone()), schema: Some(self.obj.schema_ref()), text: Some(text.clone()) });
                    }
                    if kit::button_full(ui, kit::Kind::Ghost, Some(icon::COPY), tr("Copy"), true, false).clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                    if kit::button_full(ui, kit::Kind::Ghost, Some(icon::TEXT_INDENT), tr("Format"), true, formatted)
                        .on_hover_text(if formatted {
                            tr("Formatted with the SQL editor's formatter settings: click to see the DDL as the database stores it")
                        } else {
                            tr("Show the DDL formatted with the SQL editor's formatter settings")
                        })
                        .clicked()
                    {
                        formatted = !formatted;
                        ui.data_mut(|d| d.insert_temp(format_id, formatted));
                    }
                });
            });
            let line = bar.response.rect.bottom() + 0.5;
            ui.painter().hline(bar.response.rect.x_range(), line, egui::Stroke::new(1.0, ternarygrid_ui::pal(ui).border));
            ui.add_space(1.0 - ui.spacing().item_spacing.y);
            let tokens = ddl_tokens(ui, cx, &self.conn, &text);
            widgets::code_lines(ui, ("ddl", &self.obj), &text, &tokens);
        }
        if retry {
            self.ddl = Loadable::NotLoaded;
        }
    }

    fn deps_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let session = self.session(cx);
        let obj = self.obj.clone();
        Self::load(&mut self.deps, cx.backend, session, move |s| async move { s.lock().await.object_dependencies(&obj).await });
        let mut retry = false;
        ui.add_space(14.0);
        if widgets::load_state(ui, &self.deps, &mut retry) {
            let deps = self.deps.get().cloned().unwrap_or_default();
            if deps.is_empty() {
                ui.label(RichText::new(tr("No dependencies.")).color(ternarygrid_ui::pal(ui).text_muted));
            }
            let rows: Vec<Vec<String>> = deps
                .iter()
                .map(|d| {
                    vec![
                        match d.direction {
                            DependencyDirection::DependsOn => tr("depends on").to_string(),
                            DependencyDirection::UsedBy => tr("used by").to_string(),
                        },
                        tr(d.target.kind.label()).to_string(),
                        d.target.display_path(),
                        d.kind.clone().unwrap_or_default(),
                    ]
                })
                .collect();
            let r = widgets::styled_table_widths(
                ui,
                "obj_deps",
                &[tr("Direction"), tr("Kind"), tr("Object"), tr("Dependency")],
                &rows,
                None,
                &[widgets::CellStyle::Chip],
                &[-130.0, -120.0, 1.4, 1.0],
            );
            if let Some(i) = r.double_clicked
                && let Some(d) = deps.get(i)
            {
                cx.push(AppCommand::OpenObject { connection: self.conn.clone(), object: d.target.clone(), page: ObjectPage::Properties });
            }
        }
        if retry {
            self.deps = Loadable::NotLoaded;
        }
    }

    fn stats_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let session = self.session(cx);
        let obj = self.obj.clone();
        Self::load(&mut self.stats, cx.backend, session, move |s| async move { s.lock().await.object_statistics(&obj).await });
        let mut retry = false;
        ui.add_space(14.0);
        if widgets::load_state(ui, &self.stats, &mut retry) {
            let stats = self.stats.get().cloned().unwrap_or_default();
            if stats.is_empty() {
                ui.label(RichText::new(tr("No statistics available.")).color(ternarygrid_ui::pal(ui).text_muted));
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                stat_tiles(ui, &stats);
                widgets::property_grid(ui, 220.0, &stats);
            });
        }
        if retry {
            self.stats = Loadable::NotLoaded;
        }
    }
}

/// Height of the object editor's header: its pages, underlined, and its buttons.
const HEADER_H: f32 = 36.0;

impl ObjectEditorTab {
    /// The header, one line: the pages, underlined, with the counts of columns and indexes;
    /// at the right Back / Forward of foreign key navigation, Refresh (the data page's is in its
    /// footer) and ⋯. The object's name
    /// is the tab's title (its full path, signature included, the tab's tooltip), its size and
    /// row estimate on the Statistics page.
    fn header_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let p = ternarygrid_ui::pal(ui);
        let frame = egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 10, top: 0, bottom: 0 });
        let header = frame.show(ui, |ui| {
            let width = ui.available_width();
            ui.allocate_ui_with_layout(egui::vec2(width, HEADER_H), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.set_min_height(HEADER_H);
                ui.spacing_mut().item_spacing.x = 2.0;
                let more = kit::icon_button_ex(ui, icon::DOTS_THREE, tr("More actions"), false, 28.0);
                egui::Popup::menu(&more).show(|ui| crate::nav::object_menu(ui, cx, &self.conn.clone(), &self.obj.clone(), false));
                // The data page has Refresh in its footer, with the other actions on the rows.
                if self.page != ObjectPage::Data && kit::icon_button_ex(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), false, 28.0).clicked() {
                    self.refresh();
                    if let (ObjectPage::Diagram, Some(d)) = (self.page, self.diagram.as_mut()) {
                        d.reload(cx);
                    }
                    cx.push(AppCommand::RefreshMetadata { connection: self.conn.clone(), object: Some(self.obj.clone()), schema: None });
                }
                // Back / forward of foreign key navigation on the data page.
                if let (ObjectPage::Data, Some(d)) = (self.page, self.data.as_mut()) {
                    d.header_ui(ui, cx);
                }
                ui.add_space(12.0);
                // The pages take the rest; in a narrow tab they scroll (wheel) under the buttons
                // instead of running into them.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    egui::ScrollArea::horizontal().id_salt("object_pages").scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                        ui.horizontal_centered(|ui| self.pages_ui(ui));
                    });
                });
            });
        });
        // The line under the pages (the selected one's underline sits on it).
        ui.painter().hline(ui.max_rect().x_range(), header.response.rect.bottom() + 0.5, egui::Stroke::new(1.0, p.border));
        // The page right under the line.
        ui.add_space(1.0 - ui.spacing().item_spacing.y);
    }

    /// The pages of the header, 20 apart as in the mockups' table data page.
    /// Columns, indexes … are the Properties page's own sub pages (with their counts), not pages
    /// of the header too: one route to each.
    fn pages_ui(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.x = 20.0;
        let mut pages: Vec<(ObjectPage, &str)> = vec![(ObjectPage::Properties, tr("Properties"))];
        if self.obj.kind.has_data() {
            pages.push((ObjectPage::Data, tr("Data")));
        }
        pages.push((ObjectPage::Ddl, tr("DDL")));
        pages.push((ObjectPage::Dependencies, tr("Dependencies")));
        if self.obj.kind.has_data() {
            pages.push((ObjectPage::Statistics, tr("Statistics")));
        }
        if self.obj.kind.is_table_like() {
            pages.push((ObjectPage::Diagram, tr("ER diagram")));
        }
        for (page, label) in pages {
            if kit::underline_tab(ui, None, label, None, self.page == page, HEADER_H).clicked() {
                self.page = page;
            }
        }
    }

    fn page_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        match self.page {
            ObjectPage::Properties => self.properties_page(ui, cx),
            ObjectPage::Data => {
                let data = self.data.get_or_insert_with(|| crate::results::data_tab::TableDataView::new(self.conn.clone(), self.obj.clone(), None));
                EmbeddedView::ui(data, ui, cx);
            }
            ObjectPage::Ddl => self.ddl_page(ui, cx),
            ObjectPage::Dependencies => self.deps_page(ui, cx),
            ObjectPage::Statistics => self.stats_page(ui, cx),
            ObjectPage::Diagram => {
                let diagram = self.diagram.get_or_insert_with(|| crate::tabs::er::ErTab::embedded(self.conn.clone(), self.obj.schema_ref(), self.obj.name.clone()));
                diagram.ui(ui, cx);
            }
        }
    }
}

impl TabView for ObjectEditorTab {
    fn navigator_object(&self) -> Option<(String, ObjectRef)> {
        Some((self.conn.clone(), self.obj.clone()))
    }

    fn title(&self) -> String {
        self.obj.name.clone()
    }

    /// The Data page's session (its transaction in manual commit mode).
    fn transaction_session(&self) -> Option<crate::conn::session::SessionHandle> {
        self.data.as_ref().and_then(|d| d.transaction_session())
    }

    fn icon(&self) -> &'static str {
        crate::nav::kind_icon(self.obj.kind)
    }

    fn tooltip(&self) -> Option<String> {
        Some(format!("{}: {}", tr(self.obj.kind.label()), self.obj.display_path()))
    }

    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }

    fn schema_context(&self) -> Option<ternarygrid_core::SchemaRef> {
        Some(ternarygrid_core::SchemaRef::new(self.obj.database.clone(), self.obj.schema.clone()))
    }

    fn dedupe_key(&self) -> Option<String> {
        Some(Self::key(&self.conn, &self.obj))
    }

    fn is_dirty(&self) -> bool {
        self.data.as_ref().is_some_and(|d| d.is_dirty())
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if !cx.conns.is_connected(&self.conn) {
            ui.vertical_centered(|ui| {
                ui.add_space(30.0);
                ui.label(tr("The connection is closed."));
                if ui.button(tr("Connect")).clicked() {
                    cx.push(AppCommand::Connect(self.conn.clone()));
                }
            });
            return;
        }
        let seen = cx.conns.live(&self.conn).map(|l| l.meta.invalidations);
        if self.invalidations.is_some() && self.invalidations != seen {
            self.refresh();
        }
        self.invalidations = seen;
        // The table's definition gives the facts and the counts of the header (on every page).
        if self.obj.kind.is_table_like() || matches!(self.obj.kind, ObjectKind::View | ObjectKind::MaterializedView) {
            let session = self.session(cx);
            let obj = self.obj.clone();
            Self::load(&mut self.table, cx.backend, session, move |s| async move { s.lock().await.table_definition(&obj).await.map(Arc::new) });
        }
        self.header_ui(ui, cx);
        // The pages other than Data, DDL and Diagram sit 16 px off the edges, as the mockups'
        // tables (each page leaves its own room under the header's line).
        let padded = !matches!(self.page, ObjectPage::Data | ObjectPage::Ddl | ObjectPage::Diagram);
        let pad = if padded { egui::Margin { left: 16, right: 16, top: 0, bottom: 8 } } else { egui::Margin::ZERO };
        egui::Frame::new().inner_margin(pad).show(ui, |ui| self.page_ui(ui, cx));
    }

    fn on_action(&mut self, action: crate::keys::Action, cx: &mut AppContext) -> bool {
        match (&mut self.data, self.page) {
            (Some(d), ObjectPage::Data) => d.on_action(action, cx),
            _ => false,
        }
    }

    fn session_state(&self) -> Option<serde_json::Value> {
        let tab = ternarygrid_config::SessionTab {
            kind: if self.page == ObjectPage::Data { ternarygrid_config::project::TabKind::DataEditor } else { ternarygrid_config::project::TabKind::ObjectEditor },
            title: Some(self.obj.name.clone()),
            connection_id: Some(self.conn.clone()),
            object: Some(self.obj.clone()),
            ..Default::default()
        };
        serde_json::to_value(tab).ok()
    }

    fn focus_context(&self) -> crate::keys::ActionContext {
        match (&self.data, self.page) {
            (Some(d), ObjectPage::Data) => d.focus_context(),
            _ => crate::keys::ActionContext::Global,
        }
    }

    crate::impl_tab_any!();
}

/// A numeric column type (`numeric(10,2)`, `real`, `int4` …), for an expression's example.
fn is_amount_type(data_type: &str) -> bool {
    let t = data_type.to_ascii_lowercase();
    ["numeric", "decimal", "real", "float", "double", "money", "int"].iter().any(|k| t.contains(k))
}

/// A table's columns without those empty in every row (from `keep` on: the first `keep` stay).
type Table<'a> = (Vec<&'a str>, Vec<Vec<String>>, Vec<widgets::CellStyle>, Vec<f32>);
fn drop_empty_columns<'a>(headers: &[&'a str], rows: Vec<Vec<String>>, styles: &[widgets::CellStyle], widths: &[f32], keep: usize) -> Table<'a> {
    let used: Vec<bool> = (0..headers.len()).map(|i| i < keep || rows.iter().any(|r| r.get(i).is_some_and(|c| !c.is_empty()))).collect();
    let pick = |i: &usize| used[*i];
    let idx: Vec<usize> = (0..headers.len()).filter(pick).collect();
    let rows = rows.into_iter().map(|r| idx.iter().map(|&i| r.get(i).cloned().unwrap_or_default()).collect()).collect();
    (
        idx.iter().map(|&i| headers[i]).collect(),
        rows,
        idx.iter().map(|&i| styles.get(i).copied().unwrap_or_default()).collect(),
        idx.iter().filter_map(|&i| widths.get(i).copied()).collect(),
    )
}

/// The SQL colours of a DDL text, worked out once per text.
fn ddl_tokens(ui: &Ui, cx: &AppContext, conn: &str, text: &str) -> std::sync::Arc<Vec<(std::ops::Range<usize>, ternarygrid_editor::TokenStyle)>> {
    type Tokens = std::sync::Arc<Vec<(std::ops::Range<usize>, ternarygrid_editor::TokenStyle)>>;
    use ternarygrid_editor::EditorLanguage as _;
    let key = egui::Id::new(("ddl_tokens", conn, text));
    if let Some(t) = ui.data(|d| d.get_temp::<Tokens>(key)) {
        return t;
    }
    let t: Tokens = std::sync::Arc::new(cx.conns.live(conn).map(|l| crate::sqllang::SqlLanguage::new(l.dialect.clone()).highlight(text)).unwrap_or_default());
    ui.data_mut(|d| d.insert_temp(key, t.clone()));
    t
}

/// A sub page of the table's properties: a pill, its count in a badge.
fn pill_tab(ui: &mut Ui, label: &str, count: Option<&str>, selected: bool) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let size = ternarygrid_ui::style::body_size(ui.ctx()) - 0.5;
    let font = if selected { ternarygrid_ui::style::medium(ui.ctx(), size) } else { egui::FontId::proportional(size) };
    let g = ui.painter().layout_no_wrap(label.to_string(), font, egui::Color32::PLACEHOLDER);
    let badge = count.map(|c| ui.painter().layout_no_wrap(c.to_string(), ternarygrid_ui::style::medium(ui.ctx(), ternarygrid_ui::style::SMALL), egui::Color32::PLACEHOLDER));
    let badge_w = badge.as_ref().map_or(0.0, |b| (b.size().x + 10.0).max(16.0) + 6.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 20.0 + badge_w, 28.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label));
    if selected {
        ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, p.subtle);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, p.hover);
    }
    let color = if selected { p.text } else { p.text_muted };
    let x = rect.left() + 10.0;
    let gw = g.size().x;
    ui.painter().galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, color);
    if let Some(b) = badge {
        let r = egui::Rect::from_min_size(egui::pos2(x + gw + 6.0, rect.center().y - 8.0), egui::vec2((b.size().x + 10.0).max(16.0), 16.0));
        let (fill, fg) = if selected { (p.accent_soft, p.accent_text) } else { (p.subtle, p.text_muted) };
        ui.painter().rect_filled(r, 8, fill);
        ui.painter().galley(r.center() - b.size() / 2.0, b, fg);
    }
    if resp.has_focus() {
        ternarygrid_ui::style::focus_halo(ui, rect, ternarygrid_ui::style::RADIUS);
    }
    resp
}

/// The figures worth a glance on top of a table's statistics, when the driver reports them.
fn stat_tiles(ui: &mut Ui, stats: &[Property]) {
    let get = |name: &str| stats.iter().find(|p| p.name.eq_ignore_ascii_case(name)).map(|p| p.value.clone()).filter(|v| !v.is_empty());
    let num = |v: String| v.parse::<i64>().map(|n| ternarygrid_grid::format::group_thousands(&n.to_string())).unwrap_or(v);
    let mut tiles: Vec<(&str, String, Option<String>)> = Vec::new();
    if let Some(v) = get("total_size") {
        tiles.push((tr("Total size"), v, None));
    }
    if let Some(v) = get("n_live_tup") {
        tiles.push((tr("Live rows"), num(v), get("n_dead_tup").map(|d| ternarygrid_core::trf("dead {}", &[&num(d)]))));
    }
    if let Some(v) = get("seq_scan") {
        tiles.push((tr("Sequential scans"), num(v), get("seq_tup_read").map(|d| ternarygrid_core::trf("rows read {}", &[&num(d)]))));
    }
    if let Some(v) = get("idx_scan") {
        tiles.push((tr("Index scans"), num(v), get("idx_tup_fetch").map(|d| ternarygrid_core::trf("rows fetched {}", &[&num(d)]))));
    }
    if tiles.is_empty() {
        return;
    }
    ui.columns(tiles.len(), |cols| {
        for (c, (label, value, sub)) in cols.iter_mut().zip(tiles) {
            ternarygrid_ui::parts::stat_tile(c, label, &value, sub.as_deref());
        }
    });
    ui.add_space(8.0);
}
