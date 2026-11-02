//! The look of the transfer wizards (the design canvas "TernaryGrid modern redesign",
//! `s_transfer.py`): the stepper row under the window's header, the body, the footer on the
//! chrome colour, form grids with a muted label column, bordered tables, folding cards, check
//! boxes and radio buttons, the progress block and the message log.

use crate::tabs::dbtools::look;
use egui::{Align, Color32, CornerRadius, Frame, Id, Layout, Margin, Rect, Response, RichText, Sense, Stroke, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType, pos2, vec2};
use egui_phosphor::regular as icon;
use ternarygrid_core::tr;
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::{self, Tone};
use ternarygrid_ui::style;

/// The size of a wizard's window: the design's 942 × 645 with its border (the pop-up adds 30
/// to the height asked for).
pub(crate) const WINDOW: Vec2 = vec2(942.0, 615.0);

/// Space between the parts of a section.
pub(crate) const GAP: f32 = 10.0;
/// Padding of multi-line text fields.
pub(crate) const INPUT_MARGIN: Margin = Margin::symmetric(10, 6);

// ------------------------------------------------------------------ frame

/// The row under the window's header: the stepper (and the title when the wizard is a tab).
pub(crate) fn top_panel(ui: &mut Ui, id: Id, add: impl FnOnce(&mut Ui)) {
    top_panel_dyn(ui, id, &mut once(add));
}

/// Wraps a closure that runs once so that the code around it is compiled once, not once per
/// caller (the wizards call these helpers a few hundred times).
fn once<R>(f: impl FnOnce(&mut Ui) -> R) -> impl FnMut(&mut Ui) {
    let mut f = Some(f);
    move |ui| {
        if let Some(f) = f.take() {
            f(ui);
        }
    }
}

fn top_panel_dyn(ui: &mut Ui, id: Id, add: &mut dyn FnMut(&mut Ui)) {
    let p = style::pal(ui);
    egui::Panel::top(id).resizable(false).frame(Frame::new().fill(p.surface).inner_margin(Margin { left: 22, right: 22, top: 14, bottom: 14 })).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        add(ui);
    });
}

/// The footer on the chrome colour: Back on the left, the actions on the right.
pub(crate) fn bottom_panel(ui: &mut Ui, id: Id, add: impl FnOnce(&mut Ui)) {
    bottom_panel_dyn(ui, id, &mut once(add));
}

fn bottom_panel_dyn(ui: &mut Ui, id: Id, add: &mut dyn FnMut(&mut Ui)) {
    let p = style::pal(ui);
    let r = style::RADIUS_LG + 1;
    egui::Panel::bottom(id)
        .resizable(false)
        .frame(Frame::new().fill(p.chrome).corner_radius(CornerRadius { nw: 0, ne: 0, sw: r, se: r }).inner_margin(Margin { left: 20, right: 16, top: 12, bottom: 12 }))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.set_min_height(style::CONTROL_H);
                add(ui);
            });
        });
}

/// The scrolling body on the surface colour, padded 22 × 18 (4 at the bottom: the design's body
/// is a fixed 470 high and its longest pages, the export formats, reach 4 above the footer).
pub(crate) fn body(ui: &mut Ui, id: Id, add: impl FnOnce(&mut Ui)) {
    body_dyn(ui, id, &mut once(add));
}

fn body_dyn(ui: &mut Ui, id: Id, add: &mut dyn FnMut(&mut Ui)) {
    let p = style::pal(ui);
    egui::CentralPanel::default().frame(Frame::new().fill(p.surface)).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
            Frame::new().inner_margin(Margin { left: 22, right: 22, top: 18, bottom: 4 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                style::form_spacing(ui);
                ui.spacing_mut().item_spacing.y = GAP;
                add(ui);
            });
        });
    });
}

/// The wizard's steps; returns the step clicked. A finished run shows every step done.
pub(crate) fn steps(ui: &mut Ui, steps: &[&str], current: usize, reachable: usize, finished: bool) -> Option<usize> {
    let active = if finished { steps.len() } else { current };
    parts::stepper(ui, steps, active, reachable)
}

