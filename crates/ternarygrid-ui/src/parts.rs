//! Larger shared pieces of the redesign: dialog and window frames, wizard steppers, callouts,
//! chips, text segmented controls, switches, progress bars, titled groups, stat tiles and form
//! rows. Every screen builds on these so dialogs, wizards and tools look the same everywhere
//! (see the design canvas "TernaryGrid modern redesign").

use egui::{Align, Align2, Color32, CornerRadius, Frame, Id, Layout, Margin, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType, vec2};

use crate::kit;
use crate::style::{self, RADIUS, RADIUS_LG};

// ---------------------------------------------------------------------------------------------
// Dialogs and windows
// ---------------------------------------------------------------------------------------------

/// A dialog or tool window: an icon badge, a title, a subtitle and × in its header, the body,
/// and an optional footer on the chrome colour (buttons, status).
pub struct Dialog<'a> {
    title: &'a str,
    icon: Option<&'a str>,
    subtitle: Option<&'a str>,
    width: f32,
    min_height: Option<f32>,
    pad: Margin,
}

/// What a dialog's frame reports.
#[derive(Default, Debug, Clone, Copy)]
pub struct DialogResponse {
    /// × was clicked (or, for a modal, the backdrop / Escape).
    pub close: bool,
    /// The layer the dialog is drawn on (for "Escape closes the topmost window").
    pub layer: Option<egui::LayerId>,
    /// Where it was drawn.
    pub rect: Option<Rect>,
}

impl<'a> Dialog<'a> {
    pub fn new(title: &'a str) -> Self {
        Dialog { title, icon: None, subtitle: None, width: 560.0, min_height: None, pad: Margin::symmetric(20, 16) }
    }

    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn subtitle(mut self, subtitle: &'a str) -> Self {
        self.subtitle = Some(subtitle).filter(|s| !s.is_empty());
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn min_height(mut self, h: f32) -> Self {
        self.min_height = Some(h);
        self
    }

    /// Padding around the body (default 20 × 16; zero for bodies that draw edge to edge).
    pub fn body_padding(mut self, m: Margin) -> Self {
        self.pad = m;
        self
    }

    fn frame(ui_ctx: &egui::Context) -> Frame {
        let dark = ui_ctx.global_style().visuals.dark_mode;
        let p = style::palette(dark);
        Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(RADIUS_LG + 2))
            .shadow(ui_ctx.global_style().visuals.window_shadow)
            .inner_margin(Margin::ZERO)
    }

    /// The header, the body and the footer inside an already open `ui`; returns whether × was clicked.
    pub fn contents<R>(&self, ui: &mut Ui, body: impl FnOnce(&mut Ui) -> R, footer: Option<&mut dyn FnMut(&mut Ui)>) -> (bool, R) {
        let mut body = Some(body);
        let mut out = None;
        let close = self.contents_dyn(ui, &mut |ui| out = body.take().map(|b| b(ui)), footer);
        (close, out.expect("a dialog body runs once"))
    }

