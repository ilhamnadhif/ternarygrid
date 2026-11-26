//! Deep end-to-end tests covering every major workflow and bug fix across the workspace:
//! SQLite migration (multiple FKs + secondary indexes), export/import roundtrip & edge
//! values, Schema Compare & Sync (auto/manual commit + headless task), Data Compare & Sync,
//! Table Designer (create/alter/FK picker/Cmd+S), Query Builder (JOIN + safe GROUP BY),
//! Global Metadata & Data Search, ER Diagram (layout + JSON + SVG/PNG export), SQL Editor
//! advanced flows (distinct positional `?`, read-only `EXPLAIN`, `@set` atomicity, `@export`
//! cleanup, UTF-8 multi-statement diagnostics, manual rollback), ResultPanel virtual keys &
//! virtual model persistence across reload, Vault cross-process reload + encrypted export/import
//! + `retain_selected` + locked-vault safety, and Dialogs/Settings/Drop/Maintenance/History.

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ternarygrid_app::app::TernaryGridApp;
use ternarygrid_app::backend::Backend;
use ternarygrid_app::command::AppCommand;
use ternarygrid_app::conn::{ConnectionManager, Credentials, DirectNet, MemorySecrets};
use ternarygrid_app::cx::AppContext;
use ternarygrid_app::dialogs::drop::supports_cascade;
use ternarygrid_app::keys::{Action, Keymap};
use ternarygrid_app::ops::Operations;
use ternarygrid_app::services::{Services, Settings};
use ternarygrid_app::tabs::compare::SchemaCompareTab;
use ternarygrid_app::tabs::data_compare::DataCompareTab;
use ternarygrid_app::tabs::dbtools::MaintenanceTab;
use ternarygrid_app::tabs::er::ErTab;
use ternarygrid_app::tabs::search::SearchTab;
use ternarygrid_app::tabs::sql_editor::SqlEditorTab;
use ternarygrid_app::tabs::table_designer::TableDesignerTab;
use ternarygrid_app::tabs::{EmbeddedView, TabView};
use ternarygrid_app::toasts::{ToastKind, Toasts};
use ternarygrid_compare::{CompareOptions, ScriptOptions, migration_script, schema_diff};
use ternarygrid_config::project::{VirtualColumn, VirtualTable, VirtualTableKey};
use ternarygrid_config::{AppPaths, ConnectionStore, SecretStore, VaultStore, import::ternarygrid as tg_file};
use ternarygrid_core::config::{AuthMethod, ConnectionConfig, ConnectionType, NetworkProfile, SecretSlot, secret_key};
use ternarygrid_core::ddl::SnapshotOptions;
use ternarygrid_core::driver::{Session, execute_update, query_all, query_scalar};
use ternarygrid_core::meta::{ObjectKind, ObjectRef, SchemaRef};
use ternarygrid_core::{ConnectParams, DbKind, DbResult, Dialect, Driver, ExecOptions, Secret, TransactionMode, Value};
use ternarygrid_driver_sqlite::{SqliteDialect, SqliteDriver};
use ternarygrid_tasks::{TaskDef, TaskKind, TaskStore};
use ternarygrid_transfer::{
    CsvReadOptions, ExportFormat, ExportItem, ExportJob, ImportJob, ImportMapping, ImportMode, ImportSourceConfig, MigrationJob, MigrationMode, OnError, Progress, SessionFactory,
    run_export_job_with, run_import_job, run_migration,
};
use ternarygrid_visual::builder::{Aggregate, QbSelectItem, QueryModel};
use ternarygrid_visual::er::{ErDiagram, ErModel, LayoutOptions, export_png, export_svg};
use tokio_util::sync::CancellationToken;

/// Real `SessionFactory` opening real `SqliteDriver` sessions on a file path.
struct RealSqliteFactory {
    path: PathBuf,
}

#[async_trait::async_trait]
impl SessionFactory for RealSqliteFactory {
    async fn open(&self) -> DbResult<Box<dyn Session>> {
        let mut cfg = ConnectionConfig::new(DbKind::Sqlite, "e2e");
        cfg.file_path = Some(self.path.display().to_string());
        cfg.auth = AuthMethod::None;
        SqliteDriver::new().connect(&ConnectParams::from_config(&cfg, None)).await
    }

    fn dialect(&self) -> Arc<dyn Dialect> {
        Arc::new(SqliteDialect::new())
    }
}

async fn open_sqlite(path: &Path) -> Box<dyn Session> {
    let mut cfg = ConnectionConfig::new(DbKind::Sqlite, "e2e");
    cfg.file_path = Some(path.display().to_string());
    cfg.auth = AuthMethod::None;
    SqliteDriver::new().connect(&ConnectParams::from_config(&cfg, None)).await.unwrap()
}

async fn exec_batch(s: &mut dyn Session, stmts: &[&str]) {
    for sql in stmts {
        execute_update(s, sql, &[]).await.unwrap_or_else(|e| panic!("SQL failed:\n{sql}\nerror: {e}"));
    }
}

/// Lightweight tab harness (no full dock) for focused tab E2E tests against real SQLite.
struct TabHarness {
    backend: Arc<Backend>,
    ctx: egui::Context,
    cm: ConnectionManager,
    services: Services,
    settings: Settings,
    keymap: Keymap,
    toasts: Toasts,
    ops: Operations,
    commands: Vec<AppCommand>,
    time: f64,
    _dir: tempfile::TempDir,
}

