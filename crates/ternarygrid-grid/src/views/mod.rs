//! Alternative result views (FR-D5): record, text, JSON and XML.

mod docs;
mod record;
mod text;

pub use docs::{DocumentKind, DocumentView, DocumentViewState};
pub use record::{RecordView, RecordViewState};
pub use text::{MAX_TEXT_ROWS, TextView, TextViewState, render_text_table};

use egui::{ScrollArea, TextStyle, Ui};
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

/// Show pre-rendered lines in a virtualised, monospace scroll area.
pub(crate) fn show_lines(ui: &mut Ui, id_salt: impl std::hash::Hash + std::fmt::Debug, lines: &Arc<Vec<String>>, max_chars: usize) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.fonts_mut(|f| f.row_height(&font));
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
    let width = (max_chars as f32 * char_w + 16.0).max(ui.available_width());
    ScrollArea::both().id_salt(id_salt).auto_shrink([false, false]).show_rows(ui, row_h, lines.len(), |ui, range| {
        ui.set_min_width(width);
        for i in range {
            if let Some(line) = lines.get(i) {
                ui.add(egui::Label::new(egui::RichText::new(line.as_str()).monospace()).extend());
            }
        }
    });
    if lines.is_empty() {
        ui.weak(tr("(empty)"));
    }
}