/// What a footer shows.
pub(crate) struct Footer<'a> {
    /// Back (enabled or not); `None` on the first page.
    pub back: Option<bool>,
    /// Next (enabled or not); `None` on the last page, the only one with the run button.
    pub next: Option<bool>,
    /// The run button's label and whether it can be clicked.
    pub run: (&'a str, bool),
    /// "Save as task…" (enabled, the reason shown when disabled); `None`: not offered.
    pub save: Option<(bool, Option<&'a str>)>,
    pub error: Option<&'a str>,
}

/// The footer button clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Clicked {
    Back,
    Next,
    Run,
    Save,
}

fn enabled_button(ui: &mut Ui, enabled: bool, add: impl FnOnce(&mut Ui) -> Response) -> Response {
    ui.add_enabled_ui(enabled, add).inner
}

/// Back · (error) · Save as task… · Next, and the run button instead of Next on the last page.
pub(crate) fn footer(ui: &mut Ui, f: &Footer) -> Option<Clicked> {
    let p = style::pal(ui);
    let mut clicked = None;
    if let Some(ok) = f.back
        && enabled_button(ui, ok, |ui| kit::button_ex(ui, Kind::Ghost, Some(icon::ARROW_LEFT), tr("Back"), false)).clicked()
    {
        clicked = Some(Clicked::Back);
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        match f.next {
            Some(next_ok) => {
                if enabled_button(ui, next_ok, |ui| kit::button_ex(ui, Kind::Secondary, Some(icon::ARROW_RIGHT), tr("Next"), false)).clicked() {
                    clicked = Some(Clicked::Next);
                }
            }
            None => {
                if enabled_button(ui, f.run.1, |ui| kit::primary_icon(ui, icon::PLAY, f.run.0)).clicked() {
                    clicked = Some(Clicked::Run);
                }
            }
        }
        if let Some((ok, why)) = f.save {
            let mut r = enabled_button(ui, ok, |ui| kit::button_ex(ui, Kind::Ghost, Some(icon::FLOPPY_DISK), tr("Save as task…"), false));
            if let Some(why) = why {
                r = r.on_disabled_hover_text(why);
            }
            if r.clicked() {
                clicked = Some(Clicked::Save);
            }
        }
        if let Some(e) = f.error {
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(e).color(p.err)).truncate()).on_hover_text(e);
            ui.label(RichText::new(icon::X_CIRCLE).font(style::icon_font(15.0)).color(p.err));
        }
    });
    clicked
}

// ------------------------------------------------------------------ text

/// A section title (13 px semibold); space above it unless it opens the page.
pub(crate) fn section(ui: &mut Ui, text: &str) {
    section_dyn(ui, text, 0.0, &mut |_| {});
}

/// A section title with small buttons at its right (a 26 px row). Sections are 16 apart (the
/// gap of the body's parts), their parts 10.
pub(crate) fn section_with(ui: &mut Ui, text: &str, right: impl FnOnce(&mut Ui)) {
    section_dyn(ui, text, 26.0, &mut once(right));
}

fn section_dyn(ui: &mut Ui, text: &str, h: f32, right: &mut dyn FnMut(&mut Ui)) {
    if ui.cursor().top() > ui.max_rect().top() + 1.0 {
        ui.add_space(16.0 - GAP);
    }
    // As high as its text (or the buttons at its right), not as a form control.
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = h;
        parts::section_header(ui, text, right);
    });
}

/// The title of a running step ("Exporting…"), 16 above the progress (the body's parts are
/// 16 apart in the design).
pub(crate) fn title(ui: &mut Ui, text: &str) {
    let p = style::pal(ui);
    kit::semibold_label(ui, text, 15.0, p.text);
    ui.add_space(16.0 - GAP);
}

/// The result of a run: its icon and a large title.
pub(crate) fn outcome(ui: &mut Ui, tone: Tone, text: &str, extra: Option<&str>) {
    let p = style::pal(ui);
    // As high as its 20 pt icon, not as a form control.
    row_dyn(ui, 20.0, 8.0, &mut |ui| {
        ui.label(RichText::new(tone.icon()).font(style::icon_font(20.0)).color(tone.color(&p)));
        kit::semibold_label(ui, text, 16.0, p.text);
        if let Some(e) = extra {
            ui.label(RichText::new(e).size(12.0).color(p.text_faint));
        }
    });
}

/// Muted text (labels of a form, notes).
pub(crate) fn muted(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.text_muted)).wrap())
}

