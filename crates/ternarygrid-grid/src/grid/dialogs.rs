//! Dialogs owned by the grid: advanced copy, generated SQL, virtual columns.

use super::frame::GridFrame;
use super::{selection_copy_data_limited, sql_table_name};
use crate::copy::{CopyFormat, QuoteMode, render};
use crate::expr::Expr;
use egui::{ComboBox, Key, ScrollArea, TextEdit, Ui};
use ternarygrid_core::{tr, trf};
use ternarygrid_ui::parts::{Dialog, form_row};

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
        let (resp, ()) = Dialog::new(tr("Advanced copy")).icon(egui_phosphor::regular::COPY).width(540.0).show_modal(
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
                ternarygrid_ui::parts::code_frame()(ui).show(ui, |ui| {
                    ScrollArea::both().max_height(160.0).id_salt(id.with("copy_preview")).show(ui, |ui| {
                        let mut p = preview.as_str();
                        ui.add(TextEdit::multiline(&mut p).code_editor().frame(egui::Frame::NONE).desired_width(f32::INFINITY).desired_rows(4));
                    });
                });
            },
            |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    use ternarygrid_ui::kit;
                    if kit::primary_icon(ui, egui_phosphor::regular::COPY, tr("Copy")).on_hover_text("⌘↩").clicked()
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
        Dialog::new(&title).icon(egui_phosphor::regular::FILE_SQL).width(620.0).show_window(
            ui.ctx(),
            self.id.with("sql_window"),
            &mut open,
            None,
            |ui| {
                if ternarygrid_ui::kit::button_full(ui, ternarygrid_ui::kit::Kind::Secondary, Some(egui_phosphor::regular::COPY), tr("Copy"), true, false).clicked() {
                    copy = true;
                }
                ternarygrid_ui::parts::code_frame()(ui).show(ui, |ui| {
                    ScrollArea::both().max_height(340.0).show(ui, |ui| {
                        let mut s = sql.as_str();
                        ui.add(TextEdit::multiline(&mut s).code_editor().frame(egui::Frame::NONE).desired_width(f32::INFINITY));
                    });
                });
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
