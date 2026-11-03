//! Metadata panel: the result columns (FR-D5).

use crate::grid::DataGridState;
use crate::model::ResultModel;
use egui::{RichText, ScrollArea, Ui};
use ternarygrid_core::{DataKind, tr};

/// Rows shown by the metadata panel (also used by tests).
pub fn metadata_rows(model: &ResultModel) -> Vec<[String; 7]> {
    let key_cols: Vec<usize> = model.editability().as_ref().map(|t| t.key_columns.clone()).unwrap_or_default();
    (0..model.column_count())
        .map(|c| {
            let name = model.column_name(c).to_string();
            match model.column_meta(c) {
                Some(m) => {
                    let mut flags = Vec::new();
                    if key_cols.contains(&c) {
                        flags.push(tr("key"));
                    }
                    if m.auto_increment == Some(true) {
                        flags.push(tr("auto increment"));
                    }
                    if model.foreign_key_for_column(c).is_some() {
                        flags.push(tr("foreign key"));
                    }
                    let size = match (m.precision, m.scale, m.max_length) {
                        (Some(p), Some(s), _) => format!("{p},{s}"),
                        (Some(p), None, _) => p.to_string(),
                        (None, _, Some(l)) => l.to_string(),
                        _ => String::new(),
                    };
                    [
                        name,
                        m.type_name.to_lowercase(),
                        format!("{:?}", m.kind),
                        match m.nullable {
                            Some(true) => tr("yes").to_string(),
                            Some(false) => tr("no").to_string(),
                            None => "?".to_string(),
                        },
                        m.source.as_ref().map_or_else(String::new, |s| {
                            let mut p = String::new();
                            if let Some(sc) = &s.schema {
                                p.push_str(sc);
                                p.push('.');
                            }
                            p.push_str(&s.table);
                            p.push('.');
                            p.push_str(&s.column);
                            p
                        }),
                        size,
                        flags.join(", "),
                    ]
                }
                None => {
                    let expr = model.virtual_columns().get(c - model.base_column_count()).map(|v| v.expression.source().to_string()).unwrap_or_default();
                    [name, tr("virtual").to_string(), format!("{:?}", DataKind::Other), String::new(), expr, String::new(), tr("computed").to_string()]
                }
            }
        })
        .collect()
}

/// Column list: name, type, kind, nullable, source table.column, precision/scale.
pub struct MetadataPanel<'a> {
    model: &'a ResultModel,
    grid: &'a mut DataGridState,
}

impl<'a> MetadataPanel<'a> {
    /// New panel.
    pub fn new(model: &'a ResultModel, grid: &'a mut DataGridState) -> Self {
        MetadataPanel { model, grid }
    }