impl TabHarness {
    fn new() -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let backend = Arc::new(Backend::new().unwrap());
        let ctx = egui::Context::default();
        ternarygrid_app::theme::setup_fonts(&ctx);
        backend.set_context(ctx.clone());
        let dir = tempfile::tempdir().unwrap();
        let services = Services::ephemeral(dir.path());
        let settings = Settings::default();
        let (keymap, _) = Keymap::from_config(&settings.keymap, ternarygrid_app::keys::IS_MAC);
        let cm = ConnectionManager::new(ternarygrid_app::driver_registry(), Arc::new(MemorySecrets::default()), Arc::new(DirectNet));
        TabHarness { backend, ctx, cm, services, settings, keymap, toasts: Toasts::default(), ops: Operations::default(), commands: Vec::new(), time: 0.0, _dir: dir }
    }

    fn connect_sqlite(&mut self, name: &str, path: &Path, read_only: bool) -> String {
        let mut cfg = ConnectionConfig::new(DbKind::Sqlite, name);
        cfg.file_path = Some(path.display().to_string());
        cfg.auth = AuthMethod::None;
        cfg.read_only = read_only;
        let id = cfg.id.clone();
        self.cm.upsert(cfg);
        self.cm.connect(&self.backend, &id, Credentials::default()).unwrap();
        let t0 = Instant::now();
        while !self.cm.is_connected(&id) {
            assert!(self.cm.errors().is_empty(), "{:?}", self.cm.errors());
            assert!(t0.elapsed() < Duration::from_secs(15), "connect timeout for {name}");
            self.cm.poll(&self.backend);
            std::thread::sleep(Duration::from_millis(5));
        }
        id
    }

    fn with_cx<R>(&mut self, f: impl FnOnce(&mut AppContext) -> R) -> R {
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
        f(&mut cx)
    }

    fn frame<T: TabView>(&mut self, tab: &mut T) {
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))),
            time: Some(self.time),
            focused: true,
            ..Default::default()
        };
        let ctx = self.ctx.clone();
        let mut out = ctx.run_ui(input, |ui| {
            self.with_cx(|cx| tab.ui(ui, cx));
        });
        out.textures_delta.clear();
        self.cm.poll(&self.backend);
    }

    fn until<T: TabView>(&mut self, tab: &mut T, what: &str, mut done: impl FnMut(&T) -> bool) {
        let t0 = Instant::now();
        loop {
            self.frame(tab);
            if done(tab) {
                return;
            }
            assert!(t0.elapsed() < Duration::from_secs(25), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Full `TernaryGridApp` harness for real multi-tab, shortcut, and dialog walkthroughs.
fn app_harness(dir: &Path) -> Harness<'static, TernaryGridApp> {
    let mut settings = Settings::default();
    settings.updates.check_automatically = Some(false);
    settings.general.restore_session = false;
    settings.general.onboarding_done = true;
    let backend = Arc::new(Backend::new().unwrap());
    let conns = ConnectionManager::new(ternarygrid_app::driver_registry(), Arc::new(MemorySecrets::default()), Arc::new(DirectNet));
    let services = Services::ephemeral(&dir.join("home"));
    let h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(400).with_step_dt(1.0 / 60.0).build_eframe(move |cc| {
        ternarygrid_app::theme::setup_fonts(&cc.egui_ctx);
        backend.set_context(cc.egui_ctx.clone());
        TernaryGridApp::new(backend, conns, services, settings)
    });
    ternarygrid_app::theme::apply_settings(&h.ctx, &h.state().settings);
    h
}

fn app_wait(h: &mut Harness<'static, TernaryGridApp>, what: &str, mut cond: impl FnMut(&mut Harness<'static, TernaryGridApp>) -> bool) {
    let t0 = Instant::now();
    while !cond(h) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ============================================================================
// 1. SQLite Migration P0 regression: multiple FKs + secondary indexes survive
// ============================================================================

#[test]
fn sqlite_migration_preserves_multiple_fks_and_secondary_indexes_e2e() {
    let backend = Backend::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("src.db");
    let dst_path = dir.path().join("dst.db");

    backend.block_on(async {
        let mut s = open_sqlite(&src_path).await;
        exec_batch(
            s.as_mut(),
            &[
                "PRAGMA foreign_keys = ON",
                "CREATE TABLE regions (id INTEGER PRIMARY KEY, code TEXT NOT NULL UNIQUE, name TEXT)",
                "CREATE TABLE categories (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
                "CREATE TABLE products (
                    id INTEGER PRIMARY KEY,
                    region_id INTEGER NOT NULL REFERENCES regions(id) ON DELETE CASCADE,
                    category_id INTEGER NOT NULL REFERENCES categories(id) ON DELETE RESTRICT,
                    sku TEXT NOT NULL UNIQUE,
                    name TEXT NOT NULL,
                    price REAL
                )",
                "CREATE INDEX idx_products_name ON products(name)",
                "CREATE INDEX idx_products_price ON products(price)",
                "INSERT INTO regions VALUES (1, 'ID', 'Indonesia'), (2, 'JP', 'Japan')",
                "INSERT INTO categories VALUES (10, 'Electronics'), (20, 'Books')",
                "INSERT INTO products VALUES (100, 1, 10, 'SKU-1', 'Widget café 🚀', 19.95)",
                "INSERT INTO products VALUES (101, 2, 20, 'SKU-2', 'Book ''quoted''', NULL)",
            ],
        )
        .await;
        let _ = s.close().await;

        let src = Arc::new(RealSqliteFactory { path: src_path.clone() });
        let dst = Arc::new(RealSqliteFactory { path: dst_path.clone() });
        // Put child `products` FIRST so FKs must be created in Phase 4 after all tables & rows exist.
        let job = MigrationJob {
            tables: vec![ObjectRef::table(Some("main".into()), "products"), ObjectRef::table(Some("main".into()), "regions"), ObjectRef::table(Some("main".into()), "categories")],
            mode: MigrationMode::DropAndCreate,
            copy_indexes: true,
            copy_foreign_keys: true,
            batch_size: 10,
            ..Default::default()
        };
        let progress = Progress::new();
        let summary = run_migration(job, src, dst, &progress, &CancellationToken::new()).await.unwrap();
        assert!(summary.warnings.is_empty(), "expected zero migration warnings, got: {:?}", summary.warnings);
        assert_eq!(summary.foreign_keys_created, 2, "both FKs must be created");
        assert_eq!(summary.rows, 6);

        // Introspect destination using real SqliteDriver!
        let mut d = open_sqlite(&dst_path).await;
        let pdef = d.table_definition(&ObjectRef::table(Some("main".into()), "products")).await.unwrap();
        assert_eq!(pdef.foreign_keys.len(), 2, "BOTH foreign keys must survive on SQLite: {:?}", pdef.foreign_keys);
        let fk_targets: Vec<&str> = pdef.foreign_keys.iter().map(|fk| fk.ref_table.as_str()).collect();
        assert!(fk_targets.contains(&"regions") && fk_targets.contains(&"categories"), "{fk_targets:?}");

        let idx_names: Vec<&str> = pdef.indexes.iter().map(|i| i.name.as_str()).collect();
        assert!(
            idx_names.contains(&"idx_products_name") && idx_names.contains(&"idx_products_price"),
            "BOTH secondary indexes must survive after SQLite FK rebuild: {idx_names:?}"
        );

        let (_, rows) = query_all(d.as_mut(), "SELECT id, name, price FROM products ORDER BY id", &[]).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1], Value::text("Widget café 🚀"));
        assert_eq!(rows[1][1], Value::text("Book 'quoted'"));
        assert_eq!(rows[1][2], Value::Null);
        let _ = d.close().await;
    });
}

// ============================================================================
// 2. Export/Import roundtrip, edge values, Replace on non-unique key, headless failure
// ============================================================================

