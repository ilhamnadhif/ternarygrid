//! The look shared by the compare and administration tools (schema and data
//! compare, sessions, locks, backup, maintenance, password, FDW, debugger):
//! the mockups' check boxes and radio buttons, filled destructive buttons,
//! tool window footers, coloured tables, command and log blocks, chips and
//! collapsible cards. Built on the tokens and parts of `ternarygrid_ui`.

use crate::tabs::transfer::wizard::W;
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

/// A note on a soft tinted background (the kit's callout) with `add` under the text (a button).
pub(crate) fn note_with(ui: &mut Ui, tone: parts::Tone, title: Option<&str>, body: &str, add: impl FnOnce(&mut Ui)) -> Response {
    parts::callout(ui, tone, title, body, add)
}

/// [`note_with`] without extra content.
pub(crate) fn note(ui: &mut Ui, tone: parts::Tone, title: Option<&str>, body: &str) -> Response {
    parts::callout(ui, tone, title, body, |_| {})
}

/// An error note.
pub(crate) fn error(ui: &mut Ui, text: &str) -> Response {
    note(ui, parts::Tone::Error, None, text)
}

/// The mono font of tables, commands and logs.
pub(crate) fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// The check box of a table cell: 15 px, radius 4, the accent with a white check when on.
pub(crate) fn cell_check(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let enabled = ui.is_enabled();
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *on, label));
    if ui.is_rect_visible(rect) {
        paint_check(ui, rect, *on, enabled && resp.hovered(), enabled);
        if resp.has_focus() {
            style::focus_halo(ui, rect, 4);
        }
    }
    resp
}

/// A check box's box in `b` (the check scaled to it, as the design's 24 px check icon).
fn paint_check(ui: &Ui, b: Rect, on: bool, hovered: bool, enabled: bool) {
    let p = style::pal(ui);
    let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
    let painter = ui.painter();
    if on {
        painter.rect_filled(b, CornerRadius::same(4), fade(if hovered { p.accent_hover } else { p.accent }));
        // The icon is 2/3 of the box; its path is M5.5 12.5 l4.2 4.2 L18.5 8 on 24 units.
        let k = b.width() * 2.0 / 3.0 / 24.0;
        let c = b.center();
        let pts = vec![pos2(c.x - 6.5 * k, c.y + 0.5 * k), pos2(c.x - 2.3 * k, c.y + 4.7 * k), pos2(c.x + 6.5 * k, c.y - 4.0 * k)];
        painter.add(egui::Shape::line(pts, Stroke::new(2.8 * k, fade(Color32::WHITE))));
    } else {
        painter.rect(b, CornerRadius::same(4), p.surface, Stroke::new(1.5, fade(if hovered { p.accent } else { p.border_strong })), StrokeKind::Inside);
    }
}

// ---------------------------------------------------------------------------------------------
// Check boxes, radio buttons, buttons
// ---------------------------------------------------------------------------------------------

/// A check box of the design (the kit's).
pub(crate) fn checkbox(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    kit::checkbox(ui, on, label)
}

/// A radio button drawn like the mockups (an accent ring with a dot). Returns its response;
/// `clicked()` is the choice.
pub(crate) fn radio(ui: &mut Ui, selected: bool, label: &str) -> Response {
    kit::radio(ui, selected, label)
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

/// The main action of a tool, a 24 px toolbar button of `kind`.
pub(crate) fn small(ui: &mut Ui, kind: kit::Kind, glyph: &str, text: &str) -> Response {
    kit::button_ex(ui, kind, Some(glyph), text, true)
}

// ---------------------------------------------------------------------------------------------
// Text pieces
// ---------------------------------------------------------------------------------------------

/// A label in `color` (counts in a tone, states).
pub(crate) fn tone(ui: &mut Ui, text: &str, color: Color32, strong: bool) -> Response {
    if strong {
        return kit::semibold_label(ui, text, style::body_size(ui.ctx()), color);
    }
    ui.add(egui::Label::new(RichText::new(text).color(color)).selectable(false))
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
    ternarygrid_ui::kit::semibold_label(ui, text, 13.0, p.text)
}

/// The connection a tool works on: its name in semibold and faint facts (product, version).
pub(crate) fn conn_title(ui: &mut Ui, name: &str, facts: Option<&str>) {
    let p = style::pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ternarygrid_ui::kit::semibold_label(ui, name, 13.0, p.text);
        if let Some(f) = facts.filter(|f| !f.trim().is_empty()) {
            ui.label(RichText::new(f).size(12.0).color(p.text_faint));
        }
    });
}

