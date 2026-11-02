//! Shared components drawn with the design tokens of `style.rs` (ported from
//! TernaryShell, so the Ternary apps look and behave the same): buttons, icon buttons, button
//! groups, tab strips, pills, keyboard hints, empty states, cards and search fields. Screens use
//! these instead of styling egui widgets one by one, so every view looks and behaves the same.

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType, vec2};

use crate::style::{self, RADIUS};

/// Icon followed by a label, for menus and ordinary buttons.
pub fn with_icon(icon: &str, text: &str) -> String {
    format!("{icon}  {text}")
}

// ---------------------------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Filled accent: the one main action of a view.
    Primary,
    /// Outlined surface: ordinary actions.
    Secondary,
    /// No frame until hovered: toolbars and quiet actions.
    Ghost,
    /// Outlined, red text: destructive actions.
    Danger,
    /// Filled red, white text: the confirming button of a destructive dialog.
    DangerFilled,
    /// The accent's soft tint with accent text: a mode that is on (Manual commit, Fix error).
    Soft,
}

/// Height of a button: [`style::CONTROL_H`] (30), or [`style::SMALL_H`] (26) when `small`.
fn button_metrics(ui: &Ui, small: bool) -> (f32, f32, f32) {
    // (font size, height, horizontal padding) as in the design: 13 / 30 / 12, small 12.5 / 26 / 10.
    let body = style::body_size(ui.ctx());
    if small { (body - 0.5, style::SMALL_H, 10.0) } else { (body, style::CONTROL_H, 12.0) }
}

/// A button with an optional leading icon. `small` makes it 26 points high instead of 30 (the
/// height of form controls, so it lines up with the text fields of a dialog).
pub fn button_ex(ui: &mut Ui, kind: Kind, icon: Option<&str>, text: &str, small: bool) -> Response {
    button_impl(ui, kind, icon, text, None, small, false)
}

/// [`button_ex`] that can be shown "on" (an accent-tinted toggle, e.g. Root or SFTP).
pub fn button_full(ui: &mut Ui, kind: Kind, icon: Option<&str>, text: &str, small: bool, on: bool) -> Response {
    button_impl(ui, kind, icon, text, None, small, on)
}

/// [`button_ex`] with its shortcut after the text, a little smaller and fainter ("Save ⌘S").
pub fn button_kbd(ui: &mut Ui, kind: Kind, icon: Option<&str>, text: &str, keys: &str, small: bool) -> Response {
    button_impl(ui, kind, icon, text, Some(keys).filter(|k| !k.is_empty()), small, false)
}

fn button_impl(ui: &mut Ui, kind: Kind, icon: Option<&str>, text: &str, keys: Option<&str>, small: bool, on: bool) -> Response {
    let p = style::pal(ui);
    let (size, height, pad) = button_metrics(ui, small);
    let enabled = ui.is_enabled();
    let text_color = match kind {
        Kind::Primary | Kind::DangerFilled => p.on_accent,
        Kind::Danger => p.err,
        Kind::Soft => p.accent_text,
        _ if on => p.accent_text,
        Kind::Secondary | Kind::Ghost => p.text,
    };
    let galley = (!text.is_empty()).then(|| ui.painter().layout_no_wrap(text.to_string(), style::medium(ui.ctx(), size), Color32::PLACEHOLDER));
    // A lone icon is muted until hovered (an icon button); beside a text it takes the text's colour.
    let icon_color = if galley.is_none() && matches!(kind, Kind::Secondary | Kind::Ghost) && !on { p.text_muted } else { text_color };
    let icon_galley = icon.map(|i| ui.painter().layout_no_wrap(i.to_string(), style::icon_font(size + 1.0), Color32::PLACEHOLDER));
    let keys_galley = keys.map(|k| ui.painter().layout_no_wrap(k.to_string(), FontId::proportional(size - 2.0), Color32::PLACEHOLDER));
    let gap = 6.0;
    let parts = [galley.as_ref().map(|g| g.size().x), icon_galley.as_ref().map(|g| g.size().x), keys_galley.as_ref().map(|g| g.size().x)];
    let content_w = parts.iter().flatten().sum::<f32>() + gap * (parts.iter().flatten().count().saturating_sub(1)) as f32;
    let width = if galley.is_none() { height } else { content_w + pad * 2.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(width.max(height), height), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, on, text));
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let pressed = enabled && resp.is_pointer_button_down_on();
        let quiet = |rest: Color32| {
            if pressed {
                p.subtle
            } else if hovered {
                p.hover
            } else {
                rest
            }
        };
        let (fill, stroke) = match kind {
            Kind::Primary => (if hovered { p.accent_hover } else { p.accent }, Stroke::NONE),
            Kind::DangerFilled => {
                let red = if ui.visuals().dark_mode { Color32::from_rgb(0xc4, 0x35, 0x2b) } else { p.err };
                (if hovered { style::soft(red, 0.85) } else { red }, Stroke::NONE)
            }
            Kind::Soft => (if hovered { style::soft(p.accent, 0.30) } else { p.accent_soft }, Stroke::NONE),
            Kind::Secondary | Kind::Danger if on => (p.accent_soft, Stroke::new(1.0, style::soft(p.accent, 0.45))),
            Kind::Secondary | Kind::Danger => (quiet(p.surface), Stroke::new(1.0, p.border_strong)),
            Kind::Ghost => (if on { p.accent_soft } else { quiet(Color32::TRANSPARENT) }, Stroke::NONE),
        };
        let painter = ui.painter();
        let radius = CornerRadius::same(RADIUS);
        painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
        if resp.has_focus() {
            painter.rect_stroke(rect.expand(2.0), CornerRadius::same(RADIUS + 2), Stroke::new(1.5, p.accent), StrokeKind::Outside);
        }
        let mut x = rect.center().x - content_w / 2.0;
        // Disabled, as in the design: the fill stays, the text and the icon turn faint.
        let fade = |c: Color32| if enabled { c } else { p.text_faint };
        if let Some(g) = icon_galley {
            let pos = egui::pos2(x, rect.center().y - g.size().y / 2.0);
            x += g.size().x + gap;
            painter.galley(pos, g, fade(if hovered && galley.is_none() { text_color } else { icon_color }));
        }
        if let Some(g) = galley {
            let pos = egui::pos2(x, rect.center().y - g.size().y / 2.0);
            x += g.size().x + gap;
            painter.galley(pos, g, fade(text_color));
        }
        if let Some(g) = keys_galley {
            let pos = egui::pos2(x, rect.center().y - g.size().y / 2.0);
            painter.galley(pos, g, fade(text_color).gamma_multiply(0.75));
        }
    }
    resp
}

