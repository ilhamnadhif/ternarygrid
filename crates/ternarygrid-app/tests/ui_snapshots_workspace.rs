//! Screenshots of the workspace screens (start page, navigator, projects,
//! settings, query history, tasks, search, error log) for reviewing the
//! redesign, rasterised on the CPU (no GPU needed). Not part of the normal
//! test run:
//!
//! ```text
//! cargo test -p ternarygrid-app --test ui_snapshots_workspace -- --ignored
//! ```
//!
//! writes `target/ui-snapshots/workspace/<shot>-<light|dark>.png` (the folder
//! is `workspace-<label>` with `TERNARYGRID_SNAPSHOT_LABEL=<label>`).

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
use ternarygrid_app::command::AppCommand;
use ternarygrid_app::conn::{ConnectionManager, DirectNet, MemorySecrets};
use ternarygrid_app::keys::Action;
use ternarygrid_app::services::{Services, Settings};
use ternarygrid_app::tabs::sql_editor::SqlEditorTab;
use ternarygrid_config::settings::Theme;

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
        let mut services = Services::ephemeral(&home.join("home"));
        // A scheduler (the ephemeral services have none): tasks can be run.
        if let (Some(store), Some(log)) = (services.tasks.clone(), services.runlog.clone()) {
            let exec = ternarygrid_app::headless::executor(&services.paths, services.secrets.clone());
            services.scheduler = Some(Arc::new(ternarygrid_tasks::Scheduler::new(store, log, exec, ternarygrid_tasks::SchedulerOptions::default(), backend.handle())));
        }
        let conns = ConnectionManager::new(ternarygrid_app::driver_registry(), Arc::new(MemorySecrets::default()), Arc::new(DirectNet));
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
                // As on macOS (the mockups): the menus in the system's menu bar, the window
                // buttons at the left of the top bar (painted by `shot`).
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
        // From there the clock runs on by the step (a fixed time would stop the fades).
        self.h.input_mut().time = None;
    }

    /// Write the last frame as `<name>-<theme>.png`.
    fn shot(&mut self, name: &str) {
        if self.only.as_deref().is_some_and(|o| !o.split(',').any(|o| name.starts_with(o))) {
            return;
        }
        self.h.state_mut().toasts.clear();
        // The pointer away from the content (no hover effects).
        self.h.event(egui::Event::PointerMoved(egui::pos2(1439.0, 899.0)));
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

    fn command(&mut self, c: AppCommand) {
        self.h.state_mut().push_command(c);
        self.frames(4);
    }

    fn action(&mut self, a: Action) {
        let ctx = self.h.ctx.clone();
        self.h.state_mut().dispatch_action(&ctx, a);
        self.frames(4);
    }

    /// Click the widget labelled exactly `label` (outside the top bar and the rail).
    fn click(&mut self, label: &str) {
        self.pause();
        {
            let chrome = self.h.state().chrome.top_bottom();
            let rail = self.h.state().chrome.rail;
            let n = self
                .h
                .query_all_by_label(label)
                .find(|n| n.rect().center().y > chrome && !rail.is_some_and(|r| r.contains(n.rect().center())))
                .or_else(|| self.h.query_all_by_label(label).next())
                .or_else(|| self.h.query_all_by_label_contains(label).find(|n| n.rect().center().y > chrome))
                .unwrap_or_else(|| panic!("no widget labelled {label:?}; labels: {:?}", labels(&self.h)));
            n.click();
        }
        self.frames(3);
    }

    fn nav_row_click(&mut self, label: &str, double: bool) {
        self.pause();
        {
            let nav = self.h.state().chrome.nav;
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

    fn nav_row_menu(&mut self, label: &str) {
        self.pause();
        {
            let nav = self.h.state().chrome.nav;
            let n = self
                .h
                .query_all_by_label(label)
                .find(|n| nav.is_some_and(|r| r.contains(n.rect().center())))
                .unwrap_or_else(|| panic!("no navigator row {label:?}; labels: {:?}", labels(&self.h)));
            n.click_secondary();
        }
        self.frames(3);
    }

    fn focused<T: 'static>(&mut self) -> &mut T {
        let (_, tab) = self.h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<T>().expect("the focused tab's type")
    }

    fn close_tabs(&mut self) {
        self.command(AppCommand::CloseAllTabs);
        self.frames(4);
    }
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

fn run_sql(s: &mut Shots, conn: &str, sql: &str) {
    s.command(AppCommand::NewSqlEditor { connection: Some(conn.to_string()), schema: None, text: Some(sql.into()) });
    s.wait("editor session", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| e.session_ready())
    });
    s.action(Action::ExecuteStatement);
    s.wait("the result", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| !e.is_running())
    });
}

