//! The look shared by the compare and administration tools (schema and data
//! compare, sessions, locks, backup, maintenance, password, FDW, debugger):
//! the mockups' check boxes and radio buttons, filled destructive buttons,
//! tool window footers, coloured tables, command and log blocks, chips and
//! collapsible cards. Built on the tokens and parts of `ternarygrid_ui`.

use crate::widgets::TableResponse;
use egui::text::LayoutJob;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Layout, Margin, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use std::hash::Hash;
use ternarygrid_ui::{kit, parts, style};

/// Padding of a tool window's body (the mockups' `16px 20px`).
pub(crate) const BODY: Margin = Margin { left: 20, right: 20, top: 16, bottom: 16 };

/// A note on a soft tinted background (an icon, an optional bold title, the text) and
/// `add` after the text (a button). Like `parts::callout`, but as high as its text.
pub(crate) fn note_with(ui: &mut Ui, tone: parts::Tone, title: Option<&str>, body: &str, add: impl FnOnce(&mut Ui)) -> Response {
    let p = style::pal(ui);
    let color = tone.color(&p);
    Frame::new()
        .fill(tone.fill(ui))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin { left: 12, right: 10, top: 9, bottom: 9 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new(tone.icon()).font(style::icon_font(16.0)).color(color));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    if let Some(t) = title {
                        ui.label(RichText::new(t).font(style::semibold(ui.ctx(), style::body_size(ui.ctx()))).color(p.text));
                    }
                    ui.add(egui::Label::new(RichText::new(body).color(p.text)).wrap().selectable(true));
                    add(ui);
                });
            });
        })
        .response
}

/// [`note_with`] without extra content.
pub(crate) fn note(ui: &mut Ui, tone: parts::Tone, title: Option<&str>, body: &str) -> Response {
    note_with(ui, tone, title, body, |_| {})
}

/// An error note.
pub(crate) fn error(ui: &mut Ui, text: &str) -> Response {
    note(ui, parts::Tone::Error, None, text)
}

/// The mono font of tables, commands and logs.
pub(crate) fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

// ---------------------------------------------------------------------------------------------
// Check boxes, radio buttons, buttons
// ---------------------------------------------------------------------------------------------

/// A check box drawn like the mockups: an accent box with a white check when on.
pub(crate) fn checkbox(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let p = style::pal(ui);
    let enabled = ui.is_enabled();
    let galley = (!label.is_empty()).then(|| ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(style::body_size(ui.ctx())), Color32::PLACEHOLDER));
    let w = 16.0 + galley.as_ref().map_or(0.0, |g| g.size().x + 8.0);
    let h = galley.as_ref().map_or(16.0, |g| g.size().y).max(20.0);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *on, label));
    if ui.is_rect_visible(rect) {
        let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
        let b = Rect::from_center_size(pos2(rect.left() + 8.0, rect.center().y), Vec2::splat(16.0));
        let painter = ui.painter();
        if *on {
            painter.rect_filled(b, CornerRadius::same(4), fade(p.accent));
            let c = b.center();
            let pts = vec![pos2(c.x - 3.6, c.y + 0.2), pos2(c.x - 1.0, c.y + 2.8), pos2(c.x + 3.8, c.y - 2.6)];
            painter.add(egui::Shape::line(pts, Stroke::new(2.0, fade(Color32::WHITE))));
        } else {
            let border = if enabled && resp.hovered() { p.accent } else { p.border_strong };
            painter.rect(b, CornerRadius::same(4), p.surface, Stroke::new(1.5, fade(border)), StrokeKind::Inside);
        }
        if resp.has_focus() {
            style::focus_halo(ui, b, 4);
        }
        if let Some(g) = galley {
            let pos = pos2(b.right() + 8.0, rect.center().y - g.size().y / 2.0);
            ui.painter().galley(pos, g, fade(p.text));
        }
    }
    resp
}

