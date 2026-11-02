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
            .with_size(egui::vec2(1400.0, 900.0))
            .with_pixels_per_point(PPP)
            .with_max_steps(400)
            .with_step_dt(1.0 / 60.0)
            .renderer(CpuTextures(raster.clone()))
            .build_eframe(move |cc| {
                ternarygrid_app::theme::setup_fonts(&cc.egui_ctx);
                backend.set_context(cc.egui_ctx.clone());
                TernaryGridApp::new(backend, conns, services, settings)
            });
        ternarygrid_app::theme::apply_settings(&h.ctx, &h.state().settings);
        let theme = if theme == Theme::Dark { "dark" } else { "light" };
        let only = std::env::var("TERNARYGRID_SNAPSHOT_ONLY").ok();
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
        self.frames(30);
        if self.only.as_deref().is_some_and(|o| !name.contains(o)) {
            return;
        }
        let out = self.h.output();
        let prims = self.h.ctx.tessellate(out.shapes.clone(), PPP);
        let size = self.h.ctx.content_rect().size() * PPP;
        let bg = self.h.ctx.global_style().visuals.panel_fill;
        let img = self.raster.borrow().render(&prims, [size.x as usize, size.y as usize], bg);
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
            let l = self.h.query_all_by_label(label).last().unwrap_or_else(|| panic!("no label {label:?}"));
            let lr = l.rect();
            let f = self
                .h
                .query_all_by_role(egui::accesskit::Role::TextInput)
                .find(|n| n.rect().left() > lr.left() && (n.rect().center().y - lr.center().y).abs() < 12.0)
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
}

#[test]
#[ignore = "writes screenshots for visual review; run with --ignored"]
fn ui_snapshots_dialogs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots/dialogs");
    std::fs::create_dir_all(&root).unwrap();
    for theme in [Theme::Dark, Theme::Light] {
        let home = std::env::temp_dir().join(format!("ternarygrid-dlg-shots-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        let mut s = Shots::new(&home, root.clone(), theme);

        // The sample database (connected) and a PostgreSQL connection that is not.
        s.h.query_all_by_label_contains("Create sample database").next().unwrap().click();
        s.wait("the sample connection", |h| {
            let app = h.state();
            app.conns.configs().len() == 1 && app.conns.is_connected(&app.conns.configs()[0].id.clone())
        });
        let conn = s.h.state().conns.configs()[0].id.clone();
        let mut pg = ternarygrid_core::config::ConnectionConfig::new(ternarygrid_core::DbKind::Postgres, "slypee-prod");
        pg.host = "10.0.4.12".into();
        pg.user = Some("app".into());
        pg.database = Some("slypee".into());
        pg.ssh = Some(ternarygrid_core::config::SshConfig {
            enabled: true,
            host: "bastion.office.example.com".into(),
            user: "ilham".into(),
            auth: ternarygrid_core::config::SshAuth::PublicKey { key_path: "~/.ssh/id_ed25519".into() },
            ..Default::default()
        });
        pg.folder = Some("Clients".into());
        let pg_id = pg.id.clone();
        s.h.state_mut().conns.upsert(pg);
        s.frames(3);

        // New connection: Main, a missing name, the other pages, a SQLite file.
        let ctx = s.h.ctx.clone();
        s.h.state_mut().dispatch_action(&ctx, Action::NewConnection);
        s.shot("conn_main");
        s.click("Test connection");
        s.shot("conn_error");
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
        s.click("Main");
        s.click("SQLite");
        s.shot("conn_sqlite");
        s.reset();

        // Edit connection: SSH with a tunnel.
        {
            let app = s.h.state_mut();
            app.dialogs.edit_connection(&pg_id, &app.conns);
        }
        s.frames(3);
        s.shot("conn_edit_main");
        s.click("SSH");
        s.shot("conn_edit_ssh");
        s.reset();

        // Small modals.
        {
            let app = s.h.state_mut();
            app.dialogs.open_credentials(&pg_id, vec![PromptField::Password, PromptField::SshPassphrase], &app.conns);
        }
        s.shot("credential");
        s.reset();
        {
            let app = s.h.state_mut();
            app.dialogs.confirm_delete_connection(&conn, &app.conns);
        }
        s.shot("delete_conn");
        s.reset();
        s.h.state_mut().push_command(AppCommand::NewFolder { parent: Some("Clients".into()) });
        s.frames(3);
        s.h.event(egui::Event::Text("x/y".into()));
        s.shot("folder_new");
        s.reset();
        s.h.state_mut().push_command(AppCommand::DeleteFolder("Clients".into()));
        s.shot("folder_delete");
        s.reset();
        s.h.state_mut().push_command(AppCommand::ImportConnections);
        s.frames(3);
        s.shot("import_dbeaver");
        s.click("URL");
        s.type_right_of("URL", "postgres://app@db.bantuin.internal:5432/bantuin");
        s.click("Find connections");
        s.shot("import_found");
        s.reset();
        {
            let vault = Arc::new(ternarygrid_config::VaultStore::new(home.join("vault.bin")));
            *s.h.state().services.secrets.write() = ternarygrid_config::SecretsManager::with_vault(vault);
            s.h.state_mut().dialogs.vault_open = true;
        }
        s.shot("vault_create");
        s.reset();

        // Objects and databases.
        s.h.state_mut().push_command(AppCommand::DropObjects {
            connection: conn.clone(),
            objects: vec![ObjectRef::table(Some("main".into()), "product"), ObjectRef::table(Some("main".into()), "customer")],
        });
        s.shot("drop");
        s.reset();
        s.h.state_mut().push_command(AppCommand::NewSchema { connection: conn.clone(), database: None });
        s.shot("create_schema");
        s.reset();
        s.h.state_mut().push_command(AppCommand::EditDatabase { connection: conn.clone(), database: "main".into(), rename: false });
        s.shot("edit_db");
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
        s.h.state_mut().push_command(AppCommand::Quit);
        s.h.step();
        s.h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        s.h.step();
        s.shot("quit");
        s.click("Cancel");
        s.reset();

        // Background operations.
        {
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
        }
        s.shot("operations");
        s.h.state_mut().ops.cancel_all();
        s.reset();

        // Updates, tips, about, drivers, shortcuts.
        s.h.state_mut().updates.show_whats_new();
        s.shot("whats_new");
        s.reset();
        s.h.state_mut().updates.available = Some(ternarygrid_app::updates::Release {
            version: "0.6.0".into(),
            url: "https://github.com/ilhamnadhif/ternarygrid/releases/tag/v0.6.0".into(),
            notes: "### Changed\n- A new, quieter interface: top bar, icon rail and status bar.\n- One filter bar and one footer for results.".into(),
            date: Some("2026-11-10".into()),
            ..Default::default()
        });
        s.shot("update_available");
        s.reset();
        {
            let app = s.h.state_mut();
            app.settings.updates.check_automatically = None;
            app.updates.start(&app.backend, &mut app.settings, true);
            app.updates.whats_new = None;
        }
        s.shot("update_ask");
        s.reset();
        s.h.state_mut().show_tip();
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
}
