//! Small reusable widgets: virtualised tables, property sheets, status lines.

use crate::backend::Loadable;
use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use ternarygrid_core::meta::Property;
use ternarygrid_core::tr;

/// Response of [`simple_table`].
#[derive(Default)]
pub struct TableResponse {
    pub clicked: Option<usize>,
    pub double_clicked: Option<usize>,
    pub context_row: Option<usize>,
}

/// Space before and after the text of a table cell.
const CELL_PAD: f32 = 10.0;

/// How the cells of a column of [`styled_table`] look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellStyle {
    /// Text colour, 13 points.
    #[default]
    Plain,
    /// Monospace, 12 points (types, definitions, values).
    Mono,
    /// Muted text.
    Muted,
    /// Monospace in the muted colour (defaults, long definitions).
    MonoMuted,
    /// Faint text (ordinals).
    Faint,
    /// In the success colour (a check mark).
    Ok,
    /// A small pill (kinds: PRIMARY, UNIQUE, CHECK, depends on, used by …).
    Chip,
}

/// The main action of a dialog or wizard: white text on the accent colour.
pub fn primary_button(ui: &Ui, glyph: &str, text: impl Into<String>) -> egui::Button<'static> {
    let p = ternarygrid_ui::pal(ui);
    let font = ternarygrid_ui::style::medium(ui.ctx(), ternarygrid_ui::style::body_size(ui.ctx()));
    egui::Button::new(RichText::new(format!("{glyph}  {}", text.into())).font(font).color(p.on_accent)).fill(p.accent).stroke(egui::Stroke::NONE)
}

/// Escape pressed on the dialog window shown in `layer` while it is the top window,
/// no pop-up is open (a combo box list closes first) and the keyboard focus is not
/// elsewhere (an editor behind a non-modal dialog): it closes like Cancel.
pub fn escape_closes(ctx: &egui::Context, layer: Option<egui::LayerId>) -> bool {
    if layer.is_none() || ctx.top_layer_id() != layer || egui::Popup::is_any_open(ctx) || !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return false;
    }
    let focus = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id));
    focus.is_none_or(|r| Some(r.layer_id) == layer)
}

/// A heading with a coloured icon.
pub fn icon_heading(ui: &mut Ui, glyph: &str, text: impl Into<String>) -> egui::Response {
    let mut job = egui::text::LayoutJob::default();
    let style = ui.style().clone();
    let font = egui::FontSelection::Default;
    ternarygrid_icons::ic(glyph).heading().append_to(&mut job, &style, font.clone(), egui::Align::Center);
    RichText::new(format!(" {}", text.into())).heading().append_to(&mut job, &style, font, egui::Align::Center);
    ui.label(job)
}

/// Virtualised, striped, resizable table of text cells.
pub fn simple_table(ui: &mut Ui, id_salt: impl std::hash::Hash + std::fmt::Debug, headers: &[&str], rows: &[Vec<String>], selected: Option<usize>) -> TableResponse {
    styled_table(ui, id_salt, headers, rows, selected, &[])
}

/// [`simple_table`] with a [`CellStyle`] per column (missing ones are plain): a bordered,
/// rounded frame, the header 32 high on the header colour, rows of 30 with 10 at the sides.
pub fn styled_table(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    headers: &[&str],
    rows: &[Vec<String>],
    selected: Option<usize>,
    styles: &[CellStyle],
) -> TableResponse {
    // In its own ui: the column lines end at the last row, not at the bottom of the panel.
    let id = egui::Id::new(&id_salt);
    ternarygrid_ui::parts::bordered(ui).inner_margin(egui::Margin::ZERO).show(ui, |ui| ui.scope(|ui| table_in(ui, id, headers, rows, selected, styles)).inner).inner
}

/// The colours of a [`CellStyle::Chip`]: keys warn, checks and dependants in the accent.
fn chip_colors(text: &str, pal: &ternarygrid_ui::style::Palette, dark: bool) -> (egui::Color32, egui::Color32) {
    match text {
        "PRIMARY" | "PRIMARY KEY" => (pal.warn, if dark { egui::Color32::from_rgb(0x4b, 0x44, 0x3b) } else { egui::Color32::from_rgb(0xf3, 0xea, 0xd6) }),
        "CHECK" => (pal.accent_text, pal.accent_soft),
        t if t == tr("used by") => (pal.accent_text, pal.accent_soft),
        _ => (pal.text_muted, pal.subtle),
    }
}