/// A pill on the subtle fill (the objects of a tool): 18 high, 11 px semibold.
pub(crate) fn chip(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    let g = ui.painter().layout_no_wrap(text.to_string(), style::semibold(ui.ctx(), 11.0), p.text);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 14.0, 18.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same(9), p.subtle);
        ui.painter().galley(pos2(rect.left() + 7.0, rect.center().y - g.size().y / 2.0), g, p.text);
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

/// The options of a read-only code view as the designs show code: `font` points, `line`
/// points a line, no fold markers, marked words or current line.
pub(crate) fn code_options(font: f32, line: f32) -> ternarygrid_editor::EditorOptions {
    let mut o = ternarygrid_editor::EditorOptions::read_only_viewer();
    o.font_size = font;
    o.line_height = line / font;
    o.folding = false;
    o.mark_occurrences = false;
    o.highlight_current_line = false;
    o
}

/// The design's code block: `text` highlighted in `lang`'s dialect, mono 12.5 on 20 pt lines,
/// 10 above and below; with `gutter` the line numbers in a 32 pt column 14 from the text,
/// else 12 at the left.
pub(crate) fn code_block(ui: &mut Ui, lang: Option<&crate::sqllang::SqlLanguage>, text: &str, gutter: bool) {
    use ternarygrid_editor::EditorLanguage;
    let p = style::pal(ui);
    let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
    let tokens = lang.map(|l| l.highlight(text)).unwrap_or_default();
    let mut job = crate::sqllang::highlight_job(text, &tokens, &theme, mono(12.5));
    for s in &mut job.sections {
        s.format.line_height = Some(20.0);
    }
    let margin = Margin { left: if gutter { 0 } else { 12 }, right: 12, top: 10, bottom: 10 };
    Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(margin).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            if gutter {
                let numbers = (1..=text.lines().count().max(1)).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
                ui.allocate_ui_with_layout(vec2(32.0, 0.0), Layout::top_down(Align::Max), |ui| {
                    ui.set_width(32.0);
                    ui.label(RichText::new(numbers).font(mono(12.5)).line_height(Some(20.0)).color(p.text_faint));
                });
                ui.add_space(14.0);
            }
            ui.add(egui::Label::new(job).wrap().selectable(true));
        });
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
            // The head as high as its text (the body keeps the controls' height).
            let control_h = ui.spacing().interact_size.y;
            ui.spacing_mut().interact_size.y = 0.0;
            let head = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.spacing_mut().interact_size.y = control_h;
                    let chevron = if *open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
                    ui.label(RichText::new(chevron).font(style::icon_font(12.0)).color(p.text_faint));
                    ternarygrid_ui::kit::semibold_label(ui, title, 13.0, p.text);
                    if let Some(s) = summary {
                        ui.label(RichText::new(s).size(12.0).color(p.text_faint));
                    }
                })
                .response;
            ui.spacing_mut().interact_size.y = control_h;
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
    let mut add = Some(add);
    footer_dyn(ui, Id::new(id), &mut |ui| {
        if let Some(a) = add.take() {
            a(ui)
        }
    });
}

fn footer_dyn(ui: &mut Ui, id: Id, add: &mut dyn FnMut(&mut Ui)) {
    let p = style::pal(ui);
    // In a window: across the whole window, whatever its margins; in a tab: across the tab.
    let layer = ui.layer_id();
    let area = (layer.order != egui::Order::Background).then(|| ui.ctx().memory(|m| m.area_rect(layer.id))).flatten();
    let outer = area.map(|r| r.shrink(1.0)).unwrap_or_else(|| ui.clip_rect());
    egui::Panel::bottom(id).resizable(false).show_separator_line(false).frame(Frame::new().inner_margin(Margin { left: 20, right: 16, top: 12, bottom: 12 })).show(ui, |ui| {
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
    });
}

/// The padded body of a tool window (scrolls when it does not fit).
pub(crate) fn body<R>(ui: &mut Ui, id: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut add = Some(add);
    let mut out = None;
    body_dyn(ui, Id::new(id), &mut |ui| out = add.take().map(|a| a(ui)));
    out.expect("a tool body runs once")
}

