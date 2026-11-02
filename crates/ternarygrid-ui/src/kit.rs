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
}

/// A button with an optional leading icon. `small` makes it 24 px high instead of 28 (the
/// height of form controls, so it lines up with egui's own buttons in a dialog).
pub fn button_ex(ui: &mut Ui, kind: Kind, icon: Option<&str>, text: &str, small: bool) -> Response {
    button_full(ui, kind, icon, text, small, false)
}

/// [`button_ex`] that can be shown "on" (an accent-tinted toggle, e.g. Root or SFTP).
pub fn button_full(ui: &mut Ui, kind: Kind, icon: Option<&str>, text: &str, small: bool, on: bool) -> Response {
    let p = style::pal(ui);
    let size = if small { style::SMALL + 1.0 } else { style::body_size(ui.ctx()) };
    let height = if small { 24.0 } else { style::CONTROL_H };
    let pad = if small { 8.0 } else { 12.0 };
    let enabled = ui.is_enabled();
    let text_color = match kind {
        Kind::Primary => p.on_accent,
        Kind::Danger => p.err,
        _ if on => p.accent_text,
        Kind::Secondary | Kind::Ghost => p.text,
    };
    let galley = (!text.is_empty()).then(|| ui.painter().layout_no_wrap(text.to_string(), style::medium(ui.ctx(), size), Color32::PLACEHOLDER));
    let icon_color = if matches!(kind, Kind::Primary | Kind::Danger) || galley.is_none() || on { text_color } else { p.text_muted };
    let icon_galley = icon.map(|i| ui.painter().layout_no_wrap(i.to_string(), style::icon_font(size + 2.0), Color32::PLACEHOLDER));
    let gap = if galley.is_some() && icon_galley.is_some() { 6.0 } else { 0.0 };
    let content_w = galley.as_ref().map_or(0.0, |g| g.size().x) + icon_galley.as_ref().map_or(0.0, |g| g.size().x) + gap;
    let width = if galley.is_none() { height } else { content_w + pad * 2.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(width.max(height), height), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, on, text));
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let pressed = enabled && resp.is_pointer_button_down_on();
        let (fill, stroke) = match kind {
            Kind::Primary => (if hovered { p.accent_hover } else { p.accent }, Stroke::NONE),
            Kind::Secondary | Kind::Danger => (
                if on {
                    p.accent_soft
                } else if pressed {
                    p.subtle
                } else if hovered {
                    p.hover
                } else {
                    p.surface
                },
                Stroke::new(
                    1.0,
                    if on {
                        style::soft(p.accent, 0.45)
                    } else if hovered {
                        p.border_strong
                    } else {
                        p.border
                    },
                ),
            ),
            Kind::Ghost => (
                if on {
                    p.accent_soft
                } else if pressed {
                    p.subtle
                } else if hovered {
                    p.hover
                } else {
                    Color32::TRANSPARENT
                },
                Stroke::NONE,
            ),
        };
        let painter = ui.painter();
        let radius = CornerRadius::same(RADIUS);
        painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
        if resp.has_focus() {
            painter.rect_stroke(rect.expand(2.0), CornerRadius::same(RADIUS + 2), Stroke::new(1.5, p.accent), StrokeKind::Outside);
        }
        let mut x = rect.center().x - content_w / 2.0;
        let fade = |c: Color32| {
            if enabled { c } else { c.gamma_multiply(0.45) }
        };
        if let Some(g) = icon_galley {
            let pos = egui::pos2(x, rect.center().y - g.size().y / 2.0);
            x += g.size().x + gap;
            painter.galley(pos, g, fade(icon_color));
        }
        if let Some(g) = galley {
            let pos = egui::pos2(x, rect.center().y - g.size().y / 2.0);
            painter.galley(pos, g, fade(text_color));
        }
    }
    resp
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

pub fn icon_button_ex(ui: &mut Ui, icon: &str, tooltip: &str, on: bool, side: f32) -> Response {
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
        painter.text(rect.center(), Align2::CENTER_CENTER, icon, style::icon_font((side * 0.58).round()), color);
    }
    resp
}

/// One segment of a [`button_group`]: an icon, its tooltip and whether it is "on".
pub struct Segment<'a> {
    pub icon: &'a str,
    pub tooltip: &'a str,
    pub on: bool,
}

