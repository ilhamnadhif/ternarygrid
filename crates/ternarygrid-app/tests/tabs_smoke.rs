//! Smoke test: every tab kind is drawn for a number of headless egui frames
//! against an embedded SQLite database (always available) and must not panic.

use std::sync::Arc;
use std::time::{Duration, Instant};
use ternarygrid_app::backend::Backend;
use ternarygrid_app::command::AppCommand;
use ternarygrid_app::conn::{ConnectionManager, Credentials, DirectNet, MemorySecrets};
use ternarygrid_app::cx::AppContext;
use ternarygrid_app::keys::Keymap;
use ternarygrid_app::ops::Operations;
use ternarygrid_app::services::{Services, Settings};
use ternarygrid_app::tabs::TabView;
use ternarygrid_app::toasts::Toasts;
use ternarygrid_core::meta::{ObjectKind, ObjectRef, SchemaRef};

struct H {
    backend: Arc<Backend>,
    ctx: egui::Context,
    cm: ConnectionManager,
    services: Services,
    settings: Settings,
    keymap: Keymap,
    toasts: Toasts,
    ops: Operations,
    commands: Vec<AppCommand>,
    /// The commands the tabs sent, in order.
    sent: Vec<AppCommand>,
    time: f64,
    id: String,
    dir: std::path::PathBuf,
}

impl Drop for H {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl H {
    fn new() -> H {
        let dir = std::env::temp_dir().join(format!("ternarygrid-smoke-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("smoke.db");
        let backend = Arc::new(Backend::new().unwrap());
        let ctx = egui::Context::default();
        ternarygrid_app::theme::setup_fonts(&ctx);
        backend.set_context(ctx.clone());
        let mut cfg = ternarygrid_core::config::ConnectionConfig::new(ternarygrid_core::DbKind::Sqlite, "smoke");
        cfg.file_path = Some(db.display().to_string());
        cfg.auth = ternarygrid_core::config::AuthMethod::None;
        let mut cm = ConnectionManager::new(ternarygrid_app::driver_registry(), Arc::new(MemorySecrets::default()), Arc::new(DirectNet));
        let id = cfg.id.clone();
        cm.upsert(cfg);
        cm.connect(&backend, &id, Credentials::default()).unwrap();
        let t0 = Instant::now();
        while !cm.is_connected(&id) {
            assert!(cm.errors().is_empty(), "{:?}", cm.errors());
            assert!(t0.elapsed() < Duration::from_secs(10));
            cm.poll(&backend);
            std::thread::sleep(Duration::from_millis(5));
        }
        let s = cm.live(&id).unwrap().main.clone();
        backend
            .block_on(async move {
                s.execute_update("CREATE TABLE customer (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT)", &[]).await?;
                s.execute_update("CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customer(id), total REAL)", &[]).await?;
                s.execute_update("INSERT INTO customer VALUES (1, 'Ani', 'a@x'), (2, 'Budi', NULL)", &[]).await?;
                s.execute_update("INSERT INTO orders VALUES (1, 1, 10.5), (2, 2, 3.25)", &[]).await?;
                s.execute_update("CREATE VIEW big AS SELECT * FROM orders WHERE total > 5", &[]).await
            })
            .unwrap();
        let services = Services::ephemeral(&dir.join("home"));
        let settings = Settings::default();
        let (keymap, _) = Keymap::from_config(&settings.keymap, ternarygrid_app::keys::IS_MAC);
        H { backend, ctx, cm, services, settings, keymap, toasts: Toasts::default(), ops: Operations::default(), commands: Vec::new(), sent: Vec::new(), time: 0.0, id, dir }
    }

    fn frames(&mut self, tab: &mut dyn TabView, n: usize) {
        for _ in 0..n {
            self.time += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))),
                time: Some(self.time),
                focused: true,
                ..Default::default()
            };
            let ctx = self.ctx.clone();
            let mut out = ctx.run_ui(input, |ui| {
                let mut cx = AppContext {
                    backend: &self.backend,
                    conns: &mut self.cm,
                    services: &self.services,
                    settings: &mut self.settings,
                    commands: &mut self.commands,
                    toasts: &mut self.toasts,
                    ops: &mut self.ops,
                    keymap: &self.keymap,
                    tab_active: true,
                    current_tab: Some(1),
                    popup: false,
                };
                egui::CentralPanel::default().show(ui, |ui| tab.ui(ui, &mut cx));
            });
            out.textures_delta.clear();
            let _ = self.ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            self.cm.poll(&self.backend);
            std::thread::sleep(Duration::from_millis(3));
        }
        self.sent.append(&mut self.commands);
    }

    /// Start `tab`'s operation and run frames until it is done.
    fn run_maintenance(&mut self, tab: &mut ternarygrid_app::tabs::dbtools::MaintenanceTab) {
        {
            let mut cx = AppContext {
                backend: &self.backend,
                conns: &mut self.cm,
                services: &self.services,
                settings: &mut self.settings,
                commands: &mut self.commands,
                toasts: &mut self.toasts,
                ops: &mut self.ops,
                keymap: &self.keymap,
                tab_active: true,
                current_tab: Some(1),
                popup: false,
            };
            tab.start(&mut cx);
        }
        let t0 = Instant::now();
        while tab.is_running() {
            assert!(t0.elapsed() < Duration::from_secs(10), "{}", tab.log_text());
            self.frames(tab, 1);
        }
        assert!(tab.error().is_none() && tab.results().iter().all(|r| r.error.is_none()), "{}", tab.log_text());
    }

    /// The scopes of the RowsChanged commands sent so far.
    fn rows_changed(&self) -> Vec<SchemaRef> {
        self.sent.iter().filter_map(|c| if let AppCommand::RowsChanged { connection, scope, .. } = c { (*connection == self.id).then(|| scope.clone()) } else { None }).collect()
    }

    /// The row estimate the navigator's Tables folder of `main` shows for `table`.
    fn listed_rows(&self, table: &str) -> Option<i64> {
        let live = self.cm.live(&self.id).unwrap();
        match live.meta.objects.get(&(SchemaRef::schema("main"), ObjectKind::Table)) {
            Some(ternarygrid_app::backend::Loadable::Loaded(v)) => v.iter().find(|o| o.obj.name == table).and_then(|o| o.row_estimate),
            other => panic!("the Tables folder is not shown loaded: loading = {}", other.is_some_and(|l| l.is_loading())),
        }
    }
}