/// A radio button drawn like the mockups (an accent ring with a dot). Returns its response;
/// `clicked()` is the choice.
pub(crate) fn radio(ui: &mut Ui, selected: bool, label: &str) -> Response {
    let p = style::pal(ui);
    let enabled = ui.is_enabled();
    let g = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(style::body_size(ui.ctx())), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0 + 8.0 + g.size().x, g.size().y.max(20.0)), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, enabled, selected, label));
    if ui.is_rect_visible(rect) {
        let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
        let c = pos2(rect.left() + 8.0, rect.center().y);
        let ring = if selected || (enabled && resp.hovered()) { p.accent } else { p.border_strong };
        ui.painter().circle(c, 7.25, p.surface, Stroke::new(1.5, fade(ring)));
        if selected {
            ui.painter().circle_filled(c, 4.0, fade(p.accent));
        }
        if resp.has_focus() {
            style::focus_halo(ui, Rect::from_center_size(c, Vec2::splat(16.0)), 8);
        }
        ui.painter().galley(pos2(rect.left() + 24.0, rect.center().y - g.size().y / 2.0), g, fade(p.text));
    }
    resp
}

/// [`radio`] for one value of `current`; true when it was chosen.
pub(crate) fn radio_value<T: PartialEq>(ui: &mut Ui, current: &mut T, value: T, label: &str) -> bool {
    let on = *current == value;
    let clicked = radio(ui, on, label).clicked();
    if clicked && !on {
        *current = value;
        return true;
    }
    false
}

/// A filled red button: the confirmation of a destructive action (Terminate, Truncate,
/// Restore, Run on the target).
pub(crate) fn danger_solid(ui: &mut Ui, glyph: Option<&str>, text: &str) -> Response {
    let p = style::pal(ui);
    let dark = ui.visuals().dark_mode;
    let enabled = ui.is_enabled();
    let size = style::body_size(ui.ctx());
    let galley = ui.painter().layout_no_wrap(text.to_string(), style::medium(ui.ctx(), size), Color32::PLACEHOLDER);
    let icon_g = glyph.map(|i| ui.painter().layout_no_wrap(i.to_string(), style::icon_font(size + 2.0), Color32::PLACEHOLDER));
    let gap = if icon_g.is_some() { 6.0 } else { 0.0 };
    let content = galley.size().x + icon_g.as_ref().map_or(0.0, |g| g.size().x) + gap;
    let (rect, resp) = ui.allocate_exact_size(vec2(content + 24.0, style::CONTROL_H), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, text));
    if ui.is_rect_visible(rect) {
        let base = if dark { Color32::from_rgb(0xc4, 0x35, 0x2b) } else { p.err };
        let fill = if enabled && resp.hovered() { crate::theme::darken(base, 0.08) } else { base };
        let fill = if enabled { fill } else { fill.gamma_multiply(0.5) };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(style::RADIUS), fill);
        if resp.has_focus() {
            painter.rect_stroke(rect.expand(2.0), CornerRadius::same(style::RADIUS + 2), Stroke::new(1.5, p.accent), StrokeKind::Outside);
        }
        let mut x = rect.center().x - content / 2.0;
        if let Some(g) = icon_g {
            let w = g.size().x;
            painter.galley(pos2(x, rect.center().y - g.size().y / 2.0), g, Color32::WHITE);
            x += w + gap;
        }
        painter.galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    }
    resp
}

/// The main action of a tool, a 24 px toolbar button of `kind`.
pub(crate) fn small(ui: &mut Ui, kind: kit::Kind, glyph: &str, text: &str) -> Response {
    kit::button_ex(ui, kind, Some(glyph), text, true)
}

// ---------------------------------------------------------------------------------------------
// Text pieces
// ---------------------------------------------------------------------------------------------

/// A label in `color` (counts in a tone, states).
pub(crate) fn tone(ui: &mut Ui, text: &str, color: Color32, strong: bool) -> Response {
    let t = RichText::new(text).color(color);
    let t = if strong { t.font(style::semibold(ui.ctx(), style::body_size(ui.ctx()))) } else { t };
    ui.add(egui::Label::new(t).selectable(false))
}

/// A muted label.
pub(crate) fn muted(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).color(p.text_muted)).selectable(false))
}

/// A faint label (facts, hints).
pub(crate) fn faint(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(p.text_faint)).wrap())
}

