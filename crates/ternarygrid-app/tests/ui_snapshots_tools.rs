//! Screenshots of the compare and administration tools (schema and data compare,
//! sessions, locks, dashboard, backup, restore, maintenance, truncate, password,
//! FDW, debugger, query builder) in the real application window, rasterised on the
//! CPU, for comparing them with the design mockups. Not part of the normal run:
//!
//! ```text
//! cargo test -p ternarygrid-app --test ui_snapshots_tools -- --ignored
//! ```
//!
//! writes `target/ui-snapshots/tools/<shot>-<light|dark>.png`. Server-only states
//! (sessions, locks, metrics, a stopped debugger) are filled in with sample data.

use egui::epaint::{ClippedPrimitive, Primitive};
use egui::{Color32, ColorImage, TextureId};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, TestRenderer};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use ternarygrid_app::app::TernaryGridApp;
use ternarygrid_app::backend::Backend;
use ternarygrid_app::command::AppCommand;
use ternarygrid_app::conn::{ConnectionManager, Credentials, DirectNet, MemorySecrets};
use ternarygrid_app::services::{Services, Settings};
use ternarygrid_config::settings::Theme;
use ternarygrid_core::admin::{LockList, Metric, SessionList, TabularData};
use ternarygrid_core::config::{AuthMethod, ConnectionConfig};
use ternarygrid_core::meta::{ObjectKind, ObjectRef, SchemaRef};
use ternarygrid_core::{DbKind, Value};

type H = Harness<'static, TernaryGridApp>;

/// Pixels per point of the screenshots (a Retina display).
const PPP: f32 = 2.0;

#[derive(Default)]
struct Raster {
    textures: HashMap<TextureId, ColorImage>,
}

