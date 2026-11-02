//! Dialogs owned by the grid: advanced copy, generated SQL, virtual columns.

use super::frame::GridFrame;
use super::{selection_copy_data_limited, sql_table_name};
use crate::copy::{CopyFormat, QuoteMode, render};
use crate::expr::Expr;
use egui::{ComboBox, Key, ScrollArea, TextEdit, Ui};
use ternarygrid_core::{tr, trf};
use ternarygrid_ui::parts::{Dialog, form_row};

/// Colours of a line of SQL: keywords, strings and numbers (as the mockups' code).
fn sql_job(line: &str, font: &egui::FontId, dark: bool, text: egui::Color32) -> egui::text::LayoutJob {
    const KEYWORDS: [&str; 40] = [
        "select", "from", "where", "and", "or", "not", "null", "is", "in", "insert", "into", "values", "update", "set", "delete", "create", "table", "as", "on", "join", "left",
        "right", "inner", "outer", "group", "by", "order", "limit", "having", "distinct", "union", "all", "case", "when", "then", "else", "end", "default", "true", "false",
    ];
    let rgb = |c: u32| egui::Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8);
    let (kw, string, number) = if dark { (rgb(0x7aa7ff), rgb(0xe6a96b), rgb(0xc39bf2)) } else { (rgb(0x2554c7), rgb(0xa5531a), rgb(0x7a3fc4)) };
    let mut job = egui::text::LayoutJob::default();
    let mut add = |s: &str, c: egui::Color32| job.append(s, 0.0, egui::TextFormat::simple(font.clone(), c));
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let end = if c == b'\'' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'\'' {
                j += 1;
            }
            (j + 1).min(b.len())
        } else if c.is_ascii_alphanumeric() || c == b'_' {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'.' && b[i].is_ascii_digit()) {
                j += 1;
            }
            j
        } else {
            i + line[i..].chars().next().map_or(1, char::len_utf8)
        };
        let word = &line[i..end];
        let color = if c == b'\'' {
            string
        } else if c.is_ascii_digit() {
            number
        } else if KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(word)) {
            kw
        } else {
            text
        };
        add(word, color);
        i = end;
    }
    job
}

/// Code as the mockups' blocks: a bordered box, monospace 12.5 on lines of 20 (numbered
/// in a faint gutter when `gutter`), SQL coloured when `sql`; it scrolls past `max_height`.
fn code_block(ui: &mut Ui, id: egui::Id, text: &str, gutter: bool, sql: bool, max_height: f32) {
    let pal = ternarygrid_ui::pal(ui);
    let dark = ui.visuals().dark_mode;
    let font = egui::FontId::monospace(12.5);
    let lines: Vec<&str> = text.lines().collect();
    let left = if gutter { 46.0 } else { 12.0 };
    let width = lines.iter().map(|l| ui.fonts_mut(|f| f.layout_no_wrap(l.to_string(), font.clone(), pal.text).size().x)).fold(0.0, f32::max);
    ternarygrid_ui::parts::bordered(ui).inner_margin(egui::Margin::symmetric(0, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ScrollArea::both().id_salt(id).max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(left + width + 12.0, lines.len().max(1) as f32 * 20.0), egui::Sense::hover());
            for (i, line) in lines.iter().enumerate() {
                let y = rect.top() + i as f32 * 20.0;
                if gutter {
                    ui.painter().text(egui::pos2(rect.left() + 32.0, y + 10.0), egui::Align2::RIGHT_CENTER, (i + 1).to_string(), font.clone(), pal.text_faint);
                }
                let job = if sql { sql_job(line, &font, dark, pal.text) } else { egui::text::LayoutJob::simple_singleline(line.to_string(), font.clone(), pal.text) };
                let g = ui.fonts_mut(|f| f.layout_job(job));
                ui.painter().galley(egui::pos2(rect.left() + left, y + 10.0 - g.size().y / 2.0), g, pal.text);
            }
        });
    });
}

