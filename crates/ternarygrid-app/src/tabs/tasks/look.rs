//! Pieces of the workspace screens drawn as in the redesign mockups (settings, tasks, the
//! query history, search, the projects view): check boxes that wrap, number fields with
//! their unit, text fields at the form height, bordered tables with styled cells and weekday
//! toggles. They use the tokens of `ternarygrid_ui::style`.

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Sense, Stroke, StrokeKind, TextStyle, Ui, WidgetInfo, WidgetType, vec2};
use egui_extras::{Column, TableBuilder, TableRow};
use ternarygrid_ui::style::{self, RADIUS};

/// Height of a table row: 30 points and its hairline (the header: 32 and its line).
pub const ROW_H: f32 = 31.0;

fn fade(ui: &Ui, c: Color32) -> Color32 {
    if ui.is_enabled() { c } else { c.gamma_multiply(0.45) }
}

/// The text of a check box or radio button and its optional line of explanation, wrapped
/// at `max_w`.
fn label_galleys(ui: &Ui, label: &str, sub: Option<&str>, max_w: f32) -> (std::sync::Arc<egui::Galley>, Option<std::sync::Arc<egui::Galley>>) {
    let size = style::body_size(ui.ctx());
    let wrap = Some(egui::TextWrapMode::Wrap);
    let title = egui::WidgetText::from(RichText::new(label).size(size)).into_galley(ui, wrap, max_w, TextStyle::Body);
    let sub = sub.map(|s| egui::WidgetText::from(RichText::new(s).size(12.0)).into_galley(ui, wrap, max_w, TextStyle::Body));
    (title, sub)
}

/// A check box: a 16 point box (filled with the accent and a white check when on) and its
/// label, which wraps.
pub fn checkbox(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    checkbox_sub(ui, on, label, None)
}

/// [`checkbox`] with a muted line of explanation under its label.
pub fn checkbox_sub(ui: &mut Ui, on: &mut bool, label: &str, sub: Option<&str>) -> Response {
    let p = ternarygrid_ui::pal(ui);
    let max_w = (ui.available_width() - 26.0).max(60.0);
    let (title, sub_g) = label_galleys(ui, label, sub, max_w);
    let line_h = ui.text_style_height(&TextStyle::Body);
    let text_h = title.size().y + sub_g.as_ref().map_or(0.0, |g| g.size().y + 2.0);
    let w = 26.0 + title.size().x.max(sub_g.as_ref().map_or(0.0, |g| g.size().x));
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, text_h.max(17.0)), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let enabled = ui.is_enabled();
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *on, label));
    if ui.is_rect_visible(rect) {
        // The box 1 point down from the top of the first line (as in the mockups).
        let b = Rect::from_min_size(egui::pos2(rect.left(), rect.top() + ((line_h - 16.0) / 2.0).max(0.0) + 1.0), vec2(16.0, 16.0));
        paint_box(ui, b, *on, resp.hovered());
        if resp.has_focus() {
            style::focus_halo(ui, b, 4);
        }
        let x = rect.left() + 26.0;
        let title_h = title.size().y;
        ui.painter().galley(egui::pos2(x, rect.top()), title, fade(ui, p.text));
        if let Some(g) = sub_g {
            ui.painter().galley(egui::pos2(x, rect.top() + title_h + 2.0), g, fade(ui, p.text_muted));
        }
    }
    resp
}

/// The box of a check box at `b`.
pub fn paint_box(ui: &Ui, b: Rect, on: bool, hovered: bool) {
    let p = ternarygrid_ui::pal(ui);
    let painter = ui.painter();
    if on {
        painter.rect_filled(b, CornerRadius::same(4), fade(ui, if hovered && ui.is_enabled() { p.accent_hover } else { p.accent }));
        // The design's check (as `kit::checkbox`): an 11-point mark, 2.8 on 24.
        let pts = vec![egui::pos2(b.left() + 5.0, b.top() + 8.2), egui::pos2(b.left() + 6.9, b.top() + 10.1), egui::pos2(b.left() + 11.0, b.top() + 6.2)];
        painter.add(egui::Shape::line(pts, Stroke::new(1.3, fade(ui, p.on_accent))));
    } else {
        let stroke = if hovered && ui.is_enabled() { p.accent } else { p.border_strong };
        painter.rect(b, CornerRadius::same(4), p.surface, Stroke::new(1.5, fade(ui, stroke)), StrokeKind::Inside);
    }
}

/// Radio buttons for `value` in a row (16 points apart).
pub fn radios<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, items: &[(T, &str)]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (v, l) in items {
            if ternarygrid_ui::kit::radio(ui, *value == *v, *l).clicked() && *value != *v {
                *value = *v;
                changed = true;
            }
        }
    });
    changed
}