impl Raster {
    fn apply(&mut self, delta: &egui::TexturesDelta) {
        for (id, deltas) in &delta.set {
            for d in deltas {
                let egui::ImageData::Color(img) = &d.image;
                match d.pos {
                    None => {
                        self.textures.insert(*id, (**img).clone());
                    }
                    Some([x, y]) => {
                        if let Some(t) = self.textures.get_mut(id) {
                            for row in 0..img.size[1] {
                                for col in 0..img.size[0] {
                                    let (tx, ty) = (x + col, y + row);
                                    if tx < t.size[0] && ty < t.size[1] {
                                        t.pixels[ty * t.size[0] + tx] = img.pixels[row * img.size[0] + col];
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    fn render(&self, prims: &[ClippedPrimitive], size: [usize; 2], bg: Color32) -> image::RgbaImage {
        let (w, h) = (size[0], size[1]);
        let mut fb: Vec<[f32; 4]> = vec![[bg.r() as f32, bg.g() as f32, bg.b() as f32, 255.0]; w * h];
        for p in prims {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            let Some(tex) = self.textures.get(&mesh.texture_id) else { continue };
            let clip = p.clip_rect;
            let (cx0, cy0) = ((clip.min.x * PPP).max(0.0) as i64, (clip.min.y * PPP).max(0.0) as i64);
            let (cx1, cy1) = (((clip.max.x * PPP).ceil() as i64).min(w as i64), ((clip.max.y * PPP).ceil() as i64).min(h as i64));
            for tri in mesh.indices.as_chunks::<3>().0 {
                let v = tri.map(|i| mesh.vertices[i as usize]);
                let pts = v.map(|v| (v.pos.x * PPP, v.pos.y * PPP));
                let minx = pts.iter().map(|p| p.0).fold(f32::INFINITY, f32::min).floor().max(cx0 as f32) as i64;
                let maxx = pts.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max).ceil().min(cx1 as f32) as i64;
                let miny = pts.iter().map(|p| p.1).fold(f32::INFINITY, f32::min).floor().max(cy0 as f32) as i64;
                let maxy = pts.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max).ceil().min(cy1 as f32) as i64;
                let area = (pts[1].0 - pts[0].0) * (pts[2].1 - pts[0].1) - (pts[2].0 - pts[0].0) * (pts[1].1 - pts[0].1);
                if area.abs() < 1e-9 {
                    continue;
                }
                // Each edge's pixels belong to one triangle only (a top-left style rule on the
                // edge's direction, both triangles of a shared edge see it reversed): else a
                // translucent quad's diagonal (a modal backdrop) is blended twice, a dotted seam.
                let owns = |a: (f32, f32), b: (f32, f32)| {
                    let (dx, dy) = ((b.0 - a.0) * area.signum(), (b.1 - a.1) * area.signum());
                    dy > 0.0 || (dy == 0.0 && dx > 0.0)
                };
                let edge = [owns(pts[1], pts[2]), owns(pts[2], pts[0]), owns(pts[0], pts[1])];
                for y in miny..maxy {
                    for x in minx..maxx {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let w0 = ((pts[1].0 - px) * (pts[2].1 - py) - (pts[2].0 - px) * (pts[1].1 - py)) / area;
                        let w1 = ((pts[2].0 - px) * (pts[0].1 - py) - (pts[0].0 - px) * (pts[2].1 - py)) / area;
                        let w2 = 1.0 - w0 - w1;
                        let outside = |w: f32, own: bool| w < -1e-5 || (w <= 1e-5 && !own);
                        if outside(w0, edge[0]) || outside(w1, edge[1]) || outside(w2, edge[2]) {
                            continue;
                        }
                        let u = w0 * v[0].uv.x + w1 * v[1].uv.x + w2 * v[2].uv.x;
                        let vv = w0 * v[0].uv.y + w1 * v[1].uv.y + w2 * v[2].uv.y;
                        let tx = ((u * tex.size[0] as f32) as usize).min(tex.size[0] - 1);
                        let ty = ((vv * tex.size[1] as f32) as usize).min(tex.size[1] - 1);
                        let t = tex.pixels[ty * tex.size[0] + tx];
                        let mut c = [0f32; 4];
                        for (k, ch) in c.iter_mut().enumerate() {
                            let vc = w0 * v[0].color[k] as f32 + w1 * v[1].color[k] as f32 + w2 * v[2].color[k] as f32;
                            *ch = vc * t[k] as f32 / 255.0;
                        }
                        let d = &mut fb[y as usize * w + x as usize];
                        let a = c[3] / 255.0;
                        for k in 0..4 {
                            d[k] = c[k] + d[k] * (1.0 - a);
                        }
                    }
                }
            }
        }
        let mut img = image::RgbaImage::new(w as u32, h as u32);
        for (i, px) in fb.iter().enumerate() {
            img.put_pixel((i % w) as u32, (i / w) as u32, image::Rgba([px[0] as u8, px[1] as u8, px[2] as u8, 255]));
        }
        img
    }
}

struct CpuTextures(Rc<RefCell<Raster>>);

impl TestRenderer for CpuTextures {
    fn handle_delta(&mut self, delta: &mut egui::TexturesDelta) {
        self.0.borrow_mut().apply(delta);
        delta.clear();
    }
}

struct Shots {
    h: H,
    raster: Rc<RefCell<Raster>>,
    dir: PathBuf,
    theme: &'static str,
    /// Source ("tplus-staging") and target ("slypee-prod") SQLite connections.
    a: String,
    b: String,
    /// A PostgreSQL and a MySQL connection (never connected: their forms and scripts).
    pg: String,
    my: String,
}

const SOURCE_DDL: &[&str] = &[
    "CREATE TABLE customer (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT, city TEXT, last_seen TEXT)",
    "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customer(id), status VARCHAR(32) DEFAULT 'open', total NUMERIC, promo_code TEXT, created_at TEXT)",
    "CREATE TABLE order_line (order_id INTEGER REFERENCES orders(id), quantity INTEGER, unit_price NUMERIC)",
    "CREATE TABLE coupon (code TEXT PRIMARY KEY, percent_off INTEGER NOT NULL)",
    "CREATE INDEX orders_promo_idx ON orders (promo_code)",
];

const TARGET_DDL: &[&str] = &[
    "CREATE TABLE customer (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT, city TEXT, last_seen TEXT)",
    "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customer(id), status VARCHAR(16), total NUMERIC, created_at TEXT)",
    "CREATE TABLE order_line (order_id INTEGER REFERENCES orders(id), quantity INTEGER, unit_price NUMERIC)",
    "CREATE VIEW v_legacy_sales AS SELECT id, total FROM orders",
];

const NAMES: &[(&str, &str)] = &[
    ("Rina Halim", "Medan"),
    ("Agus Pratama", "Yogyakarta"),
    ("Dewi Lestari", "Bandung"),
    ("Budi Santoso", "Jakarta"),
    ("Putri Ayu", "Denpasar"),
    ("Joko Widodo", "Solo"),
    ("Siti Rahma", "Makassar"),
    ("Andi Wijaya", "Semarang"),
];

fn customers(target: bool) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..40 {
        let id = 20 + i;
        let (name, city) = NAMES[i % NAMES.len()];
        let mut email = format!("{}@example.com", name.split(' ').next().unwrap_or("x").to_lowercase());
        let mut city = city.to_string();
        let mut seen = format!("2026-10-{:02}", 1 + i % 28);
        if target && id == 42 {
            email = "rina.halim@example.com".into();
            seen = "2026-11-01".into();
        }
        if target && id == 57 {
            city = "Jogja".into();
        }
        out.push(format!("INSERT INTO customer VALUES ({id}, '{name}', '{email}', '{city}', '{seen}')"));
    }
    if target {
        out.push("INSERT INTO customer VALUES (988, 'Test User', 'test@example.com', NULL, '2026-09-01')".into());
    } else {
        out.push("INSERT INTO customer VALUES (1201, 'Sari Wulandari', 'sari.w@example.com', 'Surabaya', '2026-11-02')".into());
        out.push("INSERT INTO customer VALUES (1202, 'Bayu Nugroho', NULL, 'Jakarta', '2026-11-02')".into());
    }
    out
}

impl Shots {
    fn new(home: &Path, dir: PathBuf, theme: Theme) -> Shots {
        let mut settings = Settings::default();
        settings.updates.check_automatically = Some(false);
        settings.general.restore_session = false;
        settings.general.onboarding_done = true;
        settings.general.show_tips = false;
        settings.general.theme = theme;
        let raster = Rc::new(RefCell::new(Raster::default()));
        let backend = Arc::new(Backend::new().unwrap());
        let services = Services::ephemeral(&home.join("home"));
        let mut conns = ConnectionManager::new(ternarygrid_app::driver_registry(), Arc::new(MemorySecrets::default()), Arc::new(DirectNet));
        let mut ids = Vec::new();
        for (name, file, prod) in [("tplus-staging", "staging.db", false), ("slypee-prod", "prod.db", true)] {
            let mut cfg = ConnectionConfig::new(DbKind::Sqlite, name);
            cfg.file_path = Some(home.join(file).display().to_string());
            cfg.auth = AuthMethod::None;
            if prod {
                cfg.connection_type = "prod".into();
            }
            ids.push(cfg.id.clone());
            conns.upsert(cfg);
        }
        let mut pg = ConnectionConfig::new(DbKind::Postgres, "bantuin");
        pg.host = "db.bantuin.internal".into();
        pg.port = Some(5432);
        pg.user = Some("app".into());
        pg.database = Some("bantuin".into());
        pg.save_password = true;
        let mut my = ConnectionConfig::new(DbKind::MySql, "tplus-staging-mysql");
        my.host = "staging.db.internal".into();
        my.port = Some(3306);
        my.user = Some("root".into());
        my.database = Some("shop".into());
        let (pg_id, my_id) = (pg.id.clone(), my.id.clone());
        conns.upsert(pg);
        conns.upsert(my);
        let h = Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .with_pixels_per_point(PPP)
            .with_max_steps(400)
            .with_step_dt(1.0 / 60.0)
            .renderer(CpuTextures(raster.clone()))
            .build_eframe(move |cc| {
                ternarygrid_app::theme::setup_fonts(&cc.egui_ctx);
                backend.set_context(cc.egui_ctx.clone());
                let mut app = TernaryGridApp::new(backend, conns, services, settings);
                // As on macOS (see `ui_snapshots_shell.rs`): the menus in the system's menu bar,
                // the window buttons at the left of the top bar (painted by `shot`).
                app.native_menu_bar = true;
                app.titlebar_inset = ternarygrid_app::app::MAC_TITLEBAR_INSET;
                app
            });
        ternarygrid_app::theme::apply_settings(&h.ctx, &h.state().settings);
        let theme = if theme == Theme::Dark { "dark" } else { "light" };
        let mut s = Shots { h, raster, dir, theme, a: ids[0].clone(), b: ids[1].clone(), pg: pg_id, my: my_id };
        s.frames(4);
        for (id, ddl, target) in [(ids[0].clone(), SOURCE_DDL, false), (ids[1].clone(), TARGET_DDL, true)] {
            let backend = s.h.state().backend.clone();
            s.h.state_mut().conns.connect(&backend, &id, Credentials::default()).unwrap();
            s.wait("connected", |h| h.state().conns.is_connected(&id));
            let session = s.h.state().conns.live(&id).unwrap().main.clone();
            let mut stmts: Vec<String> = ddl.iter().map(|s| s.to_string()).collect();
            stmts.extend(customers(target));
            backend
                .block_on(async move {
                    for st in stmts {
                        session.execute_update(&st, &[]).await?;
                    }
                    // The production connection works in manual commit mode.
                    if session.tx_state().in_transaction {
                        session.commit().await?;
                    }
                    Ok::<(), ternarygrid_core::DbError>(())
                })
                .unwrap();
        }
        s.frames(4);
        s
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.h.step();
            std::thread::sleep(Duration::from_millis(4));
        }
    }

    fn wait(&mut self, what: &str, mut cond: impl FnMut(&mut H) -> bool) {
        let t0 = Instant::now();
        while !cond(&mut self.h) {
            assert!(t0.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
            self.h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn shot(&mut self, name: &str) {
        self.h.state_mut().toasts.clear();
        // The pointer away from the content: no hover effects.
        self.h.event(egui::Event::PointerMoved(egui::pos2(1439.0, 899.0)));
        self.frames(30);
        let out = self.h.output();
        let prims = self.h.ctx.tessellate(out.shapes.clone(), PPP);
        let size = self.h.ctx.content_rect().size() * PPP;
        let bg = self.h.ctx.global_style().visuals.panel_fill;
        let mut img = self.raster.borrow().render(&prims, [size.x as usize, size.y as usize], bg);
        traffic_lights(&mut img);
        let path = self.dir.join(format!("{name}-{}.png", self.theme));
        img.save(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        eprintln!("wrote {}", path.display());
    }

    fn command(&mut self, cmd: AppCommand) {
        self.h.state_mut().push_command(cmd);
        self.frames(4);
    }

    /// Run `f` on the open tab or pop-up of type `T`.
    fn tab<T: 'static, R>(&mut self, f: impl FnOnce(&mut T) -> R) -> R {
        let app = self.h.state_mut();
        let found = app.popups.iter_mut().rev().find_map(|t| t.view.as_any_mut().downcast_mut::<T>().map(|v| v as *mut T));
        let found = found.or_else(|| app.dock.iter_all_tabs_mut().find_map(|(_, t)| t.view.as_any_mut().downcast_mut::<T>().map(|v| v as *mut T)));
        let ptr = found.expect("the tab is open");
        // SAFETY: the pointer comes from the app borrowed mutably just above and is used before
        // anything else touches the app.
        f(unsafe { &mut *ptr })
    }

    /// Close every tab and pop-up.
    fn close_all(&mut self) {
        let app = self.h.state_mut();
        let ids: Vec<u64> = app.popups.iter().map(|t| t.id).chain(app.dock.iter_all_tabs().map(|(_, t)| t.id)).collect();
        for id in ids {
            self.h.state_mut().close_tab(id);
        }
        self.h.state_mut().dialogs = Default::default();
        self.frames(6);
    }

    fn click(&mut self, label: &str) {
        {
            let n = self.h.query_by_label(label).unwrap_or_else(|| panic!("no widget labelled {label:?}"));
            n.click();
        }
        self.frames(3);
    }
}

/// The macOS window buttons, as the system paints them over the top bar's left end.
fn traffic_lights(img: &mut image::RgbaImage) {
    for (i, c) in [[0xff, 0x5f, 0x57], [0xfe, 0xbc, 0x2e], [0x28, 0xc8, 0x40]].iter().enumerate() {
        let (cx, cy, r) = ((16.0 + 6.0 + 20.0 * i as f32) * PPP, 22.0 * PPP, 6.0 * PPP);
        for y in (cy - r - 2.0) as u32..=(cy + r + 2.0) as u32 {
            for x in (cx - r - 2.0) as u32..=(cx + r + 2.0) as u32 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let a = (r + 0.5 - d).clamp(0.0, 1.0);
                if a > 0.0 {
                    let px = img.get_pixel_mut(x, y);
                    for k in 0..3 {
                        px[k] = (c[k] as f32 * a + px[k] as f32 * (1.0 - a)).round() as u8;
                    }
                }
            }
        }
    }
}

fn text(s: &str) -> Value {
    if s.is_empty() { Value::Null } else { Value::Text(s.into()) }
}

fn table(columns: &[&str], rows: &[&[&str]]) -> TabularData {
    let mut t = TabularData::new(columns.iter().map(|c| c.to_string()).collect());
    t.rows = rows.iter().map(|r| r.iter().map(|c| text(c)).collect()).collect();
    t
}

fn sample_sessions() -> SessionList {
    let data = table(
        &["pid", "user", "database", "application_name", "client_addr", "state", "wait_event_type", "blocked_by", "query_duration", "query"],
        &[
            &["18240", "app", "bantuin", "api", "10.0.2.14", "active", "", "", "1.2 s", "update orders set status = 'shipped' where id = $1"],
            &["18251", "app", "bantuin", "api", "10.0.2.15", "active", "Lock", "18240", "1.1 s", "update orders set total = total + $1 where id = $2"],
            &["18302", "reporting", "bantuin", "metabase", "10.0.3.9", "active", "IO", "", "48.6 s", "select date_trunc('day', created_at) …"],
            &["18310", "app", "bantuin", "worker", "10.0.2.20", "idle in transaction", "Client", "", "4 min", "select * from outbox for update skip locked"],
            &["18322", "ilham", "bantuin", "TernaryGrid", "10.0.9.2", "active", "", "", "0.0 s", "select pid, usename, … from pg_stat_activity"],
            &["18330", "app", "bantuin", "api", "10.0.2.16", "idle", "Client", "", "12 s", ""],
        ],
    );
    SessionList { data, id_column: 0, query_column: Some(9), state_column: Some(5), blocked_by_column: Some(7) }
}

fn sample_locks() -> LockList {
    let data = table(
        &["pid", "user", "locktype", "mode", "granted", "blocked_by", "relation", "query_duration", "query"],
        &[
            &["18251", "app", "transactionid", "ShareLock", "false", "18240", "", "1.1 s", "update orders set total = …"],
            &["18240", "app", "relation", "RowExclusiveLock", "true", "", "orders", "1.2 s", "update orders set status = …"],
            &["18330", "app", "tuple", "ExclusiveLock", "false", "18240", "orders", "0.4 s", "update orders set note = …"],
            &["18302", "reporting", "relation", "AccessShareLock", "true", "", "order_line", "48.6 s", "select date_trunc('day', …"],
        ],
    );
    LockList { data, session_column: 0, blocked_by_column: Some(5), granted_column: Some(4) }
}

/// Five minutes of metrics, one sample a second.
fn sample_metrics() -> Vec<(f64, Vec<Metric>)> {
    let wave = |t: f64, k: f64| ((t * 0.07 + k).sin() + (t * 0.023 + k * 2.0).sin() * 0.6 + (t * 0.31 + k).sin() * 0.25) / 1.85;
    let mut counters = [0.0f64; 6];
    (0..300)
        .map(|i| {
            let t = i as f64;
            let rates = [
                400.0 + 80.0 * wave(t, 1.0),
                3.0 + 2.0 * wave(t, 2.0),
                0.2 + 0.2 * wave(t, 3.0),
                0.1 + 0.1 * wave(t, 4.0),
                1200.0 + 500.0 * wave(t, 5.0),
                48000.0 + 6000.0 * wave(t, 6.0),
            ];
            for (c, r) in counters.iter_mut().zip(rates) {
                *c += r.max(0.0);
            }
            let m = vec![
                Metric::gauge("Connections", "connections.total", "Client connections", (38.0 + 4.0 * wave(t, 0.5)).round(), ""),
                Metric::gauge("Connections", "connections.active", "Active", (6.0 + 3.0 * wave(t, 1.5)).round().max(0.0), ""),
                Metric::gauge("Connections", "connections.idle_in_transaction", "Idle in transaction", (2.0 + 1.5 * wave(t, 2.5)).round().max(0.0), ""),
                Metric::gauge("Connections", "connections.waiting", "Waiting for locks", (1.0 + wave(t, 3.5)).round().max(0.0), ""),
                Metric::counter("Transactions", "xact.commit", "Commits", counters[0], ""),
                Metric::counter("Transactions", "xact.rollback", "Rollbacks", counters[1], ""),
                Metric::counter("Transactions", "xact.deadlocks", "Deadlocks", counters[2], ""),
                Metric::counter("Transactions", "xact.conflicts", "Recovery conflicts", counters[3], ""),
                Metric::counter("I/O", "io.blks_read", "Blocks read", counters[4], ""),
                Metric::counter("I/O", "io.blks_hit", "Blocks hit", counters[5], ""),
                Metric::gauge("I/O", "io.hit_ratio", "Cache hit ratio (server)", 99.0 + 0.5 * wave(t, 7.0), "%"),
                Metric::gauge("I/O", "io.db_hit_ratio", "Cache hit ratio (database)", 98.0 + 0.8 * wave(t, 8.0), "%"),
            ];
            (t, m)
        })
        .collect()
}

fn main_schema() -> SchemaRef {
    SchemaRef::new(None, Some("main".into()))
}

fn shots(s: &mut Shots) {
    let (a, b) = (s.a.clone(), s.b.clone());

    // Schema compare: the differences, a modified table's properties, the script.
    s.command(AppCommand::OpenSchemaCompare { connection: Some(a.clone()), schema: Some(main_schema()) });
    s.shot("schema_compare_empty");
    s.tab(|t: &mut ternarygrid_app::tabs::compare::SchemaCompareTab| t.set_sides((&a, main_schema()), (&b, main_schema())));
    s.frames(3);
    s.click("Compare");
    s.wait("schema diff", |h| {
        h.state_mut()
            .dock
            .iter_all_tabs_mut()
            .any(|(_, t)| t.view.as_any_mut().downcast_mut::<ternarygrid_app::tabs::compare::SchemaCompareTab>().is_some_and(|c| c.diff().is_some()))
    });
    s.tab(|t: &mut ternarygrid_app::tabs::compare::SchemaCompareTab| t.select_first_modified());
    s.shot("schema_compare");
    s.tab(|t: &mut ternarygrid_app::tabs::compare::SchemaCompareTab| t.ask_apply());
    s.shot("schema_apply");
    s.close_all();

    // Data compare: the differing rows, the sync script, the confirmation.
    type Dc = ternarygrid_app::tabs::data_compare::DataCompareTab;
    let customer = ObjectRef::table(Some("main".into()), "customer");
    s.command(AppCommand::OpenDataCompare { connection: Some(a.clone()), table: Some(customer.clone()) });
    s.tab(|t: &mut Dc| t.set_target(&b, customer.clone()));
    s.frames(20);
    s.tab(|t: &mut Dc| t.request_compare());
    s.wait("data diff", |h| {
        h.state_mut().dock.iter_all_tabs_mut().any(|(_, t)| t.view.as_any_mut().downcast_mut::<Dc>().is_some_and(|c| c.diff().is_some() || c.error().is_some()))
    });
    s.shot("data_compare");
    s.tab(|t: &mut Dc| t.show_script(true));
    s.shot("data_sync");
    s.tab(|t: &mut Dc| t.ask_apply());
    s.shot("data_apply");
    s.close_all();

    // Sessions (a row selected, Terminate asked), locks, the dashboard.
    s.command(AppCommand::OpenSessionManager { connection: a.clone() });
    s.frames(10);
    s.tab(|t: &mut ternarygrid_app::tabs::admin::SessionManagerTab| t.show_sample(sample_sessions(), false, Some("18251"), false));
    s.shot("sessions");
    s.tab(|t: &mut ternarygrid_app::tabs::admin::SessionManagerTab| t.show_sample(sample_sessions(), true, Some("18251"), true));
    s.shot("sessions_terminate");
    s.command(AppCommand::OpenLockManager { connection: a.clone() });
    s.frames(10);
    s.tab(|t: &mut ternarygrid_app::tabs::admin::LockManagerTab| t.show_sample(sample_locks(), false));
    s.shot("locks");
    s.command(AppCommand::OpenDashboard { connection: a.clone() });
    s.frames(10);
    let metrics = sample_metrics();
    s.tab(|t: &mut ternarygrid_app::tabs::admin::DashboardTab| t.show_sample(&metrics));
    s.shot("dashboard");
    s.close_all();

    // Backup of the PostgreSQL connection (not connected) and of the SQLite one.
    type Bk = ternarygrid_app::tabs::dbtools::BackupTab;
    let pg = s.pg.clone();
    s.command(AppCommand::OpenBackup { connection: pg.clone(), schema: None, restore: false });
    s.shot("backup_pg");
    s.close_all();
    s.command(AppCommand::OpenBackup { connection: a.clone(), schema: Some(main_schema()), restore: false });
    s.shot("backup");
    s.close_all();
    // Restore: the form, then its confirmation.
    s.command(AppCommand::OpenBackup { connection: pg.clone(), schema: None, restore: true });
    s.tab(|t: &mut Bk| t.restore_form_mut().input = "~/bantuin-bantuin-20261102-1931.dump".into());
    s.shot("restore_pg");
    s.close_all();
    s.command(AppCommand::OpenBackup { connection: a.clone(), schema: Some(main_schema()), restore: true });
    s.tab(|t: &mut Bk| t.restore_form_mut().input = "/tmp/staging-backup.sql".into());
    s.frames(10);
    s.click("Start restore");
    s.shot("restore_confirm");
    s.close_all();

    // Maintenance of two tables, truncate of one.
    let orders = ObjectRef::table(Some("main".into()), "orders");
    s.command(AppCommand::OpenMaintenance { connection: a.clone(), objects: vec![customer.clone(), orders.clone()], op: None });
    s.shot("maintenance");
    s.click("Run");
    s.frames(20);
    if !s.h.state().popups.is_empty() {
        s.shot("maintenance_run");
    }
    s.close_all();
    s.command(AppCommand::OpenMaintenance { connection: a.clone(), objects: vec![orders.clone()], op: Some("truncate".into()) });
    s.shot("truncate");
    s.close_all();

    // Change password: the form, then the outcome.
    s.command(AppCommand::ChangePassword { connection: a.clone() });
    s.shot("password");
    s.tab(|t: &mut ternarygrid_app::tabs::dbtools::ChangePasswordTab| t.show_changed(true));
    s.shot("password_done");
    s.close_all();

    // The FDW script of the MySQL connection in the PostgreSQL one.
    let my = s.my.clone();
    s.command(AppCommand::OpenFdwConfigurator { connection: pg.clone() });
    let source = s.h.state().conns.config(&my).unwrap().clone();
    s.tab(|t: &mut ternarygrid_app::tabs::dbtools::FdwConfigTab| {
        t.set_source(&source);
    });
    s.shot("fdw");
    s.close_all();

    // The debugger stopped at a breakpoint.
    type Dbg = ternarygrid_app::tabs::dbtools::DebuggerTab;
    let routine = ObjectRef::new(ObjectKind::Function, Some("public".into()), "recalc_order_total");
    s.command(AppCommand::OpenDebugger { connection: a.clone(), routine });
    s.frames(20);
    s.tab(|t: &mut Dbg| t.show_sample(sample_routine(), 6, &[6, 9], sample_variables(), sample_stack(), sample_messages()));
    s.shot("debugger");
    s.close_all();

    // The query builder: three joined tables, then the conditions and the table picker.
    type Qb = ternarygrid_app::tabs::query_builder::QueryBuilderTab;
    s.command(AppCommand::OpenQueryBuilder { connection: a.clone(), schema: Some(main_schema()) });
    let tables = [customer.clone(), orders.clone(), ObjectRef::table(Some("main".into()), "order_line")];
    s.tab(|t: &mut Qb| t.add_tables(&tables));
    s.wait("builder tables", |h| h.state_mut().dock.iter_all_tabs_mut().any(|(_, t)| t.view.as_any_mut().downcast_mut::<Qb>().is_some_and(|q| q.model_mut().tables.len() == 3)));
    s.tab(|t: &mut Qb| sample_query(t.model_mut()));
    s.shot("query_builder");
    s.tab(|t: &mut Qb| t.show_tab(ternarygrid_visual::builder::QbTab::Conditions, Some("ord")));
    s.shot("builder_conditions");
    s.close_all();
}

fn sample_routine() -> ternarygrid_core::ddl::RoutineDef {
    let definition = [
        "create or replace function public.recalc_order_total(p_order_id bigint)",
        "returns numeric language plpgsql as $$",
        "declare",
        "    v_total numeric := 0;",
        "    v_tax numeric;",
        "begin",
        "    select sum(quantity * unit_price) into v_total",
        "      from order_line where order_id = p_order_id;",
        "    v_tax := round(v_total * 0.11, 2);",
        "    update orders set total = v_total + v_tax where id = p_order_id;",
        "    return v_total + v_tax;",
        "end;",
        "$$;",
    ]
    .join("\n");
    ternarygrid_core::ddl::RoutineDef {
        schema: Some("public".into()),
        name: "recalc_order_total".into(),
        kind: Some(ObjectKind::Function),
        arguments: "p_order_id bigint".into(),
        returns: Some("numeric".into()),
        language: Some("plpgsql".into()),
        definition,
        comment: None,
    }
}

fn sample_variables() -> Vec<ternarygrid_core::admin::DebugVariable> {
    let v = |name: &str, data_type: &str, value: &str, is_argument: bool| ternarygrid_core::admin::DebugVariable {
        name: name.into(),
        data_type: data_type.into(),
        value: value.into(),
        is_argument,
    };
    vec![v("p_order_id", "bigint", "10423", true), v("v_total", "numeric", "0", false), v("v_tax", "numeric", "NULL", false)]
}

fn sample_stack() -> Vec<ternarygrid_core::admin::StackFrame> {
    vec![ternarygrid_core::admin::StackFrame { level: 0, routine: "recalc_order_total".into(), line: 6, args: String::new() }]
}

fn sample_messages() -> Vec<String> {
    vec!["Starting public.recalc_order_total with (CAST('10423' AS bigint))".into(), "Breakpoint at line 6: select sum(quantity * unit_price) …".into()]
}

/// Columns, aggregates, a condition, grouping and ordering as in the design.
fn sample_query(m: &mut ternarygrid_visual::builder::QueryModel) {
    use ternarygrid_visual::builder::{Aggregate, JoinKind, QbCondition, QbSelectItem};
    let aliases: Vec<String> = m.tables.iter().map(|t| t.alias.clone()).collect();
    for (t, x, y) in [(0, 60.0, 40.0), (1, 380.0, 30.0), (2, 700.0, 60.0)] {
        if let Some(tab) = m.tables.get_mut(t) {
            tab.pos = egui::pos2(x, y);
        }
    }
    if let Some(j) = m.joins.get_mut(1) {
        j.kind = JoinKind::Left;
    }
    let (c, o, l) = (&aliases[0], &aliases[1], &aliases[2]);
    m.set_column_selected(c, "name", true);
    m.set_column_selected(c, "city", true);
    m.set_column_selected(o, "total", true);
    if let Some(s) = m.select.iter_mut().find(|s| s.expr == format!("{o}.total")) {
        s.alias = "orders_total".into();
        s.aggregate = Some(Aggregate::Sum);
    }
    m.select.push(QbSelectItem { expr: format!("{l}.quantity * {l}.unit_price"), alias: "revenue".into(), aggregate: Some(Aggregate::Sum), visible: true });
    m.conditions.push(QbCondition::new(format!("{o}.status"), "<>", "'cancelled'"));
    let mut second = QbCondition::new(format!("{o}.created_at"), ">=", "'2026-10-01'");
    second.connector = ternarygrid_visual::builder::Connector::And;
    m.conditions.push(second);
    m.group_by = vec![format!("{c}.name"), format!("{c}.city")];
    m.order_by = vec![("revenue".into(), true)];
    m.limit = Some(100);
}

#[test]
#[ignore = "writes screenshots for visual review; run with --ignored"]
fn ui_snapshots_tools() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots/tools");
    std::fs::create_dir_all(&root).unwrap();
    let only = std::env::var("TERNARYGRID_SNAPSHOT_THEME").ok();
    for theme in [Theme::Dark, Theme::Light] {
        let name = if theme == Theme::Dark { "dark" } else { "light" };
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        let home = std::env::temp_dir().join(format!("ternarygrid-tool-shots-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        let mut s = Shots::new(&home, root.clone(), theme);
        shots(&mut s);
        drop(s);
        let _ = std::fs::remove_dir_all(&home);
    }
}