/// Truncate… in the navigator's menu: the maintenance dialog opened on it empties the table.
#[test]
fn truncate_empties_the_table() {
    let mut h = H::new();
    let orders = ObjectRef::table(Some("main".into()), "orders");
    let mut tab = ternarygrid_app::tabs::dbtools::MaintenanceTab::new(h.id.clone(), vec![orders]).with_op(Some("truncate".into()));
    h.frames(&mut tab, 3);
    assert_eq!(tab.title(), "Truncate: orders");
    {
        let cx = AppContext {
            backend: &h.backend,
            conns: &mut h.cm,
            services: &h.services,
            settings: &mut h.settings,
            commands: &mut h.commands,
            toasts: &mut h.toasts,
            ops: &mut h.ops,
            keymap: &h.keymap,
            tab_active: true,
            current_tab: Some(1),
            popup: false,
        };
        assert!(tab.available_ops(&cx).iter().any(|o| o == "truncate"), "{:?}", tab.available_ops(&cx));
        assert_eq!(tab.preview_sql(&cx), ["DELETE FROM main.orders"]);
    }
    h.run_maintenance(&mut tab);
    let s = h.cm.live(&h.id).unwrap().main.clone();
    let (_, rows) = h.backend.block_on(async move { s.query_all("SELECT COUNT(*) FROM orders", &[]).await }).unwrap();
    assert_eq!(rows[0][0].as_i64(), Some(0));
    // The navigator is told: the table's size there is read again (it stayed until a Refresh).
    assert_eq!(h.rows_changed(), [SchemaRef::schema("main")]);
}