/// A title in semibold 13 (groups of a tool: "Operation", "Arguments").
pub(crate) fn title(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.label(RichText::new(text).font(style::semibold(ui.ctx(), 13.0)).color(p.text))
}

/// The connection a tool works on: its name in semibold and faint facts (product, version).
pub(crate) fn conn_title(ui: &mut Ui, name: &str, facts: Option<&str>) {
    let p = style::pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(RichText::new(name).font(style::semibold(ui.ctx(), 13.0)).color(p.text));
        if let Some(f) = facts.filter(|f| !f.trim().is_empty()) {
            ui.label(RichText::new(f).size(12.0).color(p.text_faint));
        }
    });
}

/// A rounded chip on the subtle fill (objects of a tool).
pub(crate) fn chip(ui: &mut Ui, glyph: Option<&str>, text: &str) -> Response {
    let p = style::pal(ui);
    let g = ui.painter().layout_no_wrap(text.to_string(), style::medium(ui.ctx(), 11.5), p.text);
    let ig = glyph.map(|i| ui.painter().layout_no_wrap(i.to_string(), style::icon_font(12.0), p.text_muted));
    let iw = ig.as_ref().map_or(0.0, |g| g.size().x + 5.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + iw + 16.0, 20.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same(10), p.subtle);
        let mut x = rect.left() + 8.0;
        if let Some(ig) = ig {
            let w = ig.size().x;
            ui.painter().galley(pos2(x, rect.center().y - ig.size().y / 2.0), ig, p.text_muted);
            x += w + 5.0;
        }
        ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, p.text);
    }
    resp
}

/// The two-letter logo of a database kind on its brand colour.
pub(crate) fn logo(ui: &mut Ui, kind: ternarygrid_core::DbKind, size: f32) -> Response {
    use ternarygrid_core::DbKind as K;
    let label = match kind {
        K::Postgres => "Pg",
        K::CockroachDb => "Cr",
        K::MySql => "My",
        K::MariaDb => "Ma",
        K::Sqlite => "Lt",
        K::SqlServer => "Ms",
        K::ClickHouse => "Ch",
        K::Oracle => "Or",
        K::Cassandra => "Ca",
        K::Redis => "Rd",
        K::MongoDb => "Mg",
        K::Elasticsearch => "Es",
        K::Neo4j => "N4",
        K::Generic => "Db",
    };
    let bg = crate::theme::brand_color(kind);
    let fg = crate::theme::legible_on(Color32::WHITE, bg);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(4), bg);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, style::semibold(ui.ctx(), (size * 0.5).round()), fg);
    resp
}

// ---------------------------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------------------------

/// A command line on the header colour (mono 12, wrapped, selectable).
pub(crate) fn command_block(ui: &mut Ui, text: &str) {
    let p = style::pal(ui);
    Frame::new().fill(p.header).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(text).font(mono(12.0)).color(p.text)).wrap().selectable(true));
    });
}

/// A bordered log box (mono 11.5) of coloured lines, scrolled to the end; `height` is its
/// height (`None`: the rest of the space).
pub(crate) fn log_box(ui: &mut Ui, id: impl Hash + std::fmt::Debug, lines: &[(String, Color32)], height: Option<f32>) {
    let p = style::pal(ui);
    Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let h = height.unwrap_or_else(|| (ui.available_height() - 4.0).max(60.0));
        egui::ScrollArea::both().id_salt(id).auto_shrink([false, false]).max_height(h).min_scrolled_height(h).stick_to_bottom(true).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            for (l, c) in lines {
                ui.add(egui::Label::new(RichText::new(l).font(mono(11.5)).color(*c)).extend());
            }
        });
    });
}