/// The explanation under a form row's control (12 px, muted).
pub(crate) fn sub(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(p.text_muted)).wrap())
}

/// Faint text (hints next to controls, counts).
pub(crate) fn faint(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(p.text_faint)).wrap())
}

/// Faint text on one line (units and words between the fields of a row).
pub(crate) fn hint(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(p.text_faint)).extend())
}

/// Monospace text.
pub(crate) fn mono(ui: &mut Ui, text: &str, color: Color32, size: f32) -> Response {
    ui.add(egui::Label::new(RichText::new(text).monospace().size(size).color(color)).truncate())
}

/// Text in a tone's colour (statuses in tables).
pub(crate) fn tone_text(ui: &mut Ui, tone: Tone, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(tone.color(&p))).truncate())
}

// ------------------------------------------------------------------ progress

/// A progress bar `h` high across the rest of the row: the accent on the subtle track.
pub(crate) fn bar(ui: &mut Ui, frac: f32, h: f32) {
    let p = style::pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let r = CornerRadius::same((h / 2.0) as u8);
    ui.painter().rect_filled(rect, r, p.subtle);
    let done = Rect::from_min_size(rect.min, vec2(rect.width() * frac.clamp(0.0, 1.0), h));
    if done.width() > 0.5 {
        ui.painter().rect_filled(done, r, p.accent);
    }
}

/// The counters of a run in two columns (24 apart): a muted label 100 wide, rows 6 apart.
pub(crate) fn fact_grid(ui: &mut Ui, facts: &[(&str, String, Option<Color32>)]) {
    fact_rows(ui, facts, 2, 100.0, 6.0);
}

/// Read-only form rows in `cols` columns (24 apart): a muted label `label_w` wide, 16 from its
/// value; rows 21.5 high and `gap` apart, as the design's form rows whose control is plain text
/// (there the text sits on the 16 px line of the page's font: 4 below the row's top, the label 7).
fn fact_rows(ui: &mut Ui, facts: &[(&str, String, Option<Color32>)], cols: usize, label_w: f32, gap: f32) {
    let p = style::pal(ui);
    let (left, top, w) = (ui.cursor().left(), ui.cursor().top(), ui.available_width());
    let col_w = (w - 24.0 * (cols - 1) as f32) / cols as f32;
    let row_h = 21.5;
    let pitch = row_h + gap;
    for (i, (label, value, color)) in facts.iter().enumerate() {
        let x = left + (i % cols) as f32 * (col_w + 24.0);
        let y = top + (i / cols) as f32 * pitch;
        put_left(ui, Rect::from_min_size(pos2(x, y + 7.0), vec2(label_w, 16.0)), egui::Label::new(RichText::new(*label).color(p.text_muted)).truncate());
        let shown = egui::Label::new(RichText::new(value).color(color.unwrap_or(p.text))).truncate();
        put_left(ui, Rect::from_min_size(pos2(x + label_w + 16.0, y + 4.0), vec2(col_w - label_w - 16.0, 16.0)), shown);
    }
    let h = (facts.len().div_ceil(cols) as f32 * pitch - gap).max(0.0);
    ui.advance_cursor_after_rect(Rect::from_min_size(pos2(left, top), vec2(w, h)));
}

/// `label` at the left of `rect`, without moving the cursor (`Ui::put` centres it and moves
/// the cursor).
fn put_left(ui: &mut Ui, rect: Rect, label: egui::Label) {
    ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Min))).add(label);
}

// ------------------------------------------------------------------ rows

/// A hairline across the body, 16 from the parts above and below it.
pub(crate) fn divider(ui: &mut Ui) {
    ui.add_space(16.0 - GAP);
    parts::hline(ui);
    ui.add_space(16.0);
}

/// A row as high as its content (buttons, check boxes), its parts 8 apart.
pub(crate) fn tight_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    row_dyn(ui, 0.0, 8.0, &mut once(add));
}

/// The bar above a list of tables to add: 28 px controls, 8 apart.
pub(crate) fn picker_bar(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    row_dyn(ui, 28.0, 8.0, &mut once(add));
}

/// A row of controls `h` high (0: as high as they are), `gap` apart.
pub(crate) fn row_dyn(ui: &mut Ui, h: f32, gap: f32, add: &mut dyn FnMut(&mut Ui)) {
    // A horizontal row starts as high as `interact_size.y` of the ui it is in.
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = h;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            add(ui)
        });
    });
}

