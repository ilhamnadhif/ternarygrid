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

/// The size of a wizard's window inside its border, as the design's modal (940 × 643, the
/// header, the steps and the footer included).
pub(crate) const WINDOW: Vec2 = vec2(940.0, 643.0);

/// Space between the parts of a section.
pub(crate) const GAP: f32 = 10.0;
/// The label column of every form of the wizards (one width, so the controls start at the same
/// place on every page).
pub(crate) const LABEL_W: f32 = 150.0;
/// [`LABEL_W`] of a form inside a folding card (its 1 point border and 12 padding in): the
/// controls start where the page's do.
pub(crate) const CARD_LABEL_W: f32 = LABEL_W - 13.0;
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

/// The scrolling body on the surface colour, padded 22 × 18 and 16 at the bottom (long pages
/// end above the footer, not against it).
pub(crate) fn body(ui: &mut Ui, id: Id, add: impl FnOnce(&mut Ui)) {
    body_dyn(ui, id, &mut once(add));
}

fn body_dyn(ui: &mut Ui, id: Id, add: &mut dyn FnMut(&mut Ui)) {
    let p = style::pal(ui);
    egui::CentralPanel::default().frame(Frame::new().fill(p.surface)).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
            let view_h = ui.clip_rect().height();
            Frame::new().inner_margin(Margin { left: 22, right: 22, top: 18, bottom: 16 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let top = ui.max_rect().top();
                ui.data_mut(|d| d.insert_temp(body_key(), (top, view_h)));
                style::form_spacing(ui);
                ui.spacing_mut().item_spacing.y = GAP;
                add(ui);
            });
        });
    });
}

/// The wizard's steps; returns the step clicked. After a run (`outcome`) every step is done; a
/// run with failures marks its last step with an amber (warning) or red (error) "!" ring.
pub(crate) fn steps(ui: &mut Ui, steps: &[&str], current: usize, reachable: usize, outcome: Option<Tone>) -> Option<usize> {
    let active = if outcome.is_some() { steps.len() } else { current };
    let shown = ui.scope(|ui| parts::stepper(ui, steps, active, reachable));
    if let (Some(tone @ (Tone::Warning | Tone::Error)), Some(last)) = (outcome, steps.last()) {
        // The kit draws the last step done (a filled check): paint its mark over it. The row of
        // steps ends with the last one: its 22 point circle, 8, its label.
        let p = style::pal(ui);
        let size = style::body_size(ui.ctx());
        let label_w = ui.painter().layout_no_wrap(last.to_string(), style::medium(ui.ctx(), size), Color32::PLACEHOLDER).size().x;
        let row = shown.response.rect;
        let c = pos2(row.right() - label_w - 30.0 + 11.0, row.center().y);
        let color = tone.color(&p);
        let painter = ui.painter();
        painter.circle_filled(c, 11.5, p.surface);
        painter.circle_stroke(c, 10.0, Stroke::new(2.0, color));
        painter.text(c, egui::Align2::CENTER_CENTER, icon::EXCLAMATION_MARK, style::icon_font(14.0), color);
    }
    shown.inner
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
    /// A run is going (`true`: it is being cancelled): Cancel takes the run button's place and
    /// nothing else is offered.
    pub running: Option<bool>,
    /// A run has finished: Close (the primary) and Run again, no Back.
    pub finished: bool,
}

/// The footer button clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Clicked {
    Back,
    Next,
    /// The run button, or Run again after a run.
    Run,
    Save,
    /// Cancel the running job.
    Cancel,
    /// Close the wizard (after a run).
    Close,
}

fn enabled_button(ui: &mut Ui, enabled: bool, add: impl FnOnce(&mut Ui) -> Response) -> Response {
    ui.add_enabled_ui(enabled, add).inner
}