/// The width [`button_ex`] gives a button, so a row can leave room for it before laying out
/// the widgets that share what is left.
pub fn button_width(ui: &Ui, icon: Option<&str>, text: &str, small: bool) -> f32 {
    let (size, height, pad) = button_metrics(ui, small);
    if text.is_empty() {
        return height;
    }
    let text_w = ui.painter().layout_no_wrap(text.to_string(), style::medium(ui.ctx(), size), Color32::PLACEHOLDER).size().x;
    let icon_w = icon.map_or(0.0, |i| ui.painter().layout_no_wrap(i.to_string(), style::icon_font(size + 1.0), Color32::PLACEHOLDER).size().x + 6.0);
    (text_w + icon_w + pad * 2.0).max(height)
}

pub fn primary(ui: &mut Ui, text: &str) -> Response {
    button_ex(ui, Kind::Primary, None, text, false)
}

pub fn primary_icon(ui: &mut Ui, icon: &str, text: &str) -> Response {
    button_ex(ui, Kind::Primary, Some(icon), text, false)
}

pub fn secondary(ui: &mut Ui, icon: Option<&str>, text: &str) -> Response {
    button_ex(ui, Kind::Secondary, icon, text, false)
}

pub fn ghost(ui: &mut Ui, icon: Option<&str>, text: &str) -> Response {
    button_ex(ui, Kind::Ghost, icon, text, true)
}

pub fn danger(ui: &mut Ui, text: &str) -> Response {
    button_ex(ui, Kind::Danger, None, text, false)
}

/// A square, frameless icon button with a tooltip (also its accessible name).
pub fn icon_button(ui: &mut Ui, icon: &str, tooltip: &str) -> Response {
    icon_button_ex(ui, icon, tooltip, false, 28.0)
}

/// An icon button showing an on/off state (sidebar toggle, split, ...).
pub fn icon_toggle(ui: &mut Ui, icon: &str, tooltip: &str, on: bool) -> Response {
    icon_button_ex(ui, icon, tooltip, on, 28.0)
}

/// [`icon_button`] `side` points square (the design's are 20, 26, 28, 30 and 34) with an icon
/// that suits it.
pub fn icon_button_ex(ui: &mut Ui, icon: &str, tooltip: &str, on: bool, side: f32) -> Response {
    icon_button_sized(ui, icon, tooltip, on, side, (side * 0.54).round().max(11.0))
}

/// [`icon_button_ex`] with the icon at `glyph` points.
pub fn icon_button_sized(ui: &mut Ui, icon: &str, tooltip: &str, on: bool, side: f32, glyph: f32) -> Response {
    let p = style::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let resp = if tooltip.is_empty() { resp } else { resp.on_hover_text(tooltip) };
    let enabled = ui.is_enabled();
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, on, tooltip));
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let painter = ui.painter();
        let radius = CornerRadius::same(RADIUS);
        if on {
            painter.rect_filled(rect, radius, p.accent_soft);
        } else if enabled && resp.is_pointer_button_down_on() {
            painter.rect_filled(rect, radius, p.subtle);
        } else if hovered {
            painter.rect_filled(rect, radius, p.hover);
        }
        if resp.has_focus() {
            painter.rect_stroke(rect, radius, Stroke::new(1.5, p.accent), StrokeKind::Inside);
        }
        let color = if on {
            p.accent_text
        } else if hovered {
            p.text
        } else {
            p.text_muted
        };
        let color = if enabled { color } else { color.gamma_multiply(0.45) };
        painter.text(rect.center(), Align2::CENTER_CENTER, icon, style::icon_font(glyph), color);
    }
    resp
}