// ------------------------------------------------------------------ controls

/// `edit` exactly `h` high with its text centred, 13 from the left edge as the design's fields
/// (their 1 px border, 10 px padding and the input's own 2 px) and 10 from the right.
pub(crate) fn fit(edit: egui::TextEdit<'_>, h: f32) -> egui::TextEdit<'_> {
    edit.margin(Margin { left: 13, right: 10, top: 0, bottom: 0 }).min_size(vec2(0.0, h)).vertical_align(Align::Center)
}

/// A single-line text field as tall as the other controls.
pub(crate) fn line(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    fit(egui::TextEdit::singleline(text), style::CONTROL_H)
}

/// [`line`] in the monospace font, `width` wide.
pub(crate) fn mono_line(text: &mut dyn egui::TextBuffer, width: f32) -> egui::TextEdit<'_> {
    line(text).font(egui::TextStyle::Monospace).desired_width(width)
}

/// A whole number in a text field `width` wide (left-aligned, as the design's fields), shown
/// with thousands separators and kept within `range`.
pub(crate) fn number<T: egui::emath::Numeric>(ui: &mut Ui, value: &mut T, range: std::ops::RangeInclusive<T>, width: f32) {
    let shown = super::group(value.to_f64().max(0.0).round() as u64);
    let (lo, hi) = (range.start().to_f64(), range.end().to_f64());
    super::parsed_edit(ui, shown, "", width, false, |t| {
        if let Ok(n) = t.replace(',', "").trim().parse::<f64>() {
            *value = T::from_f64(n.round().clamp(lo, hi));
        }
    });
}

/// A check box of the design (the kit's); without a label it is the smaller check box of a
/// table cell.
pub(crate) fn check(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    if label.is_empty() {
        return look::cell_check(ui, on, "");
    }
    kit::checkbox(ui, on, label)
}

/// An item of a combo box's list in the menu look (call `kit::menu_look` first): a check mark
/// at the selected one. True when it was clicked.
pub(crate) fn choice(ui: &mut Ui, selected: bool, label: &str) -> bool {
    let mut on = selected;
    kit::menu_check(ui, &mut on, label).clicked()
}

/// A radio button drawn like the mockups: an accent ring with a dot when selected.
pub(crate) fn radio(ui: &mut Ui, selected: bool, label: &str) -> Response {
    kit::radio(ui, selected, label)
}

/// [`radio`] over a value; returns true when it changed.
pub(crate) fn radio_value<T: PartialEq + Clone>(ui: &mut Ui, current: &mut T, value: T, label: &str) -> bool {
    let selected = *current == value;
    if radio(ui, selected, label).clicked() && !selected {
        *current = value;
        return true;
    }
    false
}

/// A small frameless icon button (add, remove) with its tooltip.
pub(crate) fn icon_button(ui: &mut Ui, glyph: &str, tooltip: &str, side: f32) -> Response {
    kit::icon_button_ex(ui, glyph, tooltip, false, side)
}

/// A quiet small button with an optional icon.
pub(crate) fn ghost(ui: &mut Ui, glyph: Option<&str>, text: &str) -> Response {
    kit::button_ex(ui, Kind::Ghost, glyph, text, true)
}

/// A field with a prefix drawn inside it (`WHERE` before a filter) over an optional text.
pub(crate) fn prefixed_opt_text(ui: &mut Ui, value: &mut Option<String>, prefix: &str, hint: &str, width: f32) -> bool {
    let p = style::pal(ui);
    let font = egui::FontId::monospace(11.5);
    let pg = ui.painter().layout_no_wrap(prefix.to_string(), font, p.accent_text);
    let mut s = value.clone().unwrap_or_default();
    let mono = egui::FontId::monospace(12.5);
    let edit = fit(egui::TextEdit::singleline(&mut s), 26.0).font(mono.clone()).hint_text(RichText::new(hint).font(mono).color(p.text_faint)).desired_width(width).margin(Margin {
        left: (10.0 + pg.size().x + 8.0 + 2.0).round() as i8,
        right: 10,
        top: 0,
        bottom: 0,
    });
    let r = ui.add(edit);
    ui.painter().galley(pos2(r.rect.left() + 10.0, r.rect.center().y - pg.size().y / 2.0), pg, p.accent_text);
    if r.changed() {
        *value = Some(s).filter(|s| !s.is_empty());
    }
    r.changed()
}

