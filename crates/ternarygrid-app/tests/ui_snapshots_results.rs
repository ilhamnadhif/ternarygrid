//! Screenshots of the results and object pages (the data grid, its filter bar,
//! footer, views, side panels, menus and dialogs; the table, function, sequence
//! and catalog object pages) in the real application window, rasterised on the
//! CPU (no GPU needed), for comparing them with the design mockups, looking like the
//! macOS app (as `ui_snapshots_shell`: the menus in the system's menu bar, the window
//! buttons at the left of the top bar). Not part of the normal test run:
//!
//! ```text
//! cargo test -p ternarygrid-app --test ui_snapshots_results -- --ignored
//! ```
//!
//! writes `target/ui-snapshots/results/<shot>-<light|dark>.png` (1440 × 900
//! points at 2×). `TERNARYGRID_SNAPSHOT_ONLY=main,obj_` renders only the shots
//! whose names start so; `TERNARYGRID_SNAPSHOT_THEME=dark` one theme.

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
use ternarygrid_app::results::panel::ResultPanel;
use ternarygrid_app::services::{Services, Settings};
use ternarygrid_app::tabs::object::ObjectEditorTab;
use ternarygrid_app::tabs::sql_editor::SqlEditorTab;
use ternarygrid_config::settings::Theme;
use ternarygrid_core::ObjectRef;

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
        let services = Services::ephemeral(&home.join("home"));
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
                // buttons at the left of the top bar (painted by `shot`, as the system draws them).
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

    fn wants(&self, name: &str) -> bool {
        self.only.as_deref().is_none_or(|o| o.split(',').any(|o| name.starts_with(o) || o.starts_with(name)))
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

    /// Write the last frame as `<name>-<theme>.png`.
    fn shot(&mut self, name: &str) {
        if !self.wants(name) {
            return;
        }
        self.h.state_mut().toasts.clear();
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

    /// The pointer away from the content (no hover effects).
    fn park(&mut self) {
        self.h.event(egui::Event::PointerMoved(egui::pos2(1439.0, 899.0)));
        self.frames(2);
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
            let n = self
                .h
                .query_all_by_label(label)
                .find(|n| n.rect().center().y > chrome)
                .or_else(|| self.h.query_all_by_label(label).next())
                .unwrap_or_else(|| panic!("no widget labelled {label:?}; labels: {:?}", labels(&self.h).iter().rev().take(40).collect::<Vec<_>>()));
            n.click();
        }
        self.frames(3);
    }

    /// Click the first widget whose label contains `label` (menu items carry their icon).
    fn click_contains(&mut self, label: &str) {
        self.pause();
        {
            let n = self.h.query_all_by_label_contains(label).last().unwrap_or_else(|| panic!("no widget with {label:?}; labels: {:?}", labels(&self.h)));
            n.click();
        }
        self.frames(3);
    }

    /// Click the rightmost widget labelled exactly `label` (the page header's, not the navigator's).
    fn click_rightmost(&mut self, label: &str) {
        self.pause();
        {
            let n = self.h.query_all_by_label(label).max_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).unwrap_or_else(|| panic!("no widget labelled {label:?}"));
            n.click();
        }
        self.frames(3);
    }

    /// Click the last widget labelled exactly `label` (a page's own pill, not the header's tab).
    fn click_last(&mut self, label: &str) {
        self.pause();
        {
            let n = self.h.query_all_by_label(label).last().unwrap_or_else(|| panic!("no widget labelled {label:?}; labels: {:?}", labels(&self.h)));
            n.click();
        }
        self.frames(3);
    }

    fn escape(&mut self) {
        self.h.key_press(egui::Key::Escape);
        self.frames(3);
    }

    /// The pointer at `pos` (over an open menu: it stays open).
    fn park_keep_menus(&mut self, pos: egui::Pos2) {
        self.h.event(egui::Event::PointerMoved(pos));
        self.frames(2);
    }

    /// Right-click at a point.
    fn context_click(&mut self, pos: egui::Pos2) {
        self.pause();
        self.h.event(egui::Event::PointerMoved(pos));
        self.h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: true, modifiers: egui::Modifiers::NONE });
        self.h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: false, modifiers: egui::Modifiers::NONE });
        self.frames(4);
    }

    fn focused<T: 'static>(&mut self) -> &mut T {
        let (_, tab) = self.h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<T>().expect("the focused tab's type")
    }

    /// The result panel of the focused object tab's data page.
    fn data(&mut self) -> &mut ResultPanel {
        self.focused::<ObjectEditorTab>().data.as_mut().expect("the data page").panel.as_mut().expect("the rows")
    }

    fn close_tabs(&mut self) {
        self.command(AppCommand::CloseAllTabs);
        self.frames(4);
    }

    /// Opens a table of the sample database on `page`.
    fn open(&mut self, conn: &str, table: &str, page: ObjectPage) {
        self.command(AppCommand::OpenObject { connection: conn.to_string(), object: ObjectRef::table(Some("main".into()), table), page });
        let t = table.to_string();
        self.wait(table, |h| h.state_mut().dock.find_active_focused().is_some_and(|(_, tab)| tab.view.title() == t));
        if page == ObjectPage::Data {
            self.wait("rows", |h| {
                let (_, tab) = h.state_mut().dock.find_active_focused().unwrap();
                tab.view.as_any_mut().downcast_mut::<ObjectEditorTab>().and_then(|o| o.data.as_mut()).and_then(|d| d.panel.as_mut()).is_some_and(|p| p.model.row_count() > 0)
            });
        }
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

fn run_sql(s: &mut Shots, conn: &str, sql: &str, script: bool) {
    s.command(AppCommand::NewSqlEditor { connection: Some(conn.to_string()), schema: None, text: Some(sql.into()) });
    s.wait("editor session", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| e.session_ready())
    });
    s.action(if script { Action::ExecuteScript } else { Action::ExecuteStatement });
    s.wait("the result", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().expect("a focused tab");
        tab.view.as_any_mut().downcast_mut::<SqlEditorTab>().is_some_and(|e| !e.is_running())
    });
}

