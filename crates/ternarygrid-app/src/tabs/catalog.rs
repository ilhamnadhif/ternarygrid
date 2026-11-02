//! Object editor for catalog objects (roles, tablespaces, users, foreign
//! servers, policies …): their properties, DDL and sub-objects (e.g. the
//! members of a role).

use crate::backend::Loadable;
use crate::command::AppCommand;
use crate::cx::AppContext;
use crate::tabs::TabView;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use ternarygrid_core::catalog::{self, CatalogContext, CatalogFolder, CatalogRow};
use ternarygrid_core::{SchemaRef, tr};

pub struct CatalogObjectTab {
    conn: String,
    folder_id: &'static str,
    ctx: CatalogContext,
    folder: Option<CatalogFolder>,
    row: Loadable<Option<CatalogRow>>,
    ddl: Loadable<Option<String>>,
    children: Vec<(CatalogFolder, Loadable<Vec<CatalogRow>>)>,
    started: bool,
}

impl CatalogObjectTab {
    pub fn new(conn: String, folder: &'static str, ctx: CatalogContext) -> Self {
        CatalogObjectTab { conn, folder_id: folder, ctx, folder: None, row: Loadable::NotLoaded, ddl: Loadable::NotLoaded, children: Vec::new(), started: false }
    }

    pub fn key(conn: &str, folder: &str, ctx: &CatalogContext) -> String {
        format!("catalog:{conn}:{folder}:{:?}:{:?}:{:?}:{:?}:{:?}", ctx.database, ctx.schema, ctx.table, ctx.parent, ctx.name)
    }

    /// The loaded DDL.
    pub fn ddl_text(&self) -> Option<&str> {
        self.ddl.get().and_then(|d| d.as_deref())
    }

    /// The loaded properties: (column, value as text).
    pub fn properties(&self) -> Vec<(String, String)> {
        match &self.row {
            Loadable::Loaded(Some(r)) => r.values.iter().map(|(k, v)| (k.clone(), v.to_text().into_owned())).collect(),
            _ => Vec::new(),
        }
    }

    fn name(&self) -> &str {
        self.ctx.name.as_deref().unwrap_or("")
    }

    fn start(&mut self, cx: &mut AppContext) {
        let Some(live) = cx.conns.live_mut(&self.conn) else { return };
        let Some(folder) = live.catalog_folder(self.folder_id).cloned() else { return };
        let Some(session) = live.ensure_session(cx.backend, self.ctx.database.as_deref()) else { return };
        self.started = true;
        let (s, f, c, name) = (session.clone(), folder.clone(), self.ctx.clone(), self.name().to_string());
        let list_ctx = CatalogContext { name: None, ..c.clone() };
        self.row = Loadable::Loading(cx.backend.spawn(async move {
            let rows = catalog::list(s.lock().await.as_mut(), &f, &list_ctx).await?;
            Ok(rows.into_iter().find(|r| r.name == name))
        }));
        let (s, f, c) = (session.clone(), folder.clone(), self.ctx.clone());
        self.ddl = Loadable::Loading(cx.backend.spawn(async move { catalog::ddl(s.lock().await.as_mut(), &f, &c).await }));
        self.children = folder
            .children
            .iter()
            .filter_map(|id| live.catalog_folder(id).cloned())
            .map(|child| {
                let (s, f) = (session.clone(), child.clone());
                let c = CatalogContext { parent: self.ctx.name.clone(), name: None, ..self.ctx.clone() };
                let job = cx.backend.spawn(async move { catalog::list(s.lock().await.as_mut(), &f, &c).await });
                (child, Loadable::Loading(job))
            })
            .collect();
        self.folder = Some(folder);
    }

    fn poll(&mut self) -> bool {
        let mut busy = false;
        self.row.poll();
        self.ddl.poll();
        busy |= self.row.is_loading() || self.ddl.is_loading();
        for (_, l) in &mut self.children {
            l.poll();
            busy |= l.is_loading();
        }
        busy
    }
}

impl TabView for CatalogObjectTab {
    fn title(&self) -> String {
        self.name().to_string()
    }