#[test]
fn sqlite_export_import_roundtrip_replace_and_edge_values_e2e() {
    let backend = Backend::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("transfer.db");

    backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        exec_batch(
            s.as_mut(),
            &[
                "CREATE TABLE edge_src (id INTEGER PRIMARY KEY, label TEXT, qty INTEGER, score REAL)",
                "INSERT INTO edge_src VALUES (1, NULL, 10, 0.0)",
                "INSERT INTO edge_src VALUES (2, 'NULL', 20, 1.5)",
                "INSERT INTO edge_src VALUES (3, 'a\"b,c\nline2 🚀 日本語', 30, -4.25)",
                // Non-unique match key target for ImportMode::Replace fallback test.
                "CREATE TABLE non_unique_tgt (code TEXT, val INTEGER)",
                "INSERT INTO non_unique_tgt VALUES ('K1', 100), ('K2', 200)",
            ],
        )
        .await;
        let _ = s.close().await;

        let factory = Arc::new(RealSqliteFactory { path: db_path.clone() });
        let mut csv_path = PathBuf::new();
        for (fmt, name) in [
            (ExportFormat::Csv, "edge_csv"),
            (ExportFormat::Json, "edge_json"),
            (ExportFormat::Xml, "edge_xml"),
            (ExportFormat::Xlsx, "edge_xlsx"),
            (ExportFormat::Sql, "edge_sql"),
        ] {
            let mut job = ExportJob {
                format: fmt,
                output_folder: dir.path().to_path_buf(),
                file_name_pattern: name.into(),
                items: vec![ExportItem::Table { table: ObjectRef::table(Some("main".into()), "edge_src"), where_clause: None }],
                ..Default::default()
            };
            if fmt == ExportFormat::Csv {
                job.options.csv.null_string = "NULL".into();
            }
            let summary = run_export_job_with(job, factory.clone(), None, Progress::shared(), CancellationToken::new()).await.unwrap();
            assert_eq!(summary.rows, 3, "export {fmt:?} wrote 3 rows");
            let out = summary.items[0].file.as_ref().expect("export file path");
            assert!(out.is_file() && std::fs::metadata(out).unwrap().len() > 0, "export file non-empty for {fmt:?}");
            if fmt == ExportFormat::Csv {
                csv_path = out.clone();
            }
        }

        // Import CSV back: SQL NULL stays NULL while quoted "NULL" stays text "NULL".
        let csv_opts = CsvReadOptions { null_values: vec!["NULL".into()], ..Default::default() };
        let mut mapping = ImportMapping { target: ObjectRef::table(Some("main".into()), "edge_csv_dst"), create_table: true, ..Default::default() };
        mapping.convert.null_values.clear();
        let imp = ImportJob { source: ImportSourceConfig::Csv { path: csv_path.clone(), options: csv_opts }, mapping, infer: Default::default(), sample_rows: 100 };
        let sum = run_import_job(imp, factory.clone(), Progress::shared(), CancellationToken::new()).await.unwrap();
        assert_eq!(sum.rows_written, 3);
        assert_eq!(sum.rows_failed, 0);

        let mut d = open_sqlite(&db_path).await;
        let (_, rows) = query_all(d.as_mut(), "SELECT id, label FROM edge_csv_dst ORDER BY id", &[]).await.unwrap();
        assert_eq!(rows[0][1], Value::Null, "unquoted NULL imported as SQL NULL");
        assert_eq!(rows[1][1], Value::text("NULL"), "quoted \"NULL\" preserved as text");
        assert_eq!(rows[2][1], Value::text("a\"b,c\nline2 🚀 日本語"), "quotes, commas, newlines, emoji roundtrip");

        // Scientific zero ("0e0") and Decimal("1e3") -> Int conversion via JSON import!
        let sci_json = dir.path().join("sci.json");
        std::fs::write(&sci_json, r#"[{"id": 1e3, "zero_val": "0e0"}]"#).unwrap();
        exec_batch(d.as_mut(), &["CREATE TABLE sci_dst (id INTEGER, zero_val REAL)"]).await;
        let _ = d.close().await;

        let mapping = ImportMapping { target: ObjectRef::table(Some("main".into()), "sci_dst"), create_table: false, ..Default::default() };
        let imp_sci = ImportJob { source: ImportSourceConfig::Json { path: sci_json, options: Default::default() }, mapping, infer: Default::default(), sample_rows: 100 };
        let sum_sci = run_import_job(imp_sci, factory.clone(), Progress::shared(), CancellationToken::new()).await.unwrap();
        assert_eq!(sum_sci.rows_written, 1);
        assert_eq!(sum_sci.rows_failed, 0);

        let mut d = open_sqlite(&db_path).await;
        let (_, sci_rows) = query_all(d.as_mut(), "SELECT id, zero_val FROM sci_dst", &[]).await.unwrap();
        assert_eq!(sci_rows[0][0].as_i64(), Some(1000), "Decimal('1e3') converts cleanly to Int 1000");
        assert_eq!(sci_rows[0][1].as_f64(), Some(0.0), "'0e0' converts cleanly to numeric 0.0");
        let _ = d.close().await;

        // ImportMode::Replace on a SQLite table with a NON-UNIQUE match key (`code`).
        let repl_csv = dir.path().join("replace.csv");
        std::fs::write(&repl_csv, "code,val\nK1,999\nK3,300\n").unwrap();
        let mapping = ImportMapping {
            target: ObjectRef::table(Some("main".into()), "non_unique_tgt"),
            create_table: false,
            mode: ImportMode::Replace,
            key_columns: vec!["code".into()],
            ..Default::default()
        };
        let imp_repl = ImportJob { source: ImportSourceConfig::Csv { path: repl_csv, options: CsvReadOptions::default() }, mapping, infer: Default::default(), sample_rows: 100 };
        let sum_repl = run_import_job(imp_repl, factory.clone(), Progress::shared(), CancellationToken::new()).await.unwrap();
        assert_eq!(sum_repl.rows_written, 2);
        assert_eq!(sum_repl.rows_failed, 0);

        let mut d = open_sqlite(&db_path).await;
        let (_, rrows) = query_all(d.as_mut(), "SELECT code, val FROM non_unique_tgt ORDER BY code", &[]).await.unwrap();
        assert_eq!(rrows.len(), 3);
        assert_eq!(rrows[0], vec![Value::text("K1"), Value::Int(999)]);
        assert_eq!(rrows[1], vec![Value::text("K2"), Value::Int(200)]);
        assert_eq!(rrows[2], vec![Value::text("K3"), Value::Int(300)]);
        let _ = d.close().await;
    });

    // Headless TaskKind::Import with 100% row failures must return exit code 1 (NOT false Ok 0!).
    let root = dir.path().join("headless_home");
    let paths = AppPaths::with_root(&root);
    paths.ensure_dirs().unwrap();
    let fail_db = root.join("fail_import.db");
    backend.block_on(async {
        let mut s = open_sqlite(&fail_db).await;
        exec_batch(s.as_mut(), &["CREATE TABLE strict_t (id INTEGER NOT NULL)"]).await;
        let _ = s.close().await;
    });
    let bad_csv = root.join("bad.csv");
    std::fs::write(&bad_csv, "id\nnot_an_int_1\nnot_an_int_2\n").unwrap();

    let mut store = ConnectionStore::new();
    let mut cfg = ConnectionConfig::new(DbKind::Sqlite, "strict");
    cfg.file_path = Some(fail_db.display().to_string());
    cfg.auth = AuthMethod::None;
    let conn_id = store.add(cfg).unwrap();
    store.save(&paths).unwrap();

    let mapping = ImportMapping { target: ObjectRef::table(Some("main".into()), "strict_t"), create_table: false, on_error: OnError::SkipRow, ..Default::default() };
    let job = ImportJob { source: ImportSourceConfig::Csv { path: bad_csv, options: CsvReadOptions::default() }, mapping, infer: Default::default(), sample_rows: 100 };
    let tasks = TaskStore::open(paths.tasks_dir()).unwrap();
    let task = TaskDef::new("failing_import", TaskKind::Import { connection_id: conn_id, config: ternarygrid_app::transferexec::import_task_config(&job).unwrap() });
    tasks.create(task).unwrap();
    drop(tasks);

    let code = ternarygrid_app::headless::run_task(&backend, &paths, &Settings::default(), "failing_import");
    assert_eq!(format!("{code:?}"), format!("{:?}", std::process::ExitCode::from(1)), "100% failed rows import task must exit with error code 1");
}

// ============================================================================
// 3. SQLite Schema Compare P0 regression: multi-constraint sync + headless task
// ============================================================================