fn body_dyn(ui: &mut Ui, id: Id, add: &mut dyn FnMut(&mut Ui)) {
    egui::CentralPanel::default().frame(Frame::new()).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
            Frame::new().inner_margin(BODY).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                add(ui);
            });
        });
    });
}

/// A hairline across `ui`, as far from the parts above and below as they are from each other
/// (the design's dividers in a column).
pub(crate) fn divider(ui: &mut Ui) {
    parts::hline(ui);
    ui.add_space(ui.spacing().item_spacing.y);
}

/// A form row: a muted label in a column `label_w` wide, then the control(s).
pub(crate) fn row<R>(ui: &mut Ui, label: &str, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    parts::form_row(ui, label, label_w, add)
}

/// [`row`] whose only control is a segmented one: 28 high, as in the design.
pub(crate) fn seg_row(ui: &mut Ui, label: &str, label_w: f32, add: impl FnOnce(&mut Ui)) {
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = crate::tabs::transfer::wizard::SEGMENTED_H;
        parts::form_row(ui, label, label_w, add)
    });
}

/// A form row whose controls are stacked (check boxes): the label at their top.
pub(crate) fn row_top<R>(ui: &mut Ui, label: &str, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = style::pal(ui);
    // As in the design: the label 6 below the top of the first control (a 22 px row).
    let control_h = ui.spacing().interact_size.y;
    ui.spacing_mut().interact_size.y = 0.0;
    let out = ui
        .horizontal_top(|ui| {
            ui.spacing_mut().interact_size.y = control_h;
            let (rect, _) = ui.allocate_exact_size(vec2(label_w, 22.0), Sense::hover());
            let at = Rect::from_min_size(rect.min + vec2(0.0, 6.0), vec2(label_w, 16.0));
            // A child ui: the cursor stays after the label column.
            ui.new_child(egui::UiBuilder::new().max_rect(at).layout(Layout::left_to_right(Align::Min))).add(egui::Label::new(RichText::new(label).color(p.text_muted)).truncate());
            // 16 between the label column and the controls, as `parts::form_row`.
            ui.add_space((16.0 - ui.spacing().item_spacing.x).max(0.0));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                add(ui)
            })
            .inner
        })
        .inner;
    ui.spacing_mut().interact_size.y = control_h;
    out
}

/// A level from 0 to `max` as the design's 6 pt progress bar, `width` wide: a click or a drag
/// sets it (and `on`), a double click turns it off (the bar then shows faint). True on a change.
pub(crate) fn level_bar(ui: &mut Ui, label: &str, on: &mut bool, value: &mut u8, max: u8, width: f32) -> bool {
    let p = style::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 14.0), Sense::click_and_drag());
    let mut changed = false;
    if resp.double_clicked() {
        *on = false;
        changed = true;
    } else if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
        let v = (((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * max as f32).round() as u8;
        changed = !*on || v != *value;
        (*on, *value) = (true, v);
    }
    resp.widget_info(|| WidgetInfo::slider(ui.is_enabled(), *value as f64, label));
    let track = Rect::from_center_size(rect.center(), vec2(width, 6.0));
    ui.painter().rect_filled(track, CornerRadius::same(3), p.subtle);
    let fill = Rect::from_min_size(track.min, vec2(width * *value as f32 / max.max(1) as f32, 6.0));
    let color = if *on && ui.is_enabled() { p.accent } else { p.border_strong };
    ui.painter().rect_filled(fill, CornerRadius::same(3), color);
    changed
}