/// A card with a clickable header (chevron, title, faint summary) that shows `add` when open.
pub(crate) fn collapsible_card(ui: &mut Ui, open: &mut bool, title: &str, summary: Option<&str>, add: impl FnOnce(&mut Ui)) -> Response {
    let p = style::pal(ui);
    kit::card(ui)
        .corner_radius(CornerRadius::same(style::RADIUS_LG))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            let head = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let chevron = if *open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
                    ui.label(RichText::new(chevron).font(style::icon_font(12.0)).color(p.text_faint));
                    ui.label(RichText::new(title).font(style::semibold(ui.ctx(), 13.0)).color(p.text));
                    if let Some(s) = summary {
                        ui.label(RichText::new(s).size(12.0).color(p.text_faint));
                    }
                })
                .response;
            let head = ui.interact(head.rect, ui.id().with(("card_head", title)), Sense::click());
            head.widget_info(|| WidgetInfo::selected(WidgetType::CollapsingHeader, true, *open, title));
            if head.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if head.clicked() {
                *open = !*open;
            }
            if *open {
                ui.add_space(4.0);
                add(ui);
            }
        })
        .response
}

/// The footer of a tool window: a bar on the chrome colour with a hairline above it, across
/// the whole window, at the bottom of `ui`. `add` lays out its buttons from the left.
pub(crate) fn footer(ui: &mut Ui, id: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    let p = style::pal(ui);
    // In a window: across the whole window, whatever its margins; in a tab: across the tab.
    let layer = ui.layer_id();
    let area = (layer.order != egui::Order::Background).then(|| ui.ctx().memory(|m| m.area_rect(layer.id))).flatten();
    let outer = area.map(|r| r.shrink(1.0)).unwrap_or_else(|| ui.clip_rect());
    egui::Panel::bottom(Id::new(id)).resizable(false).show_separator_line(false).frame(Frame::new().inner_margin(Margin { left: 20, right: 16, top: 12, bottom: 12 })).show(
        ui,
        |ui| {
            let top = ui.max_rect().top() - 12.0;
            let fill = Rect::from_min_max(pos2(outer.left(), top), pos2(outer.right(), outer.bottom().max(ui.max_rect().bottom() + 12.0)));
            let painter = ui.ctx().layer_painter(layer).with_clip_rect(outer);
            let r = style::RADIUS_LG + 1;
            painter.rect_filled(fill, CornerRadius { nw: 0, ne: 0, sw: r, se: r }, p.chrome);
            painter.hline(fill.x_range(), top + 0.5, Stroke::new(1.0, p.border));
            ui.set_min_height(style::CONTROL_H);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                add(ui);
            });
        },
    );
}

/// The padded body of a tool window (scrolls when it does not fit).
pub(crate) fn body<R>(ui: &mut Ui, id: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::CentralPanel::default()
        .frame(Frame::new())
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(id)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::new()
                        .inner_margin(BODY)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 12.0;
                            add(ui)
                        })
                        .inner
                })
                .inner
        })
        .inner
}

/// A hairline across `ui` with room above and below.
pub(crate) fn divider(ui: &mut Ui) {
    ui.add_space(2.0);
    parts::hline(ui);
    ui.add_space(2.0);
}

/// A form row: a muted label in a column `label_w` wide, then the control(s).
pub(crate) fn row<R>(ui: &mut Ui, label: &str, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    parts::form_row(ui, label, label_w, add)
}

/// A form row whose controls are stacked (check boxes): the label at their top.
pub(crate) fn row_top<R>(ui: &mut Ui, label: &str, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = style::pal(ui);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(label_w, 20.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(label_w);
            ui.add(egui::Label::new(RichText::new(label).color(p.text_muted)).truncate());
        });
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            add(ui)
        })
        .inner
    })
    .inner
}

/// A single-line text field filling `width` (`f32::INFINITY`: the rest of the row).
pub(crate) fn field(ui: &mut Ui, value: &mut String, hint: &str, width: f32, mono_font: bool) -> Response {
    let p = style::pal(ui);
    let w = if width.is_finite() { width } else { ui.available_width() };
    let mut edit = egui::TextEdit::singleline(value).desired_width(w).hint_text(RichText::new(hint).color(p.text_faint)).margin(Margin::symmetric(8, 5));
    if mono_font {
        edit = edit.font(mono(12.5));
    }
    let r = ui.add(edit);
    if r.has_focus() {
        style::focus_halo(ui, r.rect, style::RADIUS);
    }
    r
}

// ---------------------------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------------------------