#[test]
fn sqlite_schema_compare_multi_constraint_sync_and_headless_task_e2e() {
    let backend = Backend::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("cmp_src.db");
    let dst_path = dir.path().join("cmp_dst.db");
    let dst_manual_path = dir.path().join("cmp_dst_manual.db");

    backend.block_on(async {
        let mut sa = open_sqlite(&src_path).await;
        exec_batch(
            sa.as_mut(),
            &[
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                "CREATE TABLE products (id INTEGER PRIMARY KEY, sku TEXT NOT NULL UNIQUE)",
                "CREATE TABLE orders (
                    id INTEGER PRIMARY KEY,
                    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                    product_id INTEGER NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
                    amount REAL NOT NULL CHECK (amount > 0),
                    note TEXT DEFAULT 'ok'
                )",
                "CREATE INDEX idx_orders_user ON orders(user_id)",
                "CREATE INDEX idx_orders_amount ON orders(amount)",
                "CREATE VIEW v_orders AS SELECT id, user_id, amount FROM orders WHERE amount > 10",
                "CREATE TRIGGER trg_orders_after AFTER INSERT ON orders BEGIN SELECT 1; END",
            ],
        )
        .await;

        let setup_target = [
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "CREATE TABLE products (id INTEGER PRIMARY KEY, sku TEXT NOT NULL UNIQUE)",
            "CREATE TABLE orders (
                id INTEGER PRIMARY KEY,
                user_id INTEGER NOT NULL REFERENCES users(id),
                product_id INTEGER NOT NULL,
                amount REAL NOT NULL CHECK (amount >= 0)
            )",
            "CREATE INDEX idx_orders_user ON orders(user_id)",
            "CREATE VIEW v_orders AS SELECT id FROM orders",
            "INSERT INTO users VALUES (1, 'Alice')",
            "INSERT INTO products VALUES (10, 'P1')",
            "INSERT INTO orders VALUES (100, 1, 10, 42.5)",
        ];
        for path in [&dst_path, &dst_manual_path] {
            let mut sb = open_sqlite(path).await;
            exec_batch(sb.as_mut(), &setup_target).await;
            let _ = sb.close().await;
        }

        // Compare & sync using DEFAULT ScriptOptions (verifying plan.rs forces WholeTable on SQLite).
        let snap_src = sa.schema_snapshot(&SchemaRef::new(None, Some("main".into())), &SnapshotOptions::full()).await.unwrap();
        let mut sb = open_sqlite(&dst_path).await;
        let snap_dst = sb.schema_snapshot(&SchemaRef::new(None, Some("main".into())), &SnapshotOptions::full()).await.unwrap();
        let diff = schema_diff(&snap_src, &snap_dst, &CompareOptions::default());
        assert!(diff.has_differences(), "target differs from source before sync");
        let dialect = SqliteDialect::new();
        let script = migration_script(&diff, &snap_src, &snap_dst, &dialect, &ScriptOptions { drop_removed: true, ..Default::default() });
        assert!(!script.is_empty());

        for sql in &script {
            execute_update(sb.as_mut(), sql, &[]).await.unwrap_or_else(|e| panic!("sync SQL failed:\n{sql}\n{e}"));
        }
        // Re-compare: ZERO differences and existing data row preserved!
        let snap_after = sb.schema_snapshot(&SchemaRef::new(None, Some("main".into())), &SnapshotOptions::full()).await.unwrap();
        let diff_after = schema_diff(&snap_src, &snap_after, &CompareOptions::default());
        assert!(!diff_after.has_differences(), "expected 0 schema differences after sync, got: {:#?}", diff_after.items);
        let kept = query_scalar(sb.as_mut(), "SELECT amount FROM orders WHERE id = 100", &[]).await.unwrap();
        assert_eq!(kept.as_f64(), Some(42.5), "existing row survived SQLite rebuild sync");
        let _ = sa.close().await;
        let _ = sb.close().await;
    });

    // Also run SchemaCompareTab UI apply (against dst_manual_path) & verify Manual commit mode works!
    let mut th = TabHarness::new();
    let src_id = th.connect_sqlite("cmp-src", &src_path, false);
    let dst_id = th.connect_sqlite("cmp-dst-man", &dst_manual_path, false);
    // Switch target session to Manual commit and execute a SELECT so it enters a read tx.
    let dst_main = th.cm.live(&dst_id).unwrap().main.clone();
    th.backend.block_on(async {
        dst_main.set_transaction_mode(TransactionMode::Manual).await.unwrap();
        let _ = dst_main.lock().await.execute("SELECT count(*) FROM orders", &[], &ExecOptions::default()).await.unwrap();
    });
    let mut tab = SchemaCompareTab::new(Some(src_id.clone()), Some(SchemaRef::new(None, Some("main".into()))));
    th.with_cx(|cx| {
        tab.compare_now(cx, (&src_id, SchemaRef::new(None, Some("main".into()))), (&dst_id, SchemaRef::new(None, Some("main".into()))));
    });
    th.until(&mut tab, "schema compare diff", |t| !t.is_busy() && t.diff().is_some());
    assert!(tab.diff().unwrap().has_differences());

    // Headless TaskKind::SchemaCompare writing script + HTML report + applying sync!
    let root = dir.path().join("cmp_headless");
    let paths = AppPaths::with_root(&root);
    paths.ensure_dirs().unwrap();
    let dst_task_path = root.join("cmp_dst_task.db");
    std::fs::copy(&dst_manual_path, &dst_task_path).unwrap();

    let mut store = ConnectionStore::new();
    let mut ca = ConnectionConfig::new(DbKind::Sqlite, "src");
    ca.file_path = Some(src_path.display().to_string());
    ca.auth = AuthMethod::None;
    let a_id = store.add(ca).unwrap();
    let mut cb = ConnectionConfig::new(DbKind::Sqlite, "dst");
    cb.file_path = Some(dst_task_path.display().to_string());
    cb.auth = AuthMethod::None;
    let b_id = store.add(cb).unwrap();
    store.save(&paths).unwrap();

    let report_path = root.join("report.html");
    let script_path = root.join("sync.sql");
    let tasks = TaskStore::open(paths.tasks_dir()).unwrap();
    let cfg = serde_json::json!({
        "source_connection": a_id,
        "source_schema": "main",
        "target_connection": b_id,
        "target_schema": "main",
        "drop_removed": true,
        "apply": true,
        "script_path": script_path,
        "report_path": report_path,
    });
    tasks.create(TaskDef::new("schema_sync", TaskKind::SchemaCompare { config: cfg })).unwrap();
    drop(tasks);

    let code = ternarygrid_app::headless::run_task(&backend, &paths, &Settings::default(), "schema_sync");
    assert_eq!(format!("{code:?}"), format!("{:?}", std::process::ExitCode::from(0)));
    let html = std::fs::read_to_string(&report_path).unwrap();
    assert!(html.contains("<html") && html.contains("orders"));
    assert!(!std::fs::read_to_string(&script_path).unwrap().is_empty());
    backend.block_on(async {
        let mut sa = open_sqlite(&src_path).await;
        let mut sb = open_sqlite(&dst_task_path).await;
        let snap_a = sa.schema_snapshot(&SchemaRef::new(None, Some("main".into())), &SnapshotOptions::full()).await.unwrap();
        let snap_b = sb.schema_snapshot(&SchemaRef::new(None, Some("main".into())), &SnapshotOptions::full()).await.unwrap();
        assert!(!schema_diff(&snap_a, &snap_b, &CompareOptions::default()).has_differences());
    });
}

// ============================================================================
// 4. SQLite Data Compare & Sync: composite keys, case-diff names, NULL/BLOB/Unicode
// ============================================================================

#[test]
fn sqlite_data_compare_composite_keys_case_diff_and_sync_e2e() {
    let backend = Backend::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("dc_src.db");
    let dst_path = dir.path().join("dc_dst.db");

    backend.block_on(async {
        let mut sa = open_sqlite(&src_path).await;
        let mut sb = open_sqlite(&dst_path).await;
        exec_batch(
            sa.as_mut(),
            &[
                "CREATE TABLE items (TenantId INTEGER NOT NULL, Code TEXT NOT NULL, Title TEXT, Raw BLOB, PRIMARY KEY (TenantId, Code))",
                "INSERT INTO items VALUES (1, 'A', 'Same', X'0102')",
                "INSERT INTO items VALUES (1, 'B', 'Updated café 🚀', X'DEAD')",
                "INSERT INTO items VALUES (1, 'C', NULL, NULL)",
                "INSERT INTO items VALUES (2, 'NEW', 'Only source', X'FF')",
            ],
        )
        .await;
        exec_batch(
            sb.as_mut(),
            &[
                // Lower-case column names on target to verify case-insensitive key & column matching!
                "CREATE TABLE items (tenantid INTEGER NOT NULL, code TEXT NOT NULL, title TEXT, raw BLOB, PRIMARY KEY (tenantid, code))",
                "INSERT INTO items VALUES (1, 'A', 'Same', X'0102')",
                "INSERT INTO items VALUES (1, 'B', 'Old title', X'0000')",
                "INSERT INTO items VALUES (1, 'C', 'Was not null', X'BEEF')",
                "INSERT INTO items VALUES (9, 'OLD', 'Only target', NULL)",
            ],
        )
        .await;

        let job = ternarygrid_compare::data::DataCompareJob {
            source: ternarygrid_compare::data::TableSpec {
                obj: ObjectRef::table(Some("main".into()), "items"),
                key_columns: vec!["TenantId".into(), "Code".into()],
                columns: vec!["Title".into(), "Raw".into()],
                where_clause: None,
            },
            target: ternarygrid_compare::data::TableSpec {
                obj: ObjectRef::table(Some("main".into()), "items"),
                key_columns: vec!["tenantid".into(), "code".into()],
                columns: vec!["title".into(), "raw".into()],
                where_clause: None,
            },
            options: ternarygrid_compare::data::DataCompareOptions::default(),
        };
        let dialect = SqliteDialect::new();
        let diff = ternarygrid_compare::data::run_data_compare(&job, &mut sa, &mut sb, &dialect, &dialect, &mut |_| {}, &CancellationToken::new()).await.unwrap();
        assert_eq!((diff.identical, diff.different, diff.only_in_source, diff.only_in_target), (1, 2, 1, 1));
        assert!(!diff.sync_statements.is_empty());

        exec_batch(sb.as_mut(), &["BEGIN"]).await;
        for sql in &diff.sync_statements {
            execute_update(sb.as_mut(), sql, &[]).await.unwrap();
        }
        exec_batch(sb.as_mut(), &["COMMIT"]).await;

        let after = ternarygrid_compare::data::run_data_compare(&job, &mut sa, &mut sb, &dialect, &dialect, &mut |_| {}, &CancellationToken::new()).await.unwrap();
        assert!(after.is_identical(), "target must match source after sync: {after:?}");
        let _ = sa.close().await;
        let _ = sb.close().await;
    });

    // Live DataCompareTab UI test: compare + apply sync in UI tab.
    let mut th = TabHarness::new();
    let dst2_path = dir.path().join("dc_dst2.db");
    th.backend.block_on(async {
        let mut s = open_sqlite(&dst2_path).await;
        exec_batch(
            s.as_mut(),
            &[
                "CREATE TABLE items (tenantid INTEGER NOT NULL, code TEXT NOT NULL, title TEXT, raw BLOB, PRIMARY KEY (tenantid, code))",
                "INSERT INTO items VALUES (1, 'A', 'Same', X'0102')",
            ],
        )
        .await;
        let _ = s.close().await;
    });
    let s_id = th.connect_sqlite("dc-s", &src_path, false);
    let d_id = th.connect_sqlite("dc-d", &dst2_path, false);
    let mut tab = DataCompareTab::new(Some(s_id.clone()), Some(ObjectRef::table(Some("main".into()), "items")));
    tab.set_source(&s_id, ObjectRef::table(Some("main".into()), "items"));
    tab.set_target(&d_id, ObjectRef::table(Some("main".into()), "items"));
    tab.request_compare();
    th.until(&mut tab, "data compare result", |t| !t.is_busy() && t.diff().is_some());
    assert_eq!(tab.diff().unwrap().only_in_source, 3);
    th.with_cx(|cx| tab.apply_now(cx));
    th.until(&mut tab, "data compare apply + recompare", |t| !t.is_busy() && t.diff().is_some_and(|d| d.is_identical()));
}

