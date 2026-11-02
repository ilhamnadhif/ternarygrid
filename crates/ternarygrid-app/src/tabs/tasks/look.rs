//! Pieces of the workspace screens drawn as in the redesign mockups (settings, tasks, the
//! query history, search, the projects view): check boxes and radio buttons, number fields
//! with their unit, text fields at the form height, bordered tables with styled cells, tree
//! rows and weekday toggles. They use the tokens of `ternarygrid_ui::style`.

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Sense, Stroke, StrokeKind, TextStyle, Ui, WidgetInfo, WidgetType, vec2};
use egui_extras::{Column, TableBuilder, TableRow};
use egui_phosphor::regular as icon;
use ternarygrid_ui::style::{self, RADIUS};

/// Height of a table row (the header is 2 points taller).
pub const ROW_H: f32 = 30.0;

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
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, text_h.max(18.0)), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let enabled = ui.is_enabled();
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *on, label));
    if ui.is_rect_visible(rect) {
        let b = Rect::from_min_size(egui::pos2(rect.left(), rect.top() + ((line_h - 16.0) / 2.0).max(0.0)), vec2(16.0, 16.0));
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
        let pts = vec![egui::pos2(b.left() + 4.0, b.top() + 8.3), egui::pos2(b.left() + 6.8, b.top() + 11.0), egui::pos2(b.left() + 12.0, b.top() + 5.2)];
        painter.add(egui::Shape::line(pts, Stroke::new(1.9, fade(ui, p.on_accent))));
    } else {
        let stroke = if hovered && ui.is_enabled() { p.text_muted } else { p.border_strong };
        painter.rect(b, CornerRadius::same(4), p.surface, Stroke::new(1.5, fade(ui, stroke)), StrokeKind::Inside);
    }
}

/// A radio button: a 16 point ring (with an accent dot when on) and its label. Returns the
/// response; the caller sets the value when it is clicked.
pub fn radio(ui: &mut Ui, on: bool, label: &str) -> Response {
    let p = ternarygrid_ui::pal(ui);
    let max_w = (ui.available_width() - 24.0).max(60.0);
    let (title, _) = label_galleys(ui, label, None, max_w);
    let line_h = ui.text_style_height(&TextStyle::Body);
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0 + title.size().x, title.size().y.max(18.0)), Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, enabled, on, label));
    if ui.is_rect_visible(rect) {
        let c = egui::pos2(rect.left() + 8.0, rect.top() + (line_h / 2.0).max(8.0));
        let ring = if on {
            p.accent
        } else if resp.hovered() && enabled {
            p.text_muted
        } else {
            p.border_strong
        };
        ui.painter().circle(c, 7.25, p.surface, Stroke::new(1.5, fade(ui, ring)));
        if on {
            ui.painter().circle_filled(c, 4.0, fade(ui, p.accent));
        }
        if resp.has_focus() {
            style::focus_halo(ui, Rect::from_center_size(c, vec2(16.0, 16.0)), 8);
        }
        ui.painter().galley(egui::pos2(rect.left() + 24.0, rect.top()), title, fade(ui, p.text));
    }
    resp
}

/// Radio buttons for `value` in a row (16 points apart).
pub fn radios<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, items: &[(T, &str)]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (v, l) in items {
            if radio(ui, *value == *v, l).clicked() && *value != *v {
                *value = *v;
                changed = true;
            }
        }
    });
    changed
}

/// A number field at the form height (at least 64 points wide, wider for long numbers),
/// followed by its unit in muted text.
pub fn number<N: egui::emath::Numeric>(ui: &mut Ui, value: &mut N, range: std::ops::RangeInclusive<N>, unit: &str) -> Response {
    let p = ternarygrid_ui::pal(ui);
    let digits = format!("{}", value.to_f64()).len() as f32;
    let width = (digits * 8.0 + 26.0).max(64.0);
    let r = ui.add_sized(vec2(width, style::CONTROL_H), egui::DragValue::new(value).range(range));
    let unit = unit.trim();
    if !unit.is_empty() {
        ui.label(RichText::new(unit).color(p.text_muted));
    }
    r
}