    // The frame code is not generic so it is compiled once, not once per dialog.
    fn contents_dyn(&self, ui: &mut Ui, body: &mut dyn FnMut(&mut Ui), footer: Option<&mut dyn FnMut(&mut Ui)>) -> bool {
        let p = style::pal(ui);
        ui.set_width(self.width);
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut close = false;
        // Header.
        Frame::new().inner_margin(Margin { left: 20, right: 16, top: 16, bottom: 14 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if let Some(icon) = self.icon {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::same(RADIUS_LG), p.accent_soft);
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, icon, style::icon_font(16.5), p.accent_text);
                }
                match self.subtitle {
                    // Title over subtitle; a lone title stays centred on the row.
                    Some(s) => {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            kit::semibold_label(ui, self.title, 15.0, p.text);
                            ui.label(RichText::new(s).size(12.5).color(p.text_muted));
                        });
                    }
                    None => {
                        kit::semibold_label(ui, self.title, 15.0, p.text);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if kit::icon_button_sized(ui, egui_phosphor::regular::X, crate_close_label(), false, 28.0, 12.0).clicked() {
                        close = true;
                    }
                });
            });
        });
        hline(ui);
        // Body.
        Frame::new().inner_margin(self.pad).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            if let Some(h) = self.min_height {
                ui.set_min_height(h);
            }
            body(ui);
        });
        // Footer.
        if let Some(footer) = footer {
            let top = ui.cursor().top();
            let rect = Rect::from_min_max(egui::pos2(ui.max_rect().left(), top), egui::pos2(ui.max_rect().right(), top + 54.0));
            let r = CornerRadius { nw: 0, ne: 0, sw: RADIUS_LG + 2, se: RADIUS_LG + 2 };
            ui.painter().rect_filled(rect.shrink(1.0), r, p.chrome);
            ui.painter().hline(rect.x_range(), top + 0.5, Stroke::new(1.0, p.border));
            Frame::new().inner_margin(Margin { left: 20, right: 16, top: 12, bottom: 12 }).show(ui, |ui| {
                ui.set_min_height(30.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    footer(ui);
                });
            });
        }
        close
    }

    /// A modal dialog (dimmed backdrop). `close` is set by ×, a click on the backdrop and Escape.
    pub fn show_modal<R>(&self, ctx: &egui::Context, id: Id, body: impl FnOnce(&mut Ui) -> R, mut footer: impl FnMut(&mut Ui)) -> (DialogResponse, R) {
        let mut body = Some(body);
        let mut out = None;
        let resp = self.modal_dyn(ctx, id, &mut |ui| out = body.take().map(|b| b(ui)), Some(&mut footer));
        (resp, out.expect("a dialog body runs once"))
    }

    /// A modal dialog without a footer.
    pub fn show_modal_plain<R>(&self, ctx: &egui::Context, id: Id, body: impl FnOnce(&mut Ui) -> R) -> (DialogResponse, R) {
        let mut body = Some(body);
        let mut out = None;
        let resp = self.modal_dyn(ctx, id, &mut |ui| out = body.take().map(|b| b(ui)), None);
        (resp, out.expect("a dialog body runs once"))
    }

    fn modal_dyn(&self, ctx: &egui::Context, id: Id, body: &mut dyn FnMut(&mut Ui), footer: Option<&mut dyn FnMut(&mut Ui)>) -> DialogResponse {
        let dark = ctx.global_style().visuals.dark_mode;
        let backdrop = if dark { Color32::from_black_alpha(140) } else { Color32::from_rgba_unmultiplied(30, 30, 45, 70) };
        let modal = egui::Modal::new(id).frame(Self::frame(ctx)).backdrop_color(backdrop).show(ctx, |ui| self.contents_dyn(ui, body, footer));
        DialogResponse { close: modal.inner || modal.should_close(), layer: Some(modal.response.layer_id), rect: Some(modal.response.rect) }
    }

    /// A floating, movable tool window (wizards, tools) with this frame; `open` is cleared by ×.
    pub fn show_window<R>(
        &self,
        ctx: &egui::Context,
        id: Id,
        open: &mut bool,
        default_pos: Option<egui::Pos2>,
        body: impl FnOnce(&mut Ui) -> R,
        footer: Option<&mut dyn FnMut(&mut Ui)>,
    ) -> (DialogResponse, Option<R>) {
        let mut body = Some(body);
        let mut out = None;
        let resp = self.window_dyn(ctx, id, open, default_pos, &mut |ui| out = body.take().map(|b| b(ui)), footer);
        (resp, out)
    }

    fn window_dyn(
        &self,
        ctx: &egui::Context,
        id: Id,
        open: &mut bool,
        default_pos: Option<egui::Pos2>,
        body: &mut dyn FnMut(&mut Ui),
        footer: Option<&mut dyn FnMut(&mut Ui)>,
    ) -> DialogResponse {
        if !*open {
            return DialogResponse::default();
        }
        let mut w = egui::Window::new(self.title).id(id).title_bar(false).resizable(false).collapsible(false).frame(Self::frame(ctx));
        w = match default_pos {
            Some(pos) => w.default_pos(pos),
            None => w.pivot(Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()),
        };
        let Some(shown) = w.show(ctx, |ui| self.contents_dyn(ui, body, footer)) else { return DialogResponse::default() };
        let close = shown.inner.unwrap_or(false);
        if close {
            *open = false;
        }
        DialogResponse { close, layer: Some(shown.response.layer_id), rect: Some(shown.response.rect) }
    }
}

fn crate_close_label() -> &'static str {
    "Close"
}

/// A hairline across the whole width of `ui` at the cursor.
pub fn hline(ui: &mut Ui) {
    let p = style::pal(ui);
    let y = ui.cursor().top();
    ui.painter().hline(ui.max_rect().x_range(), y + 0.5, Stroke::new(1.0, p.border));
    ui.add_space(1.0);
}