/// Height of the mockups' fields (text, number and select boxes).
pub const FIELD_H: f32 = 30.0;

/// The text of the mockups' inputs is 13 from their left edge (10 of padding, the border and
/// the 2 of an HTML input's own padding).
const FIELD_MARGIN: egui::Margin = egui::Margin { left: 13, right: 10, top: 0, bottom: 0 };

/// A number field 30 points high and at least 64 wide, its value left-aligned (thousands
/// grouped while it is not edited), followed 6 points further by its unit in muted text.
/// Dragging and the arrow keys change it as in egui's `DragValue`.
pub fn number<N: egui::emath::Numeric>(ui: &mut Ui, value: &mut N, range: std::ops::RangeInclusive<N>, unit: &str) -> Response {
    let mut v = value.to_f64();
    let r = number_dyn(ui, &mut v, range.start().to_f64()..=range.end().to_f64(), N::INTEGRAL, unit, 64.0, FIELD_H, true);
    if r.changed() {
        *value = N::from_f64(v);
    }
    r
}

/// [`number`] `w` wide and `h` high, without a unit (a limit in a toolbar: 70 × 26).
pub fn number_h<N: egui::emath::Numeric>(ui: &mut Ui, value: &mut N, range: std::ops::RangeInclusive<N>, w: f32, h: f32) -> Response {
    let mut v = value.to_f64();
    let r = number_dyn(ui, &mut v, range.start().to_f64()..=range.end().to_f64(), N::INTEGRAL, "", w, h, false);
    if r.changed() {
        *value = N::from_f64(v);
    }
    r
}

/// A port: [`number`] 80 points wide, its digits not grouped.
pub fn port(ui: &mut Ui, value: &mut u16) -> Response {
    let mut v = f64::from(*value);
    let r = number_dyn(ui, &mut v, 1.0..=65535.0, true, "", 80.0, FIELD_H, false);
    if r.changed() {
        *value = v.round().clamp(1.0, 65535.0) as u16;
    }
    r
}

// Not generic: compiled once for every number type.
#[allow(clippy::too_many_arguments)]
fn number_dyn(ui: &mut Ui, v: &mut f64, range: std::ops::RangeInclusive<f64>, integral: bool, unit: &str, min_w: f32, h: f32, group: bool) -> Response {
    let p = ternarygrid_ui::pal(ui);
    let shown = if group { grouped_number(*v, integral) } else { format!("{v}") };
    let font = FontId::proportional(13.0);
    // Tabular figures: every digit in a cell of the same width.
    let digit_w = 13.0 * 0.63;
    let glyphs: Vec<_> = shown.chars().map(|c| ui.painter().layout_no_wrap(c.to_string(), font.clone(), p.text)).collect();
    let cell = |c: char, g: &std::sync::Arc<egui::Galley>| if c.is_ascii_digit() { digit_w } else { g.size().x };
    let text_w: f32 = shown.chars().zip(&glyphs).map(|(c, g)| cell(c, g)).sum();
    let width = (text_w + 22.0).max(min_w).ceil();
    // The value is drawn by hand (left-aligned, grouped) while the field is not edited.
    let id = ui.next_auto_id();
    let editing = ui.memory(|m| m.has_focus(id));
    let saved = (ui.spacing().interact_size, ui.spacing().button_padding, ui.visuals().override_text_color);
    ui.spacing_mut().interact_size = vec2(width, h);
    ui.spacing_mut().button_padding = vec2(10.0, ((h - 16.0) / 2.0).max(2.0));
    if !editing {
        ui.visuals_mut().override_text_color = Some(Color32::TRANSPARENT);
    }
    let mut drag = egui::DragValue::new(v).range(range);
    if integral {
        drag = drag.max_decimals(0);
    }
    let r = ui.add(drag);
    ui.spacing_mut().interact_size = saved.0;
    ui.spacing_mut().button_padding = saved.1;
    ui.visuals_mut().override_text_color = saved.2;
    if !editing && ui.is_rect_visible(r.rect) {
        let mut x = r.rect.left() + 11.0;
        for (c, g) in shown.chars().zip(glyphs) {
            let w = cell(c, &g);
            ui.painter().galley(egui::pos2(x + (w - g.size().x) / 2.0, r.rect.center().y - g.size().y / 2.0), g, fade(ui, p.text));
            x += w;
        }
    }
    let unit = unit.trim();
    if !unit.is_empty() {
        let gap = 6.0 - ui.spacing().item_spacing.x;
        ui.add_space(gap);
        ui.label(RichText::new(unit).size(12.5).color(p.text_muted));
    }
    r
}

/// `v` as the settings show it: whole numbers with thousands separators ("100,000").
pub fn grouped_number(v: f64, integral: bool) -> String {
    if !integral && v.fract() != 0.0 {
        return format!("{}", (v * 100.0).round() / 100.0);
    }
    let digits = format!("{}", v.round().abs() as u64);
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if v < 0.0 { format!("-{out}") } else { out }
}