/// A single-line text field at the form height.
pub fn text_field(ui: &mut Ui, value: &mut String, width: f32, hint: &str, mono: bool) -> Response {
    let p = ternarygrid_ui::pal(ui);
    let mut edit = egui::TextEdit::singleline(value).desired_width(width).margin(egui::Margin::symmetric(10, 6)).hint_text(RichText::new(hint).color(p.text_faint));
    if mono {
        edit = edit.font(TextStyle::Monospace);
    }
    ui.add(edit)
}

/// A password field at the form height.
pub fn password_field(ui: &mut Ui, value: &mut String, width: f32, hint: &str) -> Response {
    let p = ternarygrid_ui::pal(ui);
    ui.add(egui::TextEdit::singleline(value).password(true).desired_width(width).margin(egui::Margin::symmetric(10, 6)).hint_text(RichText::new(hint).color(p.text_faint)))
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
    table_dyn(ui, egui::Id::new(id), cols, rows, &mut body);
}

// The table code is compiled once, not once per caller.
fn table_dyn(ui: &mut Ui, id: egui::Id, cols: &[Col], rows: usize, body: &mut dyn FnMut(&mut TableRow<'_, '_>, usize)) {
    let pal = ternarygrid_ui::pal(ui);
    ternarygrid_ui::parts::bordered(ui).show(ui, |ui| {
        ui.scope(|ui| {
            // Cells edge to edge: the stripes, the hairlines and the header have no gaps
            // (egui also puts the vertical spacing between rows).
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
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
                b.rows(ROW_H, rows, |mut row| {
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
        ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, ternarygrid_ui::soft(p.border, 0.75)));
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
// Trees, toggles
// ---------------------------------------------------------------------------------------------

/// What a [`tree_row`] reports.
pub struct TreeRow {
    pub resp: Response,
    /// The caret (fold / unfold) was clicked.
    pub caret: bool,
}

/// A row of a side tree (folders, scripts): 26 points high, indented by `depth`, an optional
/// caret (`Some(open)`), an icon, the label and a faint count or detail at the right; the
/// selected row on the soft accent.
#[allow(clippy::too_many_arguments)]
pub fn tree_row(ui: &mut Ui, depth: usize, caret: Option<bool>, glyph: &str, label: &str, right: Option<&str>, selected: bool, weak: bool) -> TreeRow {
    let p = ternarygrid_ui::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, label));
    let inner = rect.shrink2(vec2(6.0, 0.0));
    let mut x = inner.left() + 8.0 + depth as f32 * 14.0;
    let caret_rect = Rect::from_min_size(egui::pos2(x - 2.0, inner.top()), vec2(16.0, inner.height()));
    let caret_clicked = caret.is_some() && resp.clicked() && resp.interact_pointer_pos().is_some_and(|q| caret_rect.contains(q));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(inner, CornerRadius::same(RADIUS), p.accent_soft);
        } else if resp.hovered() {
            painter.rect_filled(inner, CornerRadius::same(RADIUS), p.hover);
        }
        if resp.has_focus() {
            painter.rect_stroke(inner, CornerRadius::same(RADIUS), Stroke::new(1.5, p.accent), StrokeKind::Inside);
        }
        let cy = inner.center().y;
        if let Some(open) = caret {
            painter.text(egui::pos2(x + 6.0, cy), Align2::CENTER_CENTER, if open { icon::CARET_DOWN } else { icon::CARET_RIGHT }, style::icon_font(11.0), p.text_faint);
        }
        x += 18.0;
        painter.text(egui::pos2(x + 7.5, cy), Align2::CENTER_CENTER, glyph, style::icon_font(15.0), if selected { p.accent_text } else { p.text_muted });
        x += 21.0;
        let size = style::body_size(ui.ctx());
        let mut right_x = inner.right() - 10.0;
        if let Some(r) = right {
            let g = painter.layout_no_wrap(r.to_string(), FontId::proportional(11.5), p.text_faint);
            right_x -= g.size().x;
            painter.galley(egui::pos2(right_x, cy - g.size().y / 2.0), g, p.text_faint);
            right_x -= 8.0;
        }
        let font = if selected || depth == 0 { style::medium(ui.ctx(), size) } else { FontId::proportional(size) };
        let color = if selected {
            p.accent_text
        } else if weak {
            p.text_faint
        } else {
            p.text
        };
        let g = egui::WidgetText::from(RichText::new(label).font(font).color(color)).into_galley(ui, Some(egui::TextWrapMode::Truncate), (right_x - x).max(20.0), TextStyle::Body);
        ui.painter().galley(egui::pos2(x, cy - g.size().y / 2.0), g, color);
    }
    TreeRow { resp, caret: caret_clicked }
}

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

/// A callout with content at its right (`right_w` wide, e.g. a button), its text wrapped
/// before it: the note of the same tone as [`callout`].
pub fn callout_with(ui: &mut Ui, tone: ternarygrid_ui::parts::Tone, body: &str, right_w: f32, add: impl FnOnce(&mut Ui)) {
    let p = ternarygrid_ui::pal(ui);
    let color = tone.color(&p);
    let fill = tone.fill(ui);
    let w = ui.available_width();
    ui.allocate_ui_with_layout(vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
        egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin { left: 12, right: 10, top: 9, bottom: 9 }).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new(tone.icon()).font(style::icon_font(16.0)).color(color));
                let text_w = (ui.available_width() - right_w - 12.0).max(80.0);
                ui.allocate_ui_with_layout(vec2(text_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(text_w);
                    ui.add(egui::Label::new(RichText::new(body).color(p.text)).wrap());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
            });
        });
    });
}

