//! Screenshots of the window's shell for comparing it with the redesign's mockups: the top bar,
//! the rail, the navigator and its menus, the tab bar, the status bar, the start page, the SQL
//! editor and the command palette, rasterised on the CPU (no GPU needed), at 1440 × 900 points
//! like the mockups, looking like the macOS app (menus in the system's menu bar, the window
//! buttons at the left of the top bar). Not part of the normal test run:
//!
//! ```text
//! cargo test -p ternarygrid-app --test ui_snapshots_shell -- --ignored
//! ```
//!
//! writes `target/ui-snapshots/shell/<shot>-<light|dark>.png` (the folder is `shell-<label>`
//! with `TERNARYGRID_SNAPSHOT_LABEL=<label>`; `TERNARYGRID_SNAPSHOT_ONLY=a,b` takes only the
//! shots whose names start with those, `TERNARYGRID_SNAPSHOT_THEME=dark|light` one theme).

use egui::epaint::{ClippedPrimitive, Primitive};
use egui::{Color32, ColorImage, TextureId};
use egui_kittest::kittest::{NodeT, Queryable};
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
use ternarygrid_core::DbKind;
use ternarygrid_core::config::ConnectionConfig;

type H = Harness<'static, TernaryGridApp>;

/// Pixels per point of the screenshots: a Retina display (2), or
/// `TERNARYGRID_SNAPSHOT_PPP` (1 for quick reviews).
fn ppp() -> f32 {
    std::env::var("TERNARYGRID_SNAPSHOT_PPP").ok().and_then(|v| v.parse().ok()).unwrap_or(2.0)
}