/// The mockups' tables in the sample database: `messages` (the main screen), `orders`
/// with the tables that reference it, a view and a trigger.
const SETUP: &str = r#"create table messages (content_encrypted bytea, normalized_text text, reply_to_message_id uuid references messages (id), metadata jsonb, created_at timestamptz, id uuid primary key);
insert into messages (rowid, content_encrypted, normalized_text, metadata, created_at, id)
with recursive n(i) as (select 0 union all select i + 1 from n where i < 999),
t(k, blen, txt, meta) as (values
 (0, 64, 'Maaf, pesanmu belum berhasil aku proses karena ada gangguan sebentar', '{}'),
 (1, 48, 'Ingatkan saya bikin CV sejam kemudian', '{"agent_retry_notice": true}'),
 (2, 96, 'Selamat pagi, Aan! Senin ini kita mulai dengan santai.¶ Ada dua agenda', '{}'),
 (3, 96, 'Selamat pagi, Wilfrid! Senin ini masih terbuka — belum ada agenda', '{}'),
 (4, 96, 'Selamat pagi, Calvin! Senin ini tidak ada agenda kalender', '{}'),
 (5, 80, 'Pagi, yarr! Senin ini ada satu hal yang perlu kamu dahulukan', '{}'),
 (6, 80, 'Pagi, Yan! Senin ini, satu hal masih perlu kamu bereskan', '{}'),
 (7, 80, 'Pagi, Ilham! Senin ini kalender kamu kosong, jadi kamu bisa fokus', '{}'),
 (8, 96, 'Aku belum bisa menampilkan atau memulai materi belajar di sini', '{}'),
 (9, 16, '/status', '{}'),
 (10, 112, 'Hai Wilfrid, aku Bantuin.¶ Bilang aja apa yang mau diingatkan', '{}'),
 (11, 32, '/start web__site__home', '{}'),
 (12, 96, 'Aku belum bisa memastikan siapa saja yang punya akses ke dashboard', '{}'),
 (13, 48, 'siapa saja yang bisa akses dashboard saya', '{}'),
 (14, 96, 'Kalau maksudmu akun Bantuin dipakai bersama oleh 2 orang, bisa', '{}'),
 (15, 32, 'bisa di akses buat 2 orang ?', '{}'),
 (16, 96, 'Pemasukan **Rp210.000** dari freelance teman sudah dicatat', '{}'),
 (17, 48, 'Pemasukan 210rb freelance dari teman', '{}'),
 (18, 96, 'Hari ini kosong — nggak ada pengingat atau tugas yang jatuh tempo', '{"memory": "exclude", "data_class": "google_calendar"}'),
 (19, 112, 'Beberapa berita nasional yang menonjol hari ini:¶ - PDIP dan', '{}'),
 (20, 32, 'berita hari ini apa aja?', '{}'))
select n.i + 1, randomblob(t.blen), t.txt, t.meta, datetime('2026-11-02 19:31:04', printf('-%d seconds', n.i * 731)),
 lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || substr(hex(randomblob(2)), 2) || '-8' || substr(hex(randomblob(2)), 2) || '-' || hex(randomblob(6)))