// ------------------------------------------------------------------ forms

/// Rows of a form: a muted label column of a fixed width, then the control(s); with `per_row`
/// 2 the rows are laid out in two columns. (Not an `egui::Grid`: its first, measuring frame
/// would widen the resizable wizard window to the full width of long texts.)
pub(crate) struct Form<'u> {
    ui: &'u mut Ui,
    per_row: usize,
    label_w: f32,
    /// Space between the columns of a line.
    col_gap: f32,
    n: usize,
    /// Top and height of the row of columns being laid out.
    top: f32,
    height: f32,
}

/// The height of `parts::segmented`.
pub(crate) const SEGMENTED_H: f32 = 28.0;

/// Space between the columns of a form with several rows per line.
const FORM_COLUMN_GAP: f32 = 28.0;

/// A form (see [`Form`]) with rows [`GAP`] apart.
pub(crate) fn form<R>(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, per_row: usize, label_w: f32, add: impl FnOnce(&mut Form<'_>) -> R) -> R {
    form_gap(ui, id, per_row, label_w, GAP, add)
}

/// [`form`] with rows `gap` apart.
pub(crate) fn form_gap<R>(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, per_row: usize, label_w: f32, gap: f32, add: impl FnOnce(&mut Form<'_>) -> R) -> R {
    let mut add = Some(add);
    let mut out = None;
    form_dyn(ui, Id::new(id), per_row, label_w, gap, &mut |f: &mut Form<'_>| out = add.take().map(|a| a(f)));
    out.expect("a form body runs once")
}

fn form_dyn(ui: &mut Ui, id: Id, per_row: usize, label_w: f32, gap: f32, add: &mut dyn FnMut(&mut Form<'_>)) {
    ui.push_id(id, |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = gap;
            let mut f = Form { ui, per_row: per_row.max(1), label_w, col_gap: FORM_COLUMN_GAP, n: 0, top: 0.0, height: 0.0 };
            add(&mut f);
            f.finish_line();
        });
    });
}

/// A label of `label_w` (wrapped when longer) and the control(s) `add` draws on its right, as
/// the design's form rows: the label 6 below the top, the controls centred on a row as high as
/// the ui's controls (`interact_size.y`).
pub(crate) fn form_row<R>(ui: &mut Ui, label: &str, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal_top(|ui| {
        row_label(ui, label, label_w);
        ui.horizontal(add).inner
    })
    .inner
}

/// See [`Form::check_row`]: a row 22 high, the check boxes 16 apart at its top.
fn check_row_dyn(ui: &mut Ui, label: &str, label_w: f32, add: &mut dyn FnMut(&mut Ui)) {
    let p = style::pal(ui);
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 0.0;
        ui.horizontal_top(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(label_w, 22.0), Sense::hover());
            put_left(ui, Rect::from_min_size(rect.min + vec2(0.0, 6.0), vec2(label_w, 16.0)), egui::Label::new(RichText::new(label).color(p.text_muted)).truncate());
            ui.add_space(16.0 - ui.spacing().item_spacing.x);
            ui.spacing_mut().item_spacing.x = 16.0;
            add(ui);
        });
    });
}

/// The label column of a form row (the label 6 below its top) and the gap after it.
fn row_label(ui: &mut Ui, label: &str, label_w: f32) {
    let p = style::pal(ui);
    ui.allocate_ui_with_layout(vec2(label_w, 0.0), Layout::top_down(Align::Min), |ui| {
        ui.set_min_width(label_w);
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(label).color(p.text_muted)).wrap());
    });
    ui.add_space(16.0 - ui.spacing().item_spacing.x);
}