/// Back · Save as task… at the left; (error) · Next, the primary, at the right (the run button
/// on the last page); only Cancel while running; Run again · Close after a run.
pub(crate) fn footer(ui: &mut Ui, f: &Footer) -> Option<Clicked> {
    let p = style::pal(ui);
    let mut clicked = None;
    if f.running.is_none() {
        if !f.finished
            && let Some(ok) = f.back
            && enabled_button(ui, ok, |ui| kit::button_ex(ui, Kind::Ghost, Some(icon::ARROW_LEFT), tr("Back"), false)).clicked()
        {
            clicked = Some(Clicked::Back);
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
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if let Some(cancelling) = f.running {
            let label = if cancelling { tr("Cancelling…") } else { tr("Cancel") };
            if enabled_button(ui, !cancelling, |ui| kit::button_ex(ui, Kind::Danger, Some(icon::STOP_CIRCLE), label, false)).clicked() {
                clicked = Some(Clicked::Cancel);
            }
        } else if f.finished {
            if kit::primary(ui, tr("Close")).clicked() {
                clicked = Some(Clicked::Close);
            }
            if enabled_button(ui, f.run.1, |ui| kit::secondary(ui, Some(icon::ARROW_CLOCKWISE), tr("Run again"))).clicked() {
                clicked = Some(Clicked::Run);
            }
        } else {
            match f.next {
                Some(next_ok) => {
                    if enabled_button(ui, next_ok, |ui| kit::primary_icon(ui, icon::ARROW_RIGHT, tr("Next"))).clicked() {
                        clicked = Some(Clicked::Next);
                    }
                }
                None => {
                    if enabled_button(ui, f.run.1, |ui| kit::primary_icon(ui, icon::PLAY, f.run.0)).clicked() {
                        clicked = Some(Clicked::Run);
                    }
                }
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

/// A read-only row of a summary or a result: a muted label and its value.
pub(crate) struct Fact<'a> {
    pub label: &'a str,
    pub value: String,
    /// The value's colour (the text colour when `None`).
    pub color: Option<Color32>,
    /// The value is a file path: its file name, then its folder (muted, shortened in the middle).
    pub path: bool,
}

pub(crate) fn fact(label: &str, value: impl Into<String>) -> Fact<'_> {
    Fact { label, value: value.into(), color: None, path: false }
}

impl Fact<'_> {
    pub fn color(mut self, color: Option<Color32>) -> Self {
        self.color = color;
        self
    }

    pub fn path(mut self) -> Self {
        self.path = true;
        self
    }
}

/// The counters of a run in two columns (24 apart): a muted label 100 wide, rows 6 apart.
pub(crate) fn fact_grid(ui: &mut Ui, facts: &[Fact]) {
    let p = style::pal(ui);
    let (cols, label_w, gap) = (2, 100.0, 6.0);
    let (left, top, w) = (ui.cursor().left(), ui.cursor().top(), ui.available_width());
    let col_w = (w - 24.0 * (cols - 1) as f32) / cols as f32;
    // Rows 22 high; the label and the value on one line, centred in the row.
    let (row_h, text_y) = (22.0, 3.0);
    let pitch = row_h + gap;
    for (i, f) in facts.iter().enumerate() {
        let x = left + (i % cols) as f32 * (col_w + 24.0);
        let y = top + (i / cols) as f32 * pitch;
        put_left(ui, Rect::from_min_size(pos2(x, y + text_y), vec2(label_w, 16.0)), egui::Label::new(RichText::new(f.label).color(p.text_muted)).truncate());
        let shown = egui::Label::new(RichText::new(&f.value).color(f.color.unwrap_or(p.text))).truncate();
        put_left(ui, Rect::from_min_size(pos2(x + label_w + 16.0, y + text_y), vec2(col_w - label_w - 16.0, 16.0)), shown);
    }
    let h = (facts.len().div_ceil(cols) as f32 * pitch - gap).max(0.0);
    ui.advance_cursor_after_rect(Rect::from_min_size(pos2(left, top), vec2(w, h)));
}

/// Read-only form rows: a muted label `label_w` wide, 16 from its value, both on one baseline;
/// rows `gap` apart. A long value wraps under itself (a path is shortened in the middle).
fn fact_rows(ui: &mut Ui, facts: &[Fact], label_w: f32, gap: f32) {
    let p = style::pal(ui);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = gap;
        for f in facts {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(vec2(label_w, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(label_w);
                    ui.add(egui::Label::new(RichText::new(f.label).color(p.text_muted)).truncate());
                });
                ui.add_space(16.0);
                let w = ui.available_width();
                if f.path {
                    let job = path_job(ui, &f.value, w, None);
                    ui.add(egui::Label::new(job)).on_hover_text(&f.value);
                } else {
                    ui.add(egui::Label::new(RichText::new(&f.value).color(f.color.unwrap_or(p.text))).wrap());
                }
            });
        }
    });
}

