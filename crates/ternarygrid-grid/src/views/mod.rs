//! Alternative result views (FR-D5): record, text, JSON and XML.

mod docs;
mod record;
mod text;

pub use docs::{DocumentKind, DocumentView, DocumentViewState};
pub use record::{RecordView, RecordViewState};
pub use text::{MAX_TEXT_ROWS, TextView, TextViewState, render_text_table};

use egui::{ScrollArea, Ui};
use std::sync::Arc;
use std::time::Duration;
use ternarygrid_core::tr;

/// Seconds between rebuilds of a cache over the fetched rows (text, JSON / XML
/// views, calc panel, column summaries) while only rows are appended to it.
const APPEND_REBUILD: f64 = 0.5;

/// Rebuild a cache over the fetched rows now? At once when anything but the
/// number of rows it covers changed (`more_rows_only` false); else (a
/// streaming fetch appends rows every frame) at most every [`APPEND_REBUILD`]
/// seconds after `built_at` (`ctx` time), with a repaint requested for then,
/// so the cache catches up with the last rows.
pub(crate) fn rebuild_due(ctx: &egui::Context, more_rows_only: bool, built_at: f64) -> bool {
    if !more_rows_only {
        return true;
    }
    let wait = APPEND_REBUILD - (ctx.input(|i| i.time) - built_at);
    if wait <= 0.0 {
        return true;
    }
    ctx.request_repaint_after(Duration::from_secs_f64(wait));
    false
}

/// The bar over a view (Copy …): 16 / 8 around its controls, a line under it.
pub(crate) fn view_bar(ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)) {
    let pal = ternarygrid_ui::pal(ui);
    let bar = egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 8)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            add(ui);
        });
    });
    ui.painter().hline(ui.max_rect().x_range(), bar.response.rect.bottom() + 0.5, egui::Stroke::new(1.0, pal.border));
    ui.add_space(1.0);
}

/// The colours of JSON's keys, strings and numbers (the JSON view and the value panel's tree).
pub(crate) fn json_token_colors(dark: bool) -> (egui::Color32, egui::Color32, egui::Color32) {
    let rgb = |c: u32| egui::Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8);
    if dark { (rgb(0x7aa7ff), rgb(0xe6a96b), rgb(0xc39bf2)) } else { (rgb(0x2554c7), rgb(0xa5531a), rgb(0x7a3fc4)) }
}

/// Colours of a JSON line: keys, strings, numbers (and `null`, `true`, `false` faint).
fn json_job(line: &str, font: &egui::FontId, dark: bool, text: egui::Color32, faint: egui::Color32) -> egui::text::LayoutJob {
    let (key, string, number) = json_token_colors(dark);
    let mut job = egui::text::LayoutJob::default();
    let mut add = |s: &str, c: egui::Color32| job.append(s, 0.0, egui::TextFormat::simple(font.clone(), c));
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut plain = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let token = if b == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(bytes.len());
            let is_key = line[end..].trim_start().starts_with(':');
            Some((end, if is_key { key } else { string }))
        } else if b == b'-' || b.is_ascii_digit() {
            let j = i + line[i..].find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))).unwrap_or(line.len() - i);
            Some((j, number))
        } else if line[i..].starts_with("null") || line[i..].starts_with("true") || line[i..].starts_with("false") {
            let n = if line[i..].starts_with("false") { 5 } else { 4 };
            Some((i + n, faint))
        } else {
            None
        };
        match token {
            Some((end, color)) => {
                if plain < i {
                    add(&line[plain..i], text);
                }
                add(&line[i..end], color);
                i = end;
                plain = end;
            }
            None => i += 1,
        }
    }
    if plain < line.len() {
        add(&line[plain..], text);
    }
    job
}

/// Show pre-rendered lines in a virtualised scroll area on the surface: monospace 12.5 on
/// lines of 20, 14 / 16 from the edges (JSON coloured when `json`).
pub(crate) fn show_lines(ui: &mut Ui, id_salt: impl std::hash::Hash + std::fmt::Debug, lines: &Arc<Vec<String>>, max_chars: usize, json: bool) {
    let pal = ternarygrid_ui::pal(ui);
    let font = egui::FontId::monospace(12.5);
    let row_h = 20.0;
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
    let dark = ui.visuals().dark_mode;
    ui.painter().rect_filled(ui.available_rect_before_wrap(), 0.0, pal.surface);
    egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 14, bottom: 0 }).show(ui, |ui| {
        let width = (max_chars as f32 * char_w + 16.0).max(ui.available_width());
        ScrollArea::both().id_salt(id_salt).auto_shrink([false, false]).show_rows(ui, row_h, lines.len(), |ui, range| {
            ui.set_min_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                if let Some(line) = lines.get(i) {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, row_h), egui::Sense::hover());
                    // Painted, not a label: the line for screen readers.
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, line.as_str()));
                    let galley = if json {
                        ui.fonts_mut(|f| f.layout_job(json_job(line, &font, dark, pal.text, pal.text_faint)))
                    } else {
                        ui.fonts_mut(|f| f.layout_no_wrap(line.clone(), font.clone(), pal.text))
                    };
                    ui.painter().galley(egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0), galley, pal.text);
                }
            }
        });
        if lines.is_empty() {
            ui.weak(tr("(empty)"));
        }
    });
}