// ---------------------------------------------------------------------------------------------
// Wizard stepper
// ---------------------------------------------------------------------------------------------

/// Numbered steps joined by lines: done steps show a check, the active one an accent ring.
/// Steps up to `reachable` can be clicked; returns the clicked step.
pub fn stepper(ui: &mut Ui, steps: &[&str], active: usize, reachable: usize) -> Option<usize> {
    let p = style::pal(ui);
    let mut clicked = None;
    let size = style::body_size(ui.ctx());
    let n = steps.len();
    // Measure the labels so the joining lines take what is left.
    let label_w: Vec<f32> = steps.iter().map(|s| ui.painter().layout_no_wrap(s.to_string(), style::medium(ui.ctx(), size), Color32::PLACEHOLDER).size().x).collect();
    let fixed: f32 = label_w.iter().map(|w| w + 22.0 + 8.0).sum::<f32>() + (n.saturating_sub(1)) as f32 * 20.0;
    let line_w = ((ui.available_width() - fixed) / (n.saturating_sub(1).max(1)) as f32).max(16.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for (i, s) in steps.iter().enumerate() {
            let w = 22.0 + 8.0 + label_w[i];
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 26.0), if i <= reachable { Sense::click() } else { Sense::hover() });
            resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, i <= reachable, i == active, *s));
            let c = egui::pos2(rect.left() + 11.0, rect.center().y);
            let painter = ui.painter();
            if i < active {
                painter.circle_filled(c, 11.0, p.accent);
                painter.text(c, Align2::CENTER_CENTER, egui_phosphor::regular::CHECK, style::icon_font(12.0), p.on_accent);
            } else if i == active {
                painter.circle_stroke(c, 10.0, Stroke::new(2.0, p.accent));
                painter.text(c, Align2::CENTER_CENTER, (i + 1).to_string(), style::medium(ui.ctx(), 11.5), p.accent_text);
            } else {
                painter.circle_stroke(c, 10.25, Stroke::new(1.5, p.border_strong));
                painter.text(c, Align2::CENTER_CENTER, (i + 1).to_string(), style::medium(ui.ctx(), 11.5), p.text_muted);
            }
            let color = if i == active { p.text } else { p.text_muted };
            let font = if i == active { style::semibold(ui.ctx(), size) } else { egui::FontId::proportional(size) };
            painter.text(egui::pos2(rect.left() + 30.0, rect.center().y), Align2::LEFT_CENTER, *s, font, color);
            if resp.hovered() && i <= reachable && i != active {
                painter.rect_stroke(rect.expand(3.0), CornerRadius::same(RADIUS), Stroke::new(1.0, p.border), StrokeKind::Outside);
            }
            if resp.clicked() {
                clicked = Some(i);
            }
            if i + 1 < n {
                let (r, _) = ui.allocate_exact_size(vec2(line_w, 26.0), Sense::hover());
                ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, p.border));
            }
        }
    });
    clicked
}

// ---------------------------------------------------------------------------------------------
// Callouts, chips, segmented controls, switches, progress
// ---------------------------------------------------------------------------------------------

/// The tone of a callout or status text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Info,
    Success,
    Warning,
    Error,
}

impl Tone {
    pub fn color(self, p: &style::Palette) -> Color32 {
        match self {
            Tone::Info => p.accent_text,
            Tone::Success => p.ok,
            Tone::Warning => p.warn,
            Tone::Error => p.err,
        }
    }

    pub fn icon(self) -> &'static str {
        use egui_phosphor::regular as i;
        match self {
            Tone::Info => i::INFO,
            Tone::Success => i::CHECK_CIRCLE,
            Tone::Warning => i::WARNING,
            Tone::Error => i::X_CIRCLE,
        }
    }

    /// The soft fill behind a callout of this tone (the design's note colours).
    pub fn fill(self, ui: &Ui) -> Color32 {
        let p = style::pal(ui);
        let dark = ui.visuals().dark_mode;
        let rgb = |c: u32| Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8);
        match self {
            Tone::Info => p.accent_soft,
            Tone::Warning => rgb(if dark { 0x3f3826 } else { 0xfbf3df }),
            Tone::Error => p.prod_soft,
            Tone::Success => rgb(if dark { 0x253b2b } else { 0xe3f4e7 }),
        }
    }
}

fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// A note on a soft tinted background: an icon, an optional bold title and the text. `add`
/// draws extra content at its right (buttons).
pub fn callout(ui: &mut Ui, tone: Tone, title: Option<&str>, body: &str, add: impl FnOnce(&mut Ui)) -> Response {
    let mut add = Some(add);
    callout_dyn(ui, tone, title, body, &mut |ui| {
        if let Some(a) = add.take() {
            a(ui)
        }
    })
}

fn callout_dyn(ui: &mut Ui, tone: Tone, title: Option<&str>, body: &str, add: &mut dyn FnMut(&mut Ui)) -> Response {
    let p = style::pal(ui);
    let color = tone.color(&p);
    Frame::new()
        .fill(tone.fill(ui))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new(tone.icon()).font(style::icon_font(14.0)).color(color));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    if let Some(t) = title {
                        kit::semibold_label(ui, t, 13.0, p.text);
                    }
                    ui.add(egui::Label::new(RichText::new(body).size(12.5).color(p.text)).wrap().selectable(true));
                    // Extra content (buttons) under the text at the right; nothing when empty
                    // (a right-to-left layout beside the text would take the panel's height).
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.allocate_ui_with_layout(vec2(ui.available_width(), 0.0), Layout::right_to_left(Align::Min), add);
                    });
                });
            });
        })
        .response
}

/// Selectable chips that wrap (export formats, filters). Returns true when the choice changed.
pub fn chips<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, items: &[(T, &str)]) -> bool {
    pick(current, items, |sel, labels| chips_idx(ui, sel, labels))
}

/// Runs a non-generic index picker over `items` and stores the clicked value.
fn pick<T: PartialEq + Copy>(current: &mut T, items: &[(T, &str)], f: impl FnOnce(Option<usize>, &[&str]) -> Option<usize>) -> bool {
    let labels: Vec<&str> = items.iter().map(|(_, l)| *l).collect();
    let sel = items.iter().position(|(v, _)| v == current);
    match f(sel, &labels) {
        Some(i) => {
            *current = items[i].0;
            true
        }
        None => false,
    }
}

fn chips_idx(ui: &mut Ui, sel: Option<usize>, labels: &[&str]) -> Option<usize> {
    let p = style::pal(ui);
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        for (i, label) in labels.iter().enumerate() {
            let on = sel == Some(i);
            let g = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(12.5), Color32::PLACEHOLDER);
            let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 24.0, 30.0), Sense::click());
            resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, on, *label));
            let hovered = resp.hovered();
            let (fill, stroke, color) =
                if on { (p.accent_soft, p.accent, p.accent_text) } else { (if hovered { p.hover } else { p.surface }, if hovered { p.border_strong } else { p.border }, p.text) };
            ui.painter().rect(rect, CornerRadius::same(15), fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
            ui.painter().galley(rect.center() - g.size() / 2.0, g, color);
            if resp.has_focus() {
                style::focus_halo(ui, rect, 15);
            }
            if resp.clicked() && !on {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Text options in a pill group (Server / Client, Light / Dark …). Returns true when changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, items: &[(T, &str)]) -> bool {
    pick(current, items, |sel, labels| segmented_idx(ui, sel, labels, false))
}

/// [`segmented`] at 24 px instead of 28.
pub fn segmented_small<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, items: &[(T, &str)]) -> bool {
    pick(current, items, |sel, labels| segmented_idx(ui, sel, labels, true))
}

fn segmented_idx(ui: &mut Ui, sel: Option<usize>, labels: &[&str], small: bool) -> Option<usize> {
    let p = style::pal(ui);
    let h = if small { 24.0 } else { 28.0 };
    let fs = 12.5;
    let galleys: Vec<_> = labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let font = if sel == Some(i) { style::medium(ui.ctx(), fs) } else { egui::FontId::proportional(fs) };
            ui.painter().layout_no_wrap(l.to_string(), font, Color32::PLACEHOLDER)
        })
        .collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 20.0).collect();
    let total = widths.iter().sum::<f32>() + 4.0 + (labels.len().saturating_sub(1)) as f32 * 2.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(7), p.subtle);
    let mut x = rect.left() + 2.0;
    let mut clicked = None;
    for (i, (label, g)) in labels.iter().zip(galleys).enumerate() {
        let r = Rect::from_min_size(egui::pos2(x, rect.top() + 2.0), vec2(widths[i], h - 4.0));
        let resp = ui.interact(r, ui.id().with(("seg", rect.min.x as i32, rect.min.y as i32, i)), Sense::click());
        let on = sel == Some(i);
        resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, on, *label));
        if on {
            kit::raised(ui.painter(), r, CornerRadius::same(5), p.surface);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(5), p.hover);
        }
        let color = if on { p.text } else { p.text_muted };
        ui.painter().galley(r.center() - g.size() / 2.0, g, color);
        if resp.has_focus() {
            ui.painter().rect_stroke(r, CornerRadius::same(5), Stroke::new(1.5, p.accent), StrokeKind::Inside);
        }
        if resp.clicked() && !on {
            clicked = Some(i);
        }
        x += widths[i] + 2.0;
    }
    clicked
}