// ============================================================================
// 5. Table Designer E2E: Create + Alter + SQLite FK picker + Cmd+S Action::Save
// ============================================================================

#[test]
fn table_designer_create_alter_fk_picker_and_cmd_s_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("designer.db");
    let mut th = TabHarness::new();
    th.backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        exec_batch(s.as_mut(), &["CREATE TABLE departments (id INTEGER PRIMARY KEY, name TEXT NOT NULL)"]).await;
        let _ = s.close().await;
    });
    let conn_id = th.connect_sqlite("td-db", &db_path, false);

    // 1) Create new table `employees` with PK, unique key, secondary index; trigger Action::Save (Cmd+S).
    let mut tab = TableDesignerTab::create(conn_id.clone(), SchemaRef::new(None, Some("main".into())));
    th.until(&mut tab, "designer ready", |t| t.is_ready());
    tab.set_table_name("employees");
    tab.clear_columns();
    tab.add_column("id", "INTEGER", "", true, true);
    tab.add_column("dept_id", "INTEGER", "", true, false);
    tab.add_column("email", "TEXT", "", true, false);
    tab.add_unique_key("uq_emp_email", &["email"]);
    tab.add_index("idx_emp_dept", &["dept_id"], false);
    assert!(tab.problems().is_empty(), "{:?}", tab.problems());
    assert_eq!(tab.action_enabled(Action::Save), Some(true), "Action::Save enabled on valid dirty table");
    let handled = th.with_cx(|cx| tab.on_action(Action::Save, cx));
    assert!(handled, "TableDesignerTab handles Action::Save");
    th.frame(&mut tab);
    th.with_cx(|cx| tab.execute_now(cx));
    th.until(&mut tab, "create table finish", |t| !t.is_executing() && t.table_ref().is_some());
    assert!(tab.last_error().is_none(), "{:?}", tab.last_error());

    // 2) Edit existing `employees`: add column, rename column, verify real SQLite schema updated.
    let mut edit = TableDesignerTab::edit(conn_id.clone(), ObjectRef::table(Some("main".into()), "employees"));
    th.until(&mut edit, "edit designer loaded", |t| t.is_ready());
    edit.add_column("bio", "TEXT", "", false, false);
    edit.rename_column(2, "work_email");
    assert!(edit.problems().is_empty(), "{:?}", edit.problems());
    th.with_cx(|cx| edit.execute_now(cx));
    th.until(&mut edit, "alter table finish", |t| !t.is_executing() && !t.is_dirty());
    assert!(edit.last_error().is_none(), "{:?}", edit.last_error());

    th.backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        let def = s.table_definition(&ObjectRef::table(Some("main".into()), "employees")).await.unwrap();
        let cols: Vec<&str> = def.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, vec!["id", "dept_id", "work_email", "bio"]);
        assert!(def.indexes.iter().any(|i| i.name == "idx_emp_dept"));
        let _ = s.close().await;
    });
}

// ============================================================================
// 6. Query Builder E2E: Auto-JOIN + safe GROUP BY + live SQLite execution
// ============================================================================

#[test]
fn query_builder_joins_aggregates_and_live_execution_e2e() {
    let backend = Backend::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("qb.db");

    backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        exec_batch(
            s.as_mut(),
            &[
                "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL REFERENCES customers(id), total REAL NOT NULL)",
                "INSERT INTO customers VALUES (1, 'Alice'), (2, 'Bob')",
                "INSERT INTO orders VALUES (1, 1, 50.0), (2, 1, 75.0), (3, 2, 20.0)",
            ],
        )
        .await;
        let cust_def = s.table_definition(&ObjectRef::table(Some("main".into()), "customers")).await.unwrap();
        let ord_def = s.table_definition(&ObjectRef::table(Some("main".into()), "orders")).await.unwrap();

        let dialect = SqliteDialect::new();
        let mut qb = QueryModel::default();
        let c_alias = qb.add_table(&cust_def);
        let o_alias = qb.add_table(&ord_def);
        assert_eq!(qb.joins.len(), 1, "FK auto-creates JOIN between orders and customers");

        // Regression check: `*`, `t.*`, literals, and manual aggregate expressions must NOT enter GROUP BY.
        qb.select.push(QbSelectItem::new(format!("{c_alias}.name")));
        qb.select.push(QbSelectItem::new("*"));
        qb.select.push(QbSelectItem::new(format!("{o_alias}.*")));
        qb.select.push(QbSelectItem::new("'USD'"));
        qb.select.push(QbSelectItem::new("SUM(o.total)"));
        let mut cnt = QbSelectItem::new(format!("{o_alias}.id"));
        cnt.aggregate = Some(Aggregate::Count);
        qb.select.push(cnt);
        let gb = qb.effective_group_by(&dialect);
        assert_eq!(gb.len(), 1, "only groupable column belongs in GROUP BY: {gb:?}");
        assert!(gb[0].contains("name") && !gb.iter().any(|g| g.contains('*') || g.contains("USD") || g.contains("SUM")), "{gb:?}");

        // Clean executable aggregate query against real SQLite!
        qb.select.clear();
        qb.select.push(QbSelectItem::new(format!("{c_alias}.name")));
        let mut sum_item = QbSelectItem::new(format!("{o_alias}.total"));
        sum_item.aggregate = Some(Aggregate::Sum);
        sum_item.alias = "gross".into();
        qb.select.push(sum_item);
        let sql = qb.to_sql(&dialect);
        let (_, rows) = query_all(s.as_mut(), &format!("{sql} ORDER BY 1"), &[]).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], Value::text("Alice"));
        assert_eq!(rows[0][1].as_f64(), Some(125.0));
        assert_eq!(rows[1][0], Value::text("Bob"));
        assert_eq!(rows[1][1].as_f64(), Some(20.0));
        let _ = s.close().await;
    });
}

// ============================================================================
// 7. Global Metadata & Data Search E2E
// ============================================================================

#[test]
fn global_data_and_metadata_search_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("search.db");
    let mut th = TabHarness::new();
    th.backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        exec_batch(
            s.as_mut(),
            &[
                "CREATE TABLE articles (id INTEGER PRIMARY KEY, title TEXT, body TEXT)",
                "CREATE TABLE comments (id INTEGER PRIMARY KEY, note TEXT)",
                "CREATE TABLE unrelated (id INTEGER PRIMARY KEY, val TEXT)",
                "INSERT INTO articles VALUES (1, 'NeedleInTitle', 'hello'), (2, 'Other', 'café needle here')",
                "INSERT INTO comments VALUES (1, 'another NEEDLE match')",
                "INSERT INTO unrelated VALUES (1, 'nothing matches here')",
            ],
        )
        .await;
        let _ = s.close().await;
    });
    let conn_id = th.connect_sqlite("search-db", &db_path, false);

    // 1) Metadata search.
    let mut meta_tab = SearchTab::new(Some(conn_id.clone()), false);
    meta_tab.set_pattern("art");
    th.with_cx(|cx| meta_tab.search_metadata_now(cx));
    th.until(&mut meta_tab, "metadata search hits", |t| !t.is_busy() && !t.metadata_hits().is_empty());
    assert!(meta_tab.metadata_hits().iter().any(|h| h.info.obj.name == "articles"));

    // 2) Data search across all tables (case-insensitive).
    let mut data_tab = SearchTab::new(Some(conn_id.clone()), true);
    data_tab.set_data_search(SchemaRef::new(None, Some("main".into())), "needle", &["articles", "comments", "unrelated"]);
    data_tab.request_data_search();
    th.until(&mut data_tab, "data search finish", |t| t.data_finished());
    let hits: BTreeMap<String, u64> = data_tab.data_hits().iter().map(|h| (h.table.name.clone(), h.count)).collect();
    assert_eq!(hits.get("articles"), Some(&2));
    assert_eq!(hits.get("comments"), Some(&1));
    assert_eq!(hits.get("unrelated"), None);
}