/// One segment of a [`button_group`]: an icon, its tooltip and whether it is "on".
pub struct Segment<'a> {
    pub icon: &'a str,
    pub tooltip: &'a str,
    pub on: bool,
}

/// A row of icon toggles on a soft track (the result's views: Grid, Text, Record, JSON): each
/// 30 × 24 on a track padded by 2 points, the one that is on raised on the surface.
pub fn button_group(ui: &mut Ui, segments: &[Segment]) -> Vec<Response> {
    let p = style::pal(ui);
    let (h, w, gap) = (24.0, 30.0, 2.0);
    let n = segments.len() as f32;
    let total = vec2(w * n + gap * (n - 1.0).max(0.0) + 4.0, h + 4.0);
    let (rect, base) = ui.allocate_exact_size(total, Sense::hover());
    let enabled = ui.is_enabled();
    ui.painter().rect_filled(rect, CornerRadius::same(7), p.subtle);
    let mut out = Vec::with_capacity(segments.len());
    for (i, seg) in segments.iter().enumerate() {
        let r = Rect::from_min_size(egui::pos2(rect.left() + 2.0 + (w + gap) * i as f32, rect.top() + 2.0), vec2(w, h));
        let resp = ui.interact(r, base.id.with(("segment", i)), Sense::click()).on_hover_text(seg.tooltip);
        resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, seg.on, seg.tooltip));
        let hovered = enabled && resp.hovered();
        let painter = ui.painter();
        let radius = CornerRadius::same(5);
        if seg.on {
            raised(painter, r, radius, p.surface);
        } else if hovered {
            painter.rect_filled(r, radius, p.hover);
        }
        let color = if seg.on || hovered { p.text } else { p.text_muted };
        let color = if enabled { color } else { color.gamma_multiply(0.45) };
        painter.text(r.center(), Align2::CENTER_CENTER, seg.icon, style::icon_font(15.0), color);
        if resp.has_focus() {
            painter.rect_stroke(r, radius, Stroke::new(1.5, p.accent), StrokeKind::Inside);
        }
        out.push(resp);
    }
    out
}

/// A raised fill (the item that is on in a segmented control): `fill` over a soft shadow a
/// point below it.
pub fn raised(painter: &egui::Painter, r: Rect, radius: CornerRadius, fill: Color32) {
    let shadow = egui::epaint::Shadow { offset: [0, 1], blur: 2, spread: 0, color: Color32::from_black_alpha(31) };
    painter.add(shadow.as_shape(r, radius));
    painter.rect_filled(r, radius, fill);
}

// ---------------------------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------------------------

/// A row of tabs with an underline under the selected one and a hairline under the whole
/// row, as in the design: 34 points high, 18 apart, the selected one in the medium weight.
/// Returns each tab's response.
pub fn tab_strip<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, items: &[(T, &str)]) -> Vec<Response> {
    let labels: Vec<&str> = items.iter().map(|(_, l)| *l).collect();
    let sel = items.iter().position(|(v, _)| v == current);
    let out = tab_strip_idx(ui, sel, &labels);
    if let Some(i) = out.iter().position(|r| r.clicked()) {
        *current = items[i].0;
    }
    out
}

fn tab_strip_idx(ui: &mut Ui, sel: Option<usize>, labels: &[&str]) -> Vec<Response> {
    let p = style::pal(ui);
    let start_x = ui.cursor().min.x;
    let right = ui.max_rect().right();
    let mut out = Vec::with_capacity(labels.len());
    let bottom = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            for (i, label) in labels.iter().enumerate() {
                out.push(underline_tab(ui, None, label, None, sel == Some(i), 34.0));
            }
        })
        .response
        .rect
        .bottom();
    ui.painter().hline(start_x..=right, bottom - 0.5, Stroke::new(1.0, p.border));
    out
}

/// A check box as in the design: 16 px, radius 4, filled with the accent and a white check
/// when on. `label` is plain or rich text. Use instead of `ui.checkbox`.
pub fn checkbox(ui: &mut Ui, on: &mut bool, label: impl Into<egui::WidgetText>) -> Response {
    let p = style::pal(ui);
    let enabled = ui.is_enabled();
    let text: egui::WidgetText = label.into();
    let label_text = text.text().to_string();
    let galley = (!label_text.is_empty()).then(|| text.into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body));
    let w = 16.0 + galley.as_ref().map_or(0.0, |g| 10.0 + g.size().x);
    let h = galley.as_ref().map_or(17.0, |g| g.size().y.max(17.0));
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *on, &label_text));
    if ui.is_rect_visible(rect) {
        let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
        let b = Rect::from_center_size(egui::pos2(rect.left() + 8.0, rect.center().y), vec2(16.0, 16.0));
        let hovered = enabled && resp.hovered();
        let painter = ui.painter();
        if *on {
            painter.rect_filled(b, CornerRadius::same(4), fade(if hovered { p.accent_hover } else { p.accent }));
            // The design's check: an 11-point mark (2.8 on 24) centred in the box.
            let pts = vec![egui::pos2(b.left() + 5.0, b.top() + 8.2), egui::pos2(b.left() + 6.9, b.top() + 10.1), egui::pos2(b.left() + 11.0, b.top() + 6.2)];
            painter.add(egui::Shape::line(pts, Stroke::new(1.3, fade(p.on_accent))));
        } else {
            painter.rect(b, CornerRadius::same(4), p.surface, Stroke::new(1.5, fade(if hovered { p.accent } else { p.border_strong })), StrokeKind::Inside);
        }
        if resp.has_focus() {
            style::focus_halo(ui, b, 4);
        }
        if let Some(g) = galley {
            ui.painter().galley(egui::pos2(b.right() + 10.0, rect.center().y - g.size().y / 2.0), g, fade(p.text));
        }
    }
    resp
}