impl GridFrame<'_> {
    /// Show open dialogs.
    pub fn dialogs(&mut self, ui: &mut Ui) {
        if self.state.rt.copy_dialog {
            self.copy_dialog(ui);
        }
        if self.state.rt.sql_window.is_some() {
            self.sql_window(ui);
        }
        if self.state.rt.virtual_dialog.is_some() {
            self.virtual_dialog(ui);
        }
    }

    fn copy_dialog(&mut self, ui: &mut Ui) {
        let mut opts = self.state.copy_options.clone();
        if opts.sql_table.is_empty() {
            opts.sql_table = sql_table_name(self.model, self.options);
        }
        let mut do_copy = false;
        let mut close = false;
        let preview = match selection_copy_data_limited(self.model, self.state, self.options, 8) {
            Ok(data) => render(&data, &opts, &self.options.format, self.options.dialect.as_deref()),
            Err(e) => e,
        };
        let id = self.id;
        let lw = 110.0;
        let rows = self.state.rt.selection.rows(self.geo.nrows, usize::MAX).len();
        let cols = self.state.selected_columns().len();
        let subtitle = trf("{} rows × {} columns selected", &[&rows, &cols]);
        let (resp, ()) = Dialog::new(tr("Advanced copy")).icon(egui_phosphor::regular::COPY).subtitle(&subtitle).width(520.0).show_modal(
            ui.ctx(),
            id.with("copy_dialog"),
            |ui| {
                form_row(ui, tr("Format"), lw, |ui| {
                    ComboBox::from_id_salt(id.with("copy_format")).icon(ternarygrid_ui::kit::combo_caret).width(220.0).selected_text(opts.format.label()).show_ui(ui, |ui| {
                        for f in CopyFormat::ALL {
                            ui.selectable_value(&mut opts.format, f, f.label());
                        }
                    });
                });
                form_row(ui, tr("Options"), lw, |ui| {
                    ui.spacing_mut().item_spacing.x = 18.0;
                    ternarygrid_ui::kit::checkbox(ui, &mut opts.include_header, tr("Column names"));
                    ternarygrid_ui::kit::checkbox(ui, &mut opts.include_row_numbers, tr("Row numbers"));
                    ternarygrid_ui::kit::checkbox(ui, &mut opts.formatted, tr("Formatted values"));
                });
                if opts.format.is_delimited() {
                    form_row(ui, tr("Delimiter"), lw, |ui| {
                        let default = if opts.format == CopyFormat::Csv { "," } else { "\\t" };
                        ui.add(TextEdit::singleline(&mut opts.delimiter).desired_width(70.0).hint_text(default).font(egui::TextStyle::Monospace));
                    });
                    form_row(ui, tr("Quote"), lw, |ui| {
                        ternarygrid_ui::parts::segmented(
                            ui,
                            &mut opts.quote,
                            &[(QuoteMode::Auto, tr("When needed")), (QuoteMode::Always, tr("Always")), (QuoteMode::Never, tr("Never"))],
                        );
                    });
                }
                form_row(ui, tr("NULL as"), lw, |ui| {
                    ui.add(TextEdit::singleline(&mut opts.null_text).desired_width(140.0).hint_text(tr("(empty)")));
                });
                if opts.format == CopyFormat::SqlInsert {
                    form_row(ui, tr("Table"), lw, |ui| {
                        ui.add(TextEdit::singleline(&mut opts.sql_table).desired_width(260.0));
                    });
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new(tr("Preview")).font(ternarygrid_ui::style::semibold(ui.ctx(), 13.0)));
                code_block(ui, id.with("copy_preview"), &preview, false, opts.format == CopyFormat::SqlInsert, 160.0);
            },
            |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    use ternarygrid_ui::kit;
                    let keys = if cfg!(target_os = "macos") { "⌘↩" } else { "Ctrl+Enter" };
                    if crate::toolbar::key_button(ui, Some(egui_phosphor::regular::COPY), tr("Copy"), Some(keys), true, 28.0).clicked()
                        || ui.input(|i| i.key_pressed(Key::Enter) && i.modifiers.command)
                    {
                        do_copy = true;
                    }
                    if kit::button_ex(ui, kit::Kind::Ghost, None, tr("Cancel"), false).clicked() {
                        close = true;
                    }
                });
            },
        );
        if resp.close {
            close = true;
        }
        self.state.copy_options = opts.clone();
        if do_copy {
            self.copy_selection(&opts);
            close = true;
        }
        if close {
            self.state.rt.copy_dialog = false;
            ui.memory_mut(|m| m.request_focus(self.id));
        }
    }

    fn sql_window(&mut self, ui: &mut Ui) {
        let Some((title, sql)) = self.state.rt.sql_window.clone() else { return };
        let mut open = true;
        let mut copy = false;
        let id = self.id.with("sql_window");
        Dialog::new(&title).icon(egui_phosphor::regular::FILE_SQL).width(640.0).body_padding(egui::Margin::symmetric(16, 14)).show_window(
            ui.ctx(),
            id,
            &mut open,
            None,
            |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                if ternarygrid_ui::kit::button_full(ui, ternarygrid_ui::kit::Kind::Secondary, Some(egui_phosphor::regular::COPY), tr("Copy"), true, false).clicked() {
                    copy = true;
                }
                code_block(ui, id.with("code"), &sql, true, true, 340.0);
            },
            None,
        );
        if copy {
            ui.ctx().copy_text(sql);
            self.notify(tr("SQL copied to the clipboard"), false);
        }
        if !open {
            self.state.rt.sql_window = None;
        }
    }

    fn virtual_dialog(&mut self, ui: &mut Ui) {
        let Some(mut dlg) = self.state.rt.virtual_dialog.take() else { return };
        let names: Vec<String> = self.model.columns().iter().map(|c| c.name.clone()).collect();
        let check = if dlg.expression.trim().is_empty() { None } else { Some(Expr::compile(&dlg.expression, &names).map_err(|e| e.display_with_position(&dlg.expression))) };
        let mut apply = false;
        let mut close = false;
        let editing = dlg.editing.is_some();
        let lw = 100.0;
        let title = if editing { tr("Edit virtual column") } else { tr("Add virtual column") };
        let (resp, ()) = Dialog::new(title).icon(egui_phosphor::regular::FUNCTION).subtitle(tr("Computed on the client from the row's values")).width(540.0).show_modal(
            ui.ctx(),
            self.id.with("virtual_dialog"),
            |ui| {
                form_row(ui, tr("Name"), lw, |ui| {
                    ui.add(TextEdit::singleline(&mut dlg.name).desired_width(300.0));
                });
                form_row(ui, tr("Expression"), lw, |ui| {
                    ui.add(TextEdit::multiline(&mut dlg.expression).code_editor().desired_rows(3).desired_width(360.0).hint_text("price * quantity"));
                });
                match &check {
                    Some(Err(e)) => {
                        ternarygrid_ui::parts::status(ui, ternarygrid_ui::parts::Tone::Error, e);
                    }
                    Some(Ok(_)) => {
                        ternarygrid_ui::parts::status(ui, ternarygrid_ui::parts::Tone::Success, tr("Expression is valid"));
                    }
                    None => {}
                }
                if let Some(e) = &dlg.error {
                    ternarygrid_ui::parts::status(ui, ternarygrid_ui::parts::Tone::Error, e);
                }
                ui.collapsing(tr("Expression help"), |ui| {
                    ui.label(crate::expr::help_text());
                    ui.label(trf("Columns: {}", &[&names.join(", ")]));
                });
            },
            |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    use ternarygrid_ui::kit;
                    let ok = matches!(check, Some(Ok(_)));
                    let label = if editing { tr("Save") } else { tr("Add") };
                    if ui.add_enabled_ui(ok, |ui| kit::button_ex(ui, kit::Kind::Primary, None, label, false).clicked()).inner {
                        apply = true;
                    }
                    if kit::button_ex(ui, kit::Kind::Ghost, None, tr("Cancel"), false).clicked() {
                        close = true;
                    }
                });
            },
        );
        if resp.close {
            close = true;
        }
        if apply {
            let result = match dlg.editing {
                Some(i) => self.model.update_virtual_column(i, &dlg.name, &dlg.expression).map(|_| ()),
                None => self.model.add_virtual_column(&dlg.name, &dlg.expression).map(|_| ()),
            };
            match result {
                Ok(()) => {
                    self.state.sync_columns(self.model);
                    close = true;
                }
                Err(e) => dlg.error = Some(e.display_with_position(&dlg.expression)),
            }
        }
        if !close {
            self.state.rt.virtual_dialog = Some(dlg);
        } else {
            ui.memory_mut(|m| m.request_focus(self.id));
        }
    }
}