/// After a maintenance run the navigator's row counts and sizes are read again without a
/// Refresh, and the old ones stay shown meanwhile (no "Loading…" over a large schema).
#[test]
fn maintenance_reloads_the_navigator_sizes_in_place() {
    let mut h = H::new();
    let s = h.cm.live(&h.id).unwrap().main.clone();
    h.backend.block_on(async move { s.execute_update("ANALYZE", &[]).await }).unwrap();
    // The navigator's Tables folder, loaded as when it is unfolded.
    let main = SchemaRef::schema("main");
    {
        let live = h.cm.live_mut(&h.id).unwrap();
        let s = live.main.clone();
        live.meta.request_objects(&h.backend, &s, &main, ObjectKind::Table);
    }
    let t0 = Instant::now();
    while !h.cm.live(&h.id).unwrap().meta.objects[&(main.clone(), ObjectKind::Table)].get().is_some() {
        assert!(t0.elapsed() < Duration::from_secs(10));
        h.cm.poll(&h.backend);
        std::thread::sleep(Duration::from_millis(3));
    }
    assert_eq!(h.listed_rows("orders"), Some(2));
    // More rows, then Analyze in the maintenance window.
    let s = h.cm.live(&h.id).unwrap().main.clone();
    h.backend.block_on(async move { s.execute_update("INSERT INTO orders VALUES (3, 1, 1.0), (4, 2, 2.0), (5, 1, 3.0)", &[]).await }).unwrap();
    let orders = ObjectRef::table(Some("main".into()), "orders");
    let mut tab = ternarygrid_app::tabs::dbtools::MaintenanceTab::new(h.id.clone(), vec![orders]).with_op(Some("analyze".into()));
    h.frames(&mut tab, 3);
    h.run_maintenance(&mut tab);
    let scopes = h.rows_changed();
    assert_eq!(scopes, std::slice::from_ref(&main));
    // As the application does with the command.
    h.cm.live_mut(&h.id).unwrap().reload_sizes(&h.backend, &scopes[0]);
    assert_eq!(h.listed_rows("orders"), Some(2), "the old count stays shown while reading");
    let t0 = Instant::now();
    while h.listed_rows("orders") != Some(5) {
        assert!(t0.elapsed() < Duration::from_secs(10), "the navigator still shows {:?}", h.listed_rows("orders"));
        h.cm.poll(&h.backend);
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// The connection's Read-only flag stops Truncate (the driver would not stop a DELETE).
#[test]
fn truncate_is_refused_on_a_read_only_connection() {
    let mut h = H::new();
    let mut cfg = h.cm.config(&h.id).unwrap().clone();
    cfg.read_only = true;
    h.cm.upsert(cfg);
    let orders = ObjectRef::table(Some("main".into()), "orders");
    let mut tab = ternarygrid_app::tabs::dbtools::MaintenanceTab::new(h.id.clone(), vec![orders]).with_op(Some("truncate".into()));
    h.frames(&mut tab, 3);
    {
        let mut cx = AppContext {
            backend: &h.backend,
            conns: &mut h.cm,
            services: &h.services,
            settings: &mut h.settings,
            commands: &mut h.commands,
            toasts: &mut h.toasts,
            ops: &mut h.ops,
            keymap: &h.keymap,
            tab_active: true,
            current_tab: Some(1),
            popup: false,
        };
        tab.start(&mut cx);
    }
    assert!(!tab.is_running());
    assert!(tab.error().is_some_and(|e| e.contains("read-only")), "{:?}", tab.error());
    h.frames(&mut tab, 3);
    let s = h.cm.live(&h.id).unwrap().main.clone();
    let (_, rows) = h.backend.block_on(async move { s.query_all("SELECT COUNT(*) FROM orders", &[]).await }).unwrap();
    assert_eq!(rows[0][0].as_i64(), Some(2));
}

#[test]
fn every_tab_draws_without_panicking() {
    let mut h = H::new();
    let conn = h.id.clone();
    let schema = SchemaRef::new(None, Some("main".into()));
    let customer = ObjectRef::table(Some("main".into()), "customer");
    let mut tabs: Vec<(&str, Box<dyn TabView>)> = vec![
        ("settings", Box::new(ternarygrid_app::tabs::settings::SettingsTab::default())),
        ("history", Box::new(ternarygrid_app::tabs::history::HistoryTab::default())),
        ("editor", Box::new(ternarygrid_app::tabs::sql_editor::SqlEditorTab::new(Some(conn.clone()), None, "select * from customer;".into(), None, false, "smoke".into()))),
        ("console", Box::new(ternarygrid_app::tabs::sql_editor::SqlEditorTab::new(Some(conn.clone()), None, String::new(), None, true, "console".into()))),
        ("object", Box::new(ternarygrid_app::tabs::object::ObjectEditorTab::new(conn.clone(), customer.clone(), ternarygrid_app::command::ObjectPage::Properties))),
        ("object-data", Box::new(ternarygrid_app::tabs::object::ObjectEditorTab::new(conn.clone(), customer.clone(), ternarygrid_app::command::ObjectPage::Data))),
        ("object-ddl", Box::new(ternarygrid_app::tabs::object::ObjectEditorTab::new(conn.clone(), customer.clone(), ternarygrid_app::command::ObjectPage::Ddl))),
        (
            "view",
            Box::new(ternarygrid_app::tabs::object::ObjectEditorTab::new(
                conn.clone(),
                ObjectRef::new(ObjectKind::View, Some("main".into()), "big"),
                ternarygrid_app::command::ObjectPage::Ddl,
            )),
        ),
        ("data", Box::new(ternarygrid_app::results::data_tab::TableDataTab::new(conn.clone(), customer.clone(), Some("id > 0".into())))),
        ("er", Box::new(ternarygrid_app::tabs::er::ErTab::new(conn.clone(), schema.clone(), Vec::new()))),
        ("query-builder", Box::new(ternarygrid_app::tabs::query_builder::QueryBuilderTab::new(conn.clone(), Some(schema.clone())))),
        ("schema-compare", Box::new(ternarygrid_app::tabs::compare::SchemaCompareTab::new(Some(conn.clone()), Some(schema.clone())))),
        ("sessions", Box::new(ternarygrid_app::tabs::admin::SessionManagerTab::new(conn.clone()))),
        ("locks", Box::new(ternarygrid_app::tabs::admin::LockManagerTab::new(conn.clone()))),
        ("dashboard", Box::new(ternarygrid_app::tabs::admin::DashboardTab::new(conn.clone()))),
        ("export", Box::new(ternarygrid_app::tabs::transfer::ExportTab::new(conn.clone(), vec![customer.clone()], None))),
        ("import", Box::new(ternarygrid_app::tabs::transfer::ImportTab::new(conn.clone(), Some(customer.clone()), Some(schema.clone())))),
        ("migration", Box::new(ternarygrid_app::tabs::transfer::MigrationTab::new(conn.clone(), vec![customer.clone()]))),
        ("mock", Box::new(ternarygrid_app::tabs::transfer::MockDataTab::new(conn.clone(), customer.clone()))),
        ("tasks", Box::new(ternarygrid_app::tabs::tasks::TasksTab::new())),
        ("backup", Box::new(ternarygrid_app::tabs::dbtools::BackupTab::new(conn.clone(), Some(schema.clone()), false))),
        ("maintenance", Box::new(ternarygrid_app::tabs::dbtools::MaintenanceTab::new(conn.clone(), vec![customer.clone()]))),
        ("password", Box::new(ternarygrid_app::tabs::dbtools::ChangePasswordTab::new(conn.clone()))),
        ("ai", Box::new(ternarygrid_app::tabs::ai::AiAssistantTab::new(Some(conn.clone())))),
        ("data-compare", Box::new(ternarygrid_app::tabs::data_compare::DataCompareTab::new(Some(conn.clone()), Some(customer.clone())))),
        ("designer-new", Box::new(ternarygrid_app::tabs::table_designer::TableDesignerTab::create(conn.clone(), schema.clone()))),
        ("designer-edit", Box::new(ternarygrid_app::tabs::table_designer::TableDesignerTab::edit(conn.clone(), customer.clone()))),
        ("search", Box::new(ternarygrid_app::tabs::search::SearchTab::new(Some(conn.clone()), false))),
        ("search-data", Box::new(ternarygrid_app::tabs::search::SearchTab::new(Some(conn.clone()), true))),
    ];
    for (name, tab) in tabs.iter_mut() {
        let t0 = Instant::now();
        h.frames(tab.as_mut(), 40);
        assert!(!tab.title().is_empty(), "{name}: title");
        eprintln!("{name}: {:?}", t0.elapsed());
    }
    // The settings tab in every section.
    let mut settings = ternarygrid_app::tabs::settings::SettingsTab::default();
    for _ in 0..12 {
        h.frames(&mut settings, 3);
        settings.next_section_for_tests();
    }
}

#[test]
fn connection_dialog_fits_the_screen() {
    let mut h = H::new();
    let mut dialogs = ternarygrid_app::dialogs::Dialogs::default();
    dialogs.new_connection(Some(ternarygrid_core::DbKind::Postgres), None, &h.cm, Default::default());
    let screen = egui::vec2(1280.0, 760.0);
    for _ in 0..6 {
        h.time += 1.0 / 60.0;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)), time: Some(h.time), ..Default::default() };
        let ctx = h.ctx.clone();
        let mut out = ctx.run_ui(input, |ui| {
            let mut cx = AppContext {
                backend: &h.backend,
                conns: &mut h.cm,
                services: &h.services,
                settings: &mut h.settings,
                commands: &mut h.commands,
                toasts: &mut h.toasts,
                ops: &mut h.ops,
                keymap: &h.keymap,
                tab_active: true,
                current_tab: None,
                popup: false,
            };
            dialogs.show(ui.ctx(), &mut cx);
        });
        out.textures_delta.clear();
    }
    let layer = h.ctx.memory(|m| m.areas().top_layer_id(egui::Order::Middle)).expect("dialog drawn");
    let rect = h.ctx.memory(|m| m.area_rect(layer.id)).expect("dialog rect");
    assert!(rect.height() < screen.y - 40.0 && rect.top() >= 0.0 && rect.bottom() <= screen.y, "dialog {rect:?} does not fit {screen:?}");
}