    fn icon(&self) -> &'static str {
        self.folder.as_ref().map(|f| crate::nav::catalog_icon(f.icon)).unwrap_or(icon::CUBE)
    }

    fn tooltip(&self) -> Option<String> {
        self.folder.as_ref().map(|f| format!("{} › {}", tr(f.label), self.name()))
    }

    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }

    fn dedupe_key(&self) -> Option<String> {
        Some(Self::key(&self.conn, self.folder_id, &self.ctx))
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if !self.started {
            if !cx.conns.is_connected(&self.conn) {
                ui.weak(tr("not connected"));
                return;
            }
            self.start(cx);
        }
        if self.poll() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        let label = self.folder.as_ref().map(|f| tr(f.label)).unwrap_or_default();
        let name = self.name();
        let mut refresh = false;
        ternarygrid_ui::parts::page_header(ui, self.icon(), &[], name, Some(label), |ui| {
            refresh = ternarygrid_ui::kit::icon_button(ui, icon::ARROW_CLOCKWISE, tr("Refresh")).clicked();
        });
        if refresh {
            self.started = false;
        }
        ternarygrid_ui::parts::hline(ui);
        // 14 / 16 around; the properties, members and DDL at most 760 wide.
        let pal = ternarygrid_ui::pal(ui);
        let title = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).font(ternarygrid_ui::style::semibold(ui.ctx(), 13.0)).color(pal.text));
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 14)).show(ui, |ui| {
                ui.set_max_width(760.0);
                ui.spacing_mut().item_spacing.y = 12.0;
                title(ui, tr("Properties"));
                match &self.row {
                    Loadable::Loaded(Some(row)) => {
                        let props: Vec<ternarygrid_core::meta::Property> = row
                            .values
                            .iter()
                            .filter(|(k, _)| !k.eq_ignore_ascii_case("detail") || row.values.len() <= 2)
                            .map(|(k, v)| ternarygrid_core::meta::Property {
                                name: k.replace('_', " "),
                                value: if v.is_null() { "NULL".to_string() } else { v.to_text().into_owned() },
                                group: None,
                            })
                            .collect();
                        ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            crate::widgets::property_grid(ui, ("catalog_props", self.folder_id), &props);
                        });
                    }
                    Loadable::Loaded(None) => {
                        ui.weak(tr("The object no longer exists."));
                    }
                    Loadable::Failed(e) => {
                        crate::widgets::error_box(ui, &e.full_text());
                    }
                    _ => {
                        ternarygrid_ui::parts::spinner_label(ui, tr("Loading…"));
                    }
                }
                for (folder, items) in &self.children {
                    title(ui, tr(folder.label));
                    match items {
                        Loadable::Loaded(v) if v.is_empty() => {
                            ui.label(RichText::new(tr("(empty)")).color(pal.text_faint));
                        }
                        Loadable::Loaded(v) => {
                            ui.scope(|ui| {
                                ui.spacing_mut().item_spacing.y = 6.0;
                                for it in v {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 8.0;
                                        ui.label(RichText::new(crate::nav::catalog_icon(folder.icon)).font(ternarygrid_ui::style::icon_font(14.0)).color(pal.text_muted));
                                        ui.label(RichText::new(&it.name).color(pal.text));
                                        if let Some(d) = &it.detail {
                                            ui.label(RichText::new(d).size(12.0).color(pal.text_faint));
                                        }
                                    });
                                }
                            });
                        }
                        Loadable::Failed(e) => {
                            crate::widgets::error_box(ui, &e.full_text());
                        }
                        _ => {
                            ternarygrid_ui::parts::spinner_label(ui, tr("Loading…"));
                        }
                    }
                }
                match &self.ddl {
                    Loadable::Loaded(Some(ddl)) => {
                        ui.horizontal(|ui| {
                            title(ui, "DDL");
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                use ternarygrid_ui::kit;
                                if kit::button_full(ui, kit::Kind::Ghost, Some(icon::FILE_SQL), tr("Open in SQL editor"), true, false).clicked() {
                                    let schema = self.ctx.database.clone().map(|d| SchemaRef { database: Some(d), schema: None });
                                    cx.push(AppCommand::NewSqlEditor { connection: Some(self.conn.clone()), schema, text: Some(ddl.clone()) });
                                }
                                if kit::button_full(ui, kit::Kind::Ghost, Some(icon::COPY), tr("Copy"), true, false).clicked() {
                                    ui.ctx().copy_text(ddl.clone());
                                }
                            });
                        });
                        let tokens = cx
                            .conns
                            .live(&self.conn)
                            .map(|l| ternarygrid_editor::EditorLanguage::highlight(&crate::sqllang::SqlLanguage::new(l.dialect.clone()), ddl))
                            .unwrap_or_default();
                        crate::widgets::code_block(ui, ("catalog_ddl", &self.conn, self.folder_id), ddl, &tokens, 400.0);
                    }
                    Loadable::Failed(e) => {
                        title(ui, "DDL");
                        crate::widgets::error_box(ui, &e.full_text());
                    }
                    _ => {}
                }
            });
        });
    }

    crate::impl_tab_any!();
}
