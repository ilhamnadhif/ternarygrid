//! Screenshots of the dialogs and windows (connection dialog, confirmations,
//! pending transactions, operations, updates, tips, toasts, the pop-up tool
//! windows) in the light and the dark theme, rasterised on the CPU. Not part
//! of the normal test run:
//!
//! ```text
//! cargo test -p ternarygrid-app --test ui_snapshots_dialogs -- --ignored
//! ```
//!
//! writes `target/ui-snapshots/dialogs/<shot>-<light|dark>.png`.

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
use ternarygrid_app::conn::{ConnectionManager, DirectNet, MemorySecrets, PromptField};
use ternarygrid_app::keys::Action;
use ternarygrid_app::ops::OpState;
use ternarygrid_app::services::{Services, Settings};
use ternarygrid_app::tabs::sql_editor::SqlEditorTab;
use ternarygrid_config::settings::Theme;
use ternarygrid_core::ObjectRef;

type H = Harness<'static, TernaryGridApp>;

/// Pixels per point of the screenshots (a Retina display).
const PPP: f32 = 2.0;

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
            assert!(t0.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
            self.h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Let windows fade in and the layout settle, then write `<name>-<theme>.png`.
    fn shot(&mut self, name: &str) {
        self.h.state_mut().toasts.clear();
        self.shot_with_toasts(name);
    }

    fn shot_with_toasts(&mut self, name: &str) {
        // The pointer away from the content: no hover effects (as in the design's pictures).
        self.h.event(egui::Event::PointerMoved(egui::pos2(1439.0, 899.0)));
        self.frames(30);
        if self.only.as_deref().is_some_and(|o| !o.split(',').any(|o| name.contains(o))) {
            return;
        }
        let out = self.h.output();
        let prims = self.h.ctx.tessellate(out.shapes.clone(), PPP);
        let size = self.h.ctx.content_rect().size() * PPP;
        let bg = self.h.ctx.global_style().visuals.panel_fill;
        let mut img = self.raster.borrow().render(&prims, [size.x as usize, size.y as usize], bg);
        traffic_lights(&mut img, PPP);
        let path = self.dir.join(format!("{name}-{}.png", self.theme));
        img.save(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        eprintln!("wrote {}", path.display());
    }

    /// Click the last widget labelled exactly `label`.
    fn click(&mut self, label: &str) {
        {
            let n = self.h.query_all_by_label(label).last().unwrap_or_else(|| panic!("no widget labelled {label:?}"));
            n.click();
        }
        self.frames(3);
    }

    /// Type `text` into the field at the right of the label `label`.
    fn type_right_of(&mut self, label: &str, text: &str) {
        {
            // The label with a field on its row (a combo box may show the same text).
            let fields: Vec<_> = self.h.query_all_by_role(egui::accesskit::Role::TextInput).chain(self.h.query_all_by_role(egui::accesskit::Role::PasswordInput)).collect();
            let f = self
                .h
                .query_all_by_label(label)
                .filter_map(|l| {
                    let lr = l.rect();
                    fields.iter().find(|n| n.rect().left() > lr.right() && (n.rect().center().y - lr.center().y).abs() < 12.0)
                })
                .next_back()
                .unwrap_or_else(|| panic!("no field next to {label:?}"));
            f.click();
        }
        self.frames(2);
        self.h.event(egui::Event::Text(text.into()));
        self.frames(2);
    }

    /// Close every dialog, window and pop-up.
    fn reset(&mut self) {
        let app = self.h.state_mut();
        app.dialogs = Default::default();
        app.tip = None;
        app.pending_tx.open = false;
        app.ops.panel_open = false;
        app.updates.whats_new = None;
        app.updates.available = None;
        app.updates.ask = false;
        let popups: Vec<u64> = app.popups.iter().map(|t| t.id).collect();
        for id in popups {
            app.close_tab(id);
        }
        self.frames(4);
    }

    /// Open the combo box at the right of the label `label`.
    fn open_combo_right_of(&mut self, label: &str) {
        {
            let combos: Vec<_> = self.h.query_all_by_role(egui::accesskit::Role::ComboBox).collect();
            let c = self
                .h
                .query_all_by_label(label)
                .filter_map(|l| {
                    let lr = l.rect();
                    combos.iter().find(|n| n.rect().left() > lr.right() && (n.rect().center().y - lr.center().y).abs() < 12.0)
                })
                .next_back()
                .unwrap_or_else(|| panic!("no combo box next to {label:?}"));
            c.click();
        }
        self.frames(3);
    }

    /// Nothing has the keyboard focus (as in the design's pictures).
    fn unfocus(&mut self) {
        self.h.ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        self.frames(2);
    }

    /// Wait until a widget whose label contains `text` is shown.
    fn wait_label(&mut self, text: &str) {
        let t = text.to_string();
        self.wait(text, |h| h.query_all_by_label_contains(&t).next().is_some());
    }

    /// Empty the field at the right of the label `label` (select all, delete).
    fn clear_right_of(&mut self, label: &str) {
        self.type_right_of(label, "");
        self.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        self.frames(1);
        self.h.key_press(egui::Key::Backspace);
        self.frames(2);
    }

    /// Replace the text of the field at the right of the label `label`.
    fn set_right_of(&mut self, label: &str, text: &str) {
        self.clear_right_of(label);
        self.h.event(egui::Event::Text(text.into()));
        self.frames(2);
    }
}

/// The query of the SQL editor behind the design's dialogs.
const BACKDROP_SQL: &str = "-- Revenue per customer, last 90 days
select c.name, c.city,
       count(distinct o.id) as orders,
       round(sum(l.quantity * l.unit_price), 2) as revenue
from customer c
join \"order\" o on o.customer_id = c.id
join order_line l on l.order_id = o.id
where o.status <> 'cancelled'
  and o.created_at > now() - interval '90 days'
group by c.id
order by revenue desc;
";

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

/// A DBeaver workspace with the connections of the design's import dialog (and two
/// databases TernaryGrid does not support, for the notes).
fn dbeaver_workspace(home: &Path) -> PathBuf {
    let ws = home.join("Library/DBeaverData/workspace6");
    let dir = ws.join("General/.dbeaver");
    std::fs::create_dir_all(&dir).unwrap();
    let json = r#"{
      "folders": {"Clients": {}, "Data": {}},
      "connections": {
        "c1": {"provider": "postgresql", "driver": "postgres-jdbc", "name": "slypee-prod", "folder": "Clients",
               "configuration": {"host": "10.0.4.12", "port": 5432, "database": "slypee", "user": "app"}},
        "c2": {"provider": "postgresql", "driver": "postgres-jdbc", "name": "bantuin",
               "configuration": {"host": "localhost", "port": 5432, "database": "bantuin"}},
        "c3": {"provider": "mysql", "driver": "mysql8", "name": "tplus-staging", "folder": "Clients",
               "configuration": {"host": "staging.db.internal", "port": 3306,
                                 "events": {"BEFORE_CONNECT": {"enabled": true, "command": "ssh -fN staging-tunnel"}}}},
        "c4": {"provider": "clickhouse", "driver": "com_clickhouse", "name": "analytics", "folder": "Data",
               "configuration": {"host": "ch.internal", "port": 8123}},
        "c5": {"provider": "generic", "driver": "sqlite_jdbc", "name": "local-notes", "configuration": {"database": "~/notes.db"}},
        "c6": {"provider": "db2", "driver": "db2_iseries", "name": "legacy-db2", "configuration": {"host": "as400.internal"}},
        "c7": {"provider": "generic", "driver": "firebird", "name": "old-firebird", "configuration": {"host": "fb.internal"}}
      }
    }"#;
    std::fs::write(dir.join("data-sources.json"), json).unwrap();
    ws
}

