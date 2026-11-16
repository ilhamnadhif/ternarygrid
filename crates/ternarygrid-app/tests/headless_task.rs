//! `ternarygrid --run-task`: a saved SQL script task on a saved SQLite connection
//! runs headless and reports its status through the exit code. Uses a
//! throw-away configuration directory (no Keychain access: SQLite needs no
//! secrets and only the secrets a connection uses are read).

use ternarygrid_app::backend::Backend;
use ternarygrid_config::{AppPaths, ConnectionStore};
use ternarygrid_tasks::{TaskDef, TaskKind, TaskStore};

#[test]
fn run_task_headless() {
    let root = std::env::temp_dir().join(format!("ternarygrid-headless-{}", uuid::Uuid::new_v4().simple()));
    let paths = AppPaths::with_root(&root);
    paths.ensure_dirs().unwrap();
    let db = root.join("tasks.db");

    let mut store = ConnectionStore::new();
    let mut cfg = ternarygrid_core::config::ConnectionConfig::new(ternarygrid_core::DbKind::Sqlite, "task db");
    cfg.file_path = Some(db.display().to_string());
    cfg.auth = ternarygrid_core::config::AuthMethod::None;
    let conn_id = store.add(cfg).unwrap();
    store.save(&paths).unwrap();

    let tasks = TaskStore::open(paths.tasks_dir()).unwrap();
    let ok = TaskDef::new(
        "nightly",
        TaskKind::SqlScript {
            connection_id: conn_id.clone(),
            database: None,
            schema: None,
            sql: Some("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);\nINSERT INTO t VALUES (1, 'a'), (2, 'b');".into()),
            script_path: None,
            stop_on_error: true,
            commit_at_end: true,
        },
    );
    tasks.create(ok).unwrap();
    let bad = TaskDef::new(
        "broken",
        TaskKind::SqlScript {
            connection_id: conn_id,
            database: None,
            schema: None,
            sql: Some("SELEC nonsense;".into()),
            script_path: None,
            stop_on_error: true,
            commit_at_end: true,
        },
    );
    tasks.create(bad).unwrap();
    drop(tasks);

    let backend = Backend::new().unwrap();
    let settings = ternarygrid_config::AppSettings::default();
    let code = ternarygrid_app::headless::run_task(&backend, &paths, &settings, "nightly");
    assert_eq!(format!("{code:?}"), format!("{:?}", std::process::ExitCode::from(0)), "successful task exits with 0");
    let code = ternarygrid_app::headless::run_task(&backend, &paths, &settings, "broken");
    assert_eq!(format!("{code:?}"), format!("{:?}", std::process::ExitCode::from(1)), "failed task exits with 1");
    let code = ternarygrid_app::headless::run_task(&backend, &paths, &settings, "missing");
    assert_eq!(format!("{code:?}"), format!("{:?}", std::process::ExitCode::from(4)), "unknown task exits with 4");

    // The task really ran: two rows in the table, and the run log has both runs.
    let conn = backend.block_on(async {
        let d = ternarygrid_driver_sqlite::SqliteDriver::new();
        let mut c = ternarygrid_core::config::ConnectionConfig::new(ternarygrid_core::DbKind::Sqlite, "check");
        c.file_path = Some(db.display().to_string());
        ternarygrid_core::Driver::connect(&d, &ternarygrid_core::ConnectParams::from_config(&c, None)).await
    });
    let mut s = conn.unwrap();
    let n = backend.block_on(ternarygrid_core::driver::query_scalar(s.as_mut(), "SELECT COUNT(*) FROM t", &[])).unwrap();
    assert_eq!(n.as_i64(), Some(2));
    let log = ternarygrid_tasks::RunLog::open(paths.tasks_dir(), Default::default()).unwrap();
    assert_eq!(log.runs().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&root);
}

/// A task script on a read-only connection with a statement that writes is refused before
/// anything runs: SQL Server and Oracle sessions do not stop writes themselves, so the
/// connection's flag was the only guard, and tasks did not look at it.
#[test]
fn a_read_only_connection_refuses_a_writing_task_script() {
    use ternarygrid_core::Driver;
    let backend = Backend::new().unwrap();
    backend.block_on(async {
        let mut cfg = ternarygrid_core::config::ConnectionConfig::new(ternarygrid_core::DbKind::Sqlite, "ro");
        cfg.file_path = Some(":memory:".into());
        let mut s = ternarygrid_driver_sqlite::SqliteDriver::new().connect(&ternarygrid_core::ConnectParams::from_config(&cfg, None)).await.unwrap();
        ternarygrid_core::driver::execute_update(s.as_mut(), "CREATE TABLE t (id INTEGER); INSERT INTO t VALUES (1)", &[]).await.unwrap();
        let ctx = ternarygrid_tasks::RunContext::new("run", "task", ternarygrid_tasks::Trigger::Manual, tokio_util::sync::CancellationToken::new(), std::sync::Arc::new(|_| {}));
        let e = ternarygrid_app::taskexec::run_script(&ctx, s.as_mut(), "SELECT count(*) FROM t;\nDELETE FROM t", None, None, true, true, Some("production")).await.unwrap_err();
        assert!(e.message.contains("read-only") && e.message.contains("statement 2"), "{}", e.full_text());
        let (_, rows) = ternarygrid_core::driver::query_all(s.as_mut(), "SELECT count(*) FROM t", &[]).await.unwrap();
        assert_eq!(rows[0][0].as_i64(), Some(1), "nothing ran");
        // Reads run.
        ternarygrid_app::taskexec::run_script(&ctx, s.as_mut(), "SELECT count(*) FROM t", None, None, true, true, Some("production")).await.unwrap();
    });
}