fn table_in(ui: &mut Ui, id: egui::Id, headers: &[&str], rows: &[Vec<String>], selected: Option<usize>, styles: &[CellStyle]) -> TableResponse {
    let mut out = TableResponse::default();
    let row_h = 31.0;
    let pal = ternarygrid_ui::pal(ui);
    let dark = ui.visuals().dark_mode;
    let style_of = |i: usize| styles.get(i).copied().unwrap_or_default();
    let chip_font = ternarygrid_ui::style::semibold(ui.ctx(), 11.0);
    let body_font = egui::TextStyle::Body.resolve(ui.style());
    let font_of = |st: CellStyle| match st {
        CellStyle::Mono | CellStyle::MonoMuted => egui::FontId::monospace(12.0),
        CellStyle::Chip => chip_font.clone(),
        _ => body_font.clone(),
    };
    // Every column at least as wide as its header (plus the cell padding).
    let header_font = ternarygrid_ui::style::semibold(ui.ctx(), 12.0);
    let text_w = |ui: &mut Ui, s: &str, f: &egui::FontId| ui.fonts_mut(|fs| fs.layout_no_wrap(s.to_string(), f.clone(), egui::Color32::WHITE).rect.width());
    let header_w: Vec<f32> = headers.iter().map(|h| text_w(ui, h, &header_font) + 2.0 * CELL_PAD + 4.0).collect();
    // Initially as wide as the first rows' text: truncated labels never widen an auto column.
    let content_w: Vec<f32> = (0..headers.len())
        .map(|i| {
            let f = font_of(style_of(i));
            let extra = if style_of(i) == CellStyle::Chip { 14.0 } else { 4.0 };
            let widest = rows.iter().take(100).filter_map(|r| r.get(i)).map(|c| text_w(ui, c.lines().next().unwrap_or(""), &f) + extra).fold(0.0, f32::max);
            widest + 2.0 * CELL_PAD
        })
        .collect();
    // Rows edge to edge, no lines between the columns (their borders show when hovered).
    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
    ui.visuals_mut().widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
    let mut tb = TableBuilder::new(ui)
        .id_salt(id)
        .striped(true)
        .resizable(true)
        .sense(egui::Sense::click())
        // As tall as its rows: the column lines end at the last row.
        .auto_shrink([false, true])
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
    for (i, w) in header_w.iter().enumerate() {
        tb = if i + 1 == headers.len() {
            tb.column(Column::remainder().at_least(w.max(60.0)).clip(true))
        } else {
            tb.column(Column::initial(content_w[i].clamp(w.max(40.0), 480.0)).at_least(w.max(40.0)).clip(true).resizable(true))
        };
    }
    tb.header(33.0, |mut header| {
        for h in headers {
            header.col(|ui| {
                // The header row on the header colour, its names muted and semibold, a line under it.
                let r = ui.max_rect().expand2(egui::vec2(4.0, 0.0));
                ui.painter().rect_filled(r, 0.0, pal.header);
                ui.painter().hline(r.x_range(), r.bottom() - 0.5, egui::Stroke::new(1.0, pal.border));
                ui.add_space(CELL_PAD);
                ui.label(RichText::new(*h).font(header_font.clone()).color(pal.text_muted));
            });
        }
    })
    .body(|body| {
        body.rows(row_h, rows.len(), |mut row| {
            let i = row.index();
            row.set_selected(selected == Some(i));
            if let Some(cells) = rows.get(i) {
                for (ci, c) in cells.iter().take(headers.len()).enumerate() {
                    let st = style_of(ci);
                    row.col(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.add_space(CELL_PAD);
                        let short: String = c.chars().take(300).collect();
                        let first_line = short.lines().next().unwrap_or("");
                        if st == CellStyle::Chip {
                            if !first_line.is_empty() {
                                let (fg, bg) = chip_colors(first_line, &pal, dark);
                                let g = ui.painter().layout_no_wrap(first_line.to_string(), font_of(st), fg);
                                let (r, _) = ui.allocate_exact_size(egui::vec2(g.size().x + 14.0, 18.0), egui::Sense::hover());
                                ui.painter().rect_filled(r, 9, bg);
                                ui.painter().galley(r.center() - g.size() / 2.0, g, fg);
                            }
                            return;
                        }
                        // A leading icon (e.g. the key of primary key columns) in its colour.
                        let mut chars = first_line.chars();
                        let icon = chars.next().map(String::from).and_then(|g| ternarygrid_icons::color(&g).map(|c| (g, c)));
                        let text = match icon {
                            Some((g, color)) => {
                                ui.label(RichText::new(g).size(12.0).color(color));
                                chars.as_str().trim_start()
                            }
                            None => first_line,
                        };
                        let color = match st {
                            CellStyle::Muted | CellStyle::MonoMuted => pal.text_muted,
                            CellStyle::Faint => pal.text_faint,
                            CellStyle::Ok => pal.ok,
                            _ => pal.text,
                        };
                        let resp = ui.add(egui::Label::new(RichText::new(text).font(font_of(st)).color(color)).truncate().selectable(false));
                        if c.len() > first_line.len() {
                            resp.on_hover_text(c.chars().take(4000).collect::<String>());
                        }
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
    out
}

/// Name/value list, grouped when properties carry a group: each group's title (12, faint,
/// semibold), then its rows in a bordered, striped list (the name muted in 180, 7 / 14 around).
pub fn property_grid(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug + Copy, props: &[Property]) {
    let _ = id;
    let mut groups: Vec<Option<&str>> = Vec::new();
    for p in props {
        let g = p.group.as_deref();
        if !groups.contains(&g) {
            groups.push(g);
        }
    }
    let pal = ternarygrid_ui::pal(ui);
    let stripe = if ui.visuals().dark_mode { egui::Color32::from_rgb(0x26, 0x28, 0x30) } else { egui::Color32::from_rgb(0xfa, 0xfa, 0xfb) };
    for (gi, g) in groups.into_iter().enumerate() {
        if let Some(name) = g {
            if gi > 0 {
                ui.add_space(10.0);
            }
            ui.label(RichText::new(name).font(ternarygrid_ui::style::semibold(ui.ctx(), 12.0)).color(pal.text_faint));
            ui.add_space(2.0);
        }
        ternarygrid_ui::parts::bordered(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, p) in props.iter().filter(|p| p.group.as_deref() == g).enumerate() {
                let bg = ui.painter().add(egui::Shape::Noop);
                let row = egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 7)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        ui.add_sized([180.0, 16.0], egui::Label::new(RichText::new(&p.name).color(pal.text_muted)).truncate());
                        ui.add(egui::Label::new(RichText::new(&p.value).color(if p.value == "NULL" { pal.text_faint } else { pal.text })).wrap());
                    });
                });
                let fill = if i % 2 == 1 { stripe } else { pal.surface };
                ui.painter().set(bg, egui::Shape::rect_filled(row.response.rect, 0.0, fill));
            }
        });
    }
}