/// The PostgreSQL connection of the design's connection dialogs, set up page by page.
fn slypee_prod() -> ternarygrid_core::config::ConnectionConfig {
    use ternarygrid_core::config::*;
    let mut pg = ConnectionConfig::new(ternarygrid_core::DbKind::Postgres, "slypee-prod");
    pg.host = "10.0.4.12".into();
    pg.user = Some("app".into());
    pg.database = Some("slypee".into());
    pg.connection_type = "prod".into();
    pg.save_password = true;
    pg.ssh = Some(SshConfig {
        enabled: true,
        host: "bastion.office.example.com".into(),
        user: "ilham".into(),
        auth: SshAuth::PublicKey { key_path: "~/.ssh/id_ed25519".into() },
        save_credentials: true,
        keepalive_secs: 30,
        ..Default::default()
    });
    pg.proxy = Some(ProxyConfig { enabled: true, host: "proxy.office.example.com".into(), user: Some("ilham".into()), ..Default::default() });
    pg.ssl = SslConfig { mode: SslMode::VerifyFull, ca_cert: Some("~/certs/rds-ca-2026.pem".into()), only_custom_ca: true, ..Default::default() };
    pg.init.default_schema = Some("public".into());
    pg.init.keep_alive_secs = 0;
    pg.init.connect_timeout_secs = 10;
    pg.init.query_timeout_secs = 30;
    pg.init.separate_editor_connection = true;
    pg.init.init_sql = "set statement_timeout = '30s';\nset search_path = public, extensions;\n".into();
    pg.properties.insert("application_name".into(), "TernaryGrid".into());
    pg.properties.insert("options".into(), "-c lock_timeout=5s".into());
    pg.filters.schemas = NameFilter { enabled: true, include: vec!["public".into(), "billing".into()], exclude: vec!["pg_temp*".into()] };
    pg.filters.tables = NameFilter { enabled: true, include: vec![], exclude: vec!["*_backup".into(), "tmp_*".into()] };
    pg.folder = Some("Clients/Slypee".into());
    pg.description = Some("Production replica, read mostly".into());
    pg.color = Some([0xf2, 0x8b, 0x7d]);
    pg
}

