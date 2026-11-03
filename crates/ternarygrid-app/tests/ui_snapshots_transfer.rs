//! Screenshots of the transfer wizards (export, import, migration, mock data) in the dark and
//! the light theme, rasterised on the CPU like `ui_snapshots.rs`. Not part of the normal run:
//!
//! ```text
//! cargo test -p ternarygrid-app --test ui_snapshots_transfer -- --ignored
//! ```
//!
//! writes `target/ui-snapshots/transfer/<shot>-<dark|light>.png`, to compare with the design
//! canvas (`s_transfer.py`, `s_more.py`).

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
use ternarygrid_app::tabs::transfer::{ExportTab, ImportTab, MigrationTab, MockDataTab, ResultSource};
use ternarygrid_config::settings::Theme;
use ternarygrid_core::config::{AuthMethod, ConnectionConfig};
use ternarygrid_core::meta::{ObjectRef, SchemaRef};
use ternarygrid_core::{ColumnMeta, DataKind, DbKind, Value};
use ternarygrid_transfer::{ExportFormat, GeneratorKind, ImportMode, MigrationMode, OnError};

type H = Harness<'static, TernaryGridApp>;

/// Pixels per point of the screenshots (a Retina display).
const PPP: f32 = 2.0;

/// Textures the frames set, kept for rasterising (the font atlas comes in
/// the first frames, so they are recorded from the start).
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

    /// Rasterise tessellated `prims` (barycentric, textured, alpha blended).
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
                // The edge function of a→b at p, computed from the edge's endpoints in one fixed
                // order: the two triangles of a shared edge get exactly opposite values, so a
                // pixel on it is neither lost (a dark speck) nor drawn twice.
                let side = |a: (f32, f32), b: (f32, f32), p: (f32, f32)| {
                    let (a, b, flip) = if (a.0, a.1) <= (b.0, b.1) { (a, b, 1.0) } else { (b, a, -1.0) };
                    flip * ((b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0))
                };
                for y in miny..maxy {
                    for x in minx..maxx {
                        let p = (x as f32 + 0.5, y as f32 + 0.5);
                        let e = [side(pts[1], pts[2], p), side(pts[2], pts[0], p), side(pts[0], pts[1], p)];
                        let inside = |i: usize| {
                            let s = e[i] * area.signum();
                            s > 0.0 || (s == 0.0 && edge[i])
                        };
                        if !(inside(0) && inside(1) && inside(2)) {
                            continue;
                        }
                        let (w0, w1) = (e[0] / area, e[1] / area);
                        let w2 = 1.0 - w0 - w1;
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

/// The harness's renderer: only records the textures (rendering is done by
/// [`Raster::render`] from the last frame's shapes).
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
        let conns = ConnectionManager::new(ternarygrid_app::driver_registry(), Arc::new(MemorySecrets::default()), Arc::new(DirectNet));
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
        let mut s = Shots { h, raster, dir, theme };
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

    /// Write the last frame as `<name>-<theme>.png`.
    fn shot(&mut self, name: &str) {
        // Without toasts, after a few calm frames (hover effects and animations settle).
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

fn has(h: &H, label: &str) -> bool {
    h.query_all_by_label_contains(label).next().is_some()
}

/// The view of the newest pop-up (the wizard opened last).
fn popup<T: 'static>(h: &mut H) -> &mut T {
    h.state_mut().popups.last_mut().expect("a pop-up").view.as_any_mut().downcast_mut::<T>().expect("the wizard's type")
}

fn has_popup<T: 'static>(h: &mut H) -> bool {
    h.state_mut().popups.last_mut().is_some_and(|t| t.view.as_any_mut().downcast_mut::<T>().is_some())
}

fn click(s: &mut Shots, label: &str) {
    // Calm frames first: two clicks in a row would count as a double click.
    s.frames(20);
    {
        let n = s.h.query_all_by_label_contains(label).next().unwrap_or_else(|| panic!("no widget labelled {label:?}"));
        n.click();
    }
    s.frames(3);
}

fn close_popups(s: &mut Shots) {
    let ids: Vec<u64> = s.h.state().popups.iter().map(|t| t.id).collect();
    for id in ids {
        s.h.state_mut().push_command(AppCommand::CloseTab(id));
    }
    s.frames(20);
}

fn table(name: &str) -> ObjectRef {
    ObjectRef::table(Some("main".into()), name)
}

/// Run statements on the connection's main session (then refresh its metadata).
fn run_sql(s: &mut Shots, conn: &str, stmts: &[&str]) {
    let backend = s.h.state().backend.clone();
    let session = s.h.state().conns.live(conn).unwrap().main.clone();
    let stmts: Vec<String> = stmts.iter().map(|s| s.to_string()).collect();
    backend
        .block_on(async move {
            for st in stmts {
                session.execute_update(&st, &[]).await?;
            }
            Ok::<(), ternarygrid_core::DbError>(())
        })
        .unwrap();
    s.h.state_mut().push_command(AppCommand::RefreshMetadata { connection: conn.to_string(), object: None, schema: None });
    s.frames(20);
}

/// Tables named like the design's (orders, payments, shipment, a view of open orders) and a
/// large one that keeps a run going for a screenshot.
const DESIGN_TABLES: &[&str] = &[
    "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL REFERENCES customer(id), status TEXT NOT NULL, total NUMERIC(10,2) NOT NULL, \
     created_at TIMESTAMP NOT NULL, note TEXT, internal_ref UUID UNIQUE)",
    "INSERT INTO orders (customer_id, status, total, created_at, note) SELECT 1 + (n % 5), CASE n % 3 WHEN 0 THEN 'open' WHEN 1 THEN 'shipped' ELSE 'delivered' END, \
     n * 1.25, datetime('now', '-' || (n % 60) || ' days'), NULL FROM (WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 480) SELECT n FROM c)",
    "CREATE TABLE payments (id INTEGER PRIMARY KEY, order_id INTEGER NOT NULL, amount NUMERIC(10,2) NOT NULL)",
    "CREATE TABLE shipment (id INTEGER PRIMARY KEY, order_id INTEGER NOT NULL, shipped_at TIMESTAMP)",
    "CREATE VIEW v_open_orders AS SELECT * FROM orders WHERE status = 'open'",
    "CREATE TABLE order_archive AS WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 600000) \
     SELECT n AS id, 'order ' || n AS name, n * 1.5 AS total, datetime('now') AS created_at FROM c",
];

#[test]
#[ignore = "writes screenshots of the transfer wizards for visual review; run with --ignored"]
fn transfer_snapshots() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots/transfer");
    std::fs::create_dir_all(&root).unwrap();
    let only = std::env::var("TERNARYGRID_SNAPSHOT_THEME").ok();
    for theme in [Theme::Dark, Theme::Light] {
        if only.as_deref().is_some_and(|o| o != if theme == Theme::Dark { "dark" } else { "light" }) {
            continue;
        }
        let home = std::env::temp_dir().join(format!("ternarygrid-transfer-shots-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        // A short folder, like the design's ~/Downloads/exports.
        let out = PathBuf::from(format!("/tmp/exports-{}", &uuid::Uuid::new_v4().simple().to_string()[..6]));
        std::fs::create_dir_all(&out).unwrap();
        let mut s = Shots::new(&home, root.clone(), theme);
        {
            let n = s.h.query_all_by_label_contains("Create sample database").next().unwrap();
            n.click();
        }
        s.wait("the sample connection", |h| {
            let app = h.state();
            app.conns.configs().len() == 1 && app.conns.is_connected(&app.conns.configs()[0].id.clone())
        });
        let conn = s.h.state().conns.configs()[0].id.clone();
        // Named like the design's connection (the wizards' subtitle).
        let mut cfg = s.h.state().conns.configs()[0].clone();
        cfg.name = "bantuin".into();
        s.h.state_mut().conns.upsert(cfg);

        run_sql(&mut s, &conn, DESIGN_TABLES);
        // The migrations' target, an empty database named like the design's.
        let target = {
            let mut cfg = ConnectionConfig::new(DbKind::Sqlite, "tplus-staging");
            cfg.file_path = Some(home.join("staging.db").display().to_string());
            cfg.auth = AuthMethod::None;
            let id = cfg.id.clone();
            s.h.state_mut().conns.upsert(cfg);
            let backend = s.h.state().backend.clone();
            s.h.state_mut().conns.connect(&backend, &id, Credentials::default()).unwrap();
            s.wait("the target connection", |h| h.state().conns.is_connected(&id));
            id
        };
        // The table behind the wizards, as in the design.
        s.h.state_mut().push_command(AppCommand::OpenObject { connection: conn.clone(), object: table("orders"), page: ternarygrid_app::command::ObjectPage::Data });
        s.frames(40);

        // ------------------------------------------------------------------ export
        s.h.state_mut().push_command(AppCommand::OpenExport { connection: conn.clone(), objects: vec![table("orders"), table("order_line")], sql: None });
        s.frames(30);
        {
            let tab = popup::<ExportTab>(&mut s.h);
            tab.set_filter("orders", "created_at > datetime('now', '-30 days')");
            // The design's batch size and workers.
            tab.settings_mut().options.fetch_size = 10_000;
            tab.settings_mut().threads = 2;
            tab.add_query("revenue_per_customer", "select c.name, sum(o.total) as revenue from customer c join orders o on o.customer_id = c.id group by c.name");
        }
        s.wait("the tables to add", |h| has(h, "Add all shown"));
        s.shot("export_1_source");
        popup::<ExportTab>(&mut s.h).set_page(1);
        s.frames(3);
        s.shot("export_2_format");
        popup::<ExportTab>(&mut s.h).settings_mut().format = ExportFormat::Xlsx;
        popup::<ExportTab>(&mut s.h).settings_mut().options.xlsx.number_format = Some("#,##0.00".into());
        popup::<ExportTab>(&mut s.h).settings_mut().options.xlsx.max_rows_per_sheet = 1_000_000;
        s.frames(3);
        s.shot("export_2_format_xlsx");
        popup::<ExportTab>(&mut s.h).settings_mut().format = ExportFormat::Csv;
        s.frames(2);
        click(&mut s, "File and reading");
        s.shot("export_2_format_open");
        {
            let tab = popup::<ExportTab>(&mut s.h);
            tab.settings_mut().output_folder = out.clone();
            tab.set_page(2);
        }
        s.frames(3);
        s.shot("export_3_output");
        popup::<ExportTab>(&mut s.h).set_page(3);
        s.frames(3);
        s.shot("export_4_summary");
        click(&mut s, "Save as task…");
        s.frames(30);
        s.shot("export_save_task");
        s.h.key_press(egui::Key::Escape);
        s.frames(30);
        // A large table: the run is still going when the screenshot is taken.
        {
            let tab = popup::<ExportTab>(&mut s.h);
            tab.add_table(table("order_archive"));
            tab.settings_mut().count_rows = true;
            tab.settings_mut().threads = 1;
        }
        click(&mut s, "Run export");
        s.frames(40);
        if has_popup::<ExportTab>(&mut s.h) && popup::<ExportTab>(&mut s.h).is_running() {
            s.shot("export_4_running");
            popup::<ExportTab>(&mut s.h).cancel();
            s.wait("the cancelled export", |h| !has_popup::<ExportTab>(h) || !popup::<ExportTab>(h).is_running());
        }
        close_popups(&mut s);
        s.h.state_mut().push_command(AppCommand::OpenExport { connection: conn.clone(), objects: vec![table("orders"), table("order_line")], sql: None });
        s.frames(30);
        {
            let tab = popup::<ExportTab>(&mut s.h);
            tab.add_query("revenue_per_customer", "select c.name, sum(o.total) as revenue from customer c join orders o on o.customer_id = c.id group by c.name");
            tab.settings_mut().output_folder = out.clone();
            tab.set_page(3);
        }
        // One object fails: the summary stays with its results.
        popup::<ExportTab>(&mut s.h).add_table(table("no_such_table"));
        click(&mut s, "Run export");
        s.wait("the export", |h| !has_popup::<ExportTab>(h) || !popup::<ExportTab>(h).is_running());
        s.frames(4);
        if has_popup::<ExportTab>(&mut s.h) {
            s.shot("export_5_done");
        }
        close_popups(&mut s);

        // Rows of a result grid.
        let store = ternarygrid_core::RowStore::shared(1 << 20);
        store.lock().extend((0..200).map(|i| vec![Value::Int(i), Value::text(format!("order {i}"))]));
        let columns = vec![ColumnMeta::new("id", "int", DataKind::Int), ColumnMeta::new("name", "text", DataKind::Text)];
        let sql = "select * from orders where status = 'open' order by created_at desc".to_string();
        let mut rows = ExportTab::from_result(conn.clone(), "orders_open".into(), None, store, columns, Some((0..14).collect()), Vec::new(), Some(sql));
        rows.set_result_source(ResultSource::Query);
        s.h.state_mut().open_tab(Box::new(rows));
        s.frames(30);
        s.shot("export_rows");
        close_popups(&mut s);

        // ------------------------------------------------------------------ import
        let csv = home.join("customers_october.csv");
        std::fs::write(
            &csv,
            "name,email,city,joined,orders\nRina Halim,rina@example.com,Medan,12/10/2026,4\nAgus Pratama,agus.p@example.com,Yogyakarta,13/10/2026,8\n\
             Dewi Sartika,dewi@example.com,Bandung,15/10/2026,1\nBayu Nugroho,,Jakarta,17/10/2026,0\nSari Wulandari,sari.w@example.com,Surabaya,20/10/2026,3\n\
             Rina Halim,rina@example.com,Medan,21/10/2026,2\n",
        )
        .unwrap();
        s.h.state_mut().push_command(AppCommand::OpenImport { connection: conn.clone(), table: None, schema: Some(SchemaRef::schema("main")) });
        s.frames(30);
        popup::<ImportTab>(&mut s.h).set_file(&csv);
        s.wait("the preview", |h| popup::<ImportTab>(h).preview_ready());
        s.frames(3);
        s.shot("import_1_file");
        {
            let tab = popup::<ImportTab>(&mut s.h);
            tab.set_new_table(Some(SchemaRef::schema("main")), "customers_october");
            tab.set_page(1);
        }
        s.wait("the new table's columns", |h| popup::<ImportTab>(h).mapping_ready());
        s.frames(3);
        s.shot("import_2_target_new");
        {
            let tab = popup::<ImportTab>(&mut s.h);
            tab.set_existing_table(table("customer"));
            tab.set_page(1);
        }
        s.wait("the mapping", |h| popup::<ImportTab>(h).mapping_ready());
        s.frames(3);
        s.shot("import_2_target_existing");
        {
            let tab = popup::<ImportTab>(&mut s.h);
            tab.options_mut().mode = ImportMode::Replace;
            tab.options_mut().on_error = OnError::LogAndContinue;
            tab.set_page(2);
        }
        s.frames(3);
        s.shot("import_3_options");
        {
            let tab = popup::<ImportTab>(&mut s.h);
            tab.options_mut().mode = ImportMode::Append;
            tab.set_page(3);
        }
        s.frames(3);
        s.shot("import_4_summary");
        // The repeated e-mail address fails (UNIQUE): the result stays with the failed row.
        click(&mut s, "Run import");
        s.wait("the import", |h| !has_popup::<ImportTab>(h) || !popup::<ImportTab>(h).is_running());
        s.frames(4);
        if has_popup::<ImportTab>(&mut s.h) {
            s.shot("import_5_done");
        }
        close_popups(&mut s);

        // --------------------------------------------------------------- migration
        s.h.state_mut().push_command(AppCommand::OpenMigration { connection: conn.clone(), objects: vec![table("orders"), table("order_line"), table("customer")] });
        s.frames(30);
        s.wait("the tables to add", |h| has(h, "Add all shown"));
        s.shot("migration_1_source");
        {
            let tab = popup::<MigrationTab>(&mut s.h);
            tab.set_target(target.clone(), None);
            tab.set_page(1);
        }
        s.frames(10);
        s.shot("migration_2_target");
        popup::<MigrationTab>(&mut s.h).set_page(2);
        s.frames(30);
        s.shot("migration_3_mapping");
        popup::<MigrationTab>(&mut s.h).set_page(3);
        s.frames(3);
        s.shot("migration_4_options");
        {
            let tab = popup::<MigrationTab>(&mut s.h);
            for t in ["orders", "order_line", "customer"] {
                tab.set_target_name(t, &format!("{t}_copy"));
            }
        }
        popup::<MigrationTab>(&mut s.h).options_mut().mode = MigrationMode::DropAndCreate;
        popup::<MigrationTab>(&mut s.h).set_page(4);
        s.frames(3);
        click(&mut s, "Run migration");
        s.frames(30);
        s.shot("migration_confirm");
        s.h.key_press(egui::Key::Escape);
        s.frames(30);
        // customer into the existing orders table fails (its NOT NULL columns): the result stays.
        {
            let tab = popup::<MigrationTab>(&mut s.h);
            tab.options_mut().mode = MigrationMode::Append;
            tab.options_mut().continue_on_table_error = true;
            tab.set_target_name("customer", "orders");
        }
        s.frames(2);
        click(&mut s, "Run migration");
        s.wait("the migration", |h| !has_popup::<MigrationTab>(h) || !popup::<MigrationTab>(h).is_running());
        s.frames(4);
        if has_popup::<MigrationTab>(&mut s.h) {
            s.shot("migration_5_done");
        }
        close_popups(&mut s);
        // A large table: the run is still going when the screenshot is taken.
        s.h.state_mut().push_command(AppCommand::OpenMigration { connection: conn.clone(), objects: vec![table("customer"), table("order_archive"), table("order_line")] });
        s.frames(30);
        {
            let tab = popup::<MigrationTab>(&mut s.h);
            tab.set_target(target.clone(), None);
            for t in ["customer", "order_archive", "order_line"] {
                tab.set_target_name(t, &format!("{t}_run"));
            }
            tab.set_page(4);
        }
        s.frames(3);
        click(&mut s, "Run migration");
        s.frames(40);
        if has_popup::<MigrationTab>(&mut s.h) && popup::<MigrationTab>(&mut s.h).is_running() {
            s.shot("migration_running");
            popup::<MigrationTab>(&mut s.h).cancel();
            s.wait("the cancelled migration", |h| !has_popup::<MigrationTab>(h) || !popup::<MigrationTab>(h).is_running());
        }
        close_popups(&mut s);

        // --------------------------------------------------------------- mock data
        s.h.state_mut().push_command(AppCommand::OpenMockData { connection: conn.clone(), table: table("orders") });
        s.frames(30);
        s.wait("the table definition", |h| popup::<MockDataTab>(h).definition_loaded());
        {
            let tab = popup::<MockDataTab>(&mut s.h);
            tab.config_mut().rows = 10_000;
            tab.config_mut().batch_size = 1_000;
            tab.config_mut().on_error = OnError::SkipRow;
        }
        s.frames(3);
        s.shot("mock_1_settings");
        // A select's list in the kit's menu look (opened, then closed by a second click).
        for open in [true, false] {
            s.frames(20);
            {
                let combos: Vec<_> = s.h.query_all_by_role(egui::accesskit::Role::ComboBox).collect();
                let label = s.h.query_all_by_label("On error").next().expect("the On error label").rect();
                let combo = combos.iter().find(|n| n.rect().left() > label.right() && (n.rect().center().y - label.center().y).abs() < 12.0).expect("the On error select");
                combo.click();
            }
            s.frames(10);
            if open {
                s.shot("mock_select_open");
            }
        }
        // Many rows: the run is still going when the screenshot is taken.
        {
            let tab = popup::<MockDataTab>(&mut s.h);
            tab.config_mut().rows = 5_000_000;
            tab.config_mut().on_error = OnError::SkipRow;
        }
        click(&mut s, "Generate");
        s.frames(20);
        if has_popup::<MockDataTab>(&mut s.h) && popup::<MockDataTab>(&mut s.h).is_running() {
            s.shot("mock_2_running");
            popup::<MockDataTab>(&mut s.h).cancel();
            s.wait("the cancelled run", |h| !has_popup::<MockDataTab>(h) || !popup::<MockDataTab>(h).is_running());
        }
        close_popups(&mut s);
        // Rows rejected by UNIQUE: the result stays.
        s.h.state_mut().push_command(AppCommand::OpenMockData { connection: conn.clone(), table: table("category") });
        s.frames(30);
        s.wait("the table definition", |h| popup::<MockDataTab>(h).definition_loaded());
        {
            let tab = popup::<MockDataTab>(&mut s.h);
            tab.config_mut().rows = 5;
            tab.config_mut().on_error = OnError::LogAndContinue;
            if let Some(g) = tab.generator_mut("name") {
                g.generator = GeneratorKind::Constant { value: "Books".into() };
                g.unique = false;
            }
        }
        s.frames(2);
        click(&mut s, "Generate");
        s.wait("the mock data", |h| {
            !has_popup::<MockDataTab>(h)
                || (!popup::<MockDataTab>(h).is_running() && popup::<MockDataTab>(h).last_error().is_none() && popup::<MockDataTab>(h).last_summary().is_some())
                || popup::<MockDataTab>(h).last_error().is_some()
        });
        s.frames(4);
        if has_popup::<MockDataTab>(&mut s.h) {
            s.shot("mock_3_done");
        }
        close_popups(&mut s);

        drop(s);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&out);
    }
}
