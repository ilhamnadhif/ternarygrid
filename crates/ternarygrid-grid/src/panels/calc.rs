//! Calc panel: statistics of the selected cells (FR-D5).

use crate::calc::{CalcStats, compute_stats, format_number};
use crate::grid::{DataGridState, Selection};
use crate::model::ResultModel;
use egui::{RichText, Ui};
use ternarygrid_core::{Value, tr, trf};

/// Cells analysed at most.
pub const MAX_CALC_CELLS: usize = 500_000;

/// Cached statistics of the selection.
#[derive(Debug, Clone, Default)]
pub struct CalcPanelState {
    /// Content generation, selection, visible columns and the selected rows that exist.
    key: Option<(u64, Selection, Vec<usize>, usize)>,
    /// When the statistics were computed (`ui` time).
    built_at: f64,
    stats: CalcStats,
    /// Selected rows of the table (by function), for ⌘C.
    selected: Vec<String>,
    /// Row a Shift+click extends from.
    anchor: Option<usize>,
    /// The function whose value was copied last, and when: "Copied" shows a moment.
    copied: Option<(String, f64)>,
}

/// A row of the table: the function, its value as shown, and as copied (plain:
/// no thousands separators, the mode without its count).
struct StatRow {
    name: &'static str,
    shown: String,
    copy: String,
}

/// Statistics of selected cells.
pub struct CalcPanel<'a> {
    model: &'a ResultModel,
    grid: &'a DataGridState,
    state: &'a mut CalcPanelState,
}

/// Compute statistics of the current selection (at most `limit` cells).
pub fn selection_stats(model: &ResultModel, grid: &DataGridState, limit: usize) -> CalcStats {
    let visible = grid.visible_columns();
    let sel = grid.selection();
    let nrows = model.row_count();
    let col_pos = sel.columns(visible.len());
    let cols: Vec<usize> = col_pos.iter().map(|&p| visible[p]).collect();
    if cols.is_empty() {
        return CalcStats::default();
    }
    let row_limit = (limit / cols.len()).max(1) + 1;
    let rows = sel.rows(nrows, row_limit);
    let values = model.values(&rows, &cols);
    let mut flat: Vec<&Value> = Vec::with_capacity(values.len() * cols.len());
    let all = sel.is_all();
    for (r, vals) in rows.iter().zip(&values) {
        for (v, &p) in vals.iter().zip(&col_pos) {
            if all || sel.contains(*r, p) {
                flat.push(v);
            }
        }
    }
    let mut stats = compute_stats(flat, limit);
    // Every selected cell was read unless rows were cut (`cell_count` counts
    // overlapping ranges twice).
    if rows.len() >= row_limit && sel.cell_count(nrows, visible.len()) > stats.count {
        stats.truncated = true;
    }
    stats
}

impl<'a> CalcPanel<'a> {
    /// New panel.
    pub fn new(model: &'a ResultModel, grid: &'a DataGridState, state: &'a mut CalcPanelState) -> Self {
        CalcPanel { model, grid, state }
    }