from n join t on t.k = n.i % 21;
update messages set reply_to_message_id = (select m.id from messages m where m.rowid = messages.rowid + 1) where (rowid - 1) % 21 in (0, 10, 12, 14, 16, 19);
create table orders (id bigint primary key, customer_id bigint not null references customer (id), status text not null default 'open' check (status in ('open', 'shipped', 'delivered', 'cancelled')), total numeric(10,2) not null default 0, currency char(3) not null default 'IDR', created_at timestamptz not null default (datetime('now')), shipped_at timestamptz, note text, internal_ref uuid unique);
insert into orders (id, customer_id, status, total, created_at, shipped_at, note) values
 (10421, 42, 'delivered', 1284.50, '2026-10-30 09:12:51', '2026-10-31 14:02:10', null),
 (10422, 17, 'shipped', 312.00, '2026-10-30 11:40:03', '2026-11-01 08:15:44', 'leave at the front desk'),
 (10423, 42, 'open', 96.90, '2026-10-31 07:59:59', null, null),
 (10424, 8, 'cancelled', 48.00, '2026-10-31 10:21:17', null, 'customer changed their mind'),
 (10425, 63, 'delivered', 2410.75, '2026-10-31 13:05:42', '2026-11-01 17:44:09', null),
 (10426, 17, 'open', 128.40, '2026-11-01 08:02:30', null, null),
 (10427, 91, 'shipped', 540.00, '2026-11-01 09:48:11', '2026-11-02 10:30:00', 'gift wrap'),
 (10428, 5, 'open', 75.20, '2026-11-01 15:16:55', null, null),
 (10429, 42, 'delivered', 860.00, '2026-11-01 18:30:12', '2026-11-02 09:12:40', null),
 (10430, 29, 'open', 33.10, '2026-11-02 07:41:09', null, null),
 (10431, 63, 'shipped', 1020.00, '2026-11-02 09:05:27', '2026-11-02 16:20:13', null),
 (10432, 12, 'open', 410.60, '2026-11-02 11:22:48', null, 'call before delivery'),
 (10433, 8, 'open', 19.99, '2026-11-02 12:01:05', null, null),
 (10434, 77, 'delivered', 1735.00, '2026-11-02 13:47:36', '2026-11-02 18:05:22', null);
create index orders_customer_idx on orders (customer_id);
create index orders_created_idx on orders (created_at desc);
create index orders_open_idx on orders (customer_id) where status = 'open';
create table order_item (id integer primary key, order_id bigint not null references orders (id) on delete cascade, product text, qty int4, price numeric);
insert into order_item values (88120, 10423, 'Kopi Gayo 250 g', 2, 38.50), (88121, 10423, 'Gula aren cair', 1, 19.90);
create table shipment (id integer primary key, order_id bigint not null references orders (id) on delete cascade, carrier text);
create view v_open_orders as select * from orders where status = 'open';
create table order_events (order_id bigint, kind text, created_at timestamptz);
insert into order_events values (10421, 'created', '2026-10-30 09:12:51'), (10421, 'paid', '2026-10-30 09:14:02'), (10422, 'created', '2026-10-30 11:40:03');
"#;

/// A trigger on `orders` (one statement: its body has semicolons).
const TRIGGER: &str = "create trigger orders_touch_updated before update on orders for each row begin select 1; end;";

