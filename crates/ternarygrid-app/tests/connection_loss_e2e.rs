//! Connections that die without a word (the computer slept and the connection went
//! half-open: what is sent is accepted, nothing comes back), that are cut (a server
//! restart), or that are reconnected while a table loads: the connection's status
//! tells the truth, it connects again by itself, and open tables read on without
//! being closed. The connection goes through a relay the test black-holes or cuts.
//! Servers: `TERNARYGRID_TEST_PG`, `TERNARYGRID_TEST_MYSQL`, `TERNARYGRID_TEST_MARIADB`.

#[path = "support/relay.rs"]
mod relay;

use relay::RelayNet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use ternarygrid_app::backend::Backend;
use ternarygrid_app::command::AppCommand;
use ternarygrid_app::conn::{ConnStatus, ConnectionManager, Credentials, MemorySecrets, SecretSource};
use ternarygrid_app::cx::AppContext;
use ternarygrid_app::keys::Keymap;
use ternarygrid_app::ops::Operations;
use ternarygrid_app::results::data_tab::TableDataView;
use ternarygrid_app::services::{Services, Settings};
use ternarygrid_app::toasts::Toasts;
use ternarygrid_core::config::{SecretSlot, secret_key};
use ternarygrid_core::meta::ObjectRef;

fn servers() -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (name, var) in [("postgres", "TERNARYGRID_TEST_PG"), ("mysql", "TERNARYGRID_TEST_MYSQL"), ("mariadb", "TERNARYGRID_TEST_MARIADB")] {
        if let Ok(url) = std::env::var(var)
            && !url.trim().is_empty()
        {
            out.push((name, url));
        }
    }
    out
}

struct H {
    name: &'static str,
    backend: Arc<Backend>,
    ctx: egui::Context,
    cm: ConnectionManager,
    net: Arc<RelayNet>,
    services: Services,
    settings: Settings,
    keymap: Keymap,
    toasts: Toasts,
    ops: Operations,
    commands: Vec<AppCommand>,
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
    fn new(name: &'static str, url: &str) -> H {
        Self::new_with(name, url, |_| {})
    }

    fn new_with(name: &'static str, url: &str, adjust: impl FnOnce(&mut ternarygrid_core::config::ConnectionConfig)) -> H {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let backend = Arc::new(Backend::new().unwrap());
        let ctx = egui::Context::default();
        backend.set_context(ctx.clone());
        let (mut cfg, password) = ternarygrid_config::parse_connection_url(url).unwrap();
        adjust(&mut cfg);
        cfg.save_password = true;
        if password.is_none() {
            cfg.auth = ternarygrid_core::config::AuthMethod::None;
        }
        let secrets = Arc::new(MemorySecrets::default());
        if let Some(pw) = &password {
            secrets.set(&secret_key(&cfg.id, SecretSlot::Password), pw).unwrap();
        }
        let net = Arc::new(RelayNet::default());
        let mut cm = ConnectionManager::new(ternarygrid_app::driver_registry(), secrets, net.clone());
        // Quick checks: the relay answers at once or never.
        cm.check_timeout = Duration::from_secs(2);
        let id = cfg.id.clone();
        cm.upsert(cfg);
        cm.connect(&backend, &id, Credentials::default()).unwrap();
        let t0 = Instant::now();
        while !cm.is_connected(&id) {
            assert!(cm.errors().is_empty(), "{:?}", cm.errors());
            assert!(t0.elapsed() < Duration::from_secs(20));
            cm.poll(&backend);
            std::thread::sleep(Duration::from_millis(10));
        }
        let dir = std::env::temp_dir().join(format!("ternarygrid-loss-e2e-home-{}", uuid::Uuid::new_v4().simple()));
        let services = Services::ephemeral(&dir);
        let settings = Settings::default();
        let (keymap, _) = Keymap::from_config(&settings.keymap, ternarygrid_app::keys::IS_MAC);
        H { name, backend, ctx, cm, net, services, settings, keymap, toasts: Toasts::default(), ops: Operations::default(), commands: Vec::new(), time: 0.0, id, dir }
    }