/// A single-line text field 30 points high.
pub fn text_field(ui: &mut Ui, value: &mut String, width: f32, hint: &str, mono: bool) -> Response {
    let p = ternarygrid_ui::pal(ui);
    let mut edit = egui::TextEdit::singleline(value)
        .desired_width(width)
        .margin(FIELD_MARGIN)
        .min_size(vec2(0.0, FIELD_H))
        .vertical_align(egui::Align::Center)
        .hint_text(RichText::new(hint).color(p.text_faint));
    if mono {
        edit = edit.font(FontId::monospace(12.5));
    }
    ui.add(edit)
}

/// A single-line text field `h` high (26 in toolbars and option rows).
pub fn text_field_h(ui: &mut Ui, value: &mut String, width: f32, h: f32, hint: &str) -> Response {
    let p = ternarygrid_ui::pal(ui);
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(width)
            .margin(FIELD_MARGIN)
            .min_size(vec2(0.0, h))
            .vertical_align(egui::Align::Center)
            .hint_text(RichText::new(hint).color(p.text_faint)),
    )
}

/// A password field 30 points high.
pub fn password_field(ui: &mut Ui, value: &mut String, width: f32, hint: &str) -> Response {
    let p = ternarygrid_ui::pal(ui);
    ui.add(
        egui::TextEdit::singleline(value)
            .password(true)
            .desired_width(width)
            .margin(FIELD_MARGIN)
            .min_size(vec2(0.0, FIELD_H))
            .vertical_align(egui::Align::Center)
            .hint_text(RichText::new(hint).color(p.text_faint)),
    )
}

// ---------------------------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------------------------

/// Width of a [`table`] column.
#[derive(Clone, Copy)]
pub enum Width {
    /// Fixed.
    Exact(f32),
    /// Resizable, starting at this width.
    Initial(f32),
    /// What is left (the last column).
    Rest(f32),
}

/// A column of a [`table`]: its header, width and whether its cells are right-aligned.
#[derive(Clone, Copy)]
pub struct Col<'a> {
    pub title: &'a str,
    pub width: Width,
    pub right: bool,
}

impl<'a> Col<'a> {
    pub fn new(title: &'a str, width: Width) -> Self {
        Col { title, width, right: false }
    }

    pub fn right(mut self) -> Self {
        self.right = true;
        self
    }
}

/// A bordered, rounded table as in the mockups: a header on the header colour with muted
/// semibold names, striped rows of [`ROW_H`] with hairlines between them. `body` draws a
/// row's cells with [`cell`] (and may use the row's response for clicks and menus).
pub fn table(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, cols: &[Col], rows: usize, mut body: impl FnMut(&mut TableRow<'_, '_>, usize)) {
    table_dyn(ui, egui::Id::new(id), cols, rows, ROW_H, &mut body);
}

/// [`table`] with rows `row_h` high (their hairline included).
pub fn table_h(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, cols: &[Col], rows: usize, row_h: f32, mut body: impl FnMut(&mut TableRow<'_, '_>, usize)) {
    table_dyn(ui, egui::Id::new(id), cols, rows, row_h, &mut body);
}

// The table code is compiled once, not once per caller.
fn table_dyn(ui: &mut Ui, id: egui::Id, cols: &[Col], rows: usize, row_h: f32, body: &mut dyn FnMut(&mut TableRow<'_, '_>, usize)) {
    let pal = ternarygrid_ui::pal(ui);
    // The design stripes the second, fourth… row; egui the first, third…: the frame shows the
    // stripe through the rows egui leaves bare, and egui's stripes are the surface.
    ternarygrid_ui::parts::bordered(ui).fill(pal.stripe).show(ui, |ui| {
        ui.scope(|ui| {
            // Cells edge to edge: the stripes, the hairlines and the header have no gaps
            // (egui also puts the vertical spacing between rows).
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.visuals_mut().faint_bg_color = pal.surface;
            ui.visuals_mut().selection.bg_fill = pal.sel;
            // No column lines at rest: they show while the pointer is on a column's edge.
            ui.visuals_mut().widgets.noninteractive.bg_stroke = Stroke::NONE;
            let mut tb = TableBuilder::new(ui)
                .id_salt(id)
                .striped(true)
                .resizable(true)
                .sense(Sense::click())
                .auto_shrink([false, true])
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            for c in cols {
                tb = tb.column(match c.width {
                    Width::Exact(w) => Column::exact(w),
                    Width::Initial(w) => Column::initial(w).at_least(40.0).clip(true).resizable(true),
                    Width::Rest(min) => Column::remainder().at_least(min).clip(true),
                });
            }
            tb.header(ROW_H + 2.0, |mut header| {
                for c in cols {
                    header.col(|ui| {
                        let r = ui.max_rect();
                        ui.painter().rect_filled(r, 0.0, pal.header);
                        ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, pal.border));
                        let text = RichText::new(c.title).font(style::semibold(ui.ctx(), 12.0)).color(pal.text_muted);
                        if c.right {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_space(10.0);
                                ui.add(egui::Label::new(text).selectable(false).truncate());
                            });
                        } else {
                            ui.add_space(10.0);
                            ui.add(egui::Label::new(text).selectable(false).truncate());
                        }
                    });
                }
            })
            .body(|b| {
                b.rows(row_h, rows, |mut row| {
                    let i = row.index();
                    body(&mut row, i);
                });
            });
        });
    });
}