/// A single-line text field filling `width` (`f32::INFINITY`: the rest of the row).
pub(crate) fn field(ui: &mut Ui, value: &mut String, hint: &str, width: f32, mono_font: bool) -> Response {
    let p = style::pal(ui);
    let w = if width.is_finite() { width } else { ui.available_width() };
    let mut edit = crate::tabs::transfer::wizard::fit(egui::TextEdit::singleline(value), style::CONTROL_H).desired_width(w).hint_text(RichText::new(hint).color(p.text_faint));
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
    let first: String = c.text.lines().next().unwrap_or("").chars().take(300).collect();
    // Headers and strong cells in the design's semibold (clipped by the cell).
    let resp = if let Some(j) = &c.job {
        ui.add(egui::Label::new(j.clone()).truncate().selectable(false))
    } else if header {
        kit::semibold_label(ui, &first, 12.0, c.color.unwrap_or(p.text_muted))
    } else if c.strong {
        kit::semibold_label(ui, &first, style::body_size(ui.ctx()), c.color.unwrap_or(p.text))
    } else {
        let t = RichText::new(first);
        let t = if c.mono { t.font(mono(12.0)).color(c.color.unwrap_or(p.text)) } else { t.color(c.color.unwrap_or(p.text)) };
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

/// A bordered table of styled cells like the mockups' (a 32 px header on the header colour,
/// striped rows 30 high with a hairline under each, resizable columns, the last column takes
/// the rest); the column widths come from the content.
pub(crate) fn table(ui: &mut Ui, id: impl Hash + std::fmt::Debug, headers: &[Cell], rows: &[Vec<Cell>], selected: Option<usize>) -> TableResponse {
    table_dyn(ui, Id::new(id), headers, rows, selected, &[])
}

/// [`table`] with the design's column widths (points or shares of the rest).
pub(crate) fn table_w(ui: &mut Ui, id: impl Hash + std::fmt::Debug, headers: &[Cell], rows: &[Vec<Cell>], selected: Option<usize>, widths: &[W]) -> TableResponse {
    table_dyn(ui, Id::new(id), headers, rows, selected, widths)
}

/// The widths of `headers` from their content (at most 420; the columns before the last
/// shrink so the last one keeps 160).
fn content_widths(ui: &Ui, headers: &[Cell], rows: &[Vec<Cell>]) -> Vec<f32> {
    let mut widths: Vec<f32> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let head = cell_width(ui, &Cell { mono: false, ..h.clone() }) + 2.0 * CELL_PAD + 4.0;
            let content = rows.iter().take(100).filter_map(|r| r.get(i)).map(|c| cell_width(ui, c)).fold(0.0, f32::max) + 2.0 * CELL_PAD + 4.0;
            head.max(content).clamp(head.max(44.0), 420.0)
        })
        .collect();
    if let Some((_, first)) = widths.split_last_mut() {
        let room = (ui.available_width() - 160.0).max(first.len() as f32 * 40.0);
        let used: f32 = first.iter().sum();
        if used > room {
            let k = room / used;
            for w in first.iter_mut() {
                *w = (*w * k).max(40.0);
            }
        }
    }
    widths
}

/// The widths of the design's columns in `avail` points.
fn spec_widths(widths: &[W], avail: f32) -> Vec<f32> {
    let fixed: f32 = widths.iter().map(|w| if let W::Px(p) = w { *p } else { 0.0 }).sum();
    let fr: f32 = widths.iter().map(|w| if let W::Fr(f) = w { *f } else { 0.0 }).sum();
    let rest = (avail - fixed).max(0.0);
    widths
        .iter()
        .map(|w| match w {
            W::Px(p) => *p,
            W::Fr(f) => (rest * f / fr.max(f32::EPSILON)).max(40.0),
            W::Min(m) => *m,
        })
        .collect()
}