/// An on/off switch with its label (and an optional line of explanation under it).
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str, sub: Option<&str>) -> Response {
    let p = style::pal(ui);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let (rect, mut resp) = ui.allocate_exact_size(vec2(30.0, 18.0), Sense::click());
        let t = ui.ctx().animate_bool(resp.id, *on);
        let track = if *on { p.accent } else { p.border_strong };
        ui.painter().rect_filled(rect, CornerRadius::same(9), blend(p.border_strong, track, t));
        let x = rect.left() + 9.0 + t * 12.0;
        ui.painter().circle_filled(egui::pos2(x, rect.center().y), 7.0, Color32::WHITE);
        if resp.has_focus() {
            style::focus_halo(ui, rect, 9);
        }
        let text = ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let l = ui.add(egui::Label::new(RichText::new(label).color(p.text)).sense(Sense::click()));
            if let Some(s) = sub {
                ui.label(RichText::new(s).size(12.0).color(p.text_muted));
            }
            l
        });
        if resp.clicked() || text.inner.clicked() {
            *on = !*on;
            resp.mark_changed();
        }
        resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, *on, label));
        resp
    })
    .inner
}

/// A thin progress bar `width` wide (`f32::INFINITY`: the rest of the row).
pub fn progress(ui: &mut Ui, fraction: f32, width: f32) -> Response {
    let p = style::pal(ui);
    let w = if width.is_finite() { width } else { ui.available_width() };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 6.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(3), p.subtle);
    let f = fraction.clamp(0.0, 1.0);
    if f > 0.0 {
        let r = Rect::from_min_size(rect.min, vec2((rect.width() * f).max(6.0), rect.height()));
        ui.painter().rect_filled(r, CornerRadius::same(3), p.accent);
    }
    resp
}

/// A small count badge (tab counts, numbers next to labels).
pub fn badge(ui: &mut Ui, text: &str, on: bool) -> Response {
    let p = style::pal(ui);
    let (fill, color) = if on { (p.accent_soft, p.accent_text) } else { (p.subtle, p.text_muted) };
    let g = ui.painter().layout_no_wrap(text.to_string(), style::medium(ui.ctx(), style::SMALL), color);
    let size = vec2((g.size().x + 10.0).max(18.0), 16.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, color);
    resp
}

/// Text in a tone's colour with its icon (status lines: "Connected", "Failed").
pub fn status(ui: &mut Ui, tone: Tone, text: &str) -> Response {
    let p = style::pal(ui);
    let c = tone.color(&p);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(tone.icon()).font(style::icon_font(15.0)).color(c));
        ui.add(egui::Label::new(RichText::new(text).color(c)).wrap());
    })
    .response
}

// ---------------------------------------------------------------------------------------------
// Groups, tiles, form rows, frames
// ---------------------------------------------------------------------------------------------

/// A titled group on a card (settings sections, wizard sections): the title in semibold, an
/// optional line under it, then `add`.
pub fn group<R>(ui: &mut Ui, title: Option<&str>, sub: Option<&str>, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut add = Some(add);
    let mut out = None;
    group_dyn(ui, title, sub, &mut |ui| out = add.take().map(|a| a(ui)));
    out.expect("a group body runs once")
}