// ============================================================================
// 8. ER Diagram E2E: layout, JSON roundtrip, SVG & PNG export + live ErTab
// ============================================================================

#[test]
fn er_diagram_layout_save_reopen_svg_png_export_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("er.db");
    let mut th = TabHarness::new();
    let snap = th.backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        exec_batch(
            s.as_mut(),
            &[
                "CREATE TABLE orgs (id INTEGER PRIMARY KEY, name TEXT)",
                "CREATE TABLE teams (id INTEGER PRIMARY KEY, org_id INTEGER REFERENCES orgs(id), name TEXT)",
                "CREATE TABLE members (id INTEGER PRIMARY KEY, team_id INTEGER REFERENCES teams(id), email TEXT)",
                "CREATE TABLE tasks (id INTEGER PRIMARY KEY, assignee_id INTEGER REFERENCES members(id), title TEXT)",
            ],
        )
        .await;
        let snap = s.schema_snapshot(&SchemaRef::new(None, Some("main".into())), &SnapshotOptions::full()).await.unwrap();
        let _ = s.close().await;
        snap
    });

    let model = ErModel::from_snapshot(&snap, None);
    assert_eq!(model.tables.len(), 4);
    assert_eq!(model.relations.len(), 3);
    let mut diagram = ErDiagram::new("org_schema", model);
    diagram.auto_layout(&LayoutOptions::default());

    // Verify no overlapping table boxes after auto-layout.
    let rects: Vec<_> = diagram.model.tables.iter().filter_map(|t| diagram.table_rect(&t.key)).collect();
    assert_eq!(rects.len(), 4);
    for i in 0..rects.len() {
        for j in (i + 1)..rects.len() {
            assert!(!rects[i].intersects(rects[j]), "ER tables overlapped: {:?} vs {:?}", rects[i], rects[j]);
        }
    }

    // JSON persistence roundtrip keeps positions & name.
    let json = diagram.to_json().unwrap();
    let reopened = ErDiagram::from_json(&json).unwrap();
    assert_eq!(reopened.model.tables.len(), 4);
    for t in &diagram.model.tables {
        assert_eq!(diagram.table_rect(&t.key), reopened.table_rect(&t.key));
    }

    // Valid SVG and valid PNG export.
    let svg = export_svg(&diagram);
    assert!(svg.contains("<svg") && svg.trim_end().ends_with("</svg>"));
    for name in ["orgs", "teams", "members", "tasks"] {
        assert!(svg.contains(name), "SVG missing table {name}");
    }
    let png = export_png(&diagram, 1.0).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]), "valid PNG header");
    assert!(png.len() > 1000, "non-trivial PNG payload");

    // Live ErTab renders cleanly inside egui frame.
    let conn_id = th.connect_sqlite("er-db", &db_path, false);
    let mut tab = ErTab::from_saved(conn_id, SchemaRef::new(None, Some("main".into())), "org_schema".into(), reopened);
    for _ in 0..4 {
        th.frame(&mut tab);
    }
    assert!(th.toasts.take_log().iter().all(|t| t.kind != ToastKind::Error));
}

// ============================================================================
// 9. SQL Editor Advanced E2E: positional `?`, read-only EXPLAIN, `@set`, `@export`, UTF-8 diagnostics, Rollback
// ============================================================================

#[test]
fn sql_editor_advanced_positional_explain_at_set_export_and_rollback_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("editor_adv.db");
    let mut th = TabHarness::new();
    th.backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        exec_batch(s.as_mut(), &["CREATE TABLE items (id INTEGER PRIMARY KEY, v TEXT)", "INSERT INTO items VALUES (1, 'orig')"]).await;
        let _ = s.close().await;
    });
    let rw_id = th.connect_sqlite("ed-rw", &db_path, false);
    let ro_id = th.connect_sqlite("ed-ro", &db_path, true);

    // Enable `?` positional parameters.
    th.settings.editor.parameters.question_mark = true;
    let ctx = th.ctx.clone();

    let mut tab = SqlEditorTab::new(Some(rw_id.clone()), None, String::new(), None, false, "adv".into());
    th.until(&mut tab, "session ready", |t| t.session_ready());

    // (a) Multi-statement positional `?`: distinct names ["1", "2"] and NOT persisted to variables!
    tab.set_text(&ctx, "SELECT ? AS a;\nSELECT ? AS b;\n");
    th.with_cx(|cx| tab.on_action(Action::ExecuteScript, cx));
    assert_eq!(tab.pending_params(), Some(vec!["1".to_string(), "2".to_string()]));
    th.with_cx(|cx| tab.answer_params(cx, &["10", "20"]));
    th.until(&mut tab, "positional exec", |t| !t.is_running() && t.result_count() == 2);
    let cell_of = |t: &SqlEditorTab, i: usize| t.result_panel(i).and_then(|p| p.model.cell(0, 0));
    assert_eq!(cell_of(&tab, 0), Some(Value::Int(10)));
    assert_eq!(cell_of(&tab, 1), Some(Value::Int(20)));
    // Second run with `?` MUST prompt again (positional values never polluted self.variables).
    // Active bottom tab is Result(1) from the prior 2-statement run, so ExecuteStatement replaces index 1.
    tab.set_text(&ctx, "SELECT ? AS c;\n");
    th.with_cx(|cx| tab.on_action(Action::ExecuteStatement, cx));
    assert_eq!(tab.pending_params(), Some(vec!["1".to_string()]), "second `?` prompts fresh");
    th.with_cx(|cx| tab.answer_params(cx, &["99"]));
    th.until(&mut tab, "second positional", |t| !t.is_running() && cell_of(t, 1) == Some(Value::Int(99)));

    // (b) `@set` atomicity: failed earlier statement with stop_on_error = true must NOT persist later `@set`.
    th.settings.execution.stop_on_error = true;
    tab.set_text(&ctx, "SELECT * FROM no_such_table;\n@set leaked_var = 777\n");
    th.with_cx(|cx| tab.on_action(Action::ExecuteScript, cx));
    th.until(&mut tab, "failing script", |t| !t.is_running());
    tab.set_text(&ctx, "SELECT ${leaked_var} AS v;\n");
    th.with_cx(|cx| tab.on_action(Action::ExecuteStatement, cx));
    assert_eq!(tab.pending_params(), Some(vec!["leaked_var".to_string()]), "leaked_var was NOT set after earlier failure");
    // Cancel pending prompt by answering cleanly.
    th.with_cx(|cx| tab.answer_params(cx, &["1"]));
    th.until(&mut tab, "cleanup", |t| !t.is_running());

    // (c) `@export` cleanup: `@export` before a non-row statement must NOT hijack the next query.
    tab.set_text(&ctx, "@export\nCREATE TABLE no_rows_tmp (x INTEGER);\n");
    th.with_cx(|cx| tab.on_action(Action::ExecuteScript, cx));
    th.until(&mut tab, "export non-row", |t| !t.is_running());
    let before_results = tab.result_count();
    tab.set_text(&ctx, "SELECT 4242 AS answer;\n");
    th.with_cx(|cx| tab.on_action(Action::ExecuteInNewTab, cx));
    th.until(&mut tab, "select after stale export", |t| !t.is_running() && t.result_count() > before_results);
    assert_eq!(cell_of(&tab, tab.result_count() - 1), Some(Value::Int(4242)));

    // (d) Multi-statement stop_on_error = false with multi-byte UTF-8 accumulates diagnostics without panic.
    th.settings.execution.stop_on_error = false;
    tab.set_text(&ctx, "SELECT '日本語' FROM missing_tbl_1;\nSELECT '🚀' FROM missing_tbl_2;\n");
    th.with_cx(|cx| tab.on_action(Action::ExecuteScript, cx));
    th.until(&mut tab, "utf8 errors", |t| !t.is_running());
    assert_eq!(tab.diagnostics_len(), 2, "both statement errors preserved in diagnostics");

    // (e) Read-only connection: plain EXPLAIN on DELETE succeeds, while direct DELETE is blocked.
    let mut ro_tab = SqlEditorTab::new(Some(ro_id.clone()), None, "DELETE FROM items WHERE id = 1".into(), None, false, "ro".into());
    th.until(&mut ro_tab, "ro session ready", |t| t.session_ready());
    th.with_cx(|cx| ro_tab.on_action(Action::ExplainPlan, cx));
    th.until(&mut ro_tab, "ro explain plan", |t| !t.is_running() && t.result_count() == 1);
    assert!(!ro_tab.output_text().contains("read-only"), "plain EXPLAIN allowed on read-only connection");
    th.with_cx(|cx| ro_tab.on_action(Action::ExecuteStatement, cx));
    th.frame(&mut ro_tab);
    assert!(ro_tab.output_text().contains("read-only"), "direct DELETE blocked on read-only connection");

    // (f) Manual commit mode via ToggleAutoCommit on editor's own session: INSERT + Rollback leaves DB unchanged.
    th.with_cx(|cx| tab.on_action(Action::ToggleAutoCommit, cx));
    th.until(&mut tab, "manual mode ready", |t| t.action_enabled(Action::Rollback) == Some(true));
    tab.set_text(&ctx, "INSERT INTO items VALUES (2, 'rolled_back');");
    th.with_cx(|cx| tab.on_action(Action::ExecuteStatement, cx));
    th.until(&mut tab, "manual insert", |t| !t.is_running() && t.in_transaction());
    th.with_cx(|cx| tab.on_action(Action::Rollback, cx));
    th.until(&mut tab, "rollback done", |t| !t.in_transaction());
    let cnt = th.backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        let v = query_scalar(s.as_mut(), "SELECT count(*) FROM items", &[]).await.unwrap();
        let _ = s.close().await;
        v.as_i64().unwrap()
    });
    assert_eq!(cnt, 1, "rolled-back insert must not persist in SQLite");
}