/// A file path on one line at most `max_w` wide: the file name in the text colour (medium),
/// after its folder in the muted colour, home-relative (`~/…`) and shortened in the middle
/// when it does not fit. `size` is the font size (the body's when `None`).
pub(crate) fn path_job(ui: &Ui, path: &str, max_w: f32, size: Option<f32>) -> egui::text::LayoutJob {
    let p = style::pal(ui);
    let size = size.unwrap_or_else(|| style::body_size(ui.ctx()));
    let shown = home_relative(path);
    let cut = shown.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    let (folder, name) = shown.split_at(cut);
    let regular = egui::FontId::proportional(size);
    let medium = style::medium(ui.ctx(), size);
    let width = |s: &str, font: &egui::FontId| ui.painter().layout_no_wrap(s.to_string(), font.clone(), Color32::PLACEHOLDER).size().x;
    let name_w = width(name, &medium);
    // The folder, its middle left out until it fits beside the name.
    let mut folder = folder.to_string();
    if width(&folder, &regular) + name_w > max_w {
        let chars: Vec<char> = folder.chars().collect();
        let elided = |keep: usize| -> String {
            let (head, tail) = (keep / 2, keep - keep / 2);
            chars[..head].iter().chain(std::iter::once(&'…')).chain(chars[chars.len() - tail..].iter()).collect()
        };
        // The most characters kept that fit (a binary search: the width grows with them).
        let (mut lo, mut hi) = (0, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if width(&elided(mid), &regular) + name_w <= max_w { lo = mid } else { hi = mid - 1 }
        }
        folder = if lo == 0 && width(&elided(0), &regular) + name_w > max_w { String::new() } else { elided(lo) };
    }
    let mut job = egui::text::LayoutJob::default();
    job.append(&folder, 0.0, egui::TextFormat::simple(regular, p.text_muted));
    job.append(name, 0.0, egui::TextFormat::simple(medium, p.text));
    // A name that is too long on its own ends in "…".
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
    job
}

/// A path with the home folder shown as `~` (lists and summaries; fields keep the real path).
pub(crate) fn home_relative(path: &str) -> String {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    match path.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() && (rest.starts_with('/') || rest.starts_with('\\')) => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// The field of a file path: while it is not being edited it shows the path as [`path_job`]
/// (the file name always visible); a click edits the path itself. True when the text changed.
pub(crate) fn path_field(ui: &mut Ui, id: Id, path: &mut String, hint: &str) -> bool {
    let edit_id = id.with("edit");
    let editing = ui.memory(|m| m.has_focus(edit_id));
    if editing || path.trim().is_empty() {
        let r = ui.add(line(path).id(edit_id).hint_text(hint).desired_width(f32::INFINITY));
        return r.changed();
    }
    let p = style::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), style::CONTROL_H), Sense::click());
    let resp = resp.on_hover_text(path.as_str()).on_hover_cursor(egui::CursorIcon::Text);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, path.as_str()));
    if ui.is_rect_visible(rect) {
        let visuals = if resp.hovered() { ui.visuals().widgets.hovered } else { ui.visuals().widgets.inactive };
        let fill = ui.visuals().text_edit_bg_color();
        ui.painter().rect(rect, visuals.corner_radius, fill, visuals.bg_stroke, egui::StrokeKind::Inside);
        let inner = rect.shrink2(vec2(13.0, 0.0));
        let galley = ui.painter().layout_job(path_job(ui, path, inner.width(), None));
        let pos = pos2(inner.left(), rect.center().y - galley.size().y / 2.0);
        ui.painter().with_clip_rect(inner.intersect(ui.clip_rect())).galley(pos, galley, p.text);
    }
    if resp.clicked() {
        // Edit with the cursor at the end (the file name).
        let mut state = egui::TextEdit::load_state(ui.ctx(), edit_id).unwrap_or_default();
        let end = egui::text::CCursor::new(path.chars().count());
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(ui.ctx(), edit_id);
        ui.memory_mut(|m| m.request_focus(edit_id));
        ui.ctx().request_repaint();
    }
    false
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