/// Shows a spinner / error with retry for a [`Loadable`]. Returns true when
/// the value is loaded (caller draws it), and sets `retry` when the user asks.
pub fn load_state<T>(ui: &mut Ui, value: &Loadable<T>, retry: &mut bool) -> bool {
    match value {
        Loadable::Loaded(_) => true,
        Loadable::NotLoaded | Loadable::Loading(_) => {
            ternarygrid_ui::parts::spinner_label(ui, tr("Loading…"));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            false
        }
        Loadable::Failed(e) => {
            error_box(ui, &e.full_text());
            if ternarygrid_ui::kit::secondary(ui, Some(icon::ARROW_CLOCKWISE), tr("Retry")).clicked() {
                *retry = true;
            }
            false
        }
    }
}

pub fn error_box(ui: &mut Ui, text: &str) {
    ternarygrid_ui::parts::callout(ui, ternarygrid_ui::parts::Tone::Error, None, text, |_| {});
}

/// Read-only, selectable monospace text (DDL, scripts) coloured by `tokens`.
pub fn code_view(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, text: &str, tokens: &[(std::ops::Range<usize>, ternarygrid_editor::TokenStyle)]) {
    let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| ui.fonts_mut(|f| f.layout_job(crate::sqllang::highlight_job(buf.as_str(), tokens, &theme, font.clone())));
    egui::ScrollArea::both().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
        let mut t = text;
        ui.add(egui::TextEdit::multiline(&mut t).code_editor().desired_width(f32::INFINITY).interactive(true).layouter(&mut layouter));
    });
}

/// Read-only code in a bordered box as the mockups' code blocks: line numbers in a faint
/// gutter, monospace 12.5 on lines of 20, coloured by `tokens`, cut at the right (no wrap);
/// it scrolls past `max_height`.
pub fn code_block(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, text: &str, tokens: &[(std::ops::Range<usize>, ternarygrid_editor::TokenStyle)], max_height: f32) {
    let pal = ternarygrid_ui::pal(ui);
    let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
    let font = egui::FontId::monospace(12.5);
    let mut job = crate::sqllang::highlight_job(text, tokens, &theme, font.clone());
    for s in &mut job.sections {
        s.format.line_height = Some(20.0);
    }
    job.wrap.max_width = f32::INFINITY;
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let lines = text.lines().count().max(1);
    let gutter = 32.0 + 14.0;
    ternarygrid_ui::parts::bordered(ui).inner_margin(egui::Margin::symmetric(0, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::both().id_salt(egui::Id::new(id)).max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(gutter + galley.size().x + 12.0, lines as f32 * 20.0), egui::Sense::hover());
            for i in 0..lines {
                let y = rect.top() + i as f32 * 20.0 + 10.0;
                ui.painter().text(egui::pos2(rect.left() + 32.0, y), egui::Align2::RIGHT_CENTER, (i + 1).to_string(), font.clone(), pal.text_faint);
            }
            ui.painter().galley(egui::pos2(rect.left() + gutter, rect.top()), galley, pal.text);
        });
    });
}

/// Human readable byte size.
pub fn format_bytes(n: i64) -> String {
    let n = n as f64;
    if n < 1024.0 {
        format!("{n:.0} B")
    } else if n < 1024.0 * 1024.0 {
        format!("{:.1} KB", n / 1024.0)
    } else if n < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", n / 1_048_576.0)
    } else {
        format!("{:.2} GB", n / 1_073_741_824.0)
    }
}

/// Human readable duration.
pub fn format_duration(d: std::time::Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else if ms < 60_000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        let s = d.as_secs();
        format!("{}m {:02}s", s / 60, s % 60)
    }
}