/// A radio button like the design's: an accent ring with a dot when selected.
pub fn radio(ui: &mut Ui, selected: bool, label: impl Into<egui::WidgetText>) -> Response {
    let p = style::pal(ui);
    let enabled = ui.is_enabled();
    let text: egui::WidgetText = label.into();
    let label_text = text.text().to_string();
    let galley = (!label_text.is_empty()).then(|| text.into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body));
    let w = 16.0 + galley.as_ref().map_or(0.0, |g| 8.0 + g.size().x);
    let h = galley.as_ref().map_or(17.0, |g| g.size().y.max(17.0));
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, enabled, selected, &label_text));
    if ui.is_rect_visible(rect) {
        let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
        let c = egui::pos2(rect.left() + 8.0, rect.center().y);
        let ring = if selected || (enabled && resp.hovered()) { p.accent } else { p.border_strong };
        ui.painter().circle(c, 7.25, p.surface, Stroke::new(1.5, fade(ring)));
        if selected {
            ui.painter().circle_filled(c, 4.0, fade(p.accent));
        }
        if resp.has_focus() {
            style::focus_halo(ui, Rect::from_center_size(c, vec2(16.0, 16.0)), 8);
        }
        if let Some(g) = galley {
            ui.painter().galley(egui::pos2(rect.left() + 24.0, rect.center().y - g.size().y / 2.0), g, fade(p.text));
        }
    }
    resp
}

/// [`radio`] over a value, like `Ui::radio_value`: a click selects `value` (the response is
/// marked changed).
pub fn radio_value<T: PartialEq>(ui: &mut Ui, current: &mut T, value: T, label: impl Into<egui::WidgetText>) -> Response {
    let mut resp = radio(ui, *current == value, label);
    if resp.clicked() && *current != value {
        *current = value;
        resp.mark_changed();
    }
    resp
}

/// A full-width row of a side navigation (settings sections, panels), 32 points high: its icon and name, the
/// selected one on a soft accent.
pub fn nav_item(ui: &mut Ui, icon: &str, text: &str, selected: bool) -> Response {
    let p = style::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, text));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, CornerRadius::same(RADIUS), p.accent_soft);
        } else if resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(RADIUS), p.hover);
        }
        if resp.has_focus() {
            painter.rect_stroke(rect, CornerRadius::same(RADIUS), Stroke::new(1.5, p.accent), StrokeKind::Inside);
        }
        let size = style::body_size(ui.ctx());
        let cy = rect.center().y;
        painter.text(egui::pos2(rect.left() + 18.0, cy), Align2::CENTER_CENTER, icon, style::icon_font(size + 2.0), if selected { p.accent_text } else { p.text_muted });
        let font = if selected { style::medium(ui.ctx(), size) } else { FontId::proportional(size) };
        painter.text(egui::pos2(rect.left() + 36.0, cy), Align2::LEFT_CENTER, text, font, p.text);
    }
    resp
}