    fn frame(&mut self, view: &mut TableDataView) {
        self.time += 1.0 / 60.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))),
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
            view.ui(ui, &mut cx);
        });
        out.textures_delta.clear();
        self.cm.poll(&self.backend);
    }

    /// Frames for `d`.
    fn frames_for(&mut self, view: &mut TableDataView, d: Duration) {
        let t0 = Instant::now();
        while t0.elapsed() < d {
            self.frame(view);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn until(&mut self, view: &mut TableDataView, what: &str, mut done: impl FnMut(&mut TableDataView) -> bool) {
        let t0 = Instant::now();
        loop {
            self.frame(view);
            if done(view) {
                return;
            }
            assert!(t0.elapsed() < Duration::from_secs(40), "{}: timed out waiting for {what} (error: {:?})", self.name, view.error());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Poll the connections (no tab) until `done`.
    fn poll_until(&mut self, what: &str, limit: Duration, mut done: impl FnMut(&ConnectionManager, &str) -> bool) {
        let t0 = Instant::now();
        loop {
            self.cm.poll(&self.backend);
            if done(&self.cm, &self.id) {
                return;
            }
            assert!(t0.elapsed() < limit, "{}: timed out waiting for {what} (status {:?})", self.name, self.cm.status(&self.id));
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn main(&self) -> ternarygrid_app::conn::session::SessionHandle {
        self.cm.live(&self.id).unwrap().main.clone()
    }

    fn sql(&self, sql: &str) {
        let s = self.main();
        let owned = sql.to_string();
        self.backend.block_on(async move { s.execute_update(&owned, &[]).await }).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    fn schema(&self) -> Option<String> {
        let live = self.cm.live(&self.id).unwrap();
        live.current_schema.clone().or(live.current_database.clone())
    }

    /// Reconnect, as the navigator's Reconnect does (no transaction is open here).
    fn reconnect(&mut self) {
        self.cm.disconnect(&self.backend, &self.id);
        self.cm.connect(&self.backend, &self.id, Credentials::default()).unwrap();
    }

    fn relay(&self) -> Arc<relay::Relay> {
        self.net.relay()
    }

    /// A view whose rows take `secs` to come (the server sleeps first).
    fn slow_view(&self, secs: u32) -> String {
        let view = format!("tg_slow_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let sleep = if self.name == "postgres" { format!("pg_sleep({secs})::text") } else { format!("SLEEP({secs})") };
        self.sql(&format!("CREATE VIEW {view} AS SELECT {sleep} AS s, 1 AS id"));
        view
    }

    fn numbered_table(&self, n: usize) -> String {
        let table = format!("tg_rows_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        self.sql(&format!("CREATE TABLE {table} (id INT PRIMARY KEY)"));
        let values: Vec<String> = (1..=n).map(|i| format!("({i})")).collect();
        for chunk in values.chunks(500) {
            self.sql(&format!("INSERT INTO {table} (id) VALUES {}", chunk.join(", ")));
        }
        table
    }
}

/// Ends the test run when a stage hangs longer than `limit` (a hang would otherwise
/// block CI without telling where).
fn watchdog(test: &'static str, limit: Duration) -> Arc<std::sync::Mutex<String>> {
    let stage = Arc::new(std::sync::Mutex::new(String::from("start")));
    let watched = stage.clone();
    std::thread::spawn(move || {
        let (mut last, mut since) = (String::from("start"), Instant::now());
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let now = watched.lock().unwrap().clone();
            if now.is_empty() {
                return;
            }
            if now != last {
                (last, since) = (now, Instant::now());
            }
            if since.elapsed() > limit {
                use std::io::Write;
                let _ = writeln!(std::io::stderr(), "{test} hung at: {last} (for {:?})", since.elapsed());
                std::process::exit(101);
            }
        }
    });
    stage
}

fn ids(view: &TableDataView) -> Vec<i64> {
    let p = view.panel.as_ref().unwrap();
    let col = p.model.columns().iter().position(|c| c.name.eq_ignore_ascii_case("id")).unwrap();
    (0..p.model.row_count()).filter_map(|r| p.model.cell(r, col).and_then(|v| v.as_i64())).collect()
}

/// After the computer slept: a connection that answers stays green (and an open
/// transaction of a tab stays); one that went silent turns red, connects again by
/// itself and turns green; one that was cut (server restart) does too.
#[test]
fn a_silent_connection_turns_red_and_connects_again() {
    for (name, url) in servers() {
        let stage = watchdog("a_silent_connection_turns_red_and_connects_again", Duration::from_secs(60));
        let set = |s: &str| *stage.lock().unwrap() = format!("{name}: {s}");
        let mut h = H::new(name, &url);
        let table = format!("tg_loss_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        h.sql(&format!("CREATE TABLE {table} (id INT)"));

        set("a healthy connection is checked");
        let tab = h.backend.block_on(h.cm.live(&h.id).unwrap().open_session_future("tab".into(), None)).unwrap();
        h.backend.block_on(tab.set_transaction_mode(ternarygrid_core::driver::TransactionMode::Manual)).unwrap();
        h.backend.block_on(tab.execute_update(&format!("INSERT INTO {table} VALUES (1)"), &[])).unwrap();
        assert!(tab.has_uncommitted_writes());
        let main = h.main();
        let epoch = h.cm.epoch(&h.id);
        h.cm.check_all(&h.backend);
        assert_eq!(h.cm.status(&h.id), ConnStatus::Checking, "{name}");
        h.poll_until("checked", Duration::from_secs(10), |cm, id| cm.status(id) == ConnStatus::Connected);
        assert!(h.main().same(&main) && h.cm.epoch(&h.id) == epoch, "{name}: a connection that answers is kept");
        assert!(!tab.is_closed() && tab.has_uncommitted_writes(), "{name}: the tab's transaction is kept");
        h.backend.block_on(tab.rollback()).unwrap();

        set("the connection goes silent");
        h.relay().freeze_existing();
        let t0 = Instant::now();
        h.cm.check_all(&h.backend);
        h.poll_until("red", Duration::from_secs(10), |cm, id| cm.status(id) == ConnStatus::Lost || !cm.live(id).unwrap().main.same(&main));
        assert!(t0.elapsed() < Duration::from_secs(8), "{name}: found within the check's time: {:?}", t0.elapsed());
        assert!(tab.is_lost(), "{name}: the tab's session on the dead path is given up too");
        set("connects again");
        h.poll_until("green again", Duration::from_secs(20), |cm, id| cm.status(id) == ConnStatus::Connected && !cm.live(id).unwrap().main.same(&main));
        assert_ne!(h.cm.epoch(&h.id), epoch);
        let ping = h.backend.block_on(h.main().ping());
        assert!(ping.is_ok(), "{name}: {ping:?}");

        set("the connection is cut");
        let main = h.main();
        h.relay().cut_existing();
        h.cm.check_all(&h.backend);
        h.poll_until("red after the cut", Duration::from_secs(10), |cm, id| cm.status(id) == ConnStatus::Lost || !cm.live(id).unwrap().main.same(&main));
        h.poll_until("green after the cut", Duration::from_secs(20), |cm, id| cm.status(id) == ConnStatus::Connected && !cm.live(id).unwrap().main.same(&main));
        h.sql(&format!("DROP TABLE {table}"));
        set("");
    }
}

/// The user's case: a table is loading ("Reading data…") when its connection dies;
/// Reconnect brings the rows without closing the tab.
#[test]
fn reconnecting_while_a_table_loads_brings_its_rows() {
    for (name, url) in servers() {
        let stage = watchdog("reconnecting_while_a_table_loads_brings_its_rows", Duration::from_secs(60));
        let set = |s: &str| *stage.lock().unwrap() = format!("{name}: {s}");
        let mut h = H::new(name, &url);
        let view_name = h.slow_view(2);
        let mut view = TableDataView::new(h.id.clone(), ObjectRef::table(h.schema(), view_name.clone()), None);
        set("loading");
        h.frames_for(&mut view, Duration::from_millis(800));
        assert!(view.is_loading(), "{name}: the query runs");
        h.relay().freeze_existing();
        h.frames_for(&mut view, Duration::from_secs(3));
        assert!(view.panel.is_none() && view.is_loading(), "{name}: the rows cannot come through the dead connection");
        set("reconnect");
        h.reconnect();
        h.until(&mut view, "the rows after Reconnect", |v| v.panel.as_ref().is_some_and(|p| p.model.row_count() == 1));
        assert!(view.error().is_none(), "{name}: {:?}", view.error());
        h.sql(&format!("DROP VIEW {view_name}"));
        set("");
    }
}

/// A table loading through a connection that went silent: Cancel stops it at once,
/// and Retry reads it through a new session.
#[test]
fn cancelling_a_load_that_never_comes_and_retrying() {
    for (name, url) in servers() {
        let stage = watchdog("cancelling_a_load_that_never_comes_and_retrying", Duration::from_secs(60));
        let set = |s: &str| *stage.lock().unwrap() = format!("{name}: {s}");
        let mut h = H::new(name, &url);
        let view_name = h.slow_view(1);
        let main = h.main();
        let mut view = TableDataView::new(h.id.clone(), ObjectRef::table(h.schema(), view_name.clone()), None);
        set("loading");
        h.frames_for(&mut view, Duration::from_millis(500));
        assert!(view.is_loading());
        h.relay().freeze_existing();
        h.frames_for(&mut view, Duration::from_secs(2));
        assert!(view.panel.is_none(), "{name}: nothing comes");
        set("cancel");
        let t0 = Instant::now();
        view.cancel_loading(&h.backend);
        h.until(&mut view, "the cancel", |v| v.error().is_some() && !v.is_loading());
        assert!(t0.elapsed() < Duration::from_secs(2), "{name}: Cancel stops at once: {:?}", t0.elapsed());
        set("retry");
        view.retry();
        h.until(&mut view, "the rows after Retry", |v| v.panel.as_ref().is_some_and(|p| p.model.row_count() == 1));
        set("");
        // The navigator's session went silent too: dropped when the connection is checked.
        h.cm.check_all(&h.backend);
        h.poll_until("green again", Duration::from_secs(20), |cm, id| cm.status(id) == ConnStatus::Connected && !cm.live(id).unwrap().main.same(&main));
        h.sql(&format!("DROP VIEW {view_name}"));
    }
}

/// An open table after the computer slept: its connection is found dead, connects again,
/// and the next page continues the rows through a new session (none twice, none missing).
#[test]
fn an_open_table_reads_on_after_a_sleep() {
    for (name, url) in servers() {
        let stage = watchdog("an_open_table_reads_on_after_a_sleep", Duration::from_secs(60));
        let set = |s: &str| *stage.lock().unwrap() = format!("{name}: {s}");
        let mut h = H::new(name, &url);
        let table = h.numbered_table(1000);
        let mut view = TableDataView::new(h.id.clone(), ObjectRef::table(h.schema(), table.clone()), None);
        set("first page");
        h.until(&mut view, "the first page", |v| v.panel.as_ref().is_some_and(|p| p.model.row_count() >= 200));
        // The grid reads a page ahead: the computer sleeps once it is idle.
        let mut last = (0, Instant::now());
        h.until(&mut view, "the grid idle", |v| {
            let p = v.panel.as_ref().unwrap();
            let n = p.model.row_count();
            if n != last.0 || p.src.is_fetching() {
                last = (n, Instant::now());
            }
            last.1.elapsed() > Duration::from_millis(500)
        });
        let first = view.panel.as_ref().unwrap().src.session.clone();
        set("sleep");
        h.relay().freeze_existing();
        h.cm.check_all(&h.backend);
        h.until(&mut view, "a new session for the table", |v| {
            let p = v.panel.as_ref().unwrap();
            !p.src.session.same(&first)
        });
        assert!(first.is_lost(), "{name}: the table's old session was given up");
        set("next page");
        let backend = h.backend.clone();
        view.panel.as_mut().unwrap().src.fetch_more(&backend, 200, None);
        h.until(&mut view, "the next page", |v| v.panel.as_ref().is_some_and(|p| p.model.row_count() >= 400 && !p.src.is_fetching()));
        assert!(view.panel.as_ref().unwrap().src.error.is_none(), "{name}: {:?}", view.panel.as_ref().unwrap().src.error);
        let n = view.panel.as_ref().unwrap().model.row_count() as i64;
        assert_eq!(ids(&view), (1..=n).collect::<Vec<_>>(), "{name}: the rows continue, none twice, none missing");
        h.poll_until("green again", Duration::from_secs(20), |cm, id| cm.status(id) == ConnStatus::Connected);
        h.sql(&format!("DROP TABLE {table}"));
        set("");
    }
}

/// Metadata read straight on the driver (the navigator) waits on a connection that
/// went silent: giving the session up ends it at once (PostgreSQL drops its connection).
#[test]
fn a_navigator_read_on_a_silent_connection_ends() {
    let Some(url) = std::env::var("TERNARYGRID_TEST_PG").ok().filter(|u| !u.trim().is_empty()) else { return };
    let stage = watchdog("a_navigator_read_on_a_silent_connection_ends", Duration::from_secs(60));
    let mut h = H::new("postgres", &url);
    let main = h.main();
    h.relay().freeze_existing();
    let mut read = {
        let main = main.clone();
        h.backend.spawn(async move { main.lock().await.list_databases().await.map(|_| ()) })
    };
    std::thread::sleep(Duration::from_millis(500));
    assert!(!read.is_finished(), "the read waits on the silent connection");
    let t0 = Instant::now();
    main.abandon(true);
    while !read.is_finished() {
        assert!(t0.elapsed() < Duration::from_secs(5), "the read ends once the session is given up");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(read.poll().unwrap().is_err_and(|e| e.is_connection_lost()));
    h.poll_until("green again", Duration::from_secs(20), |cm, id| cm.status(id) == ConnStatus::Connected && !cm.live(id).unwrap().main.same(&main));
    *stage.lock().unwrap() = String::new();
}

/// With the keep-alive on, a connection that went silent is found without a sleep
/// being noticed (its ping gets no answer).
#[test]
fn the_keep_alive_finds_a_silent_connection() {
    let Some(url) = std::env::var("TERNARYGRID_TEST_PG").ok().filter(|u| !u.trim().is_empty()) else { return };
    let stage = watchdog("the_keep_alive_finds_a_silent_connection", Duration::from_secs(90));
    let mut h = H::new_with("postgres", &url, |cfg| cfg.init.keep_alive_secs = 5);
    let main = h.main();
    h.relay().freeze_existing();
    h.poll_until("red by the keep-alive", Duration::from_secs(45), |cm, id| cm.status(id) == ConnStatus::Lost || !cm.live(id).unwrap().main.same(&main));
    h.poll_until("green again", Duration::from_secs(20), |cm, id| cm.status(id) == ConnStatus::Connected && !cm.live(id).unwrap().main.same(&main));
    *stage.lock().unwrap() = String::new();
}