    /// Show the panel.
    pub fn show(self, ui: &mut Ui) {
        let sel = self.grid.selection();
        // Rows fetched below the selection change nothing; a selection to the end
        // (columns, all) grows with a streaming fetch: computed at most every half second.
        let extent = sel.ranges().iter().map(|r| r.r1.saturating_add(1)).max().unwrap_or(0).min(self.model.row_count());
        let key = (self.model.content_generation(), sel.clone(), self.grid.visible_columns(), extent);
        let more_rows_only = self.state.key.as_ref().is_some_and(|k| (k.0, &k.1, &k.2) == (key.0, &key.1, &key.2));
        if self.state.key.as_ref() != Some(&key) && crate::views::rebuild_due(ui.ctx(), more_rows_only, self.state.built_at) {
            self.state.stats = selection_stats(self.model, self.grid, MAX_CALC_CELLS);
            self.state.key = Some(key);
            self.state.built_at = ui.input(|i| i.time);
            // Other cells: other rows (the mode, lengths come and go).
            self.state.anchor = None;
        }
        let s = &self.state.stats;
        if s.count == 0 {
            ui.weak(tr("Select cells to calculate statistics"));
            return;
        }
        let s = s.clone();
        // What is counted: "14 cells of total selected".
        let cols = self.grid.selected_columns();
        let faint = ternarygrid_ui::pal(ui).text_faint;
        let what = match cols.as_slice() {
            [c] => trf("{} cells of {} selected", &[&crate::format::group_thousands(&s.count.to_string()), &self.model.column_name(*c)]),
            _ => trf("{} cells selected", &[&crate::format::group_thousands(&s.count.to_string())]),
        };
        ui.label(RichText::new(what).size(12.0).color(faint));
        ui.add_space(6.0);
        let count = |name: &'static str, v: usize| StatRow { name, shown: crate::format::group_thousands(&v.to_string()), copy: v.to_string() };
        let plain = |name: &'static str, v: String| StatRow { name, shown: v.clone(), copy: v };
        let fmt_v = |v: &Option<Value>| v.as_ref().map(|v| v.to_text().into_owned()).unwrap_or_default();
        let mut rows = vec![count(tr("Count"), s.count), count(tr("Distinct"), s.distinct), count(tr("NULLs"), s.nulls)];
        if s.numeric_count > 0 {
            rows.push(count(tr("Numbers"), s.numeric_count));
            rows.push(plain(tr("Sum"), s.sum_text().unwrap_or_default()));
            rows.push(plain(tr("Average"), s.avg.map(format_number).unwrap_or_default()));
            rows.push(plain(tr("Median"), s.median.map(format_number).unwrap_or_default()));
        }
        // The most frequent value, when one repeats.
        if let Some((value, times)) = s.mode.as_ref().filter(|(_, t)| *t > 1) {
            let shown = trf("{} (×{})", &[value, &crate::format::group_thousands(&times.to_string())]);
            rows.push(StatRow { name: tr("Mode"), shown, copy: value.clone() });
        }
        rows.push(plain(tr("Min"), fmt_v(&s.min)));
        rows.push(plain(tr("Max"), fmt_v(&s.max)));
        if let (Some(a), Some(b)) = (s.min_len, s.max_len) {
            rows.push(count(tr("Min length"), a));
            rows.push(count(tr("Max length"), b));
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            stats_table(ui, &rows, self.state);
            ui.add_space(10.0);
            let keys = if cfg!(target_os = "macos") { "⌘C" } else { "Ctrl+C" };
            ui.add(egui::Label::new(RichText::new(trf("Copy: click a row and press {}, or the copy button at its end, or right-click.", &[&keys])).size(11.5).color(faint)).wrap());
            if s.truncated {
                ui.add_space(4.0);
                ui.weak(trf("Only the first {} cells were analysed", &[&crate::format::group_thousands(&MAX_CALC_CELLS.to_string())]));
            }
        });
    }
}