/// One tab of a row of underlined tabs: an optional icon, its text and an optional count, the
/// selected one underlined in the accent (the whole tab's width, 2 points). Lay several out
/// in a horizontal `ui` (the gap and the row's hairline are the caller's: the design's object
/// pages are 18–20 apart, the result tabs 6), so each tab keeps its own response for menus and
/// middle clicks. A tab without an icon is padded by 2 points and its selected text is in the
/// medium weight; a tab with an icon is padded by 6 and its icon is in the accent when selected.
pub fn underline_tab(ui: &mut Ui, icon: Option<&str>, text: &str, badge: Option<&str>, selected: bool, height: f32) -> Response {
    let p = style::pal(ui);
    let size = style::body_size(ui.ctx());
    let font = if selected && icon.is_none() { style::medium(ui.ctx(), size) } else { FontId::proportional(size) };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER);
    let icon_galley = icon.map(|i| ui.painter().layout_no_wrap(i.to_string(), style::icon_font(size + 1.0), Color32::PLACEHOLDER));
    let badge_galley = badge.map(|b| ui.painter().layout_no_wrap(b.to_string(), style::medium(ui.ctx(), style::SMALL), Color32::PLACEHOLDER));
    let (pad, gap) = (if icon.is_some() { 6.0 } else { 2.0 }, 6.0);
    let badge_w = |g: &std::sync::Arc<egui::Galley>| (g.size().x + 10.0).max(16.0);
    let width = pad * 2.0 + galley.size().x + icon_galley.as_ref().map_or(0.0, |g| g.size().x + gap) + badge_galley.as_ref().map_or(0.0, |g| badge_w(g) + gap);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, text));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let hovered = resp.hovered();
        let mut x = rect.left() + pad;
        if let Some(g) = icon_galley {
            let color = if selected { p.accent_text } else { p.text_faint };
            let w = g.size().x;
            painter.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, color);
            x += w + gap;
        }
        let color = if selected || hovered { p.text } else { p.text_muted };
        let w = galley.size().x;
        painter.galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, color);
        x += w + gap;
        if let Some(g) = badge_galley {
            let r = Rect::from_min_size(egui::pos2(x, rect.center().y - 8.0), vec2(badge_w(&g), 16.0));
            let (fill, color) = if selected { (p.accent_soft, p.accent_text) } else { (p.subtle, p.text_muted) };
            painter.rect_filled(r, CornerRadius::same(8), fill);
            painter.galley(r.center() - g.size() / 2.0, g, color);
        }
        if selected {
            let line = Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 2.0), rect.right_bottom());
            painter.rect_filled(line, CornerRadius::ZERO, p.accent);
        } else if hovered {
            let line = Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 2.0), rect.right_bottom());
            painter.rect_filled(line, CornerRadius::ZERO, p.border_strong);
        }
        if resp.has_focus() {
            painter.rect_stroke(rect.shrink(2.0), CornerRadius::same(RADIUS), Stroke::new(1.5, p.accent), StrokeKind::Inside);
        }
    }
    resp
}

/// Covers `rect` with `color`, from clear to opaque towards the right (`to_right`) or the
/// left: the edge of a strip that hides more.
pub fn fade(painter: &egui::Painter, rect: Rect, color: Color32, to_right: bool) {
    let (left, right) = if to_right { (Color32::TRANSPARENT, color) } else { (color, Color32::TRANSPARENT) };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), left);
    mesh.colored_vertex(rect.right_top(), right);
    mesh.colored_vertex(rect.right_bottom(), right);
    mesh.colored_vertex(rect.left_bottom(), left);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

// ---------------------------------------------------------------------------------------------
// Small pieces
// ---------------------------------------------------------------------------------------------

/// A rounded label on a soft fill of `color` (states, counts): 18 points high, 11 points
/// medium text.
pub fn pill(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let dark = ui.visuals().dark_mode;
    pill_colors(ui, text, color, style::soft(color, if dark { 0.18 } else { 0.11 }))
}

/// A [`pill`] in `fg` on `bg` (the design's chips: muted on subtle; PRODUCTION: err on prod_soft).
pub fn pill_colors(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), style::medium(ui.ctx(), style::SMALL), fg);
    let size = vec2(galley.size().x + 14.0, 18.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same(9), bg);
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
    }
    resp
}

/// A neutral chip (muted text on the subtle fill): tags, codes, counts.
pub fn chip(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    pill_colors(ui, text, p.text_muted, p.subtle)
}

/// A keyboard shortcut hint (`⌘K`) drawn as a key cap: 18 points high, 11 points text.
pub fn kbd(ui: &mut Ui, keys: &str) -> Response {
    let p = style::pal(ui);
    let galley = ui.painter().layout_no_wrap(keys.to_string(), style::medium(ui.ctx(), style::SMALL), p.text_muted);
    let size = vec2(galley.size().x + 10.0, 18.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_kbd(ui.painter(), rect, &p);
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, p.text_muted);
    }
    resp
}

/// [`kbd`] painted at the right end of `right_edge` (centred on `y`), for rows drawn by hand;
/// returns its rectangle.
pub fn paint_kbd_text(painter: &egui::Painter, ctx: &egui::Context, keys: &str, right_edge: f32, y: f32, p: &style::Palette) -> Rect {
    let galley = painter.layout_no_wrap(keys.to_string(), style::medium(ctx, style::SMALL), p.text_muted);
    let r = Rect::from_min_size(egui::pos2(right_edge - galley.size().x - 10.0, y - 9.0), vec2(galley.size().x + 10.0, 18.0));
    paint_kbd(painter, r, p);
    painter.galley(r.center() - galley.size() / 2.0, galley, p.text_muted);
    r
}

/// The key cap behind a shortcut hint.
pub fn paint_kbd(painter: &egui::Painter, rect: Rect, p: &style::Palette) {
    painter.rect(rect, CornerRadius::same(4), p.surface, Stroke::new(1.0, p.border_strong), StrokeKind::Inside);
}