/// One cell of a [`table`]: its text and how it is drawn.
#[derive(Clone, Default)]
pub(crate) struct Cell {
    pub text: String,
    pub color: Option<Color32>,
    pub mono: bool,
    pub strong: bool,
    pub glyph: Option<(&'static str, Color32)>,
    pub job: Option<LayoutJob>,
    pub hover: Option<String>,
}

impl Cell {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Cell { text: text.into(), ..Default::default() }
    }
    pub(crate) fn color(mut self, c: Color32) -> Self {
        self.color = Some(c);
        self
    }
    pub(crate) fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
    pub(crate) fn glyph(mut self, g: &'static str, c: Color32) -> Self {
        self.glyph = Some((g, c));
        self
    }
    pub(crate) fn job(mut self, job: LayoutJob) -> Self {
        self.job = Some(job);
        self
    }
    pub(crate) fn hover(mut self, text: impl Into<String>) -> Self {
        self.hover = Some(text.into());
        self
    }
}

const CELL_PAD: f32 = 10.0;

/// Width of a cell's text.
fn cell_width(ui: &Ui, c: &Cell) -> f32 {
    let font = if c.mono { mono(12.0) } else { FontId::proportional(style::body_size(ui.ctx())) };
    let first = c.text.lines().next().unwrap_or("");
    let w = match &c.job {
        Some(j) => ui.fonts_mut(|f| f.layout_job(j.clone()).size().x),
        None => ui.painter().layout_no_wrap(first.chars().take(120).collect(), font, Color32::PLACEHOLDER).size().x,
    };
    w + c.glyph.map_or(0.0, |_| 18.0)
}

fn paint_cell(ui: &mut Ui, c: &Cell, header: bool) {
    let p = style::pal(ui);
    ui.add_space(CELL_PAD);
    ui.spacing_mut().item_spacing.x = 5.0;
    if let Some((g, color)) = c.glyph {
        ui.label(RichText::new(g).font(style::icon_font(13.0)).color(color));
    }
    let resp = if let Some(j) = &c.job {
        ui.add(egui::Label::new(j.clone()).truncate().selectable(false))
    } else {
        let first: String = c.text.lines().next().unwrap_or("").chars().take(300).collect();
        let mut t = RichText::new(first);
        t = if header {
            t.font(style::semibold(ui.ctx(), 12.0)).color(c.color.unwrap_or(p.text_muted))
        } else if c.mono {
            t.font(mono(12.0)).color(c.color.unwrap_or(p.text))
        } else if c.strong {
            t.font(style::semibold(ui.ctx(), style::body_size(ui.ctx()))).color(c.color.unwrap_or(p.text))
        } else {
            t.color(c.color.unwrap_or(p.text))
        };
        ui.add(egui::Label::new(t).truncate().selectable(false))
    };
    let long = c.text.lines().nth(1).is_some() || c.text.chars().count() > 300;
    match &c.hover {
        Some(h) => {
            resp.on_hover_text(h);
        }
        None if long => {
            resp.on_hover_text(c.text.chars().take(4000).collect::<String>());
        }
        None => {}
    }
}