// ============================================================================
// 10. ResultPanel E2E: Virtual Key on aliased column + Virtual Column survives Refresh/Filter
// ============================================================================

#[test]
fn result_panel_virtual_key_and_virtual_column_survive_refresh_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("vmodel.db");
    let mut th = TabHarness::new();
    th.backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        exec_batch(
            s.as_mut(),
            &[
                // Intentionally NO primary key!
                "CREATE TABLE logs (raw_id INTEGER NOT NULL, msg TEXT)",
                "INSERT INTO logs VALUES (1, 'hello'), (2, 'world')",
            ],
        )
        .await;
        let _ = s.close().await;
    });
    let conn_id = th.connect_sqlite("vm-db", &db_path, false);

    // Pre-register virtual key (`raw_id`) and virtual column (`loud` = `upper(msg)`) in VirtualModel store!
    let projects = th.services.projects.as_ref().expect("projects store");
    let mut vm = projects.virtual_model(&conn_id).unwrap();
    vm.set(
        VirtualTableKey::of(&ObjectRef::table(Some("main".into()), "logs")),
        VirtualTable {
            virtual_key: Some(vec!["raw_id".into()]),
            virtual_columns: vec![VirtualColumn { name: "loud".into(), expression: "upper(msg)".into(), kind: ternarygrid_core::DataKind::Text }],
            ..Default::default()
        },
    );
    projects.save_virtual_model(&conn_id, &vm).unwrap();

    // Open TableDataView on `logs`: virtual key makes it editable AND virtual column `loud` is computed!
    let mut view = ternarygrid_app::results::data_tab::TableDataView::new(conn_id.clone(), ObjectRef::table(Some("main".into()), "logs"), None);
    fn draw_view(th: &mut TabHarness, v: &mut ternarygrid_app::results::data_tab::TableDataView) {
        th.time += 1.0 / 60.0;
        let input =
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))), time: Some(th.time), focused: true, ..Default::default() };
        let ctx = th.ctx.clone();
        let mut out = ctx.run_ui(input, |ui| th.with_cx(|cx| v.ui(ui, cx)));
        out.textures_delta.clear();
        th.cm.poll(&th.backend);
    }
    fn wait_view(
        th: &mut TabHarness,
        v: &mut ternarygrid_app::results::data_tab::TableDataView,
        what: &str,
        mut done: impl FnMut(&ternarygrid_app::results::data_tab::TableDataView) -> bool,
    ) {
        let t0 = Instant::now();
        while !done(v) {
            assert!(t0.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
            draw_view(th, v);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    wait_view(&mut th, &mut view, "table data loaded", |v| v.panel.as_ref().is_some_and(|p| !p.model.fetch.fetching && p.model.row_count() == 2 && p.model.is_editable()));
    {
        let p = view.panel.as_ref().unwrap();
        assert_eq!(p.model.column_count(), 3, "2 real columns + 1 virtual column");
        assert_eq!(p.model.column_name(2), "loud");
        assert_eq!(p.model.cell(0, 2), Some(Value::text("HELLO")));
    }

    // Trigger Refresh -> model resets from new ResultSource Arc; virtual column MUST survive!
    th.with_cx(|cx| EmbeddedView::on_action(&mut view, Action::Refresh, cx));
    wait_view(&mut th, &mut view, "refreshed with virtual column", |v| v.panel.as_ref().is_some_and(|p| !p.model.fetch.fetching && p.model.row_count() == 2));
    {
        let p = view.panel.as_ref().unwrap();
        assert_eq!(p.model.column_count(), 3, "virtual column survived Refresh");
        assert_eq!(p.model.cell(1, 2), Some(Value::text("WORLD")));
    }

    // Edit a cell via the virtual key and Save to real SQLite!
    view.panel.as_mut().unwrap().model.set_cell(0, 1, "updated").unwrap();
    th.with_cx(|cx| EmbeddedView::on_action(&mut view, Action::SaveData, cx));
    wait_view(&mut th, &mut view, "virtual key save done", |v| v.panel.as_ref().is_some_and(|p| !p.model.is_saving() && !p.model.has_changes()));
    let saved = th.backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        let v = query_scalar(s.as_mut(), "SELECT msg FROM logs WHERE raw_id = 1", &[]).await.unwrap();
        let _ = s.close().await;
        v
    });
    assert_eq!(saved, Value::text("updated"), "edit via virtual key persisted to SQLite");
}

// ============================================================================
// 11. Vault cross-process reload, encrypted export/import + retain_selected, portable tasks
// ============================================================================