fn table_dyn(ui: &mut Ui, id: Id, headers: &[Cell], rows: &[Vec<Cell>], selected: Option<usize>, spec: &[W]) -> TableResponse {
    let mut out = TableResponse::default();
    let p = style::pal(ui);
    let (stripe, line, sel) = (p.stripe, p.line, p.sel);
    parts::bordered(ui).show(ui, |ui| {
        ui.scope(|ui| {
            // The cells touch (their hairlines join), the column lines show while resizing only.
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.visuals_mut().selection.bg_fill = sel;
            ui.visuals_mut().widgets.noninteractive.bg_stroke = Stroke::NONE;
            let widths = if spec.len() == headers.len() { spec_widths(spec, ui.available_width()) } else { content_widths(ui, headers, rows) };
            let mut tb =
                TableBuilder::new(ui).id_salt(id).striped(false).resizable(true).sense(Sense::click()).auto_shrink([false, true]).cell_layout(Layout::left_to_right(Align::Center));
            for (i, w) in widths.iter().enumerate() {
                tb = if i + 1 == headers.len() {
                    tb.column(Column::remainder().at_least(w.min(160.0)).clip(true))
                } else {
                    tb.column(Column::initial(*w).at_least(40.0).clip(true).resizable(true))
                };
            }
            tb.header(33.0, |mut header| {
                for h in headers {
                    header.col(|ui| {
                        let r = ui.max_rect();
                        ui.painter().rect_filled(r, 0.0, p.header);
                        ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, p.border));
                        paint_cell(ui, h, true);
                    });
                }
            })
            .body(|body| {
                body.rows(31.0, rows.len(), |mut row| {
                    let i = row.index();
                    row.set_selected(selected == Some(i));
                    if let Some(cells) = rows.get(i) {
                        let n = cells.len().min(headers.len());
                        for (k, c) in cells.iter().take(n).enumerate() {
                            row.col(|ui| {
                                let r = ui.max_rect();
                                if i % 2 == 1 && selected != Some(i) {
                                    stripe_cell(ui, r, stripe, i + 1 == rows.len(), k, n);
                                }
                                ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, line));
                                paint_cell(ui, c, false);
                            });
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

/// The stripe of cell `k` of `n` in an odd row, as the design stripes its tables (egui's
/// stripes start at the first row); the last row's outer cells keep the frame's corners.
fn stripe_cell(ui: &Ui, r: Rect, stripe: Color32, last: bool, k: usize, n: usize) {
    let c = |on: bool| if last && on { 7 } else { 0 };
    ui.painter().rect_filled(r, CornerRadius { nw: 0, ne: 0, sw: c(k == 0), se: c(k + 1 == n) }, stripe);
}

/// Header cells from names.
pub(crate) fn headers(names: &[&str]) -> Vec<Cell> {
    names.iter().map(|n| Cell::new(*n)).collect()
}

/// A bordered grid of widgets with a header row like [`table`]: `cell(ui, row, column)` draws
/// each cell; `widths` are the column widths (the last one takes the rest).
pub(crate) fn widget_table(ui: &mut Ui, id: impl Hash + std::fmt::Debug, names: &[&str], widths: &[f32], rows: usize, row_h: f32, mut cell: impl FnMut(&mut Ui, usize, usize)) {
    widget_table_dyn(ui, Id::new(id), names, widths, rows, row_h, &mut cell);
}

fn widget_table_dyn(ui: &mut Ui, id: Id, names: &[&str], widths: &[f32], rows: usize, row_h: f32, cell: &mut dyn FnMut(&mut Ui, usize, usize)) {
    let p = style::pal(ui);
    let (stripe, line) = (p.stripe, p.line);
    parts::bordered(ui).show(ui, |ui| {
        ui.scope(|ui| {
            // As [`table`]: the cells touch, a hairline under the header and every row.
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.spacing_mut().interact_size.y = (row_h - 4.0).min(26.0);
            ui.visuals_mut().widgets.noninteractive.bg_stroke = Stroke::NONE;
            let mut tb = TableBuilder::new(ui).id_salt(id).striped(false).auto_shrink([false, true]).vscroll(false).cell_layout(Layout::left_to_right(Align::Center));
            for (i, w) in widths.iter().enumerate() {
                tb = if i + 1 == widths.len() { tb.column(Column::remainder().at_least(*w).clip(true)) } else { tb.column(Column::exact(*w).clip(true)) };
            }
            tb.header(33.0, |mut header| {
                for n in names {
                    header.col(|ui| {
                        let r = ui.max_rect();
                        ui.painter().rect_filled(r, 0.0, p.header);
                        ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, p.border));
                        paint_cell(ui, &Cell::new(*n), true);
                    });
                }
            })
            .body(|mut body| {
                for r in 0..rows {
                    body.row(row_h + 1.0, |mut row| {
                        for c in 0..widths.len() {
                            row.col(|ui| {
                                let rect = ui.max_rect();
                                if r % 2 == 1 {
                                    stripe_cell(ui, rect, stripe, r + 1 == rows, c, widths.len());
                                }
                                ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, line));
                                ui.add_space(CELL_PAD);
                                ui.spacing_mut().item_spacing.x = 6.0;
                                cell(ui, r, c);
                            });
                        }
                    });
                }
            });
        });
    });
}

/// A NULL in a table cell as the design draws it: `text` in italics, 12 pt, faint.
pub(crate) fn null_cell(ui: &Ui, text: String) -> Cell {
    let format = egui::TextFormat { font_id: FontId::proportional(12.0), color: style::pal(ui).text_faint, italics: true, ..Default::default() };
    Cell::new(text.clone()).job(LayoutJob::single_section(text, format))
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