    /// Show the panel: the source table, then the columns in a bordered table (a check box
    /// shows or hides each in the grid; a click on a name selects it there).
    pub fn show(self, ui: &mut Ui) {
        let rows = metadata_rows(self.model);
        let pal = ternarygrid_ui::pal(ui);
        ui.spacing_mut().item_spacing.y = 10.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            match self.model.editability() {
                Ok(t) => {
                    ui.label(RichText::new(egui_phosphor::regular::TABLE).font(ternarygrid_ui::style::icon_font(14.0)).color(pal.text_muted));
                    ternarygrid_ui::kit::semibold_label(ui, &t.table.display_path(), 13.0, pal.text);
                }
                Err(r) => {
                    ui.label(RichText::new(format!("{} {}", egui_phosphor::regular::LOCK_SIMPLE, r.short_label())).color(pal.warn)).on_hover_text(r.message());
                }
            }
        });
        let key_cols: Vec<usize> = self.model.editability().as_ref().map(|t| t.key_columns.clone()).unwrap_or_default();
        // The columns that fit: in a side panel (narrower than 600) the check box, the name, the
        // type and the flags (the rest in the row's tooltip); wider, all of them. Source only
        // when it says more than the title (another table, or a renamed column).
        let total = ui.available_width();
        let wide = total >= 600.0;
        let title = self.model.editability().as_ref().ok().map(|t| t.table.clone());
        let same_source = |c: usize| {
            let Some(m) = self.model.column_meta(c) else { return true };
            m.source.as_ref().is_none_or(|s| title.as_ref().is_some_and(|t| t.name == s.table && s.column == m.name))
        };
        let show_source = wide && !(0..self.model.column_count()).all(same_source);
        // (header, field of the row, share); the mockup's shares.
        let mut cols: Vec<(&str, usize, f32)> = vec![(tr("Column"), 0, if wide { 1.3 } else { 1.45 }), (tr("Type"), 1, if wide { 0.8 } else { 1.0 })];
        if wide {
            cols.extend([(tr("Kind"), 2, 0.7), (tr("Null"), 3, 0.5)]);
            if show_source {
                cols.push((tr("Source"), 4, 2.0));
            }
            cols.push((tr("Size"), 5, 0.5));
        }
        cols.push((tr("Flags"), 6, if wide { 0.8 } else { 0.85 }));
        let shares: f32 = cols.iter().map(|c| c.2).sum();
        let unit = ((total - 28.0) / shares).max(24.0);
        let widths: Vec<f32> = std::iter::once(28.0).chain(cols.iter().map(|c| c.2 * unit)).collect();
        let width: f32 = widths.iter().sum();
        let body = egui::FontId::proportional(13.0);
        let mono = egui::FontId::monospace(11.0);
        let head_font = ternarygrid_ui::style::semibold(ui.ctx(), 12.0);
        let chip_font = ternarygrid_ui::style::semibold(ui.ctx(), 11.0);
        let theme = crate::theme::GridTheme::from_visuals(ui.visuals());
        // A cell's text on one line, cut with "…" when it does not fit (the row's tooltip has it whole).
        let paint_cell = |ui: &Ui, rect: egui::Rect, text: &str, font: &egui::FontId, color: egui::Color32| {
            let inner = rect.shrink2(egui::vec2(10.0, 0.0));
            let mut job = egui::text::LayoutJob::single_section(text.to_string(), egui::TextFormat::simple(font.clone(), color));
            job.wrap = egui::text::TextWrapping { max_width: inner.width().max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
            let g = ui.painter().layout_job(job);
            ui.painter().with_clip_rect(inner.expand2(egui::vec2(1.0, 0.0)).intersect(ui.clip_rect())).galley(
                egui::pos2(inner.left(), rect.center().y - g.size().y / 2.0),
                g,
                color,
            );
        };
        ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let (head, _) = ui.allocate_exact_size(egui::vec2(width, 33.0), egui::Sense::hover());
            let top = head.top();
            ui.painter().rect_filled(head, egui::CornerRadius { nw: 8, ne: 8, ..Default::default() }, pal.header);
            ui.painter().hline(head.x_range(), head.bottom() - 0.5, egui::Stroke::new(1.0, pal.border));
            let mut x = head.left() + widths[0];
            for (i, (h, _, _)) in cols.iter().enumerate() {
                paint_cell(ui, egui::Rect::from_min_size(egui::pos2(x, head.top()), egui::vec2(widths[i + 1], 32.0)), h, &head_font, pal.text_muted);
                x += widths[i + 1];
            }
            let mut bottom = head.bottom();
            for (c, r) in rows.iter().enumerate() {
                let (row, row_resp) = ui.allocate_exact_size(egui::vec2(width, 31.0), egui::Sense::hover());
                ui.painter().rect_filled(row, 0.0, if c % 2 == 1 { theme.stripe } else { pal.surface });
                ui.painter().hline(row.x_range(), row.bottom() - 0.5, egui::Stroke::new(1.0, theme.line));
                let cell = |i: usize| {
                    let x0 = row.left() + widths[..i].iter().sum::<f32>();
                    egui::Rect::from_min_size(egui::pos2(x0, row.top()), egui::vec2(widths[i], 30.0))
                };
                let mut visible = self.grid.columns.get(c).is_some_and(|s| s.visible);
                let check = cell(0);
                let changed = ui
                    .scope_builder(egui::UiBuilder::new().max_rect(egui::Rect::from_center_size(check.center(), egui::vec2(18.0, 18.0))), |ui| {
                        ternarygrid_ui::kit::checkbox(ui, &mut visible, "").on_hover_text(tr("Show column")).changed()
                    })
                    .inner;
                if changed {
                    self.grid.set_column_visible(c, visible);
                }
                let name_rect = cell(1);
                let name = ui.interact(name_rect, ui.id().with(("metadata_name", c)), egui::Sense::click());
                name.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &r[0]));
                if name.clicked() {
                    let at = self.grid.active_cell().map_or(0, |(row, _)| row);
                    self.grid.select_cell(at, c);
                }
                let name_color = if name.hovered() { pal.accent_text } else { pal.text };
                paint_cell(ui, name_rect, &r[0], &body, name_color);
                for (i, (_, field, _)) in cols.iter().enumerate().skip(1) {
                    if *field == 6 {
                        continue;
                    }
                    // Types and sources in monospace, muted (as the grid header's types).
                    let (font, color) = if matches!(field, 1 | 4) { (&mono, pal.text_muted) } else { (&body, pal.text) };
                    paint_cell(ui, cell(i + 1), &r[*field], font, color);
                }
                // The whole description in the row's tooltip (what a narrow panel leaves out).
                let mut tip = vec![r[0].clone(), format!("{}: {}", tr("Type"), r[1]), format!("{}: {}", tr("Kind"), r[2])];
                if !r[3].is_empty() {
                    tip.push(format!("{}: {}", tr("Nullable"), r[3]));
                }
                if !r[5].is_empty() {
                    tip.push(format!("{}: {}", tr("Size"), r[5]));
                }
                if !r[4].is_empty() {
                    tip.push(format!("{}: {}", if self.model.is_virtual_column(c) { tr("Expression") } else { tr("Source") }, r[4]));
                }
                if !r[6].is_empty() {
                    tip.push(r[6].clone());
                }
                let tip = tip.join("\n");
                // One tooltip: the name's, else the row's (not over the check box's own).
                let over_check = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| check.contains(p));
                if name.hovered() {
                    name.on_hover_text(tip);
                } else if !over_check {
                    row_resp.on_hover_text(tip);
                }
                // Flags as small pills: key, fk, auto.
                let mut fx = cell(cols.len()).left() + 10.0;
                let virtual_col = self.model.is_virtual_column(c);
                let flags = [
                    (key_cols.contains(&c), tr("key"), true),
                    (self.model.foreign_key_for_column(c).is_some(), "fk", false),
                    (self.model.column_meta(c).is_some_and(|m| m.auto_increment == Some(true)), tr("auto"), false),
                    (virtual_col, tr("computed"), false),
                ];
                for (on, label, warn) in flags {
                    if !on {
                        continue;
                    }
                    let (fg, bg) = if warn { (pal.warn, theme.modified) } else { (pal.text_muted, pal.subtle) };
                    let g = ui.painter().layout_no_wrap(label.to_string(), chip_font.clone(), fg);
                    let chip = egui::Rect::from_min_size(egui::pos2(fx, row.center().y - 9.0), egui::vec2(g.size().x + 14.0, 18.0));
                    ui.painter().with_clip_rect(row.intersect(ui.clip_rect())).rect_filled(chip, 9, bg);
                    ui.painter().with_clip_rect(row.intersect(ui.clip_rect())).galley(chip.center() - g.size() / 2.0, g, fg);
                    fx = chip.right() + 4.0;
                }
                bottom = row.bottom();
            }
            let card = egui::Rect::from_min_max(egui::pos2(head.left(), top), egui::pos2(head.left() + width, bottom));
            ui.painter().rect_stroke(card, 8, egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternarygrid_core::{ColumnMeta, ColumnSource, Value};

    #[test]
    fn rows() {
        let mut c = ColumnMeta::new("price", "numeric(10,2)", DataKind::Decimal).with_source(ColumnSource {
            database: None,
            schema: Some("s".into()),
            table: "t".into(),
            column: "price".into(),
        });
        c.precision = Some(10);
        c.scale = Some(2);
        c.nullable = Some(false);
        let mut m = ResultModel::from_rows(vec![c], vec![vec![Value::decimal("1.00")]]);
        m.add_virtual_column("twice", "price * 2").unwrap();
        let r = metadata_rows(&m);
        assert_eq!(r[0][0], "price");
        assert_eq!(r[0][3], "no");
        assert_eq!(r[0][4], "s.t.price");
        assert_eq!(r[0][5], "10,2");
        assert_eq!(r[1][4], "price * 2");
    }
}