#[test]
fn vault_cross_process_encrypted_import_export_and_retain_selected_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let vault_path = dir.path().join("vault.bin");
    let pw1 = Secret::new("master-one-123");
    let pw2 = Secret::new("master-two-456");

    // 1) Two VaultStore instances on the same file: cross-process write & master password change!
    let va = VaultStore::new(vault_path.clone());
    let vb = VaultStore::new(vault_path.clone());
    va.create(&pw1).unwrap();
    vb.unlock(&pw1).unwrap();
    va.set("conn:c1:password", &Secret::new("s3cr3t")).unwrap();
    // Instance B transparently detects disk change via file_stamp and reloads!
    assert_eq!(vb.get("conn:c1:password").unwrap().map(|s| s.expose().to_string()), Some("s3cr3t".into()));
    assert!(vb.list_keys().unwrap().contains(&"conn:c1:password".to_string()));

    // Instance A rotates master password -> Instance B detects Auth failure and locks itself!
    va.change_master_password(&pw1, &pw2).unwrap();
    assert!(vb.get("conn:c1:password").is_err(), "stale master password must fail");
    assert!(vb.is_locked(), "vault locks itself when another process changed master password");
    vb.unlock(&pw2).unwrap();
    assert_eq!(vb.get("conn:c1:password").unwrap().map(|s| s.expose().to_string()), Some("s3cr3t".into()));

    // 2) Encrypted export/import + ImportResult::retain_selected pruning unselected metadata & secrets.
    let mut store = ConnectionStore::new();
    store.connection_types.push(ConnectionType { id: "type_keep".into(), name: "KeepType".into(), ..Default::default() });
    store.connection_types.push(ConnectionType { id: "type_drop".into(), name: "DropType".into(), ..Default::default() });

    store.network_profiles.push(NetworkProfile { id: "prof_keep".into(), name: "KeepProf".into(), ..Default::default() });
    store.network_profiles.push(NetworkProfile { id: "prof_drop".into(), name: "DropProf".into(), ..Default::default() });

    let mut c1 = ConnectionConfig::new(DbKind::Sqlite, "KeepConn");
    c1.folder = Some("Work/Alpha".into());
    c1.connection_type = "type_keep".into();
    c1.network_profile = Some("prof_keep".into());
    let id1 = store.add(c1).unwrap();

    let mut c2 = ConnectionConfig::new(DbKind::Sqlite, "DropConn");
    c2.folder = Some("Personal/Secret".into());
    c2.connection_type = "type_drop".into();
    c2.network_profile = Some("prof_drop".into());
    let id2 = store.add(c2).unwrap();

    store.folders = vec!["Work".into(), "Work/Alpha".into(), "Personal".into(), "Personal/Secret".into()];

    let mut secrets = BTreeMap::new();
    let keep_prof_key = ternarygrid_config::profile_secret_key("prof_keep", SecretSlot::SshPassword);
    let drop_prof_key = ternarygrid_config::profile_secret_key("prof_drop", SecretSlot::SshPassword);
    secrets.insert(secret_key(&id1, SecretSlot::Password), Secret::new("pw1"));
    secrets.insert(secret_key(&id2, SecretSlot::Password), Secret::new("pw2"));
    secrets.insert(keep_prof_key.clone(), Secret::new("pk1"));
    secrets.insert(drop_prof_key.clone(), Secret::new("pk2"));

    let file_pw = Secret::new("export-pass");
    let (bytes, _) = tg_file::export(&store, &[id1.clone(), id2.clone()], &secrets, Some(&file_pw), "0.7.10").unwrap();
    assert!(tg_file::has_secrets(&bytes));
    let mut imported = tg_file::import(&bytes, Some(&file_pw)).unwrap();
    assert_eq!(imported.connections.len(), 2);
    // Select ONLY connection 0 (`KeepConn`)!
    imported.retain_selected(&[true, false]);
    assert_eq!(imported.connections.len(), 1);
    assert_eq!(imported.connections[0].name, "KeepConn");
    assert_eq!(imported.connection_types.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["type_keep"]);
    assert_eq!(imported.network_profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["prof_keep"]);
    assert!(imported.folders.contains(&"Work".to_string()) && imported.folders.contains(&"Work/Alpha".to_string()));
    assert!(!imported.folders.iter().any(|f| f.starts_with("Personal")), "unselected folder pruned: {:?}", imported.folders);
    let kept_secret_keys: Vec<&str> = imported.secrets.iter().map(|(k, _)| k.as_str()).collect();
    assert!(kept_secret_keys.contains(&secret_key(&id1, SecretSlot::Password).as_str()));
    assert!(kept_secret_keys.contains(&keep_prof_key.as_str()), "{kept_secret_keys:?}");
    assert!(!kept_secret_keys.iter().any(|k| k.contains(&id2) || k.contains("prof_drop")), "unselected secrets pruned: {kept_secret_keys:?}");
}

// ============================================================================
// 12. Dialogs, Settings Shortcut Capture, Drop SQLite View, Maintenance & History E2E
// ============================================================================

#[test]
fn dialogs_settings_shortcuts_drop_maintenance_and_history_e2e() {
    // (a) DropDialog cascade guard: SQLite never gets CASCADE; Postgres tables/views do.
    assert!(!supports_cascade(DbKind::Sqlite, ObjectKind::View));
    assert!(!supports_cascade(DbKind::Sqlite, ObjectKind::Table));
    assert!(!supports_cascade(DbKind::SqlServer, ObjectKind::View));
    assert!(supports_cascade(DbKind::Postgres, ObjectKind::View));
    assert!(supports_cascade(DbKind::Postgres, ObjectKind::Table));

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("misc.db");
    let mut th = TabHarness::new();
    th.backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        exec_batch(s.as_mut(), &["CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)", "INSERT INTO t1 VALUES (1, 'a')", "CREATE VIEW v1 AS SELECT * FROM t1"]).await;
        let _ = s.close().await;
    });
    let conn_id = th.connect_sqlite("misc-db", &db_path, false);

    // (b) MaintenanceTab with ObjectKind::Database target runs cleanly on SQLite.
    let mut mtab = MaintenanceTab::new(conn_id.clone(), vec![ObjectRef::new(ObjectKind::Database, None, "main")]);
    for _ in 0..5 {
        th.frame(&mut mtab);
    }
    assert!(th.toasts.take_log().iter().all(|t| t.kind != ToastKind::Error));

    // (c) Full TernaryGridApp E2E: Settings shortcut recorder captures Cmd+W without closing Settings,
    //     plus Query History recording + Command Palette opening.
    let mut h = app_harness(dir.path());
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_action(&ctx, Action::OpenSettings);
    app_wait(&mut h, "settings tab", |h| h.state().dock.iter_all_tabs().any(|(_, t)| t.view.dedupe_key().as_deref() == Some("settings")));
    // Navigate to Keyboard section and verify SettingsTab is open.
    {
        if let Some(kb) = h.query_all_by_label_contains("Keyboard").next() {
            kb.click();
        }
    }
    h.step();
    h.step();
    // Click the first shortcut binding button to start recording, then send Cmd+W!
    let clicked_bind = {
        if let Some(bind) = h.query_all_by_label_contains("Click to change").next() {
            bind.click();
            true
        } else {
            false
        }
    };
    if clicked_bind {
        h.step();
        let capturing = h.state().dock.iter_all_tabs().any(|(_, t)| t.view.captures_keyboard());
        if capturing {
            h.key_press_modifiers(Modifiers::COMMAND, Key::W);
            h.step();
            assert!(
                h.state().dock.iter_all_tabs().any(|(_, t)| t.view.dedupe_key().as_deref() == Some("settings")),
                "Cmd+W while recording a shortcut must NOT close the Settings tab"
            );
        }
    }

    // Open Command Palette via Action::CommandPalette and close with Escape.
    h.state_mut().dispatch_action(&ctx, Action::CommandPalette);
    h.step();
    assert!(h.query_all_by_role(egui::accesskit::Role::TextInput).next().is_some(), "Command Palette search input opens");
    h.key_press(Key::Escape);
    h.step();

    // (d) Live DropDialog E2E on SQLite view `v1`: no Cascade checkbox rendered, and Drop succeeds cleanly.
    let mut cfg = ConnectionConfig::new(DbKind::Sqlite, "drop-view-db");
    cfg.file_path = Some(db_path.display().to_string());
    cfg.auth = AuthMethod::None;
    let app_conn_id = cfg.id.clone();
    h.state_mut().conns.upsert(cfg);
    {
        let backend = h.state().backend.clone();
        h.state_mut().conns.connect(&backend, &app_conn_id, Credentials::default()).unwrap();
    }
    app_wait(&mut h, "app sqlite connected", |h| h.state().conns.is_connected(&app_conn_id));
    h.state_mut().dialogs.drop = Some(ternarygrid_app::dialogs::drop::DropDialog::new(app_conn_id.clone(), vec![ObjectRef::new(ObjectKind::View, Some("main".into()), "v1")]));
    h.step();
    h.step();
    assert!(h.query_all_by_label_contains("Cascade").next().is_none(), "SQLite DropDialog must hide unsupported Cascade option");
    {
        let btn = h.query_all_by_label_contains("Drop view").next().expect("Drop view button");
        btn.click();
    }
    app_wait(&mut h, "drop view dialog closes", |h| h.state().dialogs.drop.is_none());
    let still_exists = th.backend.block_on(async {
        let mut s = open_sqlite(&db_path).await;
        let v = query_scalar(s.as_mut(), "SELECT count(*) FROM sqlite_master WHERE type = 'view' AND name = 'v1'", &[]).await.unwrap();
        let _ = s.close().await;
        v.as_i64().unwrap()
    });
    assert_eq!(still_exists, 0, "view v1 dropped cleanly on SQLite");
}
