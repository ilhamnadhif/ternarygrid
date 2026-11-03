//! Export connections into a TernaryGrid connections file (imported back with Import
//! connections › TernaryGrid file): another machine, a colleague, a backup.

use super::{Column, W, add_field_ex, cancel_button, muted, right, table, text_field};
use crate::cx::AppContext;
use egui::{Id, RichText, Ui};
use egui_phosphor::regular as icon;
use std::cell::Cell;
use std::collections::BTreeMap;
use ternarygrid_config::import::ternarygrid as file;
use ternarygrid_core::{Secret, tr, trf, trn};
use ternarygrid_ui::kit;
use ternarygrid_ui::parts::{self, Dialog, Tone};
use ternarygrid_ui::style;

pub struct ExportConnectionsDialog {
    /// The connections in the navigator's order, with whether each is exported.
    ids: Vec<String>,
    selected: Vec<bool>,
    include_passwords: bool,
    password: String,
    repeat: String,
    /// The passwords could not be read (a locked vault …).
    error: Option<String>,
    tried: bool,
}

impl ExportConnectionsDialog {
    /// `chosen`: the connections to tick (a folder's, the navigator's selection); `None`: all.
    pub fn new(store: &ternarygrid_config::ConnectionStore, chosen: Option<&[String]>) -> Self {
        let ids: Vec<String> = store.connections.iter().map(|c| c.id.clone()).collect();
        let selected = ids.iter().map(|id| chosen.is_none_or(|c| c.contains(id))).collect();
        ExportConnectionsDialog { ids, selected, include_passwords: false, password: String::new(), repeat: String::new(), error: None, tried: false }
    }

    fn chosen(&self) -> Vec<String> {
        self.ids.iter().zip(&self.selected).filter(|(_, s)| **s).map(|(id, _)| id.clone()).collect()
    }

