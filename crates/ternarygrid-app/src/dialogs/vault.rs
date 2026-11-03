//! Master password dialog of the encrypted secret vault (FR-C4).

use super::{add_field_ex, field_error, text_field};
use crate::backend::Job;
use crate::cx::AppContext;
use egui::{Id, RichText};
use egui_phosphor::regular as icon;
use std::cell::Cell;
use ternarygrid_core::{DbError, DbResult, Secret, tr};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::{self, Dialog, Tone};
use ternarygrid_ui::style;

/// A secret could not be written because the vault locked itself (its master
/// password was changed by another TernaryGrid): ask for the password again
/// instead of only showing the error. Returns true when it does.
pub fn unlock_after_failed_write(cx: &mut AppContext, e: &DbError) -> bool {
    if e.kind != ternarygrid_core::ErrorKind::Auth || !cx.services.secrets.read().is_locked() {
        return false;
    }
    cx.push(crate::command::AppCommand::UnlockVault);
    cx.toasts.info(tr("The master password was changed elsewhere: unlock the saved passwords, then save again."));
    true
}

#[derive(Default)]
pub struct VaultDialog {
    password: String,
    confirm: String,
    job: Option<Job<DbResult<()>>>,
    /// Why the password was refused, under the master password (`false`) or its repetition.
    error: Option<(bool, String)>,
    /// "Skip" (or closed): connections ask for their passwords instead of the vault.
    pub skipped: bool,
}

impl VaultDialog {
    /// Draw the dialog; clears `open` when it is done or skipped.
    pub fn show(&mut self, ctx: &egui::Context, cx: &mut AppContext, open: &mut bool) {
        let vault = cx.services.secrets.read().vault().cloned();
        let Some(vault) = vault else {
            *open = false;
            return;
        };
        if let Some(job) = self.job.as_mut()
            && let Some(res) = job.poll()
        {
            self.job = None;
            match res {
                Ok(()) => {
                    self.password.clear();
                    self.confirm.clear();
                    self.error = None;
                    cx.toasts.success(tr("Saved passwords unlocked"));
                    *open = false;
                    return;
                }
                Err(e) => self.error = Some((false, e.message)),
            }
        }
        let create = !vault.exists();
        let busy = self.job.is_some();
        let submit = Cell::new(false);
        let skip = Cell::new(false);
        let title = if create { tr("Create the password vault") } else { tr("Unlock saved passwords") };
        let error = self.error.clone();
        let on_repeat = error.as_ref().is_some_and(|(repeat, _)| *repeat);
        let (resp, _) = Dialog::new(title).icon(icon::LOCK_SIMPLE).width(super::WIDTH_S).body_padding(super::BODY).show_modal(
            ctx,
            Id::new("vault_unlock"),
            |ui| {
                super::form_spacing(ui, 14.0);
                let text = RichText::new(tr("Saved passwords are kept in a file encrypted with your master password.")).color(style::pal(ui).text_muted);
                super::label_lh(ui, text, egui::FontId::proportional(13.0), 1.45, false);
                let r = super::form_row(ui, tr("Master password"), |ui| {
                    ui.add_enabled_ui(!busy, |ui| add_field_ex(ui, text_field(&mut self.password, f32::INFINITY).password(true), error.is_some() && !on_repeat)).inner
                });
                if !busy && self.password.is_empty() {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !create {
                    submit.set(true);
                }
                if let Some((false, e)) = &error {
                    field_error(ui, e);
                }
                if create {
                    let r = super::form_row(ui, tr("Repeat password"), |ui| {
                        ui.add_enabled_ui(!busy, |ui| add_field_ex(ui, text_field(&mut self.confirm, f32::INFINITY).password(true), on_repeat)).inner
                    });
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit.set(true);
                    }
                    if let Some((true, e)) = &error {
                        field_error(ui, e);
                    }
                    super::note(ui, Tone::Warning, tr("If you forget it, the saved passwords cannot be recovered."));
                }
            },
            |ui| {
                if busy {
                    parts::spinner_label(ui, tr("Deriving the key…"));
                    return;
                }
                if kit::button_ex(ui, Kind::Ghost, None, tr("Skip"), false).on_hover_text(tr("Passwords will be asked when connecting.")).clicked() {
                    skip.set(true);
                }
                super::right(ui, |ui| {
                    let (label, glyph) = if create { (tr("Create"), icon::PLUS) } else { (tr("Unlock"), icon::LOCK_SIMPLE_OPEN) };
                    if kit::primary_icon(ui, glyph, label).clicked() {
                        submit.set(true);
                    }
                });
            },
        );
        if submit.get() && !busy {
            if self.password.is_empty() {
                self.error = Some((false, tr("Enter the master password.").to_string()));
            } else if create && self.password != self.confirm {
                self.error = Some((true, tr("The passwords do not match.").to_string()));
            } else {
                let master = Secret::new(std::mem::take(&mut self.password));
                self.confirm.clear();
                self.error = None;
                self.job = Some(cx.backend.spawn(async move {
                    tokio::task::spawn_blocking(move || if create { vault.create(&master) } else { vault.unlock(&master) }).await.map_err(|e| DbError::internal(e.to_string()))?
                }));
            }
        }
        if skip.get() || (resp.close && !busy) {
            self.skipped = true;
            *open = false;
        }
    }
}