/// A table emptied by Truncate…: its open data tab reads again (no rows that are gone are
/// shown), the data tab of another table does not.
#[test]
fn an_emptied_tables_data_tab_reads_again() {
    use ternarygrid_app::tabs::object::ObjectEditorTab;
    let mut h = H::new();
    let orders = ObjectRef::table(Some("main".into()), "orders");
    let customer = ObjectRef::table(Some("main".into()), "customer");
    let mut tab = ObjectEditorTab::new(h.id.clone(), orders.clone(), ternarygrid_app::command::ObjectPage::Data);
    let mut other = ObjectEditorTab::new(h.id.clone(), customer.clone(), ternarygrid_app::command::ObjectPage::Data);
    let rows = |t: &ObjectEditorTab| t.data.as_ref().and_then(|d| d.panel.as_ref()).filter(|p| !p.src.is_fetching()).map(|p| p.src.row_count());
    let t0 = Instant::now();
    while rows(&tab) != Some(2) || rows(&other) != Some(2) {
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?} {:?}", rows(&tab), rows(&other));
        h.frames(&mut tab, 1);
        h.frames(&mut other, 1);
    }
    let mut m = ternarygrid_app::tabs::dbtools::MaintenanceTab::new(h.id.clone(), vec![orders.clone()]).with_op(Some("truncate".into()));
    h.frames(&mut m, 3);
    h.run_maintenance(&mut m);
    // As the application does with the command.
    let sent = std::mem::take(&mut h.sent);
    let mut emptied_tables = Vec::new();
    for c in sent {
        if let AppCommand::RowsChanged { scope, emptied, .. } = c {
            let live = h.cm.live_mut(&h.id).unwrap();
            live.reload_sizes(&h.backend, &scope);
            live.note_emptied(&emptied);
            emptied_tables.extend(emptied);
        }
    }
    assert_eq!(emptied_tables, std::slice::from_ref(&orders));
    let t0 = Instant::now();
    while rows(&tab) != Some(0) {
        assert!(t0.elapsed() < Duration::from_secs(10), "the emptied table's tab still shows {:?} rows", rows(&tab));
        h.frames(&mut tab, 1);
    }
    h.frames(&mut other, 20);
    assert_eq!(rows(&other), Some(2));

    // Names compare as the server folds unquoted ones; one written without a schema
    // (TRUNCATE orders in an editor) is the table in any schema.
    let live = h.cm.live_mut(&h.id).unwrap();
    let seen = live.emptied_seq();
    assert!(!live.emptied_since(seen, &orders));
    live.note_emptied(&[ObjectRef::table(None, "ORDERS")]);
    assert!(live.emptied_since(seen, &orders));
    assert!(!live.emptied_since(seen, &customer));
    assert!(!live.emptied_since(seen, &ObjectRef::table(Some("aux".into()), "customer")));
    live.note_emptied(&[ObjectRef::table(Some("aux".into()), "customer")]);
    assert!(!live.emptied_since(seen, &customer), "another schema's table of that name");
}