impl Form<'_> {
    /// A labelled row; `add` draws the control(s).
    pub fn row<R>(&mut self, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
        if self.per_row == 1 {
            return form_row(self.ui, label, self.label_w, add);
        }
        let mut child = self.column_ui();
        let r = form_row(&mut child, label, self.label_w, add);
        self.end_column(&child);
        r
    }

    /// A row whose only control is a segmented one: 28 high, as in the design.
    pub fn seg_row<R>(&mut self, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
        let h = self.ui.spacing().interact_size.y;
        self.ui.spacing_mut().interact_size.y = SEGMENTED_H;
        let r = self.row(label, add);
        self.ui.spacing_mut().interact_size.y = h;
        r
    }

    /// A row of check boxes: as in the design they sit at its top, the label 6 lower.
    pub fn check_row<R>(&mut self, label: &str, add: impl FnOnce(&mut Ui) -> R) {
        let label_w = self.label_w;
        if self.per_row == 1 {
            check_row_dyn(self.ui, label, label_w, &mut once(add));
            return;
        }
        let mut child = self.column_ui();
        check_row_dyn(&mut child, label, label_w, &mut once(add));
        self.end_column(&child);
    }

    /// The ui of the next column of a line.
    fn column_ui(&mut self) -> Ui {
        let column = self.n % self.per_row;
        if column == 0 {
            self.top = self.ui.cursor().top();
            self.height = 0.0;
        }
        let full = self.ui.max_rect();
        let w = (full.width() - self.col_gap * (self.per_row - 1) as f32) / self.per_row as f32;
        let x = full.left() + column as f32 * (w + self.col_gap);
        let rect = Rect::from_min_size(pos2(x, self.top), vec2(w, 4000.0));
        self.ui.new_child(UiBuilder::new().id_salt(("row", self.n)).max_rect(rect).layout(Layout::top_down(Align::Min)))
    }

    fn end_column(&mut self, child: &Ui) {
        self.height = self.height.max(child.min_rect().height());
        self.n += 1;
        if self.n.is_multiple_of(self.per_row) {
            self.finish_line();
        }
    }

    /// Ends a line of columns that is not full yet.
    fn finish_line(&mut self) {
        if self.per_row > 1 && self.height > 0.0 {
            let full = self.ui.max_rect();
            self.ui.advance_cursor_after_rect(Rect::from_min_size(pos2(full.left(), self.top), vec2(full.width(), self.height)));
            self.height = 0.0;
        }
        self.n = self.n.next_multiple_of(self.per_row);
    }

    /// A row of its own under the control column (a warning under a choice).
    pub fn note(&mut self, add: impl FnOnce(&mut Ui)) {
        self.finish_line();
        let label_w = self.label_w;
        form_row(self.ui, "", label_w, add);
    }
}

/// A card (radius 10) with `pad` around its content, as wide as the row.
pub(crate) fn card<R>(ui: &mut Ui, pad: i8, add: impl FnOnce(&mut Ui) -> R) -> R {
    kit::card(ui)
        .corner_radius(CornerRadius::same(style::RADIUS_LG))
        .inner_margin(Margin::same(pad))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A card of read-only form rows (summaries).
pub(crate) fn facts(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, label_w: f32, gap: f32, rows: &[(&str, String, Option<Color32>)]) {
    card(ui, 14, |ui| {
        ui.push_id(Id::new(id), |ui| fact_rows(ui, rows, 1, label_w, gap));
    });
}

// ------------------------------------------------------------------ cards

/// A card that folds open: a chevron, its title and a faint summary; `fill` makes it as wide as
/// the row. Returns true when its header was clicked.
pub(crate) fn fold_card(ui: &mut Ui, id: Id, open: &mut bool, title: &str, summary: Option<&str>, fill: bool, body: impl FnOnce(&mut Ui)) -> bool {
    fold_card_dyn(ui, id, open, title, summary, fill, &mut once(body))
}

fn fold_card_dyn(ui: &mut Ui, id: Id, open: &mut bool, title: &str, summary: Option<&str>, fill: bool, body: &mut dyn FnMut(&mut Ui)) -> bool {
    let p = style::pal(ui);
    let mut toggled = false;
    kit::card(ui).corner_radius(CornerRadius::same(style::RADIUS_LG)).inner_margin(Margin::same(12)).show(ui, |ui| {
        if fill {
            ui.set_width(ui.available_width());
        }
        // The head as high as its text (the body keeps the form controls' height).
        let control_h = ui.spacing().interact_size.y;
        ui.spacing_mut().interact_size.y = 0.0;
        let head = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.spacing_mut().interact_size.y = control_h;
            let chevron = if *open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
            ui.label(RichText::new(chevron).font(style::icon_font(12.0)).color(p.text_faint));
            kit::semibold_label(ui, title, 13.0, p.text);
            if let Some(s) = summary {
                ui.add(egui::Label::new(RichText::new(s).size(12.0).color(p.text_faint)).truncate());
            }
            if fill {
                ui.allocate_space(vec2(ui.available_width(), 1.0));
            }
        });
        ui.spacing_mut().interact_size.y = control_h;
        let rect = head.response.rect.expand(4.0);
        let resp = ui.interact(rect, id.with("fold"), Sense::click());
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::CollapsingHeader, true, title));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            *open = !*open;
            toggled = true;
            ui.ctx().request_repaint();
        }
        // A compact card opened this frame shows its content from the next one, laid out at
        // full width (inside the row of compact cards it would widen the window).
        if *open && (fill || !toggled) {
            ui.add_space(GAP);
            body(ui);
        }
    });
    toggled
}