/// A filled dot (connection state).
pub fn dot(ui: &mut Ui, color: Color32, radius: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(radius * 2.0 + 2.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
    resp
}

/// The chevron of a drop-down, for `ComboBox::icon` (egui's default is a filled triangle that
/// doesn't match the line icons).
pub fn combo_caret(ui: &Ui, rect: Rect, visuals: &egui::style::WidgetVisuals, open: bool) {
    let c = rect.center();
    let (w, h) = (3.5, if open { -1.8 } else { 1.8 });
    let color = visuals.fg_stroke.color.gamma_multiply(0.75);
    let points = vec![egui::pos2(c.x - w, c.y - h), egui::pos2(c.x, c.y + h), egui::pos2(c.x + w, c.y - h)];
    ui.painter().add(egui::Shape::line(points, Stroke::new(1.4, color)));
}

/// A thin vertical divider between toolbar groups.
pub fn vdivider(ui: &mut Ui, height: f32) {
    let p = style::pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, height), Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0, p.border));
}

/// A title in the design's semibold (600): TernaryGrid bundles the medium weight only (binary
/// size), so the medium text is painted twice, a hair apart, which reads as the heavier weight.
pub fn semibold_label(ui: &mut Ui, text: &str, size: f32, color: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), style::medium(ui.ctx(), size), color);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(EMBOLDEN, 0.0), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    if ui.is_rect_visible(rect) {
        paint_semibold(ui.painter(), rect.min, galley, color);
    }
    resp
}

/// How far apart [`paint_semibold`] paints the two copies (points).
const EMBOLDEN: f32 = 0.35;

/// Paint `galley` (laid out in the medium weight) emboldened to the semibold weight.
pub fn paint_semibold(painter: &egui::Painter, pos: egui::Pos2, galley: std::sync::Arc<egui::Galley>, color: Color32) {
    painter.galley(pos + vec2(EMBOLDEN, 0.0), galley.clone(), color);
    painter.galley(pos, galley, color);
}

/// A section title in semibold, slightly smaller than body.
pub fn section_title(ui: &Ui, text: &str) -> RichText {
    RichText::new(text).font(style::semibold(ui.ctx(), 13.0))
}

/// An icon on its own, as a label.
pub fn icon(ui: &mut Ui, icon: &str, size: f32, color: Color32) -> Response {
    ui.add(egui::Label::new(RichText::new(icon).font(style::icon_font(size)).color(color)).selectable(false))
}

/// A bordered card: the surface, a hairline, radius 10, 16 points of padding.
pub fn card(ui: &Ui) -> egui::Frame {
    let p = style::pal(ui);
    egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(style::RADIUS_LG)).inner_margin(egui::Margin::same(16))
}

/// An icon in a soft circle of `color` (dialog headers, empty states).
pub fn icon_badge(ui: &mut Ui, icon: &str, color: Color32, diameter: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    let dark = ui.visuals().dark_mode;
    ui.painter().circle_filled(rect.center(), diameter / 2.0, style::soft(color, if dark { 0.18 } else { 0.12 }));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, style::icon_font((diameter * 0.48).round()), color);
    resp
}

/// A centered empty state as in the design: an icon in a 52-point circle, a title (15
/// points), a line of explanation (13 points, muted, at most 380 wide, 32 from the sides),
/// then whatever `actions` adds (buttons, shortcut hints). It sits a fifth of the way down
/// the space it is given (see [`empty_state_at`]).
pub fn empty_state(ui: &mut Ui, icon: &str, title: &str, body: &str, actions: impl FnOnce(&mut Ui)) {
    let top = (ui.available_height() * 0.22).clamp(12.0, 140.0);
    empty_state_at(ui, top, icon, title, body, actions);
}

/// [`empty_state`] `top` points down (the design's side panels: 32).
pub fn empty_state_at(ui: &mut Ui, top: f32, icon: &str, title: &str, body: &str, actions: impl FnOnce(&mut Ui)) {
    let mut actions = Some(actions);
    empty_state_dyn(ui, top, icon, title, body, &mut |ui| {
        if let Some(a) = actions.take() {
            a(ui)
        }
    });
}

fn empty_state_dyn(ui: &mut Ui, top: f32, icon: &str, title: &str, body: &str, actions: &mut dyn FnMut(&mut Ui)) {
    let p = style::pal(ui);
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add_space(top);
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 26.0, p.subtle);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, style::icon_font(24.0), p.text_muted);
        ui.add_space(12.0);
        semibold_label(ui, title, 15.0, p.text);
        ui.add_space(8.0);
        let w = (ui.available_width() - 64.0).clamp(120.0, 380.0);
        ui.allocate_ui(vec2(w, 0.0), |ui| {
            ui.vertical_centered(|ui| ui.add(egui::Label::new(RichText::new(body).color(p.text_muted)).wrap()));
        });
        ui.add_space(16.0);
        ui.spacing_mut().item_spacing.y = 8.0;
        actions(ui);
    });
}

/// A single-line search field with a magnifier inside and a clear button.
pub fn search_field(ui: &mut Ui, id: egui::Id, text: &mut String, hint_text: &str, width: f32) -> Response {
    field_with_icon(ui, id, text, hint_text, width, egui_phosphor::regular::MAGNIFYING_GLASS, true)
}