/// A two column table (function, value) drawn like the result grid. A click
/// selects a row (Shift / ⌘ more), ⌘C copies the selected values, the copy button
/// at a hovered row's end copies its value, a double click too, and the right-click
/// menu copies a value, a row or the whole table.
fn stats_table(ui: &mut Ui, rows: &[StatRow], state: &mut CalcPanelState) {
    use egui::{Align2, Sense, Stroke, TextStyle, pos2, vec2};
    use egui_phosphor::regular as icon;
    let theme = crate::theme::GridTheme::from_visuals(ui.visuals());
    let pal = ternarygrid_ui::pal(ui);
    // The mockups' table: a 32 high header, rows of 30 (and their lines), the value column 1.2 : 1.
    let (head_h, row_h) = (33.0, 31.0);
    let width = ui.available_width();
    let name_w = (width / 2.2).clamp(90.0, 200.0);
    let font = TextStyle::Body.resolve(ui.style());
    let head_font = ternarygrid_ui::style::semibold(ui.ctx(), 12.0);
    let mono = egui::FontId::monospace(12.0);
    let line = Stroke::new(1.0, theme.line);
    let now = ui.input(|i| i.time);
    let (rect, _) = ui.allocate_exact_size(vec2(width, head_h + row_h * rows.len() as f32), Sense::hover());
    // The table takes the keyboard focus on a click: ⌘C copies its selection, not the grid's.
    let table_id = ui.id().with("calc_table");
    let table = ui.interact(rect, table_id, Sense::click());
    let focused = table.has_focus();
    state.selected.retain(|n| rows.iter().any(|r| r.name == n));
    let mut copy: Option<(String, Option<String>)> = None; // (text, function that shows "Copied")
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 8, theme.bg);
    let header = egui::Rect::from_min_size(rect.min, vec2(width, head_h));
    painter.rect_filled(header, egui::CornerRadius { nw: 8, ne: 8, ..Default::default() }, theme.header_bg);
    painter.hline(header.x_range(), header.bottom() - 0.5, Stroke::new(1.0, pal.border));
    painter.text(pos2(header.left() + 10.0, header.center().y - 0.5), Align2::LEFT_CENTER, tr("Function"), head_font.clone(), pal.text_muted);
    painter.text(pos2(header.left() + name_w + 10.0, header.center().y - 0.5), Align2::LEFT_CENTER, tr("Value"), head_font, pal.text_muted);
    let all_text = || rows.iter().map(|r| format!("{}\t{}", r.name, r.copy)).collect::<Vec<_>>().join("\n");
    for (i, row) in rows.iter().enumerate() {
        let r = egui::Rect::from_min_size(pos2(rect.left(), rect.top() + head_h + row_h * i as f32), vec2(width, row_h));
        let resp = ui.interact(r, table_id.with(("row", i)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{}: {}", row.name, row.shown)));
        let button = egui::Rect::from_center_size(pos2(r.right() - 18.0, r.center().y), vec2(22.0, 22.0));
        let button_resp = ui.interact(button, table_id.with(("copy", i)), Sense::click());
        let selected = state.selected.iter().any(|n| n == row.name);
        let hovered = resp.hovered() || button_resp.hovered();
        let fill = if selected {
            theme.selection
        } else if hovered {
            theme.selection.gamma_multiply(0.45)
        } else if i % 2 == 1 {
            theme.stripe
        } else {
            theme.bg
        };
        let last = i + 1 == rows.len();
        painter.rect_filled(r, if last { egui::CornerRadius { sw: 8, se: 8, ..Default::default() } } else { egui::CornerRadius::ZERO }, fill);
        painter.text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, row.name, font.clone(), theme.text);
        let just_copied = state.copied.as_ref().is_some_and(|(n, t)| n == row.name && now - t < 1.2);
        // The copy button at the end of the hovered and the selected rows.
        let show_button = hovered || selected;
        let value_right = if show_button || just_copied { button.left() - 4.0 } else { r.right() - 4.0 };
        let value_rect = egui::Rect::from_min_max(pos2(r.left() + name_w + 10.0, r.top()), pos2(value_right, r.bottom()));
        let galley = painter.layout(row.shown.clone(), mono.clone(), theme.text, f32::INFINITY);
        painter.with_clip_rect(value_rect).galley(pos2(value_rect.left(), r.center().y - galley.size().y / 2.0), galley, theme.text);
        if just_copied {
            let ok = egui::Color32::from_rgb(0x3f, 0xb9, 0x50);
            painter.text(pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, format!("{} {}", icon::CHECK, tr("Copied")), font.clone(), ok);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        } else if show_button {
            let color = if button_resp.hovered() { theme.text } else { pal.text_muted };
            if button_resp.hovered() {
                painter.rect_filled(button, 6, pal.hover);
            }
            painter.text(button.center(), Align2::CENTER_CENTER, icon::COPY, ternarygrid_ui::style::icon_font(12.0), color);
        }
        if button_resp.on_hover_text(tr("Copy value")).clicked() {
            copy = Some((row.copy.clone(), Some(row.name.to_string())));
            continue;
        }
        if resp.clicked() || resp.secondary_clicked() {
            table.request_focus();
            let m = ui.input(|i| i.modifiers);
            if m.shift
                && let Some(a) = state.anchor.filter(|a| *a < rows.len())
            {
                let (lo, hi) = (a.min(i), a.max(i));
                state.selected = rows[lo..=hi].iter().map(|r| r.name.to_string()).collect();
            } else if m.command && resp.clicked() {
                if selected {
                    state.selected.retain(|n| n != row.name);
                } else {
                    state.selected.push(row.name.to_string());
                }
                state.anchor = Some(i);
            } else if !(resp.secondary_clicked() && selected) {
                state.selected = vec![row.name.to_string()];
                state.anchor = Some(i);
            }
        }
        if resp.double_clicked() {
            copy = Some((row.copy.clone(), Some(row.name.to_string())));
        }
        resp.on_hover_text(row.copy.as_str()).context_menu(|ui| {
            ternarygrid_ui::kit::menu_look(ui, 200.0);
            if ternarygrid_ui::kit::menu_item(ui, Some(icon::COPY), tr("Copy value"), "").clicked() {
                copy = Some((row.copy.clone(), Some(row.name.to_string())));
                ui.close();
            }
            if ternarygrid_ui::kit::menu_item(ui, None, tr("Copy name and value"), "").clicked() {
                copy = Some((format!("{}\t{}", row.name, row.copy), Some(row.name.to_string())));
                ui.close();
            }
            if ternarygrid_ui::kit::menu_item(ui, None, tr("Copy all"), "").clicked() {
                copy = Some((all_text(), None));
                ui.close();
            }
        });
    }
    // ⌘C (the Copy event) while the table has the focus: the selected values, one per line.
    if focused && copy.is_none() && !state.selected.is_empty() && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
        let text = rows.iter().filter(|r| state.selected.iter().any(|n| n == r.name)).map(|r| r.copy.clone()).collect::<Vec<_>>().join("\n");
        let first = state.selected.first().cloned();
        copy = Some((text, first));
    }
    if let Some((text, name)) = copy {
        ui.ctx().copy_text(text);
        state.copied = name.map(|n| (n, now));
    }
    for i in 1..rows.len() {
        let y = rect.top() + head_h + row_h * i as f32 - 0.5;
        painter.hline(rect.x_range(), y, line);
    }
    painter.rect_stroke(rect, 8, if focused { Stroke::new(1.0, pal.accent) } else { Stroke::new(1.0, pal.border) }, egui::StrokeKind::Inside);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::CellPos;
    use ternarygrid_core::{ColumnMeta, DataKind};

    #[test]
    fn stats_of_selection() {
        let m = ResultModel::from_rows(
            vec![ColumnMeta::new("a", "int", DataKind::Int), ColumnMeta::new("b", "text", DataKind::Text)],
            (0..10).map(|i| vec![Value::Int(i), Value::text("x".repeat(i as usize))]).collect(),
        );
        let mut g = DataGridState::for_model(&m);
        g.selection_mut().select_cell(CellPos::new(0, 0));
        g.selection_mut().extend_to(CellPos::new(4, 0));
        let s = selection_stats(&m, &g, 1000);
        assert_eq!(s.count, 5);
        assert_eq!(s.int_sum, Some(10));
        g.selection_mut().select_columns(1, 1, false);
        let s = selection_stats(&m, &g, 1000);
        assert_eq!(s.count, 10);
        assert_eq!(s.max_len, Some(9));
        g.selection_mut().select_all();
        let s = selection_stats(&m, &g, 7);
        assert!(s.truncated);
    }
}
