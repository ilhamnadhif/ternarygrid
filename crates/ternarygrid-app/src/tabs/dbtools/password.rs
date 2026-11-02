//! Change the password of the connection user. The statement comes from the
//! dialect (PostgreSQL sends a SCRAM verifier, never the clear text) and runs
//! on a separate short-lived session, so it never appears in a transaction
//! log or the query history. Passwords are never logged or displayed; the
//! saved password is updated when the connection stores it.

use crate::backend::Job;
use crate::command::AppCommand;
use crate::cx::AppContext;
use crate::tabs::TabView;
use crate::tabs::dbtools::look;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use std::time::Duration;
use ternarygrid_core::config::{AuthMethod, ConnectionConfig, SecretSlot, secret_key};
use ternarygrid_core::driver::TransactionMode;
use ternarygrid_core::{DbError, DbResult, Secret, tr};
use ternarygrid_ui::{kit, parts};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Changed {
    saved_updated: bool,
    saved_error: Option<String>,
}

/// Change-password tool of a connection.
pub struct ChangePasswordTab {
    /// Finished successfully: the pop-up closes.
    done: bool,
    conn: String,
    new_password: String,
    confirm: String,
    show: bool,
    update_saved: bool,
    job: Option<Job<Result<Changed, String>>>,
    outcome: Option<Result<Changed, String>>,
}

/// The connection keeps its password in the secret store.
fn saves_password(cfg: &ConnectionConfig) -> bool {
    cfg.save_password && matches!(cfg.auth, AuthMethod::Password)
}

/// Remove every occurrence of the secrets from a (server) message.
fn scrub(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    let mut list: Vec<&str> = secrets.iter().copied().filter(|s| !s.is_empty()).collect();
    // longest first, so that a statement is removed before the password inside it
    list.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for s in list {
        out = out.replace(s, "***");
    }
    out
}

impl ChangePasswordTab {
    pub fn new(conn: String) -> Self {
        ChangePasswordTab { done: false, conn, new_password: String::new(), confirm: String::new(), show: false, update_saved: true, job: None, outcome: None }
    }

    /// Enter the new password (both fields), as typed by the user.
    pub fn set_new_password(&mut self, password: &str) {
        self.new_password = password.to_string();
        self.confirm = password.to_string();
    }

    /// The change is in progress.
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// `Some(Ok(saved password updated))` after a successful change, `Some(Err(message))` after a failure.
    pub fn outcome(&self) -> Option<Result<bool, String>> {
        self.outcome.as_ref().map(|o| o.as_ref().map(|c| c.saved_updated).map_err(|e| e.clone()))
    }

    fn problem(&self, cx: &AppContext) -> Option<String> {
        let live = cx.conns.live(&self.conn)?;
        if live.config.read_only {
            return Some(tr("The connection is read-only.").to_string());
        }
        if live.dialect.change_password_sql(None, "x").is_none() {
            return Some(tr("Changing the password is not supported for this database.").to_string());
        }
        if self.new_password.is_empty() {
            return Some(tr("Enter the new password.").to_string());
        }
        if self.new_password != self.confirm {
            return Some(tr("The passwords do not match.").to_string());
        }
        // A dialect refuses a password it cannot write (Oracle: `"`).
        if live.dialect.change_password_sql(None, &self.new_password).is_none() {
            return Some(tr("This database does not accept this password (it contains a character the database cannot store in a password).").to_string());
        }
        None
    }

