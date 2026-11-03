//! Dialogs owned by the grid: advanced copy, generated SQL, virtual columns.

use super::frame::GridFrame;
use super::{selection_copy_data_limited, sql_table_name};
use crate::copy::{CopyFormat, QuoteMode, render};
use crate::expr::Expr;
use egui::{ComboBox, Key, TextEdit, Ui};
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

/// Code in the kit's code block: SQL coloured when `sql`, numbered when `gutter`, long
/// lines wrapped when `wrap`.
fn code_block(ui: &mut Ui, id: egui::Id, text: &str, gutter: bool, sql: bool, wrap: bool, max_height: f32) {
    let pal = ternarygrid_ui::pal(ui);
    let dark = ui.visuals().dark_mode;
    let font = egui::FontId::monospace(12.5);
    let mut job = egui::text::LayoutJob::default();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            job.append("\n", 0.0, egui::TextFormat::simple(font.clone(), pal.text));
        }
        if sql {
            let offset = job.text.len();
            let part = sql_job(line, &font, dark, pal.text);
            job.text.push_str(&part.text);
            job.sections.extend(part.sections.into_iter().map(|mut s| {
                s.byte_range = s.byte_range.start + offset..s.byte_range.end + offset;
                s
            }));
        } else {
            job.append(line, 0.0, egui::TextFormat::simple(font.clone(), pal.text));
        }
    }
    ternarygrid_ui::parts::code_block(ui, id, job, gutter, wrap, max_height);
}

/// A single-line field as the kit's inputs: 30 points high, 8 points of padding at the sides.
fn field(ui: &mut Ui, text: &mut String, width: f32, hint: &str, mono: bool) -> egui::Response {
    let faint = ternarygrid_ui::pal(ui).text_faint;
    let mut edit = TextEdit::singleline(text)
        .desired_width(width)
        .min_size(egui::vec2(0.0, ternarygrid_ui::style::CONTROL_H))
        .vertical_align(egui::Align::Center)
        .margin(egui::Margin::symmetric(8, 0))
        .hint_text(egui::RichText::new(hint).color(faint));
    if mono {
        edit = edit.font(egui::TextStyle::Monospace);
    }
    ui.add(edit)
}