#[test]
#[ignore = "writes screenshots for visual review; run with --ignored"]
fn ui_snapshots_workspace() {
    let folder = match std::env::var("TERNARYGRID_SNAPSHOT_LABEL") {
        Ok(l) if !l.is_empty() => format!("workspace-{l}"),
        _ => "workspace".to_string(),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots").join(folder);
    std::fs::create_dir_all(&root).unwrap();
    let themes: Vec<Theme> = match std::env::var("TERNARYGRID_SNAPSHOT_THEME").as_deref() {
        Ok("dark") => vec![Theme::Dark],
        Ok("light") => vec![Theme::Light],
        _ => vec![Theme::Dark, Theme::Light],
    };
    for theme in themes {
        let home = std::env::temp_dir().join(format!("ternarygrid-ws-shots-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        let mut s = Shots::new(&home, root.clone(), theme);
        shots(&mut s);
        drop(s);
        let _ = std::fs::remove_dir_all(&home);
    }
}

fn shots(s: &mut Shots) {
    // The start page of a new installation, the navigator's empty state.
    assert!(has(&s.h, "Create sample database"), "welcome page");
    s.shot("welcome");

    // The sample database: the start page with a recent connection (production).
    s.click("Create sample database");
    s.wait("the sample connection", |h| {
        let app = h.state();
        app.conns.configs().len() == 1 && app.conns.is_connected(&app.conns.configs()[0].id.clone())
    });
    let conn = s.h.state().conns.configs()[0].id.clone();
    let set_type = |s: &mut Shots, t: &str| {
        let mut cfg = s.h.state().conns.config(&conn).unwrap().clone();
        cfg.connection_type = t.into();
        s.h.state_mut().conns.upsert(cfg);
        s.frames(2);
    };
    set_type(s, "prod");
    // Scripts of the project (the start page's recent scripts, the projects view).
    {
        let pm = s.h.state().services.projects.clone().unwrap();
        let p = pm.default_project().unwrap();
        p.create_script_folder("reports").unwrap();
        for (f, sql) in [("reports/revenue_per_customer.sql", "select 1;"), ("reports/orders_daily.sql", "select 2;"), ("report.sql", "select 3;")] {
            std::fs::write(p.scripts_dir().join(f), sql).unwrap();
        }
        p.create_script_folder("maintenance").unwrap();
        let _ = p.add_bookmark(ternarygrid_config::Bookmark {
            id: String::new(),
            connection_id: conn.clone(),
            object: ternarygrid_core::ObjectRef::table(Some("main".into()), "customer"),
            label: "main.customer".into(),
            folder: None,
            created_ms: 0,
        });
    }
    // The start page reads the recent scripts every few seconds.
    for _ in 0..6 {
        s.pause();
    }
    s.frames(4);
    s.shot("welcome_recent");

    // The navigator: a connection unfolded, a table selected.
    s.nav_row_click("Tables", true);
    s.wait("tables", |h| h.query_all_by_label("customer").next().is_some());
    s.nav_row_click("customer", false);
    s.shot("navigator");
    s.nav_row_menu("customer");
    s.shot("nav_menu");
    s.h.key_press(egui::Key::Escape);
    s.frames(3);
    s.nav_row_menu(&s.h.state().conns.config(&conn).unwrap().name.clone());
    s.shot("conn_menu");
    s.h.key_press(egui::Key::Escape);
    s.frames(3);
    // The properties panel under the tree.
    s.action(Action::ToggleProperties);
    s.nav_row_click("customer", false);
    s.shot("nav_props");
    s.action(Action::ToggleProperties);
    // Filtered.
    s.h.state_mut().nav.filter = "cust".into();
    s.frames(4);
    s.shot("nav_filtered");
    s.h.state_mut().nav.filter.clear();
    s.frames(2);
    set_type(s, "dev");

    // Projects and scripts.
    s.click("Projects and scripts");
    s.frames(4);
    s.shot("projects");
    s.click("Database Navigator");

    // Query history: a few statements, one failing.
    run_sql(s, &conn, "select * from product order by price desc");
    run_sql(s, &conn, "select * from customer where id = 1");
    run_sql(s, &conn, "select * from nope where statuss = 'open'");
    s.close_tabs();
    s.command(AppCommand::OpenHistory);
    s.frames(10);
    s.shot("history_empty_selection");
    {
        // Select the failing statement (the newest entry).
        s.pause();
        let n = s.h.query_all_by_label_contains("statuss").next().map(|n| n.rect());
        if let Some(r) = n {
            s.h.event(egui::Event::PointerMoved(r.center()));
            for pressed in [true, false] {
                s.h.event(egui::Event::PointerButton { pos: r.center(), button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            }
            s.frames(3);
        }
    }
    s.shot("history");
    s.close_tabs();

    // Settings: every section, with the values of the mockups.
    {
        use ternarygrid_config::settings::{AiEffort, KeywordCase, RowDensity};
        let st = &mut s.h.state_mut().settings;
        st.editor.autocomplete.delay_ms = 150;
        st.editor.autocomplete.insert_single_proposal = false;
        st.editor.autocomplete.keyword_case = KeywordCase::Lower;
        st.editor.format.keyword_case = KeywordCase::Lower;
        st.editor.format.max_line_length = 100;
        st.results.row_density = RowDensity::Normal;
        st.results.store_memory_limit_mb = 512;
        st.results.global_memory_limit_mb = 2048;
        st.results.max_text_length = 1_000_000;
        st.results.format.null_text = "NULL".into();
        st.results.format.bool_style = ternarygrid_core::value::BoolStyle::TrueFalse;
        st.history.max_entries = 100_000;
        st.ai.enabled = true;
        st.ai.max_tokens = 8192;
        st.ai.effort = AiEffort::Medium;
        st.ai.model = "claude-opus-5-5".into();
    }
    // Network profiles of the mockup.
    {
        use ternarygrid_core::config::{NetworkProfile, SshAuth, SshConfig};
        for (name, host) in [("Office bastion", "bastion.office.example.com"), ("AWS jump host", "jump.aws.example.com"), ("Corporate proxy", "")] {
            let ssh = (!host.is_empty()).then(|| SshConfig {
                enabled: true,
                host: host.into(),
                user: "ilham".into(),
                auth: SshAuth::PublicKey { key_path: "~/.ssh/id_ed25519".into() },
                ..Default::default()
            });
            s.h.state_mut().conns.store.set_network_profile(NetworkProfile { id: name.to_lowercase().replace(' ', "-"), name: name.into(), ssh, proxy: None });
        }
    }
    s.command(AppCommand::OpenSettings);
    for (section, name) in [
        ("General", "set_general"),
        ("SQL editor", "set_editor"),
        ("Result sets", "set_results"),
        ("Execution", "set_execution"),
        ("Query history", "set_history"),
        ("Navigator", "set_navigator"),
        ("Keyboard shortcuts", "set_keys"),
        ("Connection types", "set_types"),
        ("Network profiles", "set_network"),
        ("AI assistant", "set_ai"),
        ("Security", "set_security"),
        ("Accessibility", "set_a11y"),
    ] {
        s.click(section);
        s.frames(3);
        match section {
            "Connection types" => s.click("Production"),
            "Network profiles" => s.click("Office bastion"),
            _ => {}
        }
        s.frames(3);
        s.shot(name);
        if section == "Network profiles" {
            s.click("New profile");
            s.frames(3);
            s.shot("set_network_form");
        }
    }
    // A select open (the menu look of the drop-downs): the formatter of the SQL editor.
    s.click("SQL editor");
    s.frames(3);
    let found = s.h.query_all_by_role(egui::accesskit::Role::ComboBox).next().map(|n| n.click()).is_some();
    if found {
        s.frames(6);
        s.shot("set_select");
        s.h.key_press(egui::Key::Escape);
        s.frames(3);
    }
    // "Search settings": the sections with a match and their counts, the matches marked.
    {
        // The settings' own search: the text field at the top of their side navigation (right
        // of the navigator's filter).
        let nav = s.h.query_all_by_label("Network profiles").next().expect("the settings navigation").rect();
        let n =
            s.h.query_all_by_role(egui::accesskit::Role::TextInput)
                .find(|n| n.rect().left() >= nav.left() - 20.0 && n.rect().right() <= nav.right() + 20.0 && n.rect().top() < nav.top())
                .expect("the settings search");
        n.focus();
        n.type_text("time");
    }
    s.frames(6);
    s.shot("set_search");
    s.close_tabs();

    // Tasks: a few tasks in folders, runs.
    let store = s.h.state().services.tasks.clone().unwrap();
    use ternarygrid_tasks::{Schedule, TaskDef, TaskKind};
    let sql_task = |sql: &str| TaskKind::SqlScript {
        connection_id: conn.clone(),
        database: None,
        schema: None,
        sql: Some(sql.into()),
        script_path: None,
        stop_on_error: true,
        commit_at_end: true,
    };
    // The mockup's tasks: a backup, an export, a SQL script, a schema compare that failed, a
    // disabled shell command and a composite task.
    for f in ["exports", "exports/nightly", "backups", "maintenance"] {
        store.create_folder(f).unwrap();
    }
    let in_folder = |mut t: TaskDef, f: &str| {
        t.folder = Some(f.into());
        t
    };
    let daily = |at: &str| Schedule::Daily { at: at.into(), weekdays: vec![] };
    let tb = store
        .create(in_folder(TaskDef::new("bantuin nightly", TaskKind::Backup { connection_id: conn.clone(), options: Default::default() }), "backups").with_schedule(daily("02:00")))
        .unwrap();
    let te = store
        .create(
            in_folder(TaskDef::new("orders to CSV", TaskKind::Export { connection_id: conn.clone(), config: serde_json::json!({}) }), "exports/nightly")
                .with_schedule(daily("01:30")),
        )
        .unwrap();
    let mut t1 = in_folder(TaskDef::new("refresh rollups", sql_task("select count(*) from customer;")), "maintenance");
    t1.description = Some("Rebuilds the daily and weekly rollup tables".into());
    t1 = t1.with_schedule(Schedule::Daily { at: "06:30".into(), weekdays: vec![1, 2, 3, 4, 5] });
    let t1 = store.create(t1).unwrap();
    let t2 = store
        .create(TaskDef::new("staging vs prod schema", TaskKind::SchemaCompare { config: serde_json::json!({}) }).with_schedule(Schedule::Cron("0 9 * * 1-5".into())))
        .unwrap();
    let mut t3 = in_folder(TaskDef::new("rotate logs", TaskKind::Shell { command: "echo rotated".into() }), "maintenance");
    t3.enabled = false;
    let t3 = store.create(t3).unwrap();
    // Its steps as in the mockup (without its deleted one: the store does not keep those).
    let steps = vec![tb.id.clone(), te.id.clone(), t1.id.clone()];
    let t4 = store
        .create(TaskDef::new("Nightly chain", TaskKind::Composite { steps, stop_on_error: true, parallel: false }).with_schedule(Schedule::Cron("30 3 * * *".into())))
        .unwrap();
    // Runs in the run log: the last one of the schema compare failed.
    let log = s.h.state().services.runlog.clone().unwrap();
    let now = chrono::Utc::now();
    let record = |task: &TaskDef,
                  ago_min: i64,
                  secs: i64,
                  status: ternarygrid_tasks::RunStatus,
                  trigger: ternarygrid_tasks::Trigger,
                  error: Option<&str>,
                  msgs: &[(i64, ternarygrid_tasks::MessageLevel, &str)]| {
        let started = now - chrono::Duration::minutes(ago_min);
        let mut r = ternarygrid_tasks::TaskRun::start(task, trigger, started, None);
        r.finished = Some(started + chrono::Duration::seconds(secs));
        r.status = status;
        r.error = error.map(str::to_string);
        for (at, level, text) in msgs {
            r.push_message(ternarygrid_tasks::RunMessage { time: started + chrono::Duration::seconds(*at), level: *level, text: text.to_string() });
        }
        log.record(&r).unwrap();
    };
    use ternarygrid_tasks::{MessageLevel as L, RunStatus as R, Trigger as T};
    record(&tb, 330, 41, R::Succeeded, T::Schedule, None, &[]);
    record(&te, 360, 6, R::Succeeded, T::Schedule, None, &[]);
    record(&t1, 300, 41, R::Succeeded, T::Schedule, None, &[(0, L::Info, "Connecting to ternarygrid-sample.db…"), (41, L::Info, "1 statement(s) run")]);
    record(&t2, 2900, 3, R::Succeeded, T::Manual, None, &[(3, L::Info, "12 differences")]);
    record(&t2, 1460, 4, R::Succeeded, T::Schedule, None, &[(4, L::Info, "14 differences")]);
    let refused = "connection refused (os error 61): tplus-staging:3306";
    record(&t2, 20, 2, R::Failed, T::Schedule, Some(refused), &[(0, L::Info, "Connecting to tplus-staging…"), (2, L::Error, "connection refused (os error 61)")]);
    record(&t4, 240, 72, R::Succeeded, T::Schedule, None, &[]);
    // Scheduled runs off: the tab says so at its top.
    s.h.state_mut().settings.tasks.scheduler_enabled = false;
    s.command(AppCommand::OpenTasks);
    s.frames(6);
    s.focused::<ternarygrid_app::tabs::tasks::TasksTab>().select_task(&t2.id);
    s.wait("run history", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().unwrap();
        let t = tab.view.as_any_mut().downcast_mut::<ternarygrid_app::tabs::tasks::TasksTab>().unwrap();
        !t.history().is_empty() && !t.is_busy()
    });
    s.frames(4);
    s.shot("tasks");
    // The task editor: a SQL script, then a composite task.
    s.focused::<ternarygrid_app::tabs::tasks::TasksTab>().edit_task(&t1.id);
    s.frames(30);
    s.shot("task_editor");
    s.click("Cancel");
    s.frames(10);
    s.focused::<ternarygrid_app::tabs::tasks::TasksTab>().edit_task(&t4.id);
    s.frames(30);
    s.shot("task_composite");
    s.click("Cancel");
    s.frames(10);
    // Delete (asks first: two tasks, as the mockup), a new folder.
    s.focused::<ternarygrid_app::tabs::tasks::TasksTab>().select_task(&t3.id);
    s.focused::<ternarygrid_app::tabs::tasks::TasksTab>().add_to_selection(&t2.id);
    s.frames(3);
    s.click("Delete");
    s.frames(20);
    s.shot("task_delete");
    s.click("Cancel");
    s.frames(10);
    s.click("New folder");
    s.frames(20);
    s.shot("task_folder");
    s.click("Cancel");
    s.close_tabs();

    // Search metadata, then data.
    s.command(AppCommand::OpenSearch { connection: None, data: false });
    s.focused::<ternarygrid_app::tabs::search::SearchTab>().set_pattern("*order*");
    s.click("Search comments");
    s.click("Search");
    s.wait("metadata results", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().unwrap();
        let t = tab.view.as_any_mut().downcast_mut::<ternarygrid_app::tabs::search::SearchTab>().unwrap();
        !t.is_busy()
    });
    s.frames(4);
    // The first result selected, as in the mockup.
    s.click("main.order");
    s.shot("search");
    s.close_tabs();
    s.command(AppCommand::OpenSearch { connection: Some(conn.clone()), data: true });
    s.focused::<ternarygrid_app::tabs::search::SearchTab>().set_data_search(ternarygrid_core::meta::SchemaRef::new(None, Some("main".into())), "a", &[]);
    s.frames(10);
    s.focused::<ternarygrid_app::tabs::search::SearchTab>().request_data_search();
    s.wait("data results", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().unwrap();
        let t = tab.view.as_any_mut().downcast_mut::<ternarygrid_app::tabs::search::SearchTab>().unwrap();
        t.data_finished()
    });
    s.frames(4);
    s.shot("search_data");
    s.close_tabs();

    // The error log.
    // Oldest first: the log shows the newest at the top, as the mockup.
    for (l, m) in [
        (tracing::Level::ERROR, "Task \"staging vs prod schema\" failed: connection refused"),
        (tracing::Level::WARN, "Spill file grew to 512 MB for result 'select * from product_events'"),
        (tracing::Level::ERROR, "Cannot connect to slypee-prod: connection refused (os error 61)"),
        (tracing::Level::WARN, "Import into customer: 3 rows had dates in another format; read as text"),
        (tracing::Level::WARN, "tplus-staging: the keep-alive ping took 2.4 s"),
        (tracing::Level::ERROR, "slypee-prod: column statuss does not exist (SQLSTATE 42703)"),
    ] {
        ternarygrid_app::applog::push(l, "ternarygrid::shots", m);
    }
    s.action(Action::ShowErrorLog);
    s.frames(4);
    s.shot("errorlog");
    s.close_tabs();

    // The navigator's states: a connection that failed, unfolded.
    let mut bad = ternarygrid_core::config::ConnectionConfig::new(ternarygrid_core::DbKind::Sqlite, "broken.db");
    bad.file_path = Some("/nonexistent/folder/broken.db".into());
    let bad_id = bad.id.clone();
    s.h.state_mut().conns.upsert(bad);
    s.frames(3);
    s.nav_row_click("broken.db", true);
    s.wait("the failed connection", |h| matches!(h.state().conns.status(&bad_id), ternarygrid_app::conn::ConnStatus::Failed(_)));
    s.frames(4);
    s.shot("nav_states");
}