/// An action inside a page's content (Add query, Select all, Copy…): a small outlined button,
/// so it reads as a button and not as a label beside the fields.
pub(crate) fn action(ui: &mut Ui, glyph: Option<&str>, text: &str) -> Response {
    kit::button_ex(ui, Kind::Secondary, glyph, text, true)
}

/// The height a list or table inside the scrolling body may take so that it ends above the
/// footer: what is left of the visible body under the cursor (less `reserve` for the parts
/// under it), between `least` and `wanted`.
pub(crate) fn fit_height(ui: &Ui, wanted: f32, least: f32, reserve: f32) -> f32 {
    // Measured from the top of the body's content, not from the scrolled view: the height does
    // not change while the body scrolls.
    let Some((top, view_h)) = ui.data(|d| d.get_temp::<(f32, f32)>(body_key())) else { return wanted };
    let left = view_h - 18.0 - (ui.cursor().top() - top) - reserve - 16.0;
    left.clamp(least.min(wanted), wanted)
}

/// Where [`body`] keeps the top of its content and the height of its view for [`fit_height`].
fn body_key() -> Id {
    Id::new("ternarygrid_wizard_body")
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

/// The height of `parts::segmented` (as the other controls).
pub(crate) const SEGMENTED_H: f32 = style::CONTROL_H;

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

/// A label of `label_w` and the control(s) `add` draws on its right: the kit's form row (the
/// label centred on the controls' first line, 16 from them).
pub(crate) fn form_row<R>(ui: &mut Ui, label: &str, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    parts::form_row(ui, label, label_w, add)
}

/// See [`Form::check_row`]: a row as high as its check boxes, 16 apart, the label on their line.
fn check_row_dyn(ui: &mut Ui, label: &str, label_w: f32, add: &mut dyn FnMut(&mut Ui)) {
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 0.0;
        ui.spacing_mut().item_spacing.x = 16.0;
        parts::form_row(ui, label, label_w, |ui| add(ui));
    });
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

    /// A row whose only control is a segmented one (as high as the other controls).
    pub fn seg_row<R>(&mut self, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
        let h = self.ui.spacing().interact_size.y;
        self.ui.spacing_mut().interact_size.y = SEGMENTED_H;
        let r = self.row(label, add);
        self.ui.spacing_mut().interact_size.y = h;
        r
    }

    /// A row of check boxes, 16 apart, the label on their line.
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

