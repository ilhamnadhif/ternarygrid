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
        self.frames(6);
        let out = self.h.output();
        let prims = self.h.ctx.tessellate(out.shapes.clone(), PPP);
        let size = self.h.ctx.content_rect().size() * PPP;
        let bg = self.h.ctx.global_style().visuals.panel_fill;
        let img = self.raster.borrow().render(&prims, [size.x as usize, size.y as usize], bg);
        let path = self.dir.join(format!("{name}-{}.png", self.theme));
        img.save(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        eprintln!("wrote {}", path.display());
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