/// Textures the frames set, kept for rasterising.
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
        let ppp = ppp();
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
                for y in miny..maxy {
                    for x in minx..maxx {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let w0 = ((pts[1].0 - px) * (pts[2].1 - py) - (pts[2].0 - px) * (pts[1].1 - py)) / area;
                        let w1 = ((pts[2].0 - px) * (pts[0].1 - py) - (pts[0].0 - px) * (pts[2].1 - py)) / area;
                        let w2 = 1.0 - w0 - w1;
                        if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
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

/// The harness's renderer: only records the textures.
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
    only: Option<String>,
}

/// Saved connections next to the sample database, named like the mockups' (not connected).
fn extra_connections() -> Vec<ConnectionConfig> {
    let make = |kind: DbKind, name: &str, host: &str, port: u16, user: &str, db: &str, last: i64| {
        let mut c = ConnectionConfig::new(kind, name);
        c.host = host.into();
        c.port = Some(port);
        c.user = Some(user.into());
        c.database = Some(db.into());
        c.last_connected = Some(last);
        c
    };
    let mut notes = ConnectionConfig::new(DbKind::Sqlite, "local-notes.db");
    notes.file_path = Some("/nonexistent/notes/local-notes.db".into());
    vec![
        make(DbKind::MySql, "tplus-staging", "staging.db.internal", 3306, "root", "tplus", 1_700_000_300),
        make(DbKind::ClickHouse, "analytics", "ch.internal", 8123, "default", "default", 1_700_000_200),
        notes,
    ]
}

impl Shots {
    fn new(home: &Path, dir: PathBuf, theme: Theme, extra: Vec<ConnectionConfig>) -> Shots {
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
        let h = Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .with_pixels_per_point(ppp())
            .with_max_steps(400)
            .with_step_dt(1.0 / 60.0)
            .renderer(CpuTextures(raster.clone()))
            .build_eframe(move |cc| {
                ternarygrid_app::theme::setup_fonts(&cc.egui_ctx);
                backend.set_context(cc.egui_ctx.clone());
                let mut app = TernaryGridApp::new(backend, conns, services, settings);
                // As on macOS: the menus in the system's menu bar, the window buttons at the
                // left of the top bar (painted by `shot`, as the system draws them).
                app.native_menu_bar = true;
                app.titlebar_inset = ternarygrid_app::app::MAC_TITLEBAR_INSET;
                app
            });
        ternarygrid_app::theme::apply_settings(&h.ctx, &h.state().settings);
        let theme = if theme == Theme::Dark { "dark" } else { "light" };
        let only = std::env::var("TERNARYGRID_SNAPSHOT_ONLY").ok().filter(|s| !s.is_empty());
        let mut s = Shots { h, raster, dir, theme, only };
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
            assert!(t0.elapsed() < Duration::from_secs(30), "timed out waiting for {what}; labels: {:?}", labels(&self.h));
            self.h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A second of simulated time, so the next click is not part of a double click.
    fn pause(&mut self) {
        let t = self.h.ctx.input(|i| i.time);
        self.h.input_mut().time = Some(t + 1.0);
        self.h.step();
        self.h.input_mut().time = None;
    }

    fn wanted(&self, name: &str) -> bool {
        self.only.as_deref().is_none_or(|o| o.split(',').any(|o| name.starts_with(o)))
    }

    /// Write the last frame as `<name>-<theme>.png`; `pointer`: where the pointer rests
    /// (away from the content by default: no hover effects).
    fn shot_at(&mut self, name: &str, pointer: Option<egui::Pos2>) {
        if !self.wanted(name) {
            return;
        }
        self.h.state_mut().toasts.clear();
        self.h.event(egui::Event::PointerMoved(pointer.unwrap_or(egui::pos2(1439.0, 899.0))));
        self.frames(8);
        let out = self.h.output();
        let prims = self.h.ctx.tessellate(out.shapes.clone(), ppp());
        let size = self.h.ctx.content_rect().size() * ppp();
        let bg = self.h.ctx.global_style().visuals.panel_fill;
        let mut img = self.raster.borrow().render(&prims, [size.x as usize, size.y as usize], bg);
        traffic_lights(&mut img, ppp());
        let path = self.dir.join(format!("{name}-{}.png", self.theme));
        img.save(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        eprintln!("wrote {}", path.display());
    }

    fn shot(&mut self, name: &str) {
        self.shot_at(name, None);
    }

    fn command(&mut self, c: AppCommand) {
        self.h.state_mut().push_command(c);
        self.frames(4);
    }

    fn action(&mut self, a: Action) {
        let ctx = self.h.ctx.clone();
        self.h.state_mut().dispatch_action(&ctx, a);
        self.frames(4);
    }

    fn nav_node(&self, label: &str) -> egui::Pos2 {
        let nav = self.h.state().chrome.nav;
        self.h
            .query_all_by_label(label)
            .find(|n| nav.is_some_and(|r| r.contains(n.rect().center())))
            .map(|n| n.rect().center())
            .unwrap_or_else(|| panic!("no navigator row {label:?}; labels: {:?}", labels(&self.h)))
    }

    fn nav_click(&mut self, label: &str, double: bool) {
        self.pause();
        let nav = self.h.state().chrome.nav;
        {
            let n = self
                .h
                .query_all_by_label(label)
                .find(|n| nav.is_some_and(|r| r.contains(n.rect().center())))
                .unwrap_or_else(|| panic!("no navigator row {label:?}; labels: {:?}", labels(&self.h)));
            n.click();
            if double {
                n.click();
            }
        }
        self.frames(3);
    }

    fn nav_menu(&mut self, label: &str) {
        self.pause();
        let nav = self.h.state().chrome.nav;
        {
            let n = self
                .h
                .query_all_by_label(label)
                .find(|n| nav.is_some_and(|r| r.contains(n.rect().center())))
                .unwrap_or_else(|| panic!("no navigator row {label:?}; labels: {:?}", labels(&self.h)));
            n.click_secondary();
        }
        self.frames(3);
    }

    /// Click the widget labelled exactly `label` (the first one).
    fn click(&mut self, label: &str) {
        self.pause();
        {
            let n = self.h.query_all_by_label(label).next().unwrap_or_else(|| panic!("no widget labelled {label:?}; labels: {:?}", labels(&self.h)));
            n.click();
        }
        self.frames(3);
    }

    /// Click at `pos` (a tab of the tab bar, which has no label of its own).
    fn click_at(&mut self, pos: egui::Pos2) {
        self.pause();
        self.h.event(egui::Event::PointerMoved(pos));
        self.h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(4);
    }

    /// The centre of the widget labelled `label` (the last one: menus are on top).
    fn center_of(&self, label: &str) -> egui::Pos2 {
        self.h.query_all_by_label_contains(label).last().unwrap_or_else(|| panic!("no widget labelled {label:?}; labels: {:?}", labels(&self.h))).rect().center()
    }

    fn escape(&mut self) {
        self.h.key_press(egui::Key::Escape);
        self.frames(3);
    }

    fn close_tabs(&mut self) {
        self.command(AppCommand::CloseAllTabs);
        self.frames(4);
    }

    /// The tab titled `title` in the tab bar.
    fn tab(&mut self, title: &str) {
        let id = self.h.state().dock.iter_all_tabs().find(|(_, t)| t.view.title() == title).map(|(_, t)| t.id).expect("the tab");
        let rect = self.h.state().tab_rect(id).expect("the tab's button");
        self.click_at(rect.center());
    }

    fn focused_editor(&mut self) -> &mut SqlEditorTab {
        let (_, tab) = self.h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().expect("a SQL editor")
    }

    /// A SQL editor on `conn` titled `title`, with `sql`.
    fn editor(&mut self, conn: &str, title: &str, sql: &str) {
        self.command(AppCommand::NewSqlEditor { connection: Some(conn.to_string()), schema: None, text: Some(sql.into()) });
        self.wait("editor session", |h| {
            let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
            tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| e.session_ready())
        });
        self.focused_editor().set_title(title);
        self.frames(2);
    }

    fn run(&mut self, a: Action) {
        self.action(a);
        self.confirm_execution();
        self.wait("the result", |h| {
            let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
            tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| !e.is_running())
        });
        self.frames(4);
    }

    /// A production connection asks before statements that change data: execute them.
    fn confirm_execution(&mut self) {
        self.frames(3);
        if has(&self.h, "Confirm execution") {
            self.click("Execute");
            self.frames(3);
        }
    }

    /// The tabs of the editor mockups: the table (a production connection), report.sql with
    /// `sql` (active), Script-4.sql.
    fn editor_tabs(&mut self, conn: &str, sql: &str) {
        self.close_tabs();
        self.command(AppCommand::OpenObject { connection: conn.to_string(), object: table("customer"), page: ObjectPage::Data });
        self.editor(conn, "report.sql", sql);
        self.editor(conn, "Script-4.sql", "");
        self.tab("report.sql");
    }
}

fn table(name: &str) -> ternarygrid_core::ObjectRef {
    ternarygrid_core::ObjectRef::table(Some("main".into()), name)
}

/// The macOS window buttons, as the system paints them over the top bar's left end.
fn traffic_lights(img: &mut image::RgbaImage, ppp: f32) {
    for (i, c) in [[0xff, 0x5f, 0x57], [0xfe, 0xbc, 0x2e], [0x28, 0xc8, 0x40]].iter().enumerate() {
        let (cx, cy, r) = ((16.0 + 6.0 + 20.0 * i as f32) * ppp, 22.0 * ppp, 6.0 * ppp);
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

fn labels(h: &H) -> Vec<String> {
    h.query_all_by_label_contains("").filter_map(|n| n.accesskit_node().label()).collect()
}

fn has(h: &H, label: &str) -> bool {
    h.query_all_by_label_contains(label).next().is_some()
}

#[test]
#[ignore = "writes screenshots for visual review; run with --ignored"]
fn ui_snapshots_shell() {
    let folder = match std::env::var("TERNARYGRID_SNAPSHOT_LABEL") {
        Ok(l) if !l.is_empty() => format!("shell-{l}"),
        _ => "shell".to_string(),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots").join(folder);
    std::fs::create_dir_all(&root).unwrap();
    let themes: Vec<Theme> = match std::env::var("TERNARYGRID_SNAPSHOT_THEME").as_deref() {
        Ok("dark") => vec![Theme::Dark],
        Ok("light") => vec![Theme::Light],
        _ => vec![Theme::Dark, Theme::Light],
    };
    for theme in themes {
        // A new installation: the start page, no connections.
        let home = std::env::temp_dir().join(format!("ternarygrid-shell-shots-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        {
            let mut s = Shots::new(&home, root.clone(), theme, Vec::new());
            assert!(has(&s.h, "Create sample database"), "welcome page");
            s.shot("welcome");
        }
        let _ = std::fs::remove_dir_all(&home);
        // A workspace with a few connections.
        let home = std::env::temp_dir().join(format!("ternarygrid-shell-shots-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        let mut s = Shots::new(&home, root.clone(), theme, extra_connections());
        shots(&mut s);
        drop(s);
        let _ = std::fs::remove_dir_all(&home);
    }
}

const REVENUE: &str = "-- Revenue per customer, last 90 days
select c.name, c.city,
       count(distinct o.id) as orders,
       round(sum(l.quantity * l.unit_price), 2) as revenue
from customer c
join \"order\" o on o.customer_id = c.id
join order_line l on l.order_id = o.id
where o.status <> 'cancelled'
  and o.ordered_at > date('now', '-9000 days')
group by c.id
order by revenue desc;
";

fn shots(s: &mut Shots) {
    // The sample database, named like the mockups' connection, a production one.
    {
        let n = s.h.query_all_by_label_contains("Create sample database").next().expect("the start page");
        n.click();
    }
    s.wait("the sample connection", |h| h.state().conns.configs().iter().any(|c| c.name.starts_with("ternarygrid-sample") && h.state().conns.is_connected(&c.id)));
    let conn = s.h.state().conns.configs().iter().find(|c| c.name.starts_with("ternarygrid-sample")).unwrap().id.clone();
    {
        let mut cfg = s.h.state().conns.config(&conn).unwrap().clone();
        cfg.name = "bantuin".into();
        cfg.connection_type = "prod".into();
        s.h.state_mut().conns.upsert(cfg);
        s.frames(2);
    }
    // Scripts of the project (recent scripts, the projects view).
    {
        let pm = s.h.state().services.projects.clone().unwrap();
        let p = pm.default_project().unwrap();
        p.create_script_folder("reports").unwrap();
        for (f, sql) in [("reports/revenue_per_customer.sql", REVENUE), ("reports/orders_daily.sql", "select 2;"), ("reports/churn_cohorts.sql", "select 3;")] {
            std::fs::write(p.scripts_dir().join(f), sql).unwrap();
        }
        p.create_script_folder("maintenance").unwrap();
        std::fs::write(p.scripts_dir().join("report.sql"), "select 1;").unwrap();
    }
    for _ in 0..6 {
        s.pause();
    }
    // The navigator: the tables unfolded, one selected.
    s.nav_click("Tables", true);
    s.wait("tables", |h| h.query_all_by_label("customer").next().is_some());
    s.nav_click("customer", false);
    s.shot("welcome_recent");

    // The table's data and a script: the chrome of the main screen.
    s.command(AppCommand::OpenObject { connection: conn.clone(), object: table("customer"), page: ObjectPage::Data });
    s.wait("customer rows", |h| has(h, "Data grid,") && !has(h, "Data grid, 0 rows"));
    s.editor(&conn, "Script-3.sql", "select 1;");
    s.tab("customer");
    s.shot("main");
    // The New menu of the top bar.
    s.click("New");
    s.shot("new_menu");
    s.escape();

    // The SQL editor with a result.
    s.editor_tabs(&conn, REVENUE);
    s.run(Action::ExecuteStatement);
    s.shot("editor");

    // The navigator's menus over the editor.
    s.nav_menu("customer");
    let generate = s.center_of("Generate SQL");
    s.shot_at("nav_menu", Some(generate));
    s.escape();
    s.nav_menu("bantuin");
    let tools = s.center_of("Tools");
    s.shot_at("conn_menu", Some(tools));
    s.escape();
    s.nav_menu("Tables");
    s.shot("schema_menu");
    s.escape();
    s.click("More");
    s.shot("nav_more_menu");
    s.escape();
    // The properties panel under the tree.
    s.action(Action::ToggleProperties);
    s.nav_click("customer", false);
    s.shot("nav_props");
    s.action(Action::ToggleProperties);

    // The command palette, searching.
    s.h.state_mut().dialogs.palette_open = true;
    s.frames(10);
    s.h.event(egui::Event::Text("ord".into()));
    s.frames(20);
    s.shot("palette");
    s.escape();

    // An error.
    s.editor_tabs(&conn, &REVENUE.replace("where o.status", "where o.statuss"));
    s.run(Action::ExecuteStatement);
    s.shot("editor_error");

    // Manual commit, three statements pending.
    s.editor_tabs(
        &conn,
        "update customer set city = 'Bandung' where id = 42;\nupdate customer set city = 'Jakarta' where id = 43;\ndelete from order_line where order_id = 977;\n",
    );
    s.action(Action::ToggleAutoCommit);
    s.run(Action::ExecuteScript);
    s.click("Transaction log");
    s.shot("editor_manual");
    s.action(Action::Rollback);
    s.action(Action::ToggleAutoCommit);

    // A long statement running.
    s.editor_tabs(
        &conn,
        "select * from customer order by id desc;\nwith recursive n(i) as (select 1 union all select i + 1 from n where i < 300000000) select count(*) from n;\nselect 3;\n",
    );
    s.action(Action::ExecuteScript);
    s.confirm_execution();
    s.frames(30);
    s.shot("editor_running");
    s.action(Action::CancelQuery);
    s.close_tabs();

    // Projects and scripts.
    s.click("Projects and scripts");
    s.frames(4);
    s.shot("projects");
    s.click("Database Navigator");

    // The navigator's states: filtered, a connection that failed, an empty folder.
    let failed = s.h.state().conns.configs().iter().find(|c| c.name == "local-notes.db").unwrap().id.clone();
    let _ = s.nav_node("local-notes.db");
    s.nav_click("local-notes.db", true);
    s.wait("the failed connection", |h| matches!(h.state().conns.status(&failed), ternarygrid_app::conn::ConnStatus::Failed(_)));
    s.h.state_mut().nav.filter = "order".into();
    s.frames(6);
    s.shot("nav_states");
    s.h.state_mut().nav.filter.clear();
    s.frames(6);
    s.shot("nav_failed");
}
