//! Screenshots of the table designer, the ER diagram and the AI assistant panel for reviewing
//! them against the design canvas ("TernaryGrid modern redesign"), rasterised on the CPU with
//! the harness of `ui_snapshots.rs`. Not part of the normal test run:
//!
//! ```text
//! cargo test -p ternarygrid-app --test ui_snapshots_designer -- --ignored
//! ```
//!
//! writes `target/ui-snapshots/designer/<shot>-<light|dark>.png`.

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
use ternarygrid_app::conn::{ConnectionManager, DirectNet, MemorySecrets};
use ternarygrid_app::keys::Action;
use ternarygrid_app::services::{Services, Settings};
use ternarygrid_app::tabs::ai::{AiAssistantTab, QuickAction};
use ternarygrid_app::tabs::ai_agent::ToolStep;
use ternarygrid_app::tabs::er::ErTab;
use ternarygrid_app::tabs::table_designer::TableDesignerTab;
use ternarygrid_config::settings::Theme;
use ternarygrid_core::meta::SchemaRef;

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
            .with_size(egui::vec2(1440.0, 900.0))
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

    fn designer(&mut self) -> &mut TableDesignerTab {
        let (_, tab) = self.h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<TableDesignerTab>().expect("a table designer")
    }

    fn er(&mut self) -> &mut ErTab {
        let (_, tab) = self.h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<ErTab>().expect("an ER diagram")
    }

    fn ai(&mut self) -> &mut AiAssistantTab {
        self.h.state_mut().ai_panel.as_mut().expect("the AI assistant")
    }

    /// Click the last widget labelled `label`.
    fn click(&mut self, label: &str) {
        {
            let n = self.h.query_all_by_label(label).last().unwrap_or_else(|| panic!("no widget {label:?}"));
            n.click();
        }
        self.frames(3);
    }

    /// A click of `button` at `pos` (canvas widgets have no accessible children).
    fn click_at(&mut self, pos: egui::Pos2, button: egui::PointerButton) {
        self.h.event(egui::Event::PointerMoved(pos));
        self.frames(1);
        self.h.event(egui::Event::PointerButton { pos, button, pressed: true, modifiers: egui::Modifiers::NONE });
        self.frames(1);
        self.h.event(egui::Event::PointerButton { pos, button, pressed: false, modifiers: egui::Modifiers::NONE });
        self.frames(3);
    }

    /// Move the pointer out of the way (no hover effects in the shot).
    fn park_pointer(&mut self) {
        self.h.event(egui::Event::PointerMoved(egui::pos2(2.0, 880.0)));
        self.frames(2);
    }

    fn open_designer(&mut self, command: AppCommand) {
        self.h.state_mut().push_command(command);
        self.frames(3);
        self.wait("the designer", |h| {
            let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
            tab.view.as_any_mut().downcast_mut::<TableDesignerTab>().is_some_and(|d| d.is_ready())
        });
        self.frames(3);
    }
}

fn has(h: &H, label: &str) -> bool {
    h.query_all_by_label_contains(label).next().is_some()
}

fn step(columns: &[&str], rows: &[[&str; 3]]) -> ToolStep {
    ToolStep {
        sql: "select c.name, count(*) as orders, sum(l.quantity * l.unit_price) as revenue\nfrom customer c join \"order\" o on o.customer_id = c.id\njoin order_line l on l.order_id = o.id\ngroup by c.name order by revenue desc limit 3".into(),
        ok: true,
        summary: format!("{} row(s)", rows.len()),
        for_model: String::new(),
        columns: columns.iter().map(|c| c.to_string()).collect(),
        rows: rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect(),
    }
}

const ANSWER: &str = "Three customers stand out: Kartika Kurniawan placed the most orders (11), and Fajar Lestari spent the most. Here is a query you can keep for next month:\n\n```sql\nselect c.name, count(*) as orders, sum(l.quantity * l.unit_price) as spent\nfrom customer c join \"order\" o on o.customer_id = c.id\njoin order_line l on l.order_id = o.id\ngroup by c.name order by spent desc;\n```";