#[test]
#[ignore = "writes screenshots for visual review; run with --ignored"]
fn ui_snapshots_results() {
    let folder = match std::env::var("TERNARYGRID_SNAPSHOT_LABEL") {
        Ok(l) if !l.is_empty() => format!("results-{l}"),
        _ => "results".to_string(),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots").join(folder);
    std::fs::create_dir_all(&root).unwrap();
    let themes: Vec<Theme> = match std::env::var("TERNARYGRID_SNAPSHOT_THEME").as_deref() {
        Ok("dark") => vec![Theme::Dark],
        Ok("light") => vec![Theme::Light],
        _ => vec![Theme::Dark, Theme::Light],
    };
    for theme in themes {
        let home = std::env::temp_dir().join(format!("ternarygrid-res-shots-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&home).unwrap();
        let mut s = Shots::new(&home, root.clone(), theme);
        shots(&mut s);
        drop(s);
        let _ = std::fs::remove_dir_all(&home);
    }
}

fn shots(s: &mut Shots) {
    assert!(has(&s.h, "Create sample database"), "welcome page");
    s.click("Create sample database");
    s.wait("the sample connection", |h| {
        let app = h.state();
        app.conns.configs().len() == 1 && app.conns.is_connected(&app.conns.configs()[0].id.clone())
    });
    let conn = s.h.state().conns.configs()[0].id.clone();
    run_sql(s, &conn, SETUP, true);
    run_sql(s, &conn, TRIGGER, false);
    run_sql(s, &conn, SETUP_MORE, true);
    s.close_tabs();
    // A production connection named like the mockups': saving data asks first (their warning).
    {
        let mut cfg = s.h.state().conns.config(&conn).unwrap().clone();
        cfg.name = "bantuin".into();
        cfg.connection_type = "prod".into();
        s.h.state_mut().conns.upsert(cfg);
        s.frames(2);
    }
    s.command(AppCommand::RefreshMetadata { connection: conn.clone(), object: None, schema: None });
    // The data of `messages` sorts by its newest rows (as the mockup's ORDER BY).
    {
        let pm = s.h.state().services.projects.clone().unwrap();
        let mut vm = pm.virtual_model(&conn).unwrap_or_default();
        let key = ternarygrid_config::project::VirtualTableKey::of(&ObjectRef::table(Some("main".into()), "messages"));
        let mut vt = vm.get(&key).cloned().unwrap_or_default();
        vt.default_order = Some("created_at DESC".into());
        vm.set(key, vt);
        pm.save_virtual_model(&conn, &vm).unwrap();
    }

    main_screens(s, &conn);
    result_screens(s, &conn);
    more_result_screens(s, &conn);
    editor_screens(s, &conn);
    object_pages(s, &conn);
}

/// `Main` / `Light`: the data of `messages`, 400 rows fetched, a selected cell, two changes.
fn main_screens(s: &mut Shots, conn: &str) {
    if !s.wants("main") && !s.wants("res_fetch_all") {
        return;
    }
    s.open(conn, "messages", ObjectPage::Data);
    s.frames(10);
    if s.data().model.row_count() < 400 {
        s.click("Fetch next 200");
    }
    s.wait("400 rows", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().unwrap();
        tab.view
            .as_any_mut()
            .downcast_mut::<ObjectEditorTab>()
            .and_then(|o| o.data.as_mut())
            .and_then(|d| d.panel.as_mut())
            .is_some_and(|p| p.model.row_count() >= 400 && !p.model.fetch.fetching)
    });
    {
        let p = s.data();
        for (col, w) in [(0, 176.0), (1, 470.0), (2, 306.0), (3, 290.0), (4, 230.0)] {
            p.view.grid.set_column_width(col, w);
        }
        p.model.set_cell(1, 1, "Ingatkan saya bikin CV sejam kemudian (edited)").unwrap();
        p.model.set_cell(40, 1, "Pagi! (edited)").unwrap();
        p.view.grid.select_cell(0, 1);
    }
    s.park();
    s.shot("main");
    s.data().model.discard_changes();
    // Fetch all, without a known total.
    s.click("More result actions");
    s.click_contains("Fetch all rows");
    s.frames(20);
    s.shot("res_fetch_all");
    s.click("Cancel");
    s.frames(20);
    s.close_tabs();
}

/// The model columns of `orders` and the widths of the mockups (currency and internal_ref hidden).
const ORDER_WIDTHS: [(usize, f32); 7] = [(0, 80.0), (1, 120.0), (2, 120.0), (3, 110.0), (5, 190.0), (6, 190.0), (7, 240.0)];

/// The data of `orders` as the results mockups show it: their seven columns, nothing selected.
fn orders(s: &mut Shots, conn: &str) {
    s.open(conn, "orders", ObjectPage::Data);
    s.wait("orders editable", |h| {
        let (_, tab) = h.state_mut().dock.find_active_focused().unwrap();
        tab.view.as_any_mut().downcast_mut::<ObjectEditorTab>().and_then(|o| o.data.as_mut()).and_then(|d| d.panel.as_mut()).is_some_and(|p| p.model.is_editable())
    });
    let p = s.data();
    for (col, w) in ORDER_WIDTHS {
        p.view.grid.set_column_width(col, w);
    }
    p.view.grid.set_column_visible(4, false);
    p.view.grid.set_column_visible(8, false);
    s.frames(3);
}

/// Screen position of a cell of the grid: `row`, the visible column `pos` (of [`ORDER_WIDTHS`]).
fn cell_at(s: &Shots, row: usize, pos: usize) -> egui::Pos2 {
    // The result's grid (the widest: a panel may have one too).
    let body = s.h.query_all_by_label_contains("Data grid,").map(|n| n.rect()).max_by(|a, b| a.width().total_cmp(&b.width())).expect("the grid");
    let x: f32 = ORDER_WIDTHS[..pos].iter().map(|(_, w)| w).sum::<f32>() + ORDER_WIDTHS[pos].1 / 2.0;
    egui::pos2(body.left() + x, body.top() + row as f32 * 29.0 + 14.5)
}

/// The views, panels, menus and dialogs of a result (`Res*`).
fn result_screens(s: &mut Shots, conn: &str) {
    if !s.wants("res_") {
        return;
    }
    use ternarygrid_grid::{PanelKind, ResultViewKind};
    // Views.
    orders(s, conn);
    {
        let p = s.data();
        p.model.set_cell(2, 2, "shipped").unwrap();
        p.view.grid.select_cell(2, 2);
        p.view.toolbar.view = ResultViewKind::Record;
    }
    s.park();
    s.shot("res_record");
    {
        let p = s.data();
        p.model.discard_changes();
        p.view.grid.selection_mut().select_rows(0, 2, false);
    }
    s.park();
    s.shot("res_record_multi");
    s.data().view.toolbar.view = ResultViewKind::Text;
    s.park();
    s.shot("res_text");
    s.data().view.toolbar.view = ResultViewKind::Json;
    s.park();
    s.shot("res_json");
    s.data().view.toolbar.view = ResultViewKind::Grid;

    // Panels.
    {
        let p = s.data();
        for k in [PanelKind::Calc, PanelKind::Grouping, PanelKind::Metadata] {
            p.view.toolbar.set_panel_open(k, true);
        }
        p.view.grid.select_cell(1, 7);
        p.view.open_panel(PanelKind::Value);
    }
    s.park();
    s.shot("res_value");
    {
        let p = s.data();
        p.view.grid.selection_mut().clear();
        p.view.active_panel = Some(PanelKind::Metadata);
    }
    s.park();
    s.shot("res_meta");
    {
        let p = s.data();
        p.view.grouping.spec = ternarygrid_grid::grouping::GroupSpec { columns: vec![2], measure: Some(3) };
        p.view.active_panel = Some(PanelKind::Grouping);
    }
    s.frames(10);
    s.park();
    s.shot("res_group");
    {
        let p = s.data();
        p.view.grid.selection_mut().select_columns(3, 3, false);
        p.view.active_panel = Some(PanelKind::Calc);
    }
    s.frames(6);
    s.park();
    s.shot("res_calc");
    {
        let p = s.data();
        for k in [PanelKind::Calc, PanelKind::Grouping, PanelKind::Metadata] {
            p.view.toolbar.set_panel_open(k, false);
        }
        p.view.grid.select_cell(2, 0);
        p.view.open_panel(PanelKind::References);
    }
    s.frames(10);
    s.park();
    s.shot("res_refs");
    {
        let p = s.data();
        p.view.toolbar.set_panel_open(PanelKind::References, false);
        p.view.toolbar.set_panel_open(PanelKind::Value, false);
        p.view.grid.selection_mut().clear();
    }

    // Menus.
    let at = cell_at(s, 2, 2);
    s.data().view.grid.select_cell(2, 2);
    s.context_click(at);
    s.click_contains("Filter by value");
    // The pointer on the submenu's first item (it stays open, its item hovered as in the mockup).
    let item = s.h.query_all_by_label_contains("Filter by value").last().expect("the item").rect();
    s.park_keep_menus(egui::pos2(item.right() + 60.0, item.center().y));
    s.park_keep_menus(egui::pos2(item.right() + 80.0, item.center().y));
    s.shot("res_cell_menu");
    s.escape();
    s.escape();
    s.data().view.toolbar.order_text = "created_at DESC".into();
    s.click("Apply");
    s.frames(10);
    s.click("Column menu: created_at");
    s.shot("res_head_menu");
    s.escape();

    // Dialogs.
    {
        let p = s.data();
        p.view.grid.select_cell(0, 0);
        p.view.grid.selection_mut().extend_to(ternarygrid_grid::grid::CellPos::new(2, 3));
    }
    let at = cell_at(s, 1, 1);
    s.context_click(at);
    s.click_contains("Advanced copy");
    s.frames(20);
    s.shot("res_copy");
    s.escape();
    s.frames(20);
    // The fetch error over the rows.
    s.data().model.fetch.error = Some("canceling statement due to statement timeout (the query ran longer than the connection's 30 s query timeout)".into());
    s.park();
    s.shot("res_fetch_err");
    s.data().model.fetch.error = None;
    // Reload with changes.
    {
        let p = s.data();
        p.model.set_cell(2, 2, "shipped").unwrap();
        p.model.set_cell(2, 6, "2026-11-02 19:20:00").unwrap();
        p.model.delete_rows(&[3]).unwrap();
    }
    s.frames(2);
    s.click_rightmost("Refresh");
    s.frames(20);
    s.shot("res_reload");
    s.click("Cancel");
    s.frames(20);
    // Save (a production connection asks first).
    s.click("Save");
    s.frames(20);
    s.shot("res_save");
    s.click("Cancel");
    s.frames(20);
    // Generated SQL of three rows.
    s.data().model.discard_changes();
    s.data().view.grid.selection_mut().select_rows(2, 4, false);
    let at = cell_at(s, 3, 2);
    s.context_click(at);
    s.click_contains("Generate SQL");
    s.click_contains("UPDATE");
    s.frames(20);
    s.shot("res_gen_sql");
    if has(&s.h, "Close window") {
        s.click("Close window");
    }
    // Add a virtual column.
    let at = cell_at(s, 1, 1);
    s.context_click(at);
    s.click_contains("Columns");
    s.click_contains("Add virtual column");
    s.frames(20);
    s.shot("res_virtual");
    s.escape();
    s.frames(20);

    // Chart and spatial view.
    s.click("More result actions");
    s.click_contains("Chart");
    s.frames(20);
    s.park();
    s.shot("res_chart");
    s.click_contains("Back to grid");
    s.close_tabs();

    // A table without a key: read-only.
    s.open(conn, "order_events", ObjectPage::Data);
    s.frames(10);
    {
        let p = s.data();
        let reason = p.model.read_only_reason().map(|r| r.message()).unwrap_or_default();
        p.view.toolbar.error_popup = Some(reason);
        p.view.grid.select_cell(1, 1);
    }
    s.frames(20);
    s.shot("res_read_only");
    s.click("Define virtual key…");
    s.frames(20);
    s.shot("res_virtual_key");
    s.escape();
    s.frames(10);
    s.close_tabs();
}

/// The object pages of `orders`.
fn object_pages(s: &mut Shots, conn: &str) {
    if !s.wants("obj_") {
        return;
    }
    s.open(conn, "orders", ObjectPage::Properties);
    s.wait("the columns", |h| has(h, "Constraints"));
    s.park();
    s.shot("obj_columns");
    for (pill, name) in [("Constraints", "obj_constraints"), ("References", "obj_refs"), ("Triggers", "obj_triggers"), ("Virtual model", "obj_virtual")] {
        s.click_last(pill);
        s.frames(6);
        s.park();
        s.shot(name);
    }
    s.click("Indexes");
    s.frames(6);
    s.park();
    s.shot("obj_indexes");
    for (page, name) in [("DDL", "obj_ddl"), ("Dependencies", "obj_deps"), ("Statistics", "obj_stats")] {
        s.click(page);
        s.frames(10);
        s.park();
        s.shot(name);
    }
    s.close_tabs();
    // A trigger's properties (the sample database has no functions or sequences).
    s.command(AppCommand::OpenObject {
        connection: conn.to_string(),
        object: ObjectRef { kind: ternarygrid_core::meta::ObjectKind::Trigger, ..ObjectRef::table(Some("main".into()), "orders_touch_updated") },
        page: ObjectPage::Properties,
    });
    s.frames(20);
    s.park();
    s.shot("obj_routine");
    s.close_tabs();
    // A catalog object (SQLite has no roles: one of its settings).
    s.command(AppCommand::OpenCatalogObject {
        connection: conn.to_string(),
        folder: "sqlite.settings",
        ctx: ternarygrid_core::catalog::CatalogContext { name: Some("encoding".into()), ..Default::default() },
    });
    s.frames(20);
    s.park();
    s.shot("obj_catalog");
    s.close_tabs();
}

/// More data for the value panel (JSON tree, image) and the spatial view.
const SETUP_MORE: &str = r#"update messages set metadata = '{"shipping": {"carrier": "JNE", "service": "YES", "eta": "2026-11-03", "address": {"street": "Jl. Sudirman 1", "city": "Jakarta", "zip": "10220", "country": "ID", "note": null}}, "items": [{"sku": "KG-250", "qty": 2, "price": 38.5}, {"sku": "GA-1", "qty": 1, "price": 19.9}], "gift": true, "coupon": null, "channel": "web", "version": 2}' where rowid = 2;
create table product_photo (id integer primary key, name text, price numeric, photo bytea);
insert into product_photo values (1, 'Kopi Gayo 250 g', 38.50, X'89504E470D0A1A0A0000000D4948445200000008000000060802000000716748AC0000001149444154789C63106CC8C58A1806520200E3F92FA1DDC6253A0000000049454E44AE426082'),
 (2, 'Gula aren cair', 19.90, X'89504E470D0A1A0A0000000D4948445200000008000000060802000000716748AC0000001149444154789C63106CC8C58A1806520200E3F92FA1DDC6253A0000000049454E44AE426082'),
 (3, 'Teh melati', 24.00, X'89504E470D0A1A0A0000000D4948445200000008000000060802000000716748AC0000001149444154789C63106CC8C58A1806520200E3F92FA1DDC6253A0000000049454E44AE426082'),
 (4, 'Madu hutan 500 ml', 89.00, X'89504E470D0A1A0A0000000D4948445200000008000000060802000000716748AC0000001149444154789C63106CC8C58A1806520200E3F92FA1DDC6253A0000000049454E44AE426082'),
 (5, 'Kacang mete', 64.00, X'89504E470D0A1A0A0000000D4948445200000008000000060802000000716748AC0000001149444154789C63106CC8C58A1806520200E3F92FA1DDC6253A0000000049454E44AE426082');
create table delivery_area (id integer primary key, name text, area geometry);
insert into delivery_area values (1, 'Jakarta Pusat', 'POLYGON((106.80 -6.15, 106.86 -6.14, 106.88 -6.18, 106.85 -6.21, 106.81 -6.20, 106.80 -6.15))'),
 (2, 'Jakarta Selatan', 'POLYGON((106.86 -6.21, 106.93 -6.20, 106.95 -6.27, 106.90 -6.31, 106.86 -6.29, 106.86 -6.21))'),
 (3, 'Route', 'LINESTRING(106.78 -6.30, 106.83 -6.28, 106.88 -6.31, 106.93 -6.34, 106.99 -6.35)'),
 (4, 'Hub A', 'POINT(106.82 -6.17)'), (5, 'Hub B', 'POINT(106.84 -6.16)'), (6, 'Hub C', 'POINT(106.86 -6.19)'), (7, 'Hub D', 'POINT(106.89 -6.18)'),
 (8, 'Hub E', 'POINT(106.91 -6.22)'), (9, 'Hub F', 'POINT(106.84 -6.25)');
"#;

/// The plan of the mockups' query (Postgres EXPLAIN ANALYZE), as a saved plan file.
const PLAN: &str = r#"{"ternarygrid_plan": 1, "sql": "select c.name, c.city, count(distinct o.id) as orders, round(sum(l.quantity * l.unit_price), 2) as revenue from customer c join orders o on o.customer_id = c.id join order_line l on l.order_id = o.id group by c.id order by revenue desc",
 "plan": {"analyzed": true, "planning_ms": 1.2, "execution_ms": 286.4,
  "text": "Sort  (cost=18190.31..18204.70 rows=192 width=72) (actual time=274.6..286.4 rows=192 loops=1)\n  Sort Key: (round(sum(...), 2)) DESC\n  ->  HashAggregate  (cost=17900.00..18120.10 rows=192 width=72) (actual time=262.8..274.6 rows=192 loops=1)\n        ->  Hash Join  (cost=1240.00..16880.20 rows=41000 width=40) (actual time=24.1..238.7 rows=48210 loops=1)\n              Hash Cond: (l.order_id = o.id)\n              ->  Seq Scan on order_line l  (cost=0.00..12400.00 rows=120 width=24) (actual time=0.01..149.2 rows=186004 loops=1)\n              ->  Hash  (cost=980.00..980.00 rows=48210 width=24) (actual time=17.0..17.0 rows=48210 loops=1)\n                    ->  Index Scan using orders_created_idx on orders o  (cost=0.42..940.00 rows=48210 width=24)\nPlanning Time: 1.2 ms\nExecution Time: 286.4 ms",
  "roots": [{"node_type": "Sort", "startup_cost": 18190.31, "total_cost": 18204.7, "plan_rows": 192, "actual_rows": 192, "actual_total_ms": 286.4, "actual_startup_ms": 274.6, "loops": 1, "condition": "c.revenue DESC",
   "children": [{"node_type": "HashAggregate", "startup_cost": 17900.0, "total_cost": 18120.1, "plan_rows": 192, "actual_rows": 192, "actual_total_ms": 274.6, "actual_startup_ms": 262.8, "loops": 1, "condition": "c.id",
    "children": [{"node_type": "Hash Join", "startup_cost": 1240.0, "total_cost": 16880.2, "plan_rows": 41000, "actual_rows": 48210, "actual_total_ms": 238.7, "actual_startup_ms": 24.1, "loops": 1, "condition": "(l.order_id = o.id)",
     "children": [{"node_type": "Seq Scan", "relation": "order_line", "alias": "l", "startup_cost": 0.0, "total_cost": 12400.0, "plan_rows": 120, "actual_rows": 186004, "actual_total_ms": 149.2, "actual_startup_ms": 0.01, "loops": 1},
      {"node_type": "Hash", "startup_cost": 980.0, "total_cost": 980.0, "plan_rows": 48210, "actual_rows": 48210, "actual_total_ms": 17.0, "actual_startup_ms": 17.0, "loops": 1,
       "children": [{"node_type": "Index Scan", "relation": "orders", "alias": "o", "index": "orders_created_idx", "condition": "created_at > now() - '90 days'", "startup_cost": 0.42, "total_cost": 940.0, "plan_rows": 48210, "actual_rows": 48210, "actual_total_ms": 22.9, "actual_startup_ms": 0.1, "loops": 1}]}]}]}]}]}}"#;

/// The SQL editor's results (`Editor`, `EditorError`) and the execution plan (`ResPlan*`, `LtPlan`).
fn editor_screens(s: &mut Shots, conn: &str) {
    if !s.wants("editor") && !s.wants("res_plan") {
        return;
    }
    run_sql(
        s,
        conn,
        "select c.name, c.city, count(distinct o.id) as orders, round(sum(l.quantity * l.unit_price), 2) as revenue, max(o.ordered_at) as last_order\nfrom customer c\njoin \"order\" o on o.customer_id = c.id\njoin order_line l on l.order_id = o.id\nwhere o.status <> 'cancelled'\ngroup by c.id\norder by revenue desc;",
        false,
    );
    s.park();
    s.shot("editor_results");
    s.focused::<SqlEditorTab>().load_plan_json(PLAN).unwrap();
    s.frames(20);
    s.park();
    s.shot("res_plan");
    s.click("Tree");
    s.frames(10);
    s.park();
    s.shot("res_plan_tree");
    s.click("Text");
    s.frames(10);
    s.park();
    s.shot("res_plan_text");
    s.close_tabs();
    run_sql(s, conn, "select c.name from customer c join \"order\" o on o.customer_id = c.id where o.statuss <> 'cancelled';", false);
    s.park();
    s.shot("editor_error");
    s.close_tabs();
}

/// The value panel on JSON and on an image; the chart and the map.
fn more_result_screens(s: &mut Shots, conn: &str) {
    if !s.wants("res_") {
        return;
    }
    use ternarygrid_grid::PanelKind;
    s.open(conn, "messages", ObjectPage::Data);
    s.frames(10);
    {
        let p = s.data();
        for (col, w) in [(0, 176.0), (1, 470.0), (2, 306.0), (3, 290.0), (4, 230.0)] {
            p.view.grid.set_column_width(col, w);
        }
        p.view.grid.select_cell(1, 3);
        p.view.toolbar.set_panel_open(PanelKind::Metadata, true);
        p.view.open_panel(PanelKind::Value);
        p.view.value.mode = ternarygrid_grid::panels::ValueMode::JsonTree;
    }
    s.frames(6);
    s.park();
    s.shot("res_value_json");
    s.close_tabs();
    s.open(conn, "product_photo", ObjectPage::Data);
    s.frames(10);
    {
        let p = s.data();
        for (col, w) in [(0, 80.0), (1, 220.0), (2, 100.0), (3, 200.0)] {
            p.view.grid.set_column_width(col, w);
        }
        p.view.grid.select_cell(3, 3);
        p.view.toolbar.set_panel_open(PanelKind::Metadata, true);
        p.view.open_panel(PanelKind::Value);
    }
    s.frames(10);
    s.park();
    s.shot("res_value_image");
    s.close_tabs();
    // The chart as a donut.
    orders(s, conn);
    s.click("More result actions");
    s.click_contains("Chart");
    s.frames(10);
    s.click_contains("Pie");
    s.frames(10);
    s.park();
    s.shot("res_pie");
    s.close_tabs();
    // The spatial view.
    s.open(conn, "delivery_area", ObjectPage::Data);
    s.frames(10);
    s.click("More result actions");
    s.click_contains("Spatial view");
    s.frames(20);
    s.park();
    s.shot("res_map");
    s.close_tabs();
}