/// A row of icon buttons in one outlined surface, split by hairlines: related toolbar actions
/// (navigation, folder actions) read as one finished control instead of a row of boxes.
pub fn button_group(ui: &mut Ui, segments: &[Segment]) -> Vec<Response> {
    let p = style::pal(ui);
    let (h, w) = (style::CONTROL_H, 30.0);
    let total = vec2(w * segments.len() as f32, h);
    let (rect, base) = ui.allocate_exact_size(total, Sense::hover());
    let enabled = ui.is_enabled();
    let radius = CornerRadius::same(RADIUS);
    ui.painter().rect_filled(rect, radius, p.surface);
    let mut out = Vec::with_capacity(segments.len());
    for (i, seg) in segments.iter().enumerate() {
        let r = Rect::from_min_size(egui::pos2(rect.left() + w * i as f32, rect.top()), vec2(w, h));
        let resp = ui.interact(r, base.id.with(("segment", i)), Sense::click()).on_hover_text(seg.tooltip);
        resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, seg.on, seg.tooltip));
        let hovered = enabled && resp.hovered();
        // Only the outer corners are rounded.
        let corners = CornerRadius {
            nw: if i == 0 { RADIUS } else { 0 },
            sw: if i == 0 { RADIUS } else { 0 },
            ne: if i + 1 == segments.len() { RADIUS } else { 0 },
            se: if i + 1 == segments.len() { RADIUS } else { 0 },
        };
        let painter = ui.painter();
        if seg.on {
            painter.rect_filled(r, corners, p.accent_soft);
        } else if enabled && resp.is_pointer_button_down_on() {
            painter.rect_filled(r, corners, p.subtle);
        } else if hovered {
            painter.rect_filled(r, corners, p.hover);
        }
        let color = if seg.on {
            p.accent_text
        } else if hovered {
            p.text
        } else {
            p.text_muted
        };
        let color = if enabled { color } else { color.gamma_multiply(0.45) };
        painter.text(r.center(), Align2::CENTER_CENTER, seg.icon, style::icon_font(16.0), color);
        if i > 0 {
            painter.vline(r.left(), r.y_range().shrink(6.0), Stroke::new(1.0, p.border));
        }
        if resp.has_focus() {
            painter.rect_stroke(r.shrink(1.0), corners, Stroke::new(1.5, p.accent), StrokeKind::Inside);
        }
        out.push(resp);
    }
    let hovered = ui.rect_contains_pointer(rect) && enabled;
    ui.painter().rect_stroke(rect, radius, Stroke::new(1.0, if hovered { p.border_strong } else { p.border }), StrokeKind::Inside);
    out
}

// ---------------------------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------------------------

/// A row of tabs with an underline under the selected one and a hairline under the whole
/// row. Returns each tab's response.
pub fn tab_strip<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, items: &[(T, &str)]) -> Vec<Response> {
    let p = style::pal(ui);
    let start_x = ui.cursor().min.x;
    let right = ui.max_rect().right();
    let pad = 10.0;
    let mut out = Vec::with_capacity(items.len());
    let bottom = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (value, label) in items {
                let selected = *current == *value;
                let galley = ui.painter().layout_no_wrap(label.to_string(), style::medium(ui.ctx(), style::body_size(ui.ctx())), Color32::PLACEHOLDER);
                let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 2.0 * pad, 34.0), Sense::click());
                resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, *label));
                if resp.clicked() {
                    *current = *value;
                }
                let painter = ui.painter();
                let color = if selected || resp.hovered() { p.text } else { p.text_muted };
                painter.galley(egui::pos2(rect.left() + pad, rect.center().y - galley.size().y / 2.0), galley, color);
                if selected {
                    let line = Rect::from_min_max(egui::pos2(rect.left() + pad - 4.0, rect.bottom() - 2.0), egui::pos2(rect.right() - pad + 4.0, rect.bottom()));
                    painter.rect_filled(line, CornerRadius::same(1), p.accent);
                }
                if resp.has_focus() {
                    painter.rect_stroke(rect.shrink(2.0), CornerRadius::same(RADIUS), Stroke::new(1.5, p.accent), StrokeKind::Inside);
                }
                out.push(resp);
            }
        })
        .response
        .rect
        .bottom();
    ui.painter().hline(start_x..=right, bottom - 0.5, Stroke::new(1.0, p.border));
    out
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