#[test]
#[ignore = "writes screenshots for visual review; run with --ignored"]
fn ui_snapshots_dialogs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots/dialogs");
    std::fs::create_dir_all(&root).unwrap();
    let themes: Vec<Theme> = match std::env::var("TERNARYGRID_SNAPSHOT_THEME").as_deref() {
        Ok("dark") => vec![Theme::Dark],
        Ok("light") => vec![Theme::Light],
        _ => vec![Theme::Dark, Theme::Light],
    };
    // A home of its own, so paths read as the design's (`~/Library/DBeaverData/workspace6`).
    let base = std::env::temp_dir().join(format!("ternarygrid-dlg-shots-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&base).unwrap();
    // SAFETY: set before the app and its threads start; nothing else runs in this process.
    unsafe { std::env::set_var("HOME", &base) };
    for theme in themes {
        let home = base.join(if theme == Theme::Dark { "dark" } else { "light" });
        std::fs::create_dir_all(&home).unwrap();
        let mut s = Shots::new(&home, root.clone(), theme);

        // The sample database (connected) and the PostgreSQL connections of the design (not).
        s.h.query_all_by_label_contains("Create sample database").next().unwrap().click();
        s.wait("the sample connection", |h| {
            let app = h.state();
            app.conns.configs().len() == 1 && app.conns.is_connected(&app.conns.configs()[0].id.clone())
        });
        let conn = s.h.state().conns.configs()[0].id.clone();
        let pg = slypee_prod();
        let pg_id = pg.id.clone();
        {
            use ternarygrid_core::config::{SecretSlot, secret_key};
            let app = s.h.state_mut();
            app.conns.secrets.set(&secret_key(&pg_id, SecretSlot::Password), &ternarygrid_core::Secret::new("secret-password")).unwrap();
            app.conns.secrets.set(&secret_key(&pg_id, SecretSlot::ProxyPassword), &ternarygrid_core::Secret::new("proxy-pw")).unwrap();
            app.conns.upsert(pg);
            for (kind, name) in [(ternarygrid_core::DbKind::MySql, "tplus-staging"), (ternarygrid_core::DbKind::Postgres, "slypee-replica")] {
                let mut c = ternarygrid_core::config::ConnectionConfig::new(kind, name);
                c.host = "10.0.4.13".into();
                c.folder = Some("Clients/Slypee".into());
                app.conns.upsert(c);
            }
        }
        s.frames(3);
        // Behind the dialogs, as in the design: a SQL editor with the report's query.
        s.h.state_mut().push_command(AppCommand::NewSqlEditor { connection: Some(conn.clone()), schema: None, text: Some(BACKDROP_SQL.into()) });
        s.frames(4);

        // New connection: Main (as filled in), a missing name after a failed test, a SQLite file.
        let ctx = s.h.ctx.clone();
        s.h.state_mut().dispatch_action(&ctx, Action::NewConnection);
        s.frames(3);
        for (label, text) in
            [("Connection name", "bantuin"), ("Host", "db.bantuin.internal"), ("Port", "5432"), ("Database", "bantuin"), ("User", "app"), ("Password", "secret-pass!")]
        {
            s.set_right_of(label, text);
        }
        s.open_combo_right_of("Connection type");
        // A drop-down in the design's menu look.
        s.shot("conn_type_menu");
        s.click("Production");
        s.unfocus();
        s.shot("conn_main");
        s.reset();
        s.h.state_mut().dispatch_action(&ctx, Action::NewConnection);
        s.frames(3);
        for (label, text) in [("Connection name", "x"), ("Host", "127.0.0.1"), ("Port", "1"), ("Database", "slypee"), ("User", "app"), ("Password", "secret")] {
            s.set_right_of(label, text);
        }
        s.click("Test connection");
        s.wait_label("refused");
        s.clear_right_of("Connection name");
        s.click("Save");
        s.shot("conn_error");
        s.reset();
        s.h.state_mut().dispatch_action(&ctx, Action::NewConnection);
        s.frames(3);
        s.click("SQLite");
        s.type_right_of("Connection name", "local-notes");
        s.type_right_of("Database file", "~/notes.db");
        s.unfocus();
        s.shot("conn_sqlite");
        s.reset();

        // Edit connection: every page of slypee-prod.
        {
            let app = s.h.state_mut();
            app.dialogs.edit_connection(&pg_id, &app.conns);
        }
        s.frames(3);
        s.shot("conn_edit_main");
        for (page, name) in [
            ("SSH", "conn_ssh"),
            ("Proxy", "conn_proxy"),
            ("SSL", "conn_ssl"),
            ("Initialization", "conn_init"),
            ("Driver properties", "conn_driver"),
            ("Filters", "conn_filters"),
            ("General", "conn_general"),
        ] {
            s.click(page);
            s.shot(name);
        }
        s.reset();

        // Small modals.
        {
            let app = s.h.state_mut();
            app.dialogs.open_credentials(&pg_id, vec![PromptField::Password, PromptField::SshPassword], &app.conns);
        }
        s.shot("credential");
        s.reset();
        {
            // The design's name for the connected connection (the sample one), while it opens.
            let app = s.h.state_mut();
            let name = app.conns.config(&conn).map(|c| c.name.clone()).unwrap_or_default();
            if let Some(c) = app.conns.config_mut(&conn) {
                c.name = "slypee-prod".into();
            }
            app.dialogs.confirm_delete_connection(&conn, &app.conns);
            if let Some(c) = app.conns.config_mut(&conn) {
                c.name = name;
            }
        }
        s.shot("delete_conn");
        s.reset();
        s.h.state_mut().push_command(AppCommand::NewFolder { parent: Some("Clients".into()) });
        s.frames(3);
        s.h.event(egui::Event::Text("Slypee".into()));
        s.shot("folder_new");
        s.reset();
        s.h.state_mut().push_command(AppCommand::DeleteFolder("Clients/Slypee".into()));
        s.shot("folder_delete");
        s.reset();
        let ws = dbeaver_workspace(&base);
        s.h.state_mut().dialogs.import = Some(ternarygrid_app::dialogs::import::ImportDialog::with_workspace(ws));
        s.frames(3);
        s.shot("import_start");
        s.click("Find connections");
        {
            // As in the design: every connection but the SQLite one (the last row's box).
            let mut boxes: Vec<_> = s.h.query_all_by_role(egui::accesskit::Role::CheckBox).filter(|n| n.rect().left() < 400.0).collect();
            boxes.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
            boxes.last().expect("the import table's check boxes").click();
        }
        s.frames(3);
        s.shot("import_found");
        s.reset();
        {
            // A vault saved before: unlock it.
            let path = home.join("vault-saved.bin");
            ternarygrid_config::VaultStore::new(path.clone()).create(&ternarygrid_core::Secret::new("master-password")).unwrap();
            let vault = Arc::new(ternarygrid_config::VaultStore::new(path));
            *s.h.state().services.secrets.write() = ternarygrid_config::SecretsManager::with_vault(vault);
            s.h.state_mut().dialogs.vault_open = true;
        }
        s.frames(3);
        s.h.event(egui::Event::Text("master-pw".into()));
        s.shot("vault");
        s.reset();
        {
            let vault = Arc::new(ternarygrid_config::VaultStore::new(home.join("vault.bin")));
            *s.h.state().services.secrets.write() = ternarygrid_config::SecretsManager::with_vault(vault);
            s.h.state_mut().dialogs.vault_open = true;
        }
        s.frames(3);
        s.type_right_of("Master password", "master-pw");
        s.type_right_of("Repeat password", "master");
        s.click("Create");
        s.shot("vault_create");
        s.reset();

        // Objects and databases.
        s.h.state_mut().push_command(AppCommand::DropObjects {
            connection: conn.clone(),
            objects: vec![
                ObjectRef::table(Some("main".into()), "tmp_import_2024"),
                ObjectRef::table(Some("main".into()), "tmp_import_2025"),
                ObjectRef::new(ternarygrid_core::meta::ObjectKind::View, Some("main".into()), "v_old_orders"),
            ],
        });
        s.shot("drop");
        s.reset();
        s.h.state_mut().push_command(AppCommand::NewDatabase { connection: conn.clone() });
        s.frames(3);
        s.h.event(egui::Event::Text("analytics_2026".into()));
        s.shot("create_db");
        s.reset();
        s.h.state_mut().push_command(AppCommand::NewSchema { connection: conn.clone(), database: None });
        s.frames(3);
        s.h.event(egui::Event::Text("reporting".into()));
        s.shot("create_schema");
        s.reset();
        s.h.state_mut().push_command(AppCommand::EditDatabase { connection: conn.clone(), database: "main".into(), rename: false });
        s.shot("edit_db");
        s.reset();

        // Unsaved changes: a table designer with a change, closed.
        s.h.state_mut().push_command(AppCommand::CreateTable { connection: conn.clone(), schema: ternarygrid_core::meta::SchemaRef::new(None, Some("main".into())) });
        s.frames(6);
        {
            let f = s.h.query_all_by_role(egui::accesskit::Role::TextInput).find(|n| n.rect().left() > 400.0 && n.rect().top() > 120.0).expect("a field of the table designer");
            f.click();
        }
        s.frames(2);
        s.h.event(egui::Event::Text("orders".into()));
        s.frames(3);
        {
            let app = s.h.state_mut();
            let (_, tab) = app.dock.find_active_focused().expect("the designer");
            let id = tab.id;
            app.ask_close(id);
        }
        s.shot("unsaved");
        // (The designer asks about its changes itself when it is closed: it stays open.)
        s.click("Cancel");
        s.reset();

        // Background operations.
        let (export_id, backup_id) = {
            let app = s.h.state_mut();
            let export = app.ops.start("Export orders");
            export.progress.set(28_400, Some(48_210));
            export.progress.set_message("orders");
            let backup = app.ops.start("Backup of bantuin");
            backup.progress.set_message("pg_dump running");
            let import = app.ops.start("Import into customer");
            app.ops.finish(import.id, OpState::Succeeded("1,204 rows imported".into()));
            let migrate = app.ops.start("Migrate 4 tables");
            app.ops.finish(migrate.id, OpState::Failed("payments: duplicate key value violates unique constraint".into()));
            app.ops.panel_open = true;
            (export.id, backup.id)
        };
        s.shot("operations");
        {
            let app = s.h.state_mut();
            app.ops.cancel_all();
            for id in [export_id, backup_id] {
                app.ops.finish(id, OpState::Cancelled);
            }
        }
        s.reset();

        // Pending transactions, disconnect and quit with them.
        s.h.state_mut().push_command(AppCommand::NewSqlEditor { connection: Some(conn.clone()), schema: None, text: None });
        s.frames(3);
        s.wait("editor session", |h| {
            let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
            tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| e.session_ready())
        });
        let ctx = s.h.ctx.clone();
        s.h.state_mut().dispatch_action(&ctx, Action::ToggleAutoCommit);
        s.frames(3);
        {
            let (_, tab) = s.h.state_mut().dock.find_active_focused().unwrap();
            let e = tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().unwrap();
            e.set_text(&ctx, "create table tx_probe (id integer);\ninsert into tx_probe values (1);");
        }
        s.frames(2);
        s.h.state_mut().dispatch_action(&ctx, Action::ExecuteScript);
        s.wait("uncommitted insert", |h| {
            let app = h.state();
            !ternarygrid_app::pending_tx::collect(&app.dock, &app.conns).is_empty()
        });
        s.h.state_mut().dispatch_action(&ctx, Action::PendingTransactions);
        s.shot("pending_tx");
        s.reset();
        s.h.state_mut().push_command(AppCommand::Disconnect(conn.clone()));
        s.shot("disconnect_tx");
        s.click("Cancel");
        s.reset();
        // (The SQL editor asks about its own unsaved script and transaction when it is closed.)
        let running = s.h.state_mut().ops.start("Export orders");
        s.h.state_mut().push_command(AppCommand::Quit);
        s.h.step();
        s.h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        s.h.step();
        s.shot("quit");
        s.click("Cancel");
        s.h.state_mut().ops.finish(running.id, OpState::Cancelled);
        s.reset();

        // Updates, tips, about, drivers, shortcuts.
        s.h.state_mut().updates.show_whats_new();
        s.shot("whats_new");
        s.reset();
        let release = ternarygrid_app::updates::Release {
            version: "0.6.0".into(),
            url: "https://github.com/ilhamnadhif/ternarygrid/releases/tag/v0.6.0".into(),
            notes: "- A new, quieter interface: top bar, icon rail and status bar.\n- One filter bar and one footer for results.".into(),
            date: Some("2026-11-10".into()),
            ..Default::default()
        };
        s.h.state_mut().updates.available = Some(release.clone());
        s.shot("update_available");
        s.reset();
        #[cfg(target_os = "macos")]
        {
            {
                let app = s.h.state_mut();
                app.updates.preview_install(&app.backend, release.clone(), Some((32_925_286, 51_694_796)));
            }
            s.shot("updating");
            s.click("Hide");
            {
                let app = s.h.state_mut();
                app.updates.preview_install(&app.backend, release.clone(), None);
            }
            s.shot("update_ready");
            s.click("Later");
            s.reset();
        }
        {
            let app = s.h.state_mut();
            app.settings.updates.check_automatically = None;
            app.updates.start(&app.backend, &mut app.settings, true);
            app.updates.whats_new = None;
        }
        s.shot("update_ask");
        s.reset();
        s.h.state_mut().settings.general.show_tips = true;
        s.h.state_mut().show_tip();
        // The design's tip (about the filter bar's WHERE and FILTER).
        if let Some(t) = s.h.state_mut().tip.as_mut() {
            t.index = 4;
        }
        s.shot("tip");
        s.reset();
        s.h.state_mut().dialogs.about_open = true;
        s.shot("about");
        s.reset();
        s.h.state_mut().dialogs.drivers_open = true;
        s.shot("drivers");
        s.reset();
        s.h.state_mut().dialogs.shortcuts_open = true;
        s.shot("shortcuts");
        s.reset();

        // Toasts.
        {
            let app = s.h.state_mut();
            app.toasts.success_file("Export finished", home.join("orders_20261102_1931.csv"));
            app.toasts.warning("Import notes", Some("3 rows had dates in another format; they were read as text.".into()));
            app.toasts.error("Cannot connect to slypee-prod", Some("connection refused (os error 61): is the SSH tunnel up?".into()));
        }
        s.shot_with_toasts("toasts");
        s.h.state_mut().toasts.clear();

        // A tool in a pop-up window.
        s.h.state_mut().push_command(AppCommand::OpenExport { connection: conn.clone(), objects: vec![ObjectRef::table(Some("main".into()), "product")], sql: None });
        s.shot("popup_export");
        s.reset();

        drop(s);
        let _ = std::fs::remove_dir_all(&home);
    }
    let _ = std::fs::remove_dir_all(&base);
}