/// A single-line field as in the design (30 points high, the surface, a hairline, radius 6)
/// with `icon` inside on the left (and, with `clear`, a button that empties it on the right).
pub fn field_with_icon(ui: &mut Ui, id: egui::Id, text: &mut String, hint_text: &str, width: f32, icon: &str, clear: bool) -> Response {
    let p = style::pal(ui);
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let frame = egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, if hovered { p.border_strong } else { p.border }))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(egui::Margin { left: 32, right: if clear { 28 } else { 10 }, top: 0, bottom: 0 });
    let edit = egui::TextEdit::singleline(text)
        .id(id)
        .hint_text(RichText::new(hint_text).color(p.text_faint))
        .desired_width(width)
        .min_size(vec2(0.0, style::CONTROL_H))
        .vertical_align(egui::Align::Center)
        .frame(frame);
    let resp = ui.add(edit);
    let r = resp.rect;
    if resp.has_focus() {
        style::focus_halo(ui, r, RADIUS);
    }
    ui.painter().text(egui::pos2(r.left() + 17.0, r.center().y), Align2::CENTER_CENTER, icon, style::icon_font(14.0), p.text_faint);
    if clear && !text.is_empty() {
        let x_rect = Rect::from_center_size(egui::pos2(r.right() - 15.0, r.center().y), Vec2::splat(18.0));
        let x = ui.interact(x_rect, resp.id.with("clear"), Sense::click());
        let color = if x.hovered() { p.text } else { p.text_faint };
        ui.painter().text(x_rect.center(), Align2::CENTER_CENTER, egui_phosphor::regular::X_CIRCLE, style::icon_font(15.0), color);
        if x.clicked() {
            text.clear();
        }
    }
    resp
}

// ---------------------------------------------------------------------------------------------
// Trees
// ---------------------------------------------------------------------------------------------

/// What a [`tree_row`] reports.
pub struct TreeRow {
    pub resp: Response,
    /// The caret (fold / unfold) was clicked.
    pub caret: bool,
}

/// A row of a side tree as in the design (the navigator's, the projects', folders): 26 points
/// high inset by 6, indented 8 + 14 per level, a caret (`Some(open)`) or its room, a muted icon,
/// the label (medium at the top level and when selected) and a faint count or detail at the
/// right; the selected row on the soft accent. `weak`: the label faint (a disabled item).
#[allow(clippy::too_many_arguments)]
pub fn tree_row(ui: &mut Ui, depth: usize, caret: Option<bool>, glyph: &str, label: &str, right: Option<&str>, selected: bool, weak: bool) -> TreeRow {
    let p = style::pal(ui);
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
            let chevron = if open { egui_phosphor::regular::CARET_DOWN } else { egui_phosphor::regular::CARET_RIGHT };
            painter.text(egui::pos2(x + 6.0, cy), Align2::CENTER_CENTER, chevron, style::icon_font(12.0), p.text_faint);
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
        let g = egui::WidgetText::from(RichText::new(label).font(font).color(color)).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (right_x - x).max(20.0),
            egui::TextStyle::Body,
        );
        ui.painter().galley(egui::pos2(x, cy - g.size().y / 2.0), g, color);
    }
    TreeRow { resp, caret: caret_clicked }
}

/// The heading of a group of tree rows (the projects' Scripts, Bookmarks, Diagrams): 28 points,
/// a chevron, a small icon and the name in small semibold muted text; a click flips `open`
/// (the response is then marked changed).
pub fn tree_section(ui: &mut Ui, open: &mut bool, glyph: &str, title: &str) -> Response {
    let p = style::pal(ui);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::CollapsingHeader, true, *open, title));
    if resp.clicked() {
        *open = !*open;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let cy = rect.center().y;
        let color = if resp.hovered() { p.text } else { p.text_muted };
        let chevron = if *open { egui_phosphor::regular::CARET_DOWN } else { egui_phosphor::regular::CARET_RIGHT };
        painter.text(egui::pos2(rect.left() + 19.5, cy), Align2::CENTER_CENTER, chevron, style::icon_font(11.0), p.text_faint);
        painter.text(egui::pos2(rect.left() + 37.5, cy), Align2::CENTER_CENTER, glyph, style::icon_font(13.0), p.text_muted);
        let g = painter.layout_no_wrap(title.to_string(), style::medium(ui.ctx(), 12.0), color);
        paint_semibold(painter, egui::pos2(rect.left() + 50.0, cy - g.size().y / 2.0), g, color);
        if resp.has_focus() {
            painter.rect_stroke(rect.shrink2(vec2(6.0, 0.0)), CornerRadius::same(RADIUS), Stroke::new(1.5, p.accent), StrokeKind::Inside);
        }
    }
    resp
}

// ---------------------------------------------------------------------------------------------
// Menus
// ---------------------------------------------------------------------------------------------

