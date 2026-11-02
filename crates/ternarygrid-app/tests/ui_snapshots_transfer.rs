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
use ternarygrid_app::conn::{ConnectionManager, DirectNet, MemorySecrets};
use ternarygrid_app::services::{Services, Settings};
use ternarygrid_app::tabs::transfer::{ExportTab, ImportTab, MigrationTab, MockDataTab, ResultSource};
use ternarygrid_config::settings::Theme;
use ternarygrid_core::meta::{ObjectKind, ObjectRef, SchemaRef};
use ternarygrid_core::{ColumnMeta, DataKind, Value};
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

#[test]
#[ignore = "writes screenshots of the transfer wizards for visual review; run with --ignored"]
fn transfer_snapshots() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots/transfer");
    std::fs::create_dir_all(&root).unwrap();
    for theme in [Theme::Dark, Theme::Light] {
        let home = std::env::temp_dir().join(format!("ternarygrid-transfer-shots-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        let out = home.join("exports");
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

        // ------------------------------------------------------------------ export
        let view = ObjectRef::new(ObjectKind::View, Some("main".into()), "order_total");
        s.h.state_mut().push_command(AppCommand::OpenExport { connection: conn.clone(), objects: vec![table("customer"), table("order_line"), view], sql: None });
        s.frames(30);
        s.wait("the tables to add", |h| has(h, "Add all shown"));
        s.shot("export_1_source");
        popup::<ExportTab>(&mut s.h).set_page(1);
        s.frames(3);
        s.shot("export_2_format");
        popup::<ExportTab>(&mut s.h).settings_mut().format = ExportFormat::Xlsx;
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
        store.lock().extend((0..20).map(|i| vec![Value::Int(i), Value::text(format!("customer {i}"))]));
        let columns = vec![ColumnMeta::new("id", "int", DataKind::Int), ColumnMeta::new("name", "text", DataKind::Text)];
        let sql = "select * from customer where city = 'Bandung' order by name".to_string();
        let mut rows = ExportTab::from_result(conn.clone(), "customer_bandung".into(), None, store, columns, Some(vec![1, 2, 3]), Vec::new(), Some(sql));
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
        s.h.state_mut().push_command(AppCommand::OpenMigration { connection: conn.clone(), objects: vec![table("customer"), table("product"), table("category")] });
        s.frames(30);
        s.wait("the tables to add", |h| has(h, "Add all shown"));
        s.shot("migration_1_source");
        {
            let tab = popup::<MigrationTab>(&mut s.h);
            tab.set_target(conn.clone(), None);
            for t in ["customer", "product", "category"] {
                tab.set_target_name(t, &format!("{t}_copy"));
            }
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
        popup::<MigrationTab>(&mut s.h).options_mut().mode = MigrationMode::DropAndCreate;
        popup::<MigrationTab>(&mut s.h).set_page(4);
        s.frames(3);
        click(&mut s, "Run migration");
        s.frames(30);
        s.shot("migration_confirm");
        s.h.key_press(egui::Key::Escape);
        s.frames(30);
        // category into the existing product table fails (its NOT NULL columns): the result stays.
        {
            let tab = popup::<MigrationTab>(&mut s.h);
            tab.options_mut().mode = MigrationMode::Append;
            tab.options_mut().continue_on_table_error = true;
            tab.set_target_name("category", "product");
        }
        s.frames(2);
        click(&mut s, "Run migration");
        s.wait("the migration", |h| !has_popup::<MigrationTab>(h) || !popup::<MigrationTab>(h).is_running());
        s.frames(4);
        if has_popup::<MigrationTab>(&mut s.h) {
            s.shot("migration_5_done");
        }
        close_popups(&mut s);

        // --------------------------------------------------------------- mock data
        s.h.state_mut().push_command(AppCommand::OpenMockData { connection: conn.clone(), table: table("product") });
        s.frames(30);
        s.wait("the table definition", |h| popup::<MockDataTab>(h).definition_loaded());
        s.frames(3);
        s.shot("mock_1_settings");
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
    }
}