/// A disclosure row: the kit's caret and `title` (muted); its body below while open.
fn disclosure(ui: &mut Ui, id: egui::Id, title: &str, body: impl FnOnce(&mut Ui)) {
    let p = ternarygrid_ui::pal(ui);
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let font = egui::FontId::proportional(13.0);
    let g = ui.painter().layout_no_wrap(title.to_string(), font, p.text_muted);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0 + g.size().x, 22.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title));
    if resp.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    let color = if resp.hovered() { p.text } else { p.text_muted };
    let caret = if open { egui_phosphor::regular::CARET_DOWN } else { egui_phosphor::regular::CARET_RIGHT };
    ui.painter().text(egui::pos2(rect.left() + 6.0, rect.center().y), egui::Align2::CENTER_CENTER, caret, ternarygrid_ui::style::icon_font(11.0), p.text_faint);
    ui.painter().galley(egui::pos2(rect.left() + 18.0, rect.center().y - g.size().y / 2.0), g, color);
    if open {
        body(ui);
    }
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
        let lw = 100.0;
        let rows = self.state.rt.selection.rows(self.geo.nrows, usize::MAX).len();
        let cols = self.state.selected_columns().len();
        let subtitle = trf("{} rows × {} columns selected", &[&rows, &cols]);
        let (resp, ()) = Dialog::new(tr("Advanced copy")).icon(egui_phosphor::regular::COPY).subtitle(&subtitle).width(580.0).show_modal(
            ui.ctx(),
            id.with("copy_dialog"),
            |ui| {
                // Combo boxes and fields as tall as the kit's inputs.
                ui.spacing_mut().interact_size.y = ternarygrid_ui::style::CONTROL_H;
                form_row(ui, tr("Format"), lw, |ui| {
                    ComboBox::from_id_salt(id.with("copy_format")).icon(ternarygrid_ui::kit::combo_caret).width(220.0).selected_text(opts.format.label()).show_ui(ui, |ui| {
                        ternarygrid_ui::kit::menu_look(ui, 220.0);
                        for f in CopyFormat::ALL {
                            if ternarygrid_ui::kit::menu_check(ui, &mut (opts.format == f), f.label()).clicked() {
                                opts.format = f;
                            }
                        }
                    });
                });
                form_row(ui, tr("Options"), lw, |ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    ternarygrid_ui::kit::checkbox(ui, &mut opts.include_header, tr("Column names"));
                    ternarygrid_ui::kit::checkbox(ui, &mut opts.include_row_numbers, tr("Row numbers"));
                    ternarygrid_ui::kit::checkbox(ui, &mut opts.formatted, tr("Formatted values"));
                });
                if opts.format.is_delimited() {
                    form_row(ui, tr("Delimiter"), lw, |ui| {
                        let default = if opts.format == CopyFormat::Csv { "," } else { "\\t" };
                        field(ui, &mut opts.delimiter, 70.0, default, true);
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
                    field(ui, &mut opts.null_text, 140.0, tr("(empty)"), false);
                });
                if opts.format == CopyFormat::SqlInsert {
                    form_row(ui, tr("Table"), lw, |ui| {
                        field(ui, &mut opts.sql_table, 260.0, "", true);
                    });
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new(tr("Preview")).font(ternarygrid_ui::style::semibold(ui.ctx(), 13.0)));
                code_block(ui, id.with("copy_preview"), &preview, false, opts.format == CopyFormat::SqlInsert, false, 160.0);
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
                let wrap_id = id.with("wrap");
                let mut wrap = ui.data(|d| d.get_temp::<bool>(wrap_id)).unwrap_or(false);
                ui.horizontal(|ui| {
                    if ternarygrid_ui::kit::button_full(ui, ternarygrid_ui::kit::Kind::Secondary, Some(egui_phosphor::regular::COPY), tr("Copy"), true, false).clicked() {
                        copy = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ternarygrid_ui::kit::checkbox(ui, &mut wrap, tr("Wrap lines")).changed() {
                            ui.data_mut(|d| d.insert_temp(wrap_id, wrap));
                        }
                    });
                });
                code_block(ui, id.with("code"), &sql, true, true, wrap, 340.0);
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
        // The hint from one of this table's own amounts (not a key), else a plain example.
        let measure = self.model.columns().iter().enumerate().find(|(i, c)| c.is_numeric() && !crate::format::is_identifier_column(self.model, *i)).map(|(_, c)| c.name.clone());
        let hint = match &measure {
            Some(m) => format!("{m} * 1.1"),
            None => "upper(name)".to_string(),
        };
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
                // Both fields to the body's right edge.
                let field_w = ui.available_width() - lw - 16.0;
                form_row(ui, tr("Name"), lw, |ui| {
                    field(ui, &mut dlg.name, field_w, "", false);
                });
                form_row(ui, tr("Expression"), lw, |ui| {
                    let faint = ternarygrid_ui::pal(ui).text_faint;
                    ui.add(
                        TextEdit::multiline(&mut dlg.expression)
                            .code_editor()
                            .desired_rows(3)
                            .desired_width(field_w)
                            .margin(egui::Margin::symmetric(8, 6))
                            .hint_text(egui::RichText::new(&hint).color(faint)),
                    );
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
                disclosure(ui, self.id.with("virtual_help"), tr("Expression help"), |ui| {
                    let muted = ternarygrid_ui::pal(ui).text_muted;
                    ui.label(egui::RichText::new(crate::expr::help_text()).color(muted));
                    ui.label(egui::RichText::new(trf("Columns: {}", &[&names.join(", ")])).color(muted));
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