/// A bordered table of styled cells like the mockups' (header on the header colour, striped
/// rows, resizable columns, the last column takes the rest).
pub(crate) fn table(ui: &mut Ui, id: impl Hash + std::fmt::Debug, headers: &[Cell], rows: &[Vec<Cell>], selected: Option<usize>) -> TableResponse {
    let mut out = TableResponse::default();
    parts::bordered(ui).show(ui, |ui| {
        ui.scope(|ui| {
            let p = style::pal(ui);
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 13.0;
            let widths: Vec<f32> = headers
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let head = cell_width(ui, &Cell { mono: false, ..h.clone() }) + 2.0 * CELL_PAD + 4.0;
                    let content = rows.iter().take(100).filter_map(|r| r.get(i)).map(|c| cell_width(ui, c)).fold(0.0, f32::max) + 2.0 * CELL_PAD + 4.0;
                    head.max(content).clamp(head.max(44.0), 420.0)
                })
                .collect();
            // The columns before the last share the width the last one leaves (it takes the
            // rest, at least 160): shrunk to fit, they can be widened again.
            let mut widths = widths;
            if let Some((_, first)) = widths.split_last_mut() {
                let room = (ui.available_width() - 160.0 - first.len() as f32 * 8.0).max(first.len() as f32 * 40.0);
                let used: f32 = first.iter().sum();
                if used > room {
                    let k = room / used;
                    for w in first.iter_mut() {
                        *w = (*w * k).max(40.0);
                    }
                }
            }
            let mut tb =
                TableBuilder::new(ui).id_salt(id).striped(true).resizable(true).sense(Sense::click()).auto_shrink([false, true]).cell_layout(Layout::left_to_right(Align::Center));
            for (i, w) in widths.iter().enumerate() {
                tb = if i + 1 == headers.len() {
                    tb.column(Column::remainder().at_least(w.min(160.0)).clip(true))
                } else {
                    tb.column(Column::initial(*w).at_least(40.0).clip(true).resizable(true))
                };
            }
            tb.header(row_h + 2.0, |mut header| {
                for h in headers {
                    header.col(|ui| {
                        ui.painter().rect_filled(ui.max_rect().expand2(vec2(4.0, 0.0)), 0.0, p.header);
                        paint_cell(ui, h, true);
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, rows.len(), |mut row| {
                    let i = row.index();
                    row.set_selected(selected == Some(i));
                    if let Some(cells) = rows.get(i) {
                        for c in cells.iter().take(headers.len()) {
                            row.col(|ui| paint_cell(ui, c, false));
                        }
                    }
                    let resp = row.response();
                    if resp.clicked() {
                        out.clicked = Some(i);
                    }
                    if resp.double_clicked() {
                        out.double_clicked = Some(i);
                    }
                    if resp.secondary_clicked() {
                        out.context_row = Some(i);
                    }
                });
            });
        });
    });
    out
}

/// Header cells from names.
pub(crate) fn headers(names: &[&str]) -> Vec<Cell> {
    names.iter().map(|n| Cell::new(*n)).collect()
}

/// A bordered grid of widgets with a header row like [`table`]: `cell(ui, row, column)` draws
/// each cell; `widths` are the column widths (the last one takes the rest).
pub(crate) fn widget_table(ui: &mut Ui, id: impl Hash + std::fmt::Debug, names: &[&str], widths: &[f32], rows: usize, row_h: f32, mut cell: impl FnMut(&mut Ui, usize, usize)) {
    parts::bordered(ui).show(ui, |ui| {
        ui.scope(|ui| {
            let p = style::pal(ui);
            let mut tb = TableBuilder::new(ui).id_salt(id).striped(true).auto_shrink([false, true]).vscroll(false).cell_layout(Layout::left_to_right(Align::Center));
            for (i, w) in widths.iter().enumerate() {
                tb = if i + 1 == widths.len() { tb.column(Column::remainder().at_least(*w).clip(true)) } else { tb.column(Column::exact(*w).clip(true)) };
            }
            tb.header(32.0, |mut header| {
                for n in names {
                    header.col(|ui| {
                        ui.painter().rect_filled(ui.max_rect().expand2(vec2(4.0, 0.0)), 0.0, p.header);
                        paint_cell(ui, &Cell::new(*n), true);
                    });
                }
            })
            .body(|mut body| {
                for r in 0..rows {
                    body.row(row_h, |mut row| {
                        for c in 0..widths.len() {
                            row.col(|ui| {
                                ui.add_space(8.0);
                                cell(ui, r, c);
                            });
                        }
                    });
                }
            });
        });
    });
}

/// A "before → after" text: both semibold, the arrow faint.
pub(crate) fn change_job(ui: &Ui, before: &str, after: &str) -> LayoutJob {
    let p = style::pal(ui);
    let font = style::semibold(ui.ctx(), style::body_size(ui.ctx()));
    let mut job = LayoutJob::default();
    let fmt = |c: Color32| egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
    job.append(before, 0.0, fmt(p.text));
    job.append(" → ", 0.0, fmt(p.text_faint));
    job.append(after, 0.0, fmt(p.text));
    job
}