    fn password_problem(&self) -> Option<&'static str> {
        if !self.include_passwords {
            None
        } else if self.password.is_empty() {
            Some(tr("Enter a password for the file."))
        } else if self.password != self.repeat {
            Some(tr("The passwords do not match."))
        } else {
            None
        }
    }

    /// Writes the file (asks where); true when it was written.
    fn export(&mut self, cx: &mut AppContext) -> bool {
        self.tried = true;
        self.error = None;
        if self.password_problem().is_some() {
            return false;
        }
        let ids = self.chosen();
        let store = &cx.conns.store;
        let mut secrets = BTreeMap::new();
        if self.include_passwords {
            let manager = cx.services.secrets.read();
            for key in file::secret_keys(store, &ids) {
                match manager.get(&key) {
                    Ok(Some(v)) => {
                        secrets.insert(key, v);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.error = Some(trf("The saved passwords cannot be read: {}", &[&e.full_text()]));
                        return false;
                    }
                }
            }
        }
        let password = self.include_passwords.then(|| Secret::new(self.password.clone()));
        let (bytes, summary) = match file::export(store, &ids, &secrets, password.as_ref(), env!("CARGO_PKG_VERSION")) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(e.full_text());
                return false;
            }
        };
        let Some(path) = rfd::FileDialog::new().add_filter(tr("TernaryGrid connections"), &[file::EXTENSION]).set_file_name("TernaryGrid connections.json").save_file() else {
            return false;
        };
        if let Err(e) = ternarygrid_config::write_secure(&path, &bytes) {
            self.error = Some(trf("Cannot write {}: {}", &[&path.display(), &e]));
            return false;
        }
        let mut what = trn(summary.connections, "Exported 1 connection", "Exported {} connections");
        if summary.secrets > 0 {
            what = format!("{what} {}", trn(summary.secrets, "with 1 encrypted password", "with {} encrypted passwords"));
        }
        cx.toasts.success_file(what, path);
        true
    }

    /// Draw the dialog; false when it was closed.
    pub fn show(&mut self, ctx: &egui::Context, cx: &mut AppContext) -> bool {
        let mut open = true;
        let export = Cell::new(false);
        let cancel = Cell::new(false);
        let n = self.selected.iter().filter(|s| **s).count();
        let label = trn(n, "Export 1 connection…", "Export {} connections…");
        let (resp, _) = Dialog::new(tr("Export connections"))
            .icon(icon::UPLOAD_SIMPLE)
            .subtitle(tr("A file to import in another TernaryGrid"))
            .width(super::WIDTH_L)
            .body_padding(egui::Margin::symmetric(20, 16))
            .show_window(
                ctx,
                Id::new("ternarygrid_export_connections"),
                &mut open,
                None,
                |ui| self.body(ui, cx),
                Some(&mut |ui: &mut Ui| {
                    right(ui, |ui| {
                        if ui.add_enabled_ui(n > 0, |ui| kit::primary_icon(ui, icon::UPLOAD_SIMPLE, &label)).inner.clicked() {
                            export.set(true);
                        }
                        if cancel_button(ui).clicked() {
                            cancel.set(true);
                        }
                    });
                }),
            );
        super::dim_behind(ctx, resp.layer);
        let done = export.get() && self.export(cx);
        open && !done && !cancel.get() && !crate::widgets::escape_closes(ctx, resp.layer)
    }

    fn body(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let p = style::pal(ui);
        super::form_spacing(ui, 12.0);
        let store = &cx.conns.store;
        if store.connections.is_empty() {
            muted(ui, tr("There are no connections to export."));
            return;
        }
        let n = self.selected.iter().filter(|s| **s).count();
        ui.horizontal(|ui| {
            kit::semibold_label(ui, &trn(self.ids.len(), "1 connection", "{} connections"), 13.0, p.text);
            ui.label(RichText::new(trf("· {} selected", &[&n])).size(12.5).color(p.text_muted));
        });
        let cols = [Column::new("", W::Px(32.0)).center(), Column::new(tr("Connection"), W::Fr(1.3)), Column::new(tr("Host"), W::Fr(1.6)), Column::new(tr("Folder"), W::Fr(1.0))];
        let max_h = (ui.ctx().content_rect().height() - 520.0).max(140.0);
        let ids = &self.ids;
        let selected = &mut self.selected;
        table(ui, "export_conns", &cols, ids.len(), 30.0, max_h, |ui, row, col| {
            let Some(row) = row else {
                if col == 0 {
                    super::import::header_checkbox(ui, selected);
                }
                return;
            };
            let Some(c) = store.get(&ids[row]) else { return };
            match col {
                0 => {
                    if let Some(s) = selected.get_mut(row) {
                        kit::checkbox(ui, s, "");
                    }
                }
                1 => {
                    super::db_logo(ui, c.kind, 15.0);
                    ui.label(RichText::new(&c.name).font(style::medium(ui.ctx(), 13.0)).color(p.text));
                }
                2 => {
                    let target = if c.kind.is_file_based() { c.file_path.clone().unwrap_or_default() } else { format!("{}:{}", c.host, c.effective_port()) };
                    ui.add(egui::Label::new(RichText::new(target).monospace().size(12.0).color(p.text_muted)).truncate());
                }
                _ => {
                    if let Some(f) = &c.folder {
                        ui.label(RichText::new(icon::FOLDER).font(style::icon_font(13.0)).color(p.text_faint));
                        ui.add(egui::Label::new(RichText::new(f).size(12.5).color(p.text_muted)).truncate());
                    }
                }
            }
        });
        parts::hline(ui);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            kit::checkbox(ui, &mut self.include_passwords, tr("Include the saved passwords"));
            if self.include_passwords {
                let problem = if self.tried { self.password_problem() } else { None };
                super::form_row(ui, tr("File password"), |ui| {
                    add_field_ex(ui, text_field(&mut self.password, 320.0).password(true), problem == Some(tr("Enter a password for the file.")));
                });
                super::form_row(ui, tr("Repeat password"), |ui| {
                    add_field_ex(ui, text_field(&mut self.repeat, 320.0).password(true), problem == Some(tr("The passwords do not match.")));
                });
                if let Some(problem) = problem {
                    super::field_error(ui, problem);
                }
                parts::callout(
                    ui,
                    Tone::Warning,
                    None,
                    tr("The passwords are encrypted with this password; it is needed to import them. Keep the file private all the same."),
                    |_| {},
                );
            } else {
                ui.label(
                    RichText::new(tr(
                        "The file has no passwords: TernaryGrid asks for them when connecting. Connection settings, folders, connection types and network profiles are included.",
                    ))
                    .size(12.5)
                    .color(p.text_muted),
                );
            }
            if let Some(e) = &self.error {
                super::error_note(ui, e);
            }
        });
    }
}