fn group_dyn(ui: &mut Ui, title: Option<&str>, sub: Option<&str>, add: &mut dyn FnMut(&mut Ui)) {
    let p = style::pal(ui);
    kit::card(ui).corner_radius(CornerRadius::same(RADIUS_LG)).inner_margin(Margin::symmetric(20, 16)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 12.0;
        if title.is_some() || sub.is_some() {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                if let Some(t) = title {
                    kit::semibold_label(ui, t, 13.5, p.text);
                }
                if let Some(s) = sub {
                    ui.add(egui::Label::new(RichText::new(s).size(12.5).color(p.text_muted)).wrap());
                }
            });
        }
        add(ui);
    });
}

/// A section title with optional content at its right (buttons, counts).
pub fn section_header(ui: &mut Ui, title: &str, right: impl FnOnce(&mut Ui)) {
    let p = style::pal(ui);
    ui.horizontal(|ui| {
        kit::semibold_label(ui, title, 13.0, p.text);
        ui.with_layout(Layout::right_to_left(Align::Center), right);
    });
}

/// A number tile: a muted label, a large value and an optional faint line.
pub fn stat_tile(ui: &mut Ui, label: &str, value: &str, sub: Option<&str>) -> Response {
    let p = style::pal(ui);
    kit::card(ui)
        .corner_radius(CornerRadius::same(RADIUS_LG))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(RichText::new(label).size(12.0).color(p.text_muted));
            ui.label(RichText::new(value).font(style::semibold(ui.ctx(), 22.0)).color(p.text));
            if let Some(s) = sub {
                ui.label(RichText::new(s).size(12.0).color(p.text_faint));
            }
        })
        .response
}

/// A form row with a label column of `label_w`: the label (muted), then `add`.
pub fn form_row<R>(ui: &mut Ui, label: &str, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        form_label(ui, label, label_w);
        add(ui)
    })
    .inner
}

fn form_label(ui: &mut Ui, label: &str, label_w: f32) {
    let p = style::pal(ui);
    let h = ui.spacing().interact_size.y.max(style::CONTROL_H);
    ui.allocate_ui_with_layout(vec2(label_w, h), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(label_w);
        ui.add(egui::Label::new(RichText::new(label).color(p.text_muted)).truncate());
    });
}

/// A bordered frame for code, logs and previews (radius 8, surface fill).
pub fn code_frame() -> impl Fn(&Ui) -> Frame {
    |ui: &Ui| {
        let p = style::pal(ui);
        Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 10))
    }
}

/// A frame drawn like the grid header: lists of name / value rows (property grids).
pub fn bordered(ui: &Ui) -> Frame {
    let p = style::pal(ui);
    Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::ZERO)
}

/// A spinner with a muted label (loading states).
pub fn spinner_label(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.add(egui::Spinner::new().size(14.0).color(p.accent_text));
        ui.label(RichText::new(text).color(p.text_muted));
    })
    .response
}

/// A page header for tools and object tabs: icon, faint place parts, the title in semibold and
/// faint facts; `right` adds the actions at the right.
pub fn page_header(ui: &mut Ui, icon: &str, place: &[&str], title: &str, facts: Option<&str>, right: impl FnOnce(&mut Ui)) {
    let mut right = Some(right);
    page_header_dyn(ui, icon, place, title, facts, &mut |ui| {
        if let Some(r) = right.take() {
            r(ui)
        }
    });
}

fn page_header_dyn(ui: &mut Ui, icon: &str, place: &[&str], title: &str, facts: Option<&str>, right: &mut dyn FnMut(&mut Ui)) {
    let p = style::pal(ui);
    Frame::new().inner_margin(Margin { left: 16, right: 16, top: 12, bottom: 6 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.set_min_height(28.0);
            ui.label(RichText::new(icon).font(style::icon_font(15.5)).color(p.text_muted));
            for part in place {
                ui.label(RichText::new(*part).size(13.0).color(p.text_muted));
                ui.label(RichText::new("›").size(13.0).color(p.text_faint));
            }
            kit::semibold_label(ui, title, 15.0, p.text);
            if let Some(f) = facts {
                ui.add_space(4.0);
                ui.label(RichText::new(f).size(12.5).color(p.text_faint));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), right);
        });
    });
}

/// A toolbar row with padding and a hairline under it.
pub fn toolbar<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut add = Some(add);
    let mut out = None;
    toolbar_dyn(ui, &mut |ui| out = add.take().map(|a| a(ui)));
    out.expect("a toolbar body runs once")
}

fn toolbar_dyn(ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)) {
    Frame::new().inner_margin(Margin::symmetric(16, 8)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.set_min_height(30.0);
            add(ui);
        });
    });
    hline(ui);
}