#[test]
#[ignore = "writes screenshots for visual review; run with --ignored"]
fn ui_snapshots_designer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots/designer");
    std::fs::create_dir_all(&root).unwrap();
    let only = std::env::var("TERNARYGRID_SNAPSHOT_ONLY").unwrap_or_default();
    let want = |part: &str| only.is_empty() || only.split(',').any(|p| p == part);
    for theme in [Theme::Dark, Theme::Light] {
        let home = std::env::temp_dir().join(format!("ternarygrid-shots-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
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
        let schema = SchemaRef::new(None, Some("main".into()));

        // A new table: columns, an index; then the confirmation and the other pages.
        if want("designer") {
            s.open_designer(AppCommand::CreateTable { connection: conn.clone(), schema: schema.clone() });
            {
                let d = s.designer();
                d.set_table_name("invoice");
                d.add_column("order_id", "INTEGER", "", true, false);
                d.add_column("number", "VARCHAR", "32", true, false);
                d.add_column("amount", "NUMERIC", "10,2", true, false);
                d.add_column("issued_at", "DATETIME", "", true, false);
                d.add_column("paid_at", "DATETIME", "", false, false);
                d.add_index("invoice_order_idx", &["order_id"], false);
            }
            s.frames(3);
            s.park_pointer();
            s.shot("designer_columns");
            s.click("Create table…");
            s.frames(20);
            s.park_pointer();
            s.shot("designer_confirm");
            s.click("Cancel");
            s.frames(10);
            for (page, name) in [("Indexes", "designer_indexes"), ("Keys", "designer_keys"), ("Options", "designer_options")] {
                s.click(page);
                s.park_pointer();
                s.shot(name);
            }
            // An existing table: its foreign keys with an incomplete one, its checks, its triggers.
            let order = ternarygrid_core::ObjectRef::table(Some("main".into()), "order");
            s.open_designer(AppCommand::EditTable { connection: conn.clone(), table: order });
            s.click("Foreign keys");
            s.click("Add foreign key");
            s.park_pointer();
            s.shot("designer_fk");
            s.click("Checks");
            s.park_pointer();
            s.shot("designer_checks");
            s.click("Revert");
            let line = ternarygrid_core::ObjectRef::table(Some("main".into()), "order_line");
            s.open_designer(AppCommand::EditTable { connection: conn.clone(), table: line });
            s.click("Triggers");
            s.park_pointer();
            s.shot("designer_triggers");
        }

        // The diagram of the schema, a table selected; its menu in edit mode.
        if want("er") {
            s.h.state_mut().push_command(AppCommand::OpenErDiagram { connection: conn.clone(), schema: schema.clone(), tables: Vec::new() });
            s.frames(3);
            s.wait("the diagram", |h| {
                let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
                tab.view.as_any_mut().downcast_mut::<ErTab>().is_some_and(|e| e.table_screen_rect("main.order").is_some())
            });
            s.frames(10);
            let order = s.er().table_screen_rect("main.order").expect("the order table");
            s.click_at(order.center_top() + egui::vec2(0.0, 10.0), egui::PointerButton::Primary);
            s.park_pointer();
            s.shot("er_diagram");
            s.click("Edit mode");
            let customer = s.er().table_screen_rect("main.customer").expect("the customer table");
            s.click_at(customer.center_top() + egui::vec2(0.0, 10.0), egui::PointerButton::Secondary);
            s.frames(10);
            s.shot("er_menu");
            s.h.key_press(egui::Key::Escape);
            s.frames(5);
        }

        // The assistant next to an editor: without a key, an answer with its query, running
        // with the history open, a fixed error.
        if want("ai") {
            s.h.state_mut().push_command(AppCommand::NewSqlEditor {
                connection: Some(conn.clone()),
                schema: None,
                text: Some("select * from product order by price desc;\n".into()),
            });
            s.frames(3);
            s.h.state_mut().settings.ai.enabled = true;
            let ctx = s.h.ctx.clone();
            s.h.state_mut().dispatch_action(&ctx, Action::AiAssistant);
            s.frames(10);
            s.park_pointer();
            s.shot("ai_setup");
            let provider = s.h.state().settings.ai.provider.id().to_string();
            s.h.state().services.secrets.read().set_ai_api_key(&provider, Some(&ternarygrid_core::Secret::new("test"))).unwrap();
            s.wait("the assistant ready", |h| {
                h.step();
                !has(h, "Open settings")
            });
            for q in ["Why is the revenue query slow?", "Fix: column statuss does not exist", "Describe table payments"] {
                s.ai().push_exchange(QuickAction::Chat, q, "…");
                s.frames(2);
                s.ai().reset();
            }
            s.ai().push_exchange(QuickAction::Chat, "Which customers ordered most in October, and how much did they spend?", ANSWER);
            s.ai().push_step(step(
                &["name", "orders", "revenue"],
                &[["Eko Lestari", "10", "18,483.74"], ["Fajar Lestari", "6", "19,438.54"], ["Kartika Kurniawan", "11", "17,327.29"]],
            ));
            // The step opens on its result.
            s.frames(8);
            s.park_pointer();
            s.shot("ai_chat");
            s.ai().reset();
            s.frames(2);
            s.ai().push_running("Why is the revenue query slow?", "explain query plan\nselect c.name, … from customer c join \"order\" o …");
            s.frames(3);
            s.click("Conversation history");
            s.frames(6);
            s.shot("ai_running_history");
            s.h.key_press(egui::Key::Escape);
            s.frames(3);
            s.ai().reset();
            s.frames(2);
            s.ai().set_input(QuickAction::FixError, "select name, statuss from \"order\"", "no such column: statuss");
            s.ai().push_exchange(QuickAction::FixError, "Fix error: column o.statuss does not exist", "The column is called status (one s). Here is the statement with the name fixed:\n\n```sql\nselect c.name, c.city, count(distinct o.id) as orders\nfrom customer c join \"order\" o on o.customer_id = c.id\nwhere o.status <> 'cancelled'\ngroup by c.id;\n```");
            s.frames(4);
            s.park_pointer();
            s.shot("ai_fix");
        }

        drop(s);
        let _ = std::fs::remove_dir_all(&home);
    }
}