/// An error [`callout`].
pub fn error(ui: &mut Ui, text: &str) {
    callout(ui, ternarygrid_ui::parts::Tone::Error, None, text, |_| {});
}

/// A small muted heading (a panel's title: "Folders", "Messages").
pub fn small_heading(ui: &mut Ui, text: &str) -> Response {
    let p = ternarygrid_ui::pal(ui);
    ui.label(RichText::new(text).font(style::semibold(ui.ctx(), 12.0)).color(p.text_muted))
}

// ---------------------------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------------------------

/// A destructive confirmation: white text on a red fill (the main action of a "Delete …?"
/// dialog).
pub fn danger_button(ui: &mut Ui, glyph: Option<&str>, text: &str) -> Response {
    let p = ternarygrid_ui::pal(ui);
    let dark = ui.visuals().dark_mode;
    let size = style::body_size(ui.ctx());
    let label = match glyph {
        Some(g) => format!("{g}  {text}"),
        None => text.to_string(),
    };
    let g = ui.painter().layout_no_wrap(label, style::medium(ui.ctx(), size), Color32::WHITE);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 24.0, style::CONTROL_H), Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, text));
    if ui.is_rect_visible(rect) {
        let base = if dark { Color32::from_rgb(0xc4, 0x35, 0x2b) } else { p.err };
        let fill = if resp.hovered() && enabled { base.gamma_multiply(0.88) } else { base };
        ui.painter().rect_filled(rect, CornerRadius::same(RADIUS), fade(ui, fill));
        if resp.has_focus() {
            ui.painter().rect_stroke(rect.expand(2.0), CornerRadius::same(RADIUS + 2), Stroke::new(1.5, p.accent), StrokeKind::Outside);
        }
        ui.painter().galley(rect.center() - g.size() / 2.0, g, Color32::WHITE);
    }
    resp
}

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