/// Whether a folding card stored in the ui's memory is open.
pub(crate) fn remembered_open(ui: &Ui, id: Id, default: bool) -> bool {
    ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(default)
}

pub(crate) fn remember_open(ui: &Ui, id: Id, open: bool) {
    ui.data_mut(|d| d.insert_temp(id, open));
}

// ------------------------------------------------------------------ tables

/// Width of a table column: fixed points, a share of what is left (at least 70 points a
/// share), or one share of at least the given points (the table scrolls sideways below it).
#[derive(Debug, Clone, Copy)]
pub(crate) enum W {
    Px(f32),
    Fr(f32),
    Min(f32),
}

impl W {
    /// The share of the flexible width and the least width.
    fn flex(self) -> (f32, f32) {
        match self {
            W::Px(_) => (0.0, 0.0),
            W::Fr(f) => (f, f * 70.0),
            W::Min(m) => (1.0, m),
        }
    }
}

/// A table column.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Col<'a> {
    pub title: &'a str,
    pub w: W,
    pub right: bool,
    /// Like a data grid's column: its name in the text colour, a hairline at its right.
    pub grid: bool,
}

pub(crate) fn col(title: &str, w: W) -> Col<'_> {
    Col { title, w, right: false, grid: false }
}

impl Col<'_> {
    /// Content aligned to the right (numbers).
    pub fn right(mut self) -> Self {
        self.right = true;
        self
    }

    /// A data grid's column (see [`Col::grid`]).
    pub fn grid(mut self) -> Self {
        self.grid = true;
        self
    }
}

/// A bordered table like the mockups: a 32 px header row on the header colour, striped rows
/// `row_h` high, each with a hairline under it. `header` adds to a header cell (a check box),
/// `cell(ui, row, column)` draws a cell. With `max_h` the rows scroll under the header.
#[allow(clippy::too_many_arguments)]
pub(crate) fn table(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    cols: &[Col],
    row_h: f32,
    max_h: Option<f32>,
    rows: usize,
    mut header: impl FnMut(&mut Ui, usize),
    mut cell: impl FnMut(&mut Ui, usize, usize),
) {
    table_dyn(ui, Id::new(id), cols, row_h, max_h, rows, &mut header, &mut cell);
}