    /// Change the password (the tab must be connected and the fields valid).
    pub fn start(&mut self, cx: &mut AppContext) {
        if self.job.is_some() || self.problem(cx).is_some() {
            return;
        }
        let Some(live) = cx.conns.live(&self.conn) else { return };
        let Some(sql) = live.dialect.change_password_sql(None, &self.new_password) else {
            self.outcome = Some(Err(tr("Changing the password is not supported for this database.").to_string()));
            return;
        };
        let driver = live.driver.clone();
        let params = live.params().clone();
        let cfg = live.config.clone();
        let save = self.update_saved && saves_password(&cfg);
        let secrets = cx.conns.secrets.clone();
        let key = secret_key(&self.conn, SecretSlot::Password);
        let secret = Secret::new(self.new_password.clone());
        let literal = live.dialect.string_literal(&self.new_password);
        let password = self.new_password.clone();
        self.outcome = None;
        self.job = Some(cx.backend.spawn(async move {
            let changed: DbResult<()> = async {
                let mut s = driver.connect(&params).await?;
                let r = async {
                    crate::conn::init_session(s.as_mut(), &cfg, &params).await?;
                    ternarygrid_core::driver::execute_update(s.as_mut(), &sql, &[]).await?;
                    if s.transaction_mode() == TransactionMode::Manual {
                        s.commit().await?;
                    }
                    Ok::<(), DbError>(())
                }
                .await;
                let _ = s.close().await;
                r
            }
            .await;
            if let Err(e) = changed {
                return Err(scrub(&e.full_text(), &[&sql, &literal, &password]));
            }
            let mut out = Changed { saved_updated: false, saved_error: None };
            if save {
                match tokio::task::spawn_blocking(move || secrets.set(&key, &secret)).await {
                    Ok(Ok(())) => out.saved_updated = true,
                    Ok(Err(e)) => out.saved_error = Some(scrub(&e.full_text(), &[&password])),
                    Err(e) => out.saved_error = Some(e.to_string()),
                }
            }
            Ok(out)
        }));
    }

    /// Show the outcome of a change as if it had run (`saved_updated`: the saved password too).
    #[doc(hidden)]
    pub fn show_changed(&mut self, saved_updated: bool) {
        self.outcome = Some(Ok(Changed { saved_updated, saved_error: None }));
    }

    fn poll(&mut self) {
        if let Some(r) = self.job.as_mut().and_then(|j| j.poll()) {
            self.job = None;
            if r.is_ok() {
                self.new_password.clear();
                self.confirm.clear();
            }
            self.outcome = Some(r);
        }
    }
}

impl TabView for ChangePasswordTab {
    fn take_done(&mut self) -> bool {
        std::mem::take(&mut self.done)
    }

    fn popup_size(&self) -> Option<egui::Vec2> {
        Some(egui::vec2(540.0, 420.0))
    }