/// A rounded label on a soft fill of `color` (states, counts).
pub fn pill(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let dark = ui.visuals().dark_mode;
    let galley = ui.painter().layout_no_wrap(text.to_string(), style::medium(ui.ctx(), style::SMALL), color);
    let size = galley.size() + vec2(14.0, 6.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let fill = style::soft(color, if dark { 0.18 } else { 0.11 });
        ui.painter().rect_filled(rect, CornerRadius::same((size.y / 2.0) as u8), fill);
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    resp
}

/// A keyboard shortcut hint (`⌘K`) drawn as a key cap.
pub fn kbd(ui: &mut Ui, keys: &str) -> Response {
    let p = style::pal(ui);
    let galley = ui.painter().layout_no_wrap(keys.to_string(), FontId::proportional(style::SMALL), p.text_muted);
    let size = galley.size() + vec2(10.0, 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_kbd(ui.painter(), rect, &p);
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, p.text_muted);
    }
    resp
}

/// The key cap behind a shortcut hint.
pub fn paint_kbd(painter: &egui::Painter, rect: Rect, p: &style::Palette) {
    painter.rect(rect, CornerRadius::same(4), p.subtle, Stroke::new(1.0, p.border), StrokeKind::Inside);
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

/// A section title in semibold, slightly smaller than body.
pub fn section_title(ui: &Ui, text: &str) -> RichText {
    RichText::new(text).font(style::semibold(ui.ctx(), 13.0))
}

/// An icon on its own, as a label.
pub fn icon(ui: &mut Ui, icon: &str, size: f32, color: Color32) -> Response {
    ui.add(egui::Label::new(RichText::new(icon).font(style::icon_font(size)).color(color)).selectable(false))
}

/// A bordered card.
pub fn card(ui: &Ui) -> egui::Frame {
    let p = style::pal(ui);
    egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(14))
}

/// An icon in a soft circle of `color` (dialog headers, empty states).
pub fn icon_badge(ui: &mut Ui, icon: &str, color: Color32, diameter: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    let dark = ui.visuals().dark_mode;
    ui.painter().circle_filled(rect.center(), diameter / 2.0, style::soft(color, if dark { 0.18 } else { 0.12 }));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, style::icon_font((diameter * 0.48).round()), color);
    resp
}

/// A centered empty state: an icon in a soft circle, a title, a line of explanation, then
/// whatever `actions` adds (buttons, shortcut hints).
pub fn empty_state(ui: &mut Ui, icon: &str, title: &str, body: &str, actions: impl FnOnce(&mut Ui)) {
    let p = style::pal(ui);
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.22).clamp(12.0, 140.0));
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 28.0, p.subtle);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, style::icon_font(26.0), p.text_muted);
        ui.add_space(10.0);
        ui.label(RichText::new(title).font(style::semibold(ui.ctx(), 15.0)).color(p.text));
        ui.add_space(2.0);
        ui.label(RichText::new(body).color(p.text_muted));
        ui.add_space(12.0);
        actions(ui);
    });
}

/// A single-line search field with a magnifier inside and a clear button.
pub fn search_field(ui: &mut Ui, id: egui::Id, text: &mut String, hint_text: &str, width: f32) -> Response {
    field_with_icon(ui, id, text, hint_text, width, egui_phosphor::regular::MAGNIFYING_GLASS, true)
}

/// A single-line field with `icon` inside on the left (and, with `clear`, a button that
/// empties it on the right).
pub fn field_with_icon(ui: &mut Ui, id: egui::Id, text: &mut String, hint_text: &str, width: f32, icon: &str, clear: bool) -> Response {
    let p = style::pal(ui);
    let edit = egui::TextEdit::singleline(text).id(id).hint_text(RichText::new(hint_text).color(p.text_faint)).desired_width(width).margin(egui::Margin {
        left: 28,
        right: if clear { 24 } else { 8 },
        top: 6,
        bottom: 6,
    });
    let resp = ui.add(edit);
    let r = resp.rect;
    if resp.has_focus() {
        style::focus_halo(ui, r, RADIUS);
    }
    ui.painter().text(egui::pos2(r.left() + 14.0, r.center().y), Align2::CENTER_CENTER, icon, style::icon_font(14.0), p.text_faint);
    if clear && !text.is_empty() {
        let x_rect = Rect::from_center_size(egui::pos2(r.right() - 13.0, r.center().y), Vec2::splat(16.0));
        let x = ui.interact(x_rect, resp.id.with("clear"), Sense::click());
        let color = if x.hovered() { p.text } else { p.text_faint };
        ui.painter().text(x_rect.center(), Align2::CENTER_CENTER, egui_phosphor::regular::X_CIRCLE, style::icon_font(14.0), color);
        if x.clicked() {
            text.clear();
        }
    }
    resp
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