/// A cell of a [`table`] row: 10 points of padding, the hairline under the row, then `add`
/// (right-aligned with `right`).
pub fn cell(row: &mut TableRow<'_, '_>, right: bool, add: impl FnOnce(&mut Ui)) {
    row.col(|ui| {
        let p = ternarygrid_ui::pal(ui);
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, p.line));
        ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
        // Selected rows keep their text colours (only the fill shows the selection).
        ui.visuals_mut().override_text_color = None;
        if right {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(10.0);
                add(ui);
            });
        } else {
            ui.add_space(10.0);
            add(ui);
        }
    });
}

/// Text of a cell, cut with "…" at its edge.
pub fn cell_text(ui: &mut Ui, text: impl Into<egui::WidgetText>) -> Response {
    ui.add(egui::Label::new(text).truncate().selectable(false))
}

/// Monospace text in `color` (times, SQL, shortcuts).
pub fn mono(ui: &Ui, text: &str, color: Color32) -> RichText {
    let size = (style::body_size(ui.ctx()) - 1.0).max(9.0);
    RichText::new(text).font(FontId::monospace(size)).color(color)
}

// ---------------------------------------------------------------------------------------------
// Toggles
// ---------------------------------------------------------------------------------------------

/// A day of the week that can be switched on (36 × 26, the soft accent when on).
pub fn day_toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let p = ternarygrid_ui::pal(ui);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(36.0, 26.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, *on, label));
    if ui.is_rect_visible(rect) {
        let (fill, color) = if *on { (p.accent_soft, p.accent_text) } else { (if resp.hovered() { p.hover } else { p.subtle }, p.text_muted) };
        ui.painter().rect_filled(rect, CornerRadius::same(RADIUS), fill);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.0), color);
        if resp.has_focus() {
            ui.painter().rect_stroke(rect, CornerRadius::same(RADIUS), Stroke::new(1.5, p.accent), StrokeKind::Inside);
        }
    }
    resp
}

/// [`ternarygrid_ui::parts::callout`] as tall as its text, also where the space below is
/// unbounded (a panel).
pub fn callout(ui: &mut Ui, tone: ternarygrid_ui::parts::Tone, title: Option<&str>, body: &str, add: impl FnOnce(&mut Ui)) {
    let w = ui.available_width();
    ui.allocate_ui_with_layout(vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
        ternarygrid_ui::parts::callout(ui, tone, title, body, add);
    });
}

/// An error [`callout`].
pub fn error(ui: &mut Ui, text: &str) {
    callout(ui, ternarygrid_ui::parts::Tone::Error, None, text, |_| {});
}

/// A small muted heading (a panel's title: "Folders", "Messages").
pub fn small_heading(ui: &mut Ui, text: &str) -> Response {
    let p = ternarygrid_ui::pal(ui);
    ternarygrid_ui::kit::semibold_label(ui, text, 12.0, p.text_muted)
}

// ---------------------------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------------------------

/// A modal [`ternarygrid_ui::parts::Dialog`] that only closes by its × (the first value) or
/// the caller's buttons: a click beside it or Escape do not throw a form away.
pub fn modal_keep<R>(ctx: &egui::Context, id: egui::Id, dialog: &ternarygrid_ui::parts::Dialog, body: impl FnOnce(&mut Ui) -> R, footer: &mut dyn FnMut(&mut Ui)) -> (bool, R) {
    let dark = ctx.global_style().visuals.dark_mode;
    let p = ternarygrid_ui::palette(dark);
    let frame = egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(style::RADIUS_LG + 2))
        .shadow(ctx.global_style().visuals.window_shadow)
        .inner_margin(egui::Margin::ZERO);
    let backdrop = if dark { Color32::from_black_alpha(140) } else { Color32::from_rgba_unmultiplied(30, 30, 45, 70) };
    egui::Modal::new(id).frame(frame).backdrop_color(backdrop).show(ctx, |ui| dialog.contents(ui, body, Some(footer))).inner
}