/// A card of read-only form rows (summaries, results), the labels [`LABEL_W`] wide.
pub(crate) fn facts(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, gap: f32, rows: &[Fact]) {
    card(ui, 14, |ui| {
        // The values where the other pages' controls start (the card's border and padding in).
        ui.push_id(Id::new(id), |ui| fact_rows(ui, rows, LABEL_W - 15.0, gap));
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
    let shown = kit::card(ui).corner_radius(CornerRadius::same(style::RADIUS_LG)).inner_margin(Margin::same(12)).show(ui, |ui| {
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
    // Opened: the scrolling body brings it into view (as much of it as fits).
    if toggled && *open && fill {
        ui.scroll_to_rect(shown.response.rect, None);
    }
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

/// A row (or an object) that failed in a run.
pub(crate) struct Failure {
    /// The table it belongs to (migrations of several tables).
    pub object: Option<String>,
    pub row: Option<u64>,
    pub column: Option<String>,
    pub message: String,
}

/// The failures of a run in one table: identical errors grouped ("5 × …") with their rows, the
/// whole text on hover; the Table and Column columns only when some failure has one.
pub(crate) fn failures_table(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, failures: &[Failure], max_h: f32) {
    let p = style::pal(ui);
    // (object, column, message) → rows, in the order of their first failure.
    let mut groups: Vec<(&Option<String>, &Option<String>, &str, Vec<u64>, usize)> = Vec::new();
    for f in failures {
        match groups.iter_mut().find(|g| g.0 == &f.object && g.1 == &f.column && g.2 == f.message) {
            Some(g) => {
                g.3.extend(f.row);
                g.4 += 1;
            }
            None => groups.push((&f.object, &f.column, &f.message, f.row.into_iter().collect(), 1)),
        }
    }
    let objects = failures.iter().any(|f| f.object.is_some());
    let columns = failures.iter().any(|f| f.column.is_some());
    let mut cols = Vec::new();
    if objects {
        cols.push(col(tr("Table"), W::Fr(0.8)));
    }
    cols.push(col(tr("Row"), W::Px(110.0)));
    if columns {
        cols.push(col(tr("Column"), W::Px(120.0)));
    }
    cols.push(col(tr("Error"), W::Fr(2.0)));
    let kinds: Vec<u8> = [(objects, 0), (true, 1), (columns, 2), (true, 3)].iter().filter(|(on, _)| *on).map(|(_, k)| *k).collect();
    table(
        ui,
        id,
        &cols,
        30.0,
        Some(max_h),
        groups.len(),
        |_, _| {},
        |ui, r, c| {
            let (object, column, message, rows, n) = &groups[r];
            match kinds[c] {
                0 => {
                    ui.add(egui::Label::new(object.as_deref().unwrap_or_default()).truncate());
                }
                1 => {
                    let all = row_ranges(rows);
                    let shown = if all.len() > 3 { format!("{} …", all[..3].join(", ")) } else { all.join(", ") };
                    let r = ui.add(egui::Label::new(if shown.is_empty() { "—".to_string() } else { shown }).truncate());
                    if all.len() > 3 {
                        r.on_hover_text(all.join(", "));
                    }
                }
                2 => {
                    ui.add(egui::Label::new(RichText::new(column.as_deref().unwrap_or_default()).color(p.text_muted)).truncate());
                }
                _ => {
                    ui.label(RichText::new(icon::X_CIRCLE).font(style::icon_font(13.0)).color(p.err));
                    let text = if *n > 1 { format!("{n} × {message}") } else { message.to_string() };
                    ui.add(egui::Label::new(text).truncate()).on_hover_text(*message);
                }
            }
        },
    );
}

/// Row numbers as ranges: `[1, 2, 3, 7, 9, 10]` → `1–3`, `7`, `9–10`.
fn row_ranges(rows: &[u64]) -> Vec<String> {
    let mut rows = rows.to_vec();
    rows.sort_unstable();
    rows.dedup();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for r in rows {
        match out.last_mut() {
            Some((_, end)) if *end + 1 == r => *end = r,
            _ => out.push((r, r)),
        }
    }
    out.into_iter().map(|(a, b)| if a == b { a.to_string() } else { format!("{a}–{b}") }).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_as_ranges() {
        assert_eq!(row_ranges(&[3, 1, 2, 7, 9, 10, 2]), vec!["1–3", "7", "9–10"]);
        assert!(row_ranges(&[]).is_empty());
    }

    #[test]
    fn paths_keep_their_file_name() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let path = "/var/folders/rm/a_very_long_folder_name_of_the_system/T/customers_october.csv";
            let job = path_job(ui, path, 260.0, Some(13.0));
            let text = job.text.clone();
            assert!(text.ends_with("customers_october.csv"), "{text}");
            assert!(text.contains('…') && text.starts_with("/var"), "{text}");
            assert!(ui.painter().layout_job(job).size().x <= 260.5);
        });
        out.textures_delta.clear();
    }
}