#[allow(clippy::too_many_arguments)]
fn table_dyn(ui: &mut Ui, id: Id, cols: &[Col], row_h: f32, max_h: Option<f32>, rows: usize, header: &mut dyn FnMut(&mut Ui, usize), cell: &mut dyn FnMut(&mut Ui, usize, usize)) {
    let p = style::pal(ui);
    let (stripe, line) = (p.stripe, p.line);
    // The rows' hairlines are part of their height.
    let pitch = row_h + 1.0;
    let avail = ui.available_width() - 2.0;
    let fixed: f32 = cols.iter().map(|c| if let W::Px(w) = c.w { w } else { 0.0 }).sum();
    let fr: f32 = cols.iter().map(|c| c.w.flex().0).sum();
    let least: f32 = cols.iter().map(|c| c.w.flex().1).sum();
    let total = avail.max(fixed + least);
    let widths: Vec<f32> = cols
        .iter()
        .map(|c| match c.w {
            W::Px(w) => w,
            w => ((total - fixed).max(0.0) * w.flex().0 / fr.max(f32::EPSILON)).max(w.flex().1),
        })
        .collect();
    let total = widths.iter().sum::<f32>().max(total);
    let layout_of = |c: &Col| if c.right { Layout::right_to_left(Align::Center) } else { Layout::left_to_right(Align::Center) };
    ui.push_id(id, |ui| {
        parts::bordered(ui).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let mut draw = |ui: &mut Ui| {
                // Header.
                let (hr, _) = ui.allocate_exact_size(vec2(total, 33.0), Sense::hover());
                ui.painter().rect_filled(hr, CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, p.header);
                ui.painter().hline(hr.x_range(), hr.bottom() - 0.5, Stroke::new(1.0, p.border));
                let mut x = hr.left();
                for (i, c) in cols.iter().enumerate() {
                    let r = Rect::from_min_size(pos2(x, hr.top()), vec2(widths[i], hr.height() - 1.0));
                    // A data grid's header keeps its name at the left (its type at the right).
                    let layout = if c.grid { Layout::left_to_right(Align::Center) } else { layout_of(c) };
                    let mut child = ui.new_child(UiBuilder::new().id_salt(("h", i)).max_rect(r.shrink2(vec2(10.0, 0.0))).layout(layout));
                    child.set_clip_rect(r.intersect(ui.clip_rect()));
                    child.spacing_mut().item_spacing.x = 6.0;
                    if !c.title.is_empty() {
                        let (size, color) = if c.grid { (12.5, p.text) } else { (12.0, p.text_muted) };
                        kit::semibold_label(&mut child, c.title, size, color);
                    }
                    header(&mut child, i);
                    x += widths[i];
                    if c.grid {
                        ui.painter().vline(x - 0.5, r.y_range(), Stroke::new(1.0, line));
                    }
                }
                // Rows.
                let mut body = |ui: &mut Ui| {
                    for row in 0..rows {
                        let top = ui.cursor().top();
                        let left = ui.cursor().left();
                        let rr = Rect::from_min_size(pos2(left, top), vec2(total, pitch));
                        if row % 2 == 1 {
                            let last = row + 1 == rows && max_h.is_none();
                            let r = if last { CornerRadius { nw: 0, ne: 0, sw: 7, se: 7 } } else { CornerRadius::ZERO };
                            ui.painter().rect_filled(rr, r, stripe);
                        }
                        ui.painter().hline(rr.x_range(), rr.bottom() - 0.5, Stroke::new(1.0, line));
                        if ui.is_rect_visible(rr) {
                            let mut x = left;
                            for (i, c) in cols.iter().enumerate() {
                                let r = Rect::from_min_size(pos2(x, top), vec2(widths[i], row_h));
                                let mut child = ui.new_child(UiBuilder::new().id_salt((row, i)).max_rect(r.shrink2(vec2(10.0, 0.0))).layout(layout_of(c)));
                                child.set_clip_rect(r.intersect(ui.clip_rect()));
                                child.spacing_mut().item_spacing.x = 6.0;
                                child.spacing_mut().interact_size.y = (row_h - 4.0).min(26.0);
                                cell(&mut child, row, i);
                                x += widths[i];
                                if c.grid {
                                    ui.painter().vline(x - 0.5, r.y_range(), Stroke::new(1.0, line));
                                }
                            }
                        }
                        ui.allocate_rect(rr, Sense::hover());
                    }
                };
                match max_h {
                    Some(h) => {
                        egui::ScrollArea::vertical().id_salt("rows").max_height(h).auto_shrink([false, true]).show(ui, |ui| body(ui));
                    }
                    None => body(ui),
                }
            };
            if total > avail + 0.5 {
                egui::ScrollArea::horizontal().id_salt("h").auto_shrink([false, true]).show(ui, |ui| draw(ui));
            } else {
                draw(ui);
            }
        });
    });
}

/// A check box in a table's header that turns every row on or off: `states` are the rows'.
pub(crate) fn check_all(ui: &mut Ui, states: &[bool]) -> Option<bool> {
    let mut all = !states.is_empty() && states.iter().all(|s| *s);
    let tooltip = if all { tr("Select none") } else { tr("Select all") };
    check(ui, &mut all, "").on_hover_text(tooltip).changed().then_some(all)
}

// ------------------------------------------------------------------ boxes

/// A bordered box on the surface colour (file lists, logs, code).
pub(crate) fn boxed<R>(ui: &mut Ui, margin: Margin, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = style::pal(ui);
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}
