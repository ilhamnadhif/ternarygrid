//! Screenshots of the real application window for reviewing visual changes,
//! rasterised on the CPU (no GPU needed). Not part of the normal test run:
//!
//! ```text
//! TERNARYGRID_SNAPSHOT_LABEL=before cargo test -p ternarygrid-app --test ui_snapshots -- --ignored
//! ```
//!
//! writes `target/ui-snapshots/<label>/<shot>-<light|dark>.png` (label
//! `current` by default), so two runs (before / after a change) can be
//! compared side by side.

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
use ternarygrid_app::command::{AppCommand, ObjectPage};
use ternarygrid_app::conn::{ConnectionManager, DirectNet, MemorySecrets};
use ternarygrid_app::keys::Action;
use ternarygrid_app::services::{Services, Settings};
use ternarygrid_app::tabs::sql_editor::SqlEditorTab;
use ternarygrid_config::settings::Theme;

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
    fn render(&self, prims: &[ClippedPrimitive], size: [usize; 2], bg: Color32, ppp: f32) -> image::RgbaImage {
        let (w, h) = (size[0], size[1]);
        let mut fb: Vec<[f32; 4]> = vec![[bg.r() as f32, bg.g() as f32, bg.b() as f32, 255.0]; w * h];
        for p in prims {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            let Some(tex) = self.textures.get(&mesh.texture_id) else { continue };
            let clip = p.clip_rect;
            let (cx0, cy0) = ((clip.min.x * ppp).max(0.0) as i64, (clip.min.y * ppp).max(0.0) as i64);
            let (cx1, cy1) = (((clip.max.x * ppp).ceil() as i64).min(w as i64), ((clip.max.y * ppp).ceil() as i64).min(h as i64));
            for tri in mesh.indices.as_chunks::<3>().0 {
                let v = tri.map(|i| mesh.vertices[i as usize]);
                let pts = v.map(|v| (v.pos.x * ppp, v.pos.y * ppp));
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
    /// Added to the file names ("-light" / "-dark"); empty for the README's.
    suffix: String,
}

impl Shots {
    fn new(home: &Path, dir: PathBuf, theme: Theme) -> Shots {
        Shots::with(home, dir, theme, egui::vec2(1440.0, 900.0), Vec::new(), true)
    }

    /// A window of `size` points with the `extra` connections saved before it opens; `mac`: as
    /// on macOS (no in-window menus, room for the window buttons).
    fn with(home: &Path, dir: PathBuf, theme: Theme, size: egui::Vec2, extra: Vec<ternarygrid_core::config::ConnectionConfig>, mac: bool) -> Shots {
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
        for c in extra {
            conns.upsert(c);
        }
        let h = Harness::builder().with_size(size).with_pixels_per_point(PPP).with_max_steps(400).with_step_dt(1.0 / 60.0).renderer(CpuTextures(raster.clone())).build_eframe(
            move |cc| {
                ternarygrid_app::theme::setup_fonts(&cc.egui_ctx);
                backend.set_context(cc.egui_ctx.clone());
                let mut app = TernaryGridApp::new(backend, conns, services, settings);
                // As on macOS (the mockups): the menus in the system's menu bar, room for the
                // window buttons at the left of the top bar.
                if mac {
                    app.native_menu_bar = true;
                    app.titlebar_inset = ternarygrid_app::app::MAC_TITLEBAR_INSET;
                }
                app
            },
        );
        ternarygrid_app::theme::apply_settings(&h.ctx, &h.state().settings);
        let suffix = if theme == Theme::Dark { "-dark" } else { "-light" }.to_string();
        let mut s = Shots { h, raster, dir, suffix };
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
        self.frames(30);
        let out = self.h.output();
        let prims = self.h.ctx.tessellate(out.shapes.clone(), PPP);
        let size = self.h.ctx.content_rect().size() * PPP;
        let bg = self.h.ctx.global_style().visuals.panel_fill;
        let img = self.raster.borrow().render(&prims, [size.x as usize, size.y as usize], bg, PPP);
        let path = self.dir.join(format!("{name}{}.png", self.suffix));
        img.save(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        eprintln!("wrote {}", path.display());
    }

    /// Opens the sample database (created on the start page) and waits until it is connected.
    fn sample_database(&mut self) -> String {
        {
            let n = self.h.query_all_by_label_contains("Create sample database").next().expect("the start page");
            n.click();
        }
        self.wait("the sample connection", |h| h.state().conns.configs().iter().any(|c| c.name.starts_with("ternarygrid-sample") && h.state().conns.is_connected(&c.id)));
        self.h.state().conns.configs().iter().find(|c| c.name.starts_with("ternarygrid-sample")).unwrap().id.clone()
    }

    /// Double-clicks a row of the navigator (the leftmost widget with that name).
    fn nav_open(&mut self, label: &str) {
        self.wait(label, |h| h.query_all_by_label(label).next().is_some());
        // A second apart from the last click, so that the two clicks make a double-click.
        let t = self.h.ctx.input(|i| i.time);
        self.h.input_mut().time = Some(t + 1.0);
        self.h.step();
        {
            let n = self.h.query_all_by_label(label).min_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).unwrap();
            n.click();
            n.click();
        }
        self.frames(4);
    }

    /// The pointer away from the widgets (no hover effects in the shot).
    fn park_pointer(&mut self) {
        let r = self.h.ctx.content_rect();
        self.h.event(egui::Event::PointerMoved(egui::pos2(r.right() - 4.0, r.center().y)));
        self.frames(2);
    }

    fn focused_editor(&mut self) -> &mut SqlEditorTab {
        let (_, tab) = self.h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().expect("a SQL editor")
    }
}

fn has(h: &H, label: &str) -> bool {
    h.query_all_by_label_contains(label).next().is_some()
}

#[test]
#[ignore = "writes screenshots for visual review; run with --ignored"]
fn ui_snapshots() {
    let label = std::env::var("TERNARYGRID_SNAPSHOT_LABEL").unwrap_or_else(|_| "current".into());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots").join(label);
    std::fs::create_dir_all(&root).unwrap();
    for theme in [Theme::Light, Theme::Dark] {
        let home = std::env::temp_dir().join(format!("ternarygrid-shots-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        let mut s = Shots::new(&home, root.clone(), theme);

        // The start page of a new installation.
        assert!(has(&s.h, "Create sample database"), "welcome page");
        s.shot("welcome");

        // The sample database, a SQL editor with a result.
        {
            let n = s.h.query_all_by_label_contains("Create sample database").next().unwrap();
            n.click();
        }
        s.wait("the sample connection", |h| {
            let app = h.state();
            app.conns.configs().len() == 1 && app.conns.is_connected(&app.conns.configs()[0].id.clone())
        });
        let conn = s.h.state().conns.configs()[0].id.clone();
        s.h.state_mut().push_command(AppCommand::NewSqlEditor { connection: Some(conn.clone()), schema: None, text: Some("select * from product order by price desc".into()) });
        s.frames(3);
        s.wait("editor session", |h| {
            let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
            tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| e.session_ready())
        });
        let ctx = s.h.ctx.clone();
        s.h.state_mut().dispatch_action(&ctx, Action::ExecuteStatement);
        s.frames(3);
        s.wait("the result", |h| {
            let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
            tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| !e.is_running() && e.result_count() > 0)
        });
        let _ = s.focused_editor();
        s.shot("editor_results");

        // A table's data and its properties.
        for (page, name) in [(ObjectPage::Data, "table_data"), (ObjectPage::Properties, "table_properties")] {
            s.h.state_mut().push_command(AppCommand::OpenObject { connection: conn.clone(), object: ternarygrid_core::ObjectRef::table(Some("main".into()), "product"), page });
            s.frames(3);
            s.wait(name, |h| h.state_mut().dock.find_active_focused().is_some_and(|(_, t)| t.view.title() == "product"));
            if page == ObjectPage::Data {
                s.wait("product rows", |h| has(h, "Data grid,") && !has(h, "Data grid, 0 rows"));
            }
            s.shot(name);
        }

        // Settings and the connection dialog.
        s.h.state_mut().push_command(AppCommand::OpenSettings);
        s.frames(4);
        s.shot("settings");
        let ctx = s.h.ctx.clone();
        s.h.state_mut().dispatch_action(&ctx, Action::NewConnection);
        // Windows fade in: let the animation end.
        s.frames(30);
        s.shot("connection_dialog");
        s.h.key_press(egui::Key::Escape);
        s.frames(30);
        // The command palette over the start page's tabs.
        s.h.state_mut().dialogs.palette_open = true;
        s.frames(30);
        s.shot("command_palette");

        drop(s);
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// The query of the README's editor screenshot.
const REVENUE: &str = "-- Revenue per customer
select c.name, c.city,
       count(distinct o.id) as orders,
       round(sum(l.quantity * l.unit_price), 2) as revenue
from customer c
join \"order\" o on o.customer_id = c.id
join order_line l on l.order_id = o.id
where o.status <> 'cancelled'
group by c.id
order by revenue desc;
";

#[test]
#[ignore = "writes the README screenshots into docs/screenshots; run with --ignored"]
fn readme_screenshots() {
    use ternarygrid_core::DbKind;
    use ternarygrid_core::config::ConnectionConfig;
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screenshots");
    std::fs::create_dir_all(&docs).unwrap();
    let home = std::env::temp_dir().join(format!("ternarygrid-readme-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&home).unwrap();
    // A few saved connections next to the sample database, as in a real workspace.
    let example = |kind: DbKind, name: &str, host: &str, port: u16, db: &str, kind_of: &str| {
        let mut c = ConnectionConfig::new(kind, name);
        c.host = host.into();
        c.port = Some(port);
        c.user = Some("app".into());
        c.database = Some(db.into());
        c.connection_type = kind_of.into();
        c
    };
    let extra = vec![
        example(DbKind::Postgres, "Analytics", "analytics.example.com", 5432, "analytics", "prod"),
        example(DbKind::ClickHouse, "Events", "events.example.com", 8123, "events", "dev"),
        example(DbKind::MariaDb, "Reports", "reports.example.com", 3306, "reports", "dev"),
        example(DbKind::MySql, "Shop", "shop.example.com", 3306, "shop", "test"),
    ];
    // 1280 × 848 points at 2× (downscaled to 1600 × 1060 below).
    let mut s = Shots::with(&home, docs.clone(), Theme::Light, egui::vec2(1280.0, 848.0), extra, false);
    s.suffix.clear();
    let conn = s.sample_database();
    s.nav_open("Tables");

    // The editor with a result.
    s.h.state_mut().push_command(AppCommand::NewSqlEditor { connection: Some(conn.clone()), schema: None, text: Some(REVENUE.into()) });
    s.frames(3);
    s.wait("editor session", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| e.session_ready())
    });
    let ctx = s.h.ctx.clone();
    s.h.state_mut().dispatch_action(&ctx, Action::ExecuteStatement);
    s.frames(3);
    s.wait("the result", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| !e.is_running() && e.result_count() > 0)
    });
    s.park_pointer();
    s.shot("editor");

    // A table's data.
    s.nav_open("customer");
    s.wait("customer rows", |h| has(h, "Data grid,") && !has(h, "Data grid, 0 rows"));
    s.park_pointer();
    s.shot("data");

    // The diagram of the schema.
    s.h.state_mut().push_command(AppCommand::OpenErDiagram { connection: conn.clone(), schema: ternarygrid_core::SchemaRef::new(None, Some("main".into())), tables: Vec::new() });
    s.frames(3);
    s.wait("the diagram", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<ternarygrid_app::tabs::er::ErTab>().is_some_and(|e| e.table_screen_rect("main.order").is_some())
    });
    s.frames(20);
    s.park_pointer();
    s.shot("er-diagram");

    drop(s);
    let _ = std::fs::remove_dir_all(&home);
    for name in ["editor", "data", "er-diagram"] {
        let path = docs.join(format!("{name}.png"));
        let img = image::open(&path).unwrap();
        img.resize_exact(1600, 1060, image::imageops::FilterType::Lanczos3).save(&path).unwrap();
    }
}