    fn title(&self) -> String {
        tr("Change password").to_string()
    }
    fn icon(&self) -> &'static str {
        icon::KEY
    }
    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }
    fn dedupe_key(&self) -> Option<String> {
        Some(format!("password:{}", self.conn))
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let was_running = self.job.is_some();
        self.poll();
        // Changed (and the saved password updated): the pop-up closes.
        if was_running
            && let Some(Ok(c)) = &self.outcome
            && c.saved_error.is_none()
        {
            cx.toasts.push(crate::toasts::ToastKind::Success, tr("The password was changed."), Some(tr("Reconnect so that new sessions use the new password.").to_string()));
            self.done = true;
        }
        // The saved password could not be updated because the vault locked itself (its
        // master password was changed elsewhere): ask for it again.
        if was_running
            && let Some(Ok(c)) = &self.outcome
            && c.saved_error.is_some()
            && cx.conns.secrets.is_locked()
        {
            cx.push(crate::command::AppCommand::UnlockVault);
        }
        let Some(cfg) = cx.conns.config(&self.conn).cloned() else {
            egui::Frame::new().inner_margin(look::BODY).show(ui, |ui| look::error(ui, tr("The connection does not exist.")));
            return;
        };
        let p = ternarygrid_ui::pal(ui);
        let conn = self.conn.clone();
        // Changed: what happened, Reconnect and Close.
        if let Some(Ok(c)) = self.outcome.clone() {
            look::footer(ui, ("pw_footer", &conn), |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::button_ex(ui, kit::Kind::Ghost, None, tr("Close"), false).clicked() {
                        self.done = true;
                    }
                });
            });
            look::body(ui, ("pw_body", &conn), |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::CHECK_CIRCLE).font(ternarygrid_ui::style::icon_font(18.0)).color(p.ok));
                    ui.label(RichText::new(tr("The password was changed.")).font(ternarygrid_ui::style::semibold(ui.ctx(), 14.0)).color(p.ok));
                });
                if c.saved_updated {
                    look::muted(ui, tr("The saved password was updated."));
                }
                if let Some(e) = &c.saved_error {
                    look::error(ui, &format!("{}\n{e}", tr("The saved password could not be updated.")));
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(tr("Reconnect so that new sessions use the new password."));
                    if kit::button_ex(ui, kit::Kind::Secondary, Some(icon::ARROW_CLOCKWISE), tr("Reconnect"), true).clicked() {
                        cx.push(AppCommand::Reconnect(self.conn.clone()));
                    }
                });
            });
            return;
        }
        let connected = cx.conns.is_connected(&self.conn);
        let busy = self.job.is_some();
        let problem = self.problem(cx);
        look::footer(ui, ("pw_footer", &conn), |ui| {
            if busy {
                parts::spinner_label(ui, tr("Changing the password…"));
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled_ui(connected && !busy && problem.is_none(), |ui| kit::primary_icon(ui, icon::KEY, tr("Change password"))).inner.clicked() {
                    self.start(cx);
                }
                if kit::button_ex(ui, kit::Kind::Ghost, None, tr("Cancel"), false).clicked() {
                    self.done = true;
                }
            });
        });
        look::body(ui, ("pw_body", &conn), |ui| {
            if !cx.popup {
                look::title(ui, tr("Change password"));
            }
            let lw = 150.0;
            look::row(ui, tr("Connection"), lw, |ui| ui.label(RichText::new(&cfg.name).font(ternarygrid_ui::style::semibold(ui.ctx(), 13.0)).color(p.text)));
            look::row(ui, tr("User"), lw, |ui| ui.label(cfg.user.clone().filter(|u| !u.is_empty()).unwrap_or_else(|| tr("(current user)").to_string())));
            look::divider(ui);
            if !connected {
                look::note_with(ui, parts::Tone::Info, None, tr("Connect first: the password is changed by the server."), |ui| {
                    if kit::button_ex(ui, kit::Kind::Secondary, Some(icon::PLUGS_CONNECTED), tr("Connect"), true).clicked() {
                        cx.push(AppCommand::Connect(self.conn.clone()));
                    }
                });
                return;
            }
            ui.add_enabled_ui(!busy, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                let field = |ui: &mut Ui, value: &mut String, show: bool| {
                    let r = ui.add(egui::TextEdit::singleline(value).password(!show).desired_width(ui.available_width()).margin(egui::Margin::symmetric(8, 5)));
                    if r.has_focus() {
                        ternarygrid_ui::style::focus_halo(ui, r.rect, ternarygrid_ui::style::RADIUS);
                    }
                };
                let show = self.show;
                look::row(ui, tr("New password"), lw, |ui| field(ui, &mut self.new_password, show));
                look::row(ui, tr("Confirm password"), lw, |ui| field(ui, &mut self.confirm, show));
                look::row(ui, "", lw, |ui| look::checkbox(ui, &mut self.show, tr("Show the password")));
                look::row(ui, "", lw, |ui| {
                    if saves_password(&cfg) {
                        look::checkbox(ui, &mut self.update_saved, tr("Update the saved password of the connection"));
                    } else {
                        ui.add(
                            egui::Label::new(
                                RichText::new(tr("The connection does not save its password: enter the new password when you connect next time.")).color(p.text_muted),
                            )
                            .wrap(),
                        );
                    }
                });
            });
            if !busy
                && let Some(pr) = &problem
                && (!self.new_password.is_empty() || !self.confirm.is_empty() || pr != tr("Enter the new password."))
            {
                look::note(ui, parts::Tone::Warning, None, pr);
            }
            if let Some(Err(e)) = &self.outcome {
                look::error(ui, &format!("{}\n{e}", tr("The password could not be changed.")));
            }
        });
    }

    crate::impl_tab_any!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubbing() {
        let sql = "ALTER USER CURRENT_USER() IDENTIFIED BY 's3cret'";
        let msg = format!("You have an error near '{sql}' and s3cret");
        let out = scrub(&msg, &[sql, "'s3cret'", "s3cret"]);
        assert!(!out.contains("s3cret"), "{out}");
        assert_eq!(scrub("nothing", &["", "x"]), "nothing");
    }
}