/// The design's menu look for the `ui` of a context menu or a pop-up menu (call it first in
/// the menu's closure): items 28 points high padded by 10, the icon 10 points from the text,
/// no gap between items, hovered items on the hover fill with a radius of 5, the menu
/// `min_width` wide (shortcuts at its right edge). Items added with `ui.button` get the height and padding; for the
/// muted icon and the faint shortcut use [`menu_item`].
pub fn menu_look(ui: &mut Ui, min_width: f32) {
    let p = style::pal(ui);
    let s = ui.spacing_mut();
    s.interact_size.y = 28.0;
    s.button_padding = vec2(10.0, 0.0);
    s.item_spacing.y = 0.0;
    s.icon_spacing = 10.0;
    let w = &mut ui.visuals_mut().widgets;
    for v in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        v.corner_radius = CornerRadius::same(5);
    }
    w.hovered.weak_bg_fill = p.hover;
    w.active.weak_bg_fill = p.subtle;
    w.open.weak_bg_fill = p.hover;
    ui.set_min_width(min_width);
    ui.set_max_width(min_width);
}

/// A menu item as in the design: a muted icon (or the room of one), the text, and the
/// shortcut at the right in small faint text.
pub fn menu_item(ui: &mut Ui, icon: Option<&str>, text: &str, keys: &str) -> Response {
    menu_item_ex(ui, icon, text, keys, false)
}

/// A destructive [`menu_item`] (Drop, Delete): the icon and the text in the error colour.
pub fn menu_item_danger(ui: &mut Ui, icon: Option<&str>, text: &str, keys: &str) -> Response {
    menu_item_ex(ui, icon, text, keys, true)
}

fn menu_item_ex(ui: &mut Ui, icon: Option<&str>, text: &str, keys: &str, danger: bool) -> Response {
    let p = style::pal(ui);
    let size = style::body_size(ui.ctx());
    let (icon_color, text_color) = if danger { (p.err, p.err) } else { (p.text_muted, p.text) };
    let lead =
        RichText::new(icon.unwrap_or(egui_phosphor::regular::SQUARE)).font(style::icon_font(size + 1.0)).color(if icon.is_some() { icon_color } else { Color32::TRANSPARENT });
    let mut b = egui::Button::new((lead, RichText::new(text).color(text_color)));
    if !keys.is_empty() {
        b = b.shortcut_text(RichText::new(keys).size(size - 1.5).color(p.text_faint));
    }
    ui.add(b)
}

/// A menu item with a check mark in the icon's place when `on` (Properties panel, Link with
/// editor); a click flips `on`.
pub fn menu_check(ui: &mut Ui, on: &mut bool, text: &str) -> Response {
    let p = style::pal(ui);
    let size = style::body_size(ui.ctx());
    let lead = RichText::new(egui_phosphor::regular::CHECK).font(style::icon_font(size + 1.0)).color(if *on { p.accent_text } else { Color32::TRANSPARENT });
    let mut resp = ui.add(egui::Button::new((lead, RichText::new(text).color(p.text))));
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, *on, text));
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp
}

/// An item that opens a submenu (`width` wide): the icon, the text and a chevron at the right.
pub fn submenu(ui: &mut Ui, icon: Option<&str>, text: &str, width: f32, add: &mut dyn FnMut(&mut Ui)) -> Response {
    let p = style::pal(ui);
    let size = style::body_size(ui.ctx());
    let lead =
        RichText::new(icon.unwrap_or(egui_phosphor::regular::SQUARE)).font(style::icon_font(size + 1.0)).color(if icon.is_some() { p.text_muted } else { Color32::TRANSPARENT });
    let chevron = RichText::new(egui_phosphor::regular::CARET_RIGHT).font(style::icon_font(size - 1.0)).color(p.text_faint);
    let button = egui::Button::new((lead, RichText::new(text).color(p.text))).right_text(chevron);
    egui::containers::menu::SubMenuButton::from_button(button)
        .ui(ui, |ui| {
            menu_look(ui, width);
            add(ui)
        })
        .0
}

/// A hairline between groups of menu items (9 points with its margins, inset by 6).
pub fn menu_separator(ui: &mut Ui) {
    let p = style::pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
    ui.painter().hline(rect.x_range().shrink(6.0), rect.center().y, Stroke::new(1.0, p.border));
}

/// A small faint heading between menu items ("Cell value").
pub fn menu_caption(ui: &mut Ui, text: &str) {
    let p = style::pal(ui);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(RichText::new(text).font(style::medium(ui.ctx(), style::SMALL)).color(p.text_faint));
    });
    ui.add_space(2.0);
}

/// `ScrollArea::show_rows` for rows of exactly `row_h` laid out edge to edge. egui places the
/// rows `row_h + item_spacing.y` apart, so the spacing is zeroed around the area too:
/// otherwise the rows slip by that much each time one scrolls out of view and the bottom of
/// the list is left empty.
pub fn show_rows<R>(
    area: egui::ScrollArea,
    ui: &mut Ui,
    row_h: f32,
    total: usize,
    add: impl FnOnce(&mut Ui, std::ops::Range<usize>) -> R,
) -> egui::scroll_area::ScrollAreaOutput<R> {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        area.show_rows(ui, row_h, total, add)
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_with_icons() {
        assert_eq!(with_icon("+", "New"), "+  New");
    }
}
