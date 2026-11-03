//! Modal and floating dialogs owned by the application, and the pieces they
//! share on top of `ternarygrid_ui` (form fields, notes, semibold text, tables,
//! SQL previews): every dialog is a [`Dialog`] with the icon badge, title,
//! subtitle and × in its header and the buttons in its footer, as in the
//! design canvas "TernaryGrid modern redesign".

pub mod alter_db;
pub mod connection;
pub mod create_db;
pub mod drop;
pub mod import;
pub mod vault;

use crate::command::AppCommand;
use crate::conn::{ConnectionManager, Credentials, PromptField};
use crate::cx::AppContext;
use connection::ConnectionDialog;
use egui::{Align, Align2, Color32, CornerRadius, Id, Layout, Margin, Rect, Response, RichText, Sense, Stroke, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType, vec2};
use egui_phosphor::regular as icon;
use std::cell::Cell;
use ternarygrid_core::config::{SecretSlot, secret_key};
use ternarygrid_core::{Secret, tr, trf};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::{self, Dialog, Tone};
use ternarygrid_ui::style::{self, CONTROL_H, RADIUS, RADIUS_LG};

struct CredentialPrompt {
    conn_id: String,
    conn_name: String,
    /// Who connects where (`app@10.0.4.12:5432 via bastion`).
    target: String,
    fields: Vec<PromptField>,
    password: String,
    ssh_password: String,
    ssh_passphrase: String,
    proxy_password: String,
    save: bool,
}

struct DeleteConfirm {
    conn_id: String,
    name: String,
    /// "Disconnect and delete" was clicked (in this pass): deleted once it is disconnected.
    disconnecting: Option<u64>,
}

/// What the folder dialog does.
#[derive(Debug, Clone, PartialEq)]
pub enum FolderAction {
    /// A new folder inside `parent` (`None`: the top level).
    New { parent: Option<String> },
    /// Rename this folder (a `/` separated path).
    Rename(String),
    /// Delete this folder; its connections and subfolders move up.
    Delete(String),
}

struct FolderDialog {
    action: FolderAction,
    name: String,
    error: Option<String>,
    focused: bool,
}

/// The folder a path is in, and its own name.
fn split_folder(path: &str) -> (Option<&str>, &str) {
    match path.rsplit_once('/') {
        Some((parent, name)) => (Some(parent), name),
        None => (None, path),
    }
}

#[derive(Default)]
pub struct Dialogs {
    folder: Option<FolderDialog>,
    connection: Vec<ConnectionDialog>,
    prompts: Vec<CredentialPrompt>,
    ready: Vec<(String, Credentials)>,
    delete: Option<DeleteConfirm>,
    pub palette_open: bool,
    pub about_open: bool,
    pub shortcuts_open: bool,
    /// The text of the keyboard shortcuts' filter.
    shortcuts_filter: String,
    pub vault_open: bool,
    pub drivers_open: bool,
    pub vault: vault::VaultDialog,
    pub import: Option<import::ImportDialog>,
    pub drop: Option<drop::DropDialog>,
    pub create_db: Option<create_db::CreateDatabaseDialog>,
    pub alter_db: Option<alter_db::AlterDatabaseDialog>,
}

impl Dialogs {
    pub fn new_connection(
        &mut self,
        kind: Option<ternarygrid_core::DbKind>,
        folder: Option<String>,
        _conns: &ConnectionManager,
        default_view: ternarygrid_core::config::NavigatorView,
    ) {
        let mut d = ConnectionDialog::new(kind, folder);
        d.cfg.navigator_view = default_view;
        // One new-connection dialog: asking again starts it over (another database type).
        self.connection.retain(|d| !d.is_new);
        self.connection.push(d);
    }

    pub fn edit_connection(&mut self, id: &str, conns: &ConnectionManager) {
        if self.connection.iter().any(|d| d.cfg.id == id) {
            return;
        }
        if let Some(cfg) = conns.config(id) {
            self.connection.push(ConnectionDialog::edit(cfg.clone()));
        }
    }

    pub fn confirm_delete_connection(&mut self, id: &str, conns: &ConnectionManager) {
        if let Some(cfg) = conns.config(id) {
            self.delete = Some(DeleteConfirm { conn_id: id.to_string(), name: cfg.name.clone(), disconnecting: None });
        }
    }

    /// New / rename / delete a navigator folder.
    pub fn folder_dialog(&mut self, action: FolderAction) {
        let name = match &action {
            FolderAction::Rename(path) => split_folder(path).1.to_string(),
            _ => String::new(),
        };
        self.folder = Some(FolderDialog { action, name, error: None, focused: false });
    }

    pub fn open_credentials(&mut self, id: &str, fields: Vec<PromptField>, conns: &ConnectionManager) {
        if self.prompts.iter().any(|p| p.conn_id == id) {
            return;
        }
        let Some(cfg) = conns.config(id) else { return };
        self.prompts.push(CredentialPrompt {
            conn_id: id.to_string(),
            conn_name: cfg.name.clone(),
            target: connection_target(cfg),
            fields,
            password: String::new(),
            ssh_password: String::new(),
            ssh_passphrase: String::new(),
            proxy_password: String::new(),
            save: cfg.save_password && matches!(cfg.auth, ternarygrid_core::config::AuthMethod::Password),
        });
    }

    /// A dialog that takes keyboard input is open (shortcuts are suspended).
    pub fn captures_keyboard(&self) -> bool {
        self.palette_open
            || self.vault_open
            || self.delete.is_some()
            || self.folder.is_some()
            || self.drop.is_some()
            || !self.prompts.is_empty()
            || self.import.is_some()
            || self.create_db.is_some()
            || self.alter_db.is_some()
            || !self.connection.is_empty()
    }

    /// Credentials entered by the user since the last call.
    pub fn take_credentials(&mut self) -> Vec<(String, Credentials)> {
        std::mem::take(&mut self.ready)
    }

    pub fn show(&mut self, ctx: &egui::Context, cx: &mut AppContext) {
        for d in &mut self.connection {
            d.show(ctx, cx);
        }
        self.connection.retain(|d| d.open);
        self.show_prompts(ctx, cx);
        self.show_delete(ctx, cx);
        self.show_folder(ctx, cx);
        if self.vault_open {
            let mut open = true;
            self.vault.show(ctx, cx, &mut open);
            self.vault_open = open;
        }
        if let Some(d) = self.import.as_mut()
            && !d.show(ctx, cx)
        {
            self.import = None;
        }
        if let Some(d) = self.drop.as_mut()
            && !d.show(ctx, cx)
        {
            self.drop = None;
        }
        if let Some(d) = self.create_db.as_mut()
            && !d.show(ctx, cx)
        {
            self.create_db = None;
        }
        if let Some(d) = self.alter_db.as_mut()
            && !d.show(ctx, cx)
        {
            self.alter_db = None;
        }
        if self.about_open {
            let mut open = true;
            let (resp, _) = Dialog::new(tr("About TernaryGrid")).width(WIDTH_S).body_padding(Margin { left: 24, right: 24, top: 28, bottom: 24 }).show_window(
                ctx,
                Id::new("ternarygrid_about"),
                &mut open,
                None,
                about_body,
                None,
            );
            dim_behind(ctx, resp.layer);
            self.about_open = open && !crate::widgets::escape_closes(ctx, resp.layer);
        }
        if self.drivers_open {
            let mut open = true;
            let (resp, _) = Dialog::new(tr("Database drivers")).icon(icon::PLUG).width(WIDTH_L).body_padding(Margin::symmetric(20, 16)).show_window(
                ctx,
                Id::new("ternarygrid_drivers"),
                &mut open,
                None,
                |ui| drivers_body(ui, cx),
                None,
            );
            dim_behind(ctx, resp.layer);
            self.drivers_open = open && !crate::widgets::escape_closes(ctx, resp.layer);
        }
        if self.shortcuts_open {
            let mut open = true;
            let customize = Cell::new(false);
            let preset = trf("Preset: {}", &[&cx.keymap.preset().label()]);
            let filter = &mut self.shortcuts_filter;
            let (resp, _) = Dialog::new(tr("Keyboard shortcuts")).icon(icon::KEYBOARD).width(WIDTH_L).show_window(
                ctx,
                Id::new("ternarygrid_shortcuts"),
                &mut open,
                None,
                |ui| shortcuts_body(ui, cx, filter),
                Some(&mut |ui: &mut Ui| {
                    if kit::button_ex(ui, Kind::Ghost, Some(icon::GEAR), tr("Customize in Settings"), false).clicked() {
                        customize.set(true);
                    }
                    right(ui, |ui| {
                        ui.label(RichText::new(&preset).size(12.0).color(style::pal(ui).text_muted));
                    });
                }),
            );
            if customize.get() {
                cx.push(AppCommand::OpenSettings);
                open = false;
            }
            dim_behind(ctx, resp.layer);
            self.shortcuts_open = open && !crate::widgets::escape_closes(ctx, resp.layer);
        }
        if self.palette_open {
            let mut open = true;
            crate::palette::show(ctx, cx, &mut open);
            self.palette_open = open;
        }
    }

    fn show_prompts(&mut self, ctx: &egui::Context, cx: &mut AppContext) {
        let mut done: Vec<usize> = Vec::new();
        for (i, p) in self.prompts.iter_mut().enumerate() {
            let mut open = true;
            let submit = Cell::new(false);
            let cancel = Cell::new(false);
            let title = trf("Connect to {}", &[&p.conn_name]);
            let target = p.target.clone();
            let (resp, _) = Dialog::new(&title).icon(icon::KEY).subtitle(&target).width(WIDTH_S).body_padding(BODY).show_window(
                ctx,
                Id::new(("cred_prompt", &p.conn_id)),
                &mut open,
                None,
                |ui| {
                    form_spacing(ui, 12.0);
                    let mut first = true;
                    for f in p.fields.clone() {
                        let (label, value) = match f {
                            PromptField::Password => (tr("Password"), &mut p.password),
                            PromptField::SshPassword => (tr("SSH password"), &mut p.ssh_password),
                            PromptField::SshPassphrase => (tr("Key passphrase"), &mut p.ssh_passphrase),
                            PromptField::ProxyPassword => (tr("Proxy password"), &mut p.proxy_password),
                        };
                        let r = form_row(ui, label, |ui| add_field(ui, text_field(value, f32::INFINITY).password(true)));
                        // Enter first: re-focusing would hide that the field lost focus.
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            submit.set(true);
                        }
                        if first {
                            if !submit.get() && !ui.ctx().memory(|m| m.focused().is_some()) {
                                r.request_focus();
                            }
                            first = false;
                        }
                    }
                    let save = if p.fields.len() > 1 { tr("Save passwords") } else { tr("Save password") };
                    form_row(ui, "", |ui| ternarygrid_ui::kit::checkbox(ui, &mut p.save, save));
                },
                Some(&mut |ui: &mut Ui| {
                    right(ui, |ui| {
                        if kit::primary_icon(ui, icon::PLUG, tr("Connect")).clicked() {
                            submit.set(true);
                        }
                        if cancel_button(ui).clicked() {
                            cancel.set(true);
                        }
                    });
                }),
            );
            dim_behind(ctx, resp.layer);
            if cancel.get() {
                done.push(i);
            }
            if submit.get() {
                let opt = |s: &str| if s.is_empty() { None } else { Some(Secret::new(s)) };
                let creds =
                    Credentials { password: opt(&p.password), ssh_password: opt(&p.ssh_password), ssh_passphrase: opt(&p.ssh_passphrase), proxy_password: opt(&p.proxy_password) };
                if p.save {
                    let pairs = [
                        (SecretSlot::Password, &creds.password),
                        (SecretSlot::SshPassword, &creds.ssh_password),
                        (SecretSlot::SshPassphrase, &creds.ssh_passphrase),
                        (SecretSlot::ProxyPassword, &creds.proxy_password),
                    ];
                    for (slot, v) in pairs {
                        if let Some(v) = v {
                            let key = match cx.conns.config(&p.conn_id) {
                                Some(cfg) => cx.conns.store.network_secret_key(cfg, slot),
                                None => secret_key(&p.conn_id, slot),
                            };
                            if let Err(e) = cx.conns.secrets.set(&key, v)
                                && !crate::dialogs::vault::unlock_after_failed_write(cx, &e)
                            {
                                cx.toasts.error(tr("Cannot store the password securely"), Some(e.to_string()));
                            }
                        }
                    }
                    if let Some(cfg) = cx.conns.config_mut(&p.conn_id) {
                        // "Ask every time" with a saved password would keep asking: it is used from now on.
                        let ask = matches!(cfg.auth, ternarygrid_core::config::AuthMethod::Prompt) && creds.password.is_some();
                        if !cfg.save_password || ask {
                            cfg.save_password = true;
                            if ask {
                                cfg.auth = ternarygrid_core::config::AuthMethod::Password;
                            }
                            cx.push(AppCommand::SaveConnections);
                        }
                    }
                }
                self.ready.push((p.conn_id.clone(), creds));
                done.push(i);
            } else if !open || crate::widgets::escape_closes(ctx, resp.layer) {
                done.push(i);
            }
        }
        done.sort_unstable();
        done.dedup();
        for i in done.into_iter().rev() {
            self.prompts.remove(i);
        }
    }

    fn show_folder(&mut self, ctx: &egui::Context, cx: &mut AppContext) {
        let Some(d) = &mut self.folder else { return };
        let ok = Cell::new(false);
        let cancel = Cell::new(false);
        let close = match d.action.clone() {
            FolderAction::Delete(path) => {
                let inside = |f: &Option<String>| f.as_deref().is_some_and(|f| f == path || f.starts_with(&format!("{path}/")));
                let n = cx.conns.configs().iter().filter(|c| inside(&c.folder)).count();
                let target = split_folder(&path).0.map(str::to_string).unwrap_or_else(|| tr("the top level").to_string());
                let question = trf("Delete the folder \"{}\"?", &[&split_folder(&path).1]);
                let moved = match n {
                    0 => trf("Its subfolders move to {}.", &[&target]),
                    1 => trf("Its connection and subfolders move to {}.", &[&target]),
                    n => trf("Its {} connections and subfolders move to {}.", &[&n, &target]),
                };
                let text = format!("{question} {moved} {}", tr("No connection is deleted."));
                let place = path.replace('/', " › ");
                let error = d.error.clone();
                let (resp, _) = Dialog::new(tr("Delete folder")).icon(icon::TRASH).tone(Tone::Error).subtitle(&place).width(WIDTH_S).body_padding(BODY).show_modal(
                    ctx,
                    Id::new("ternarygrid_folder"),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 10.0;
                        body_text_lh(ui, &text, 1.5);
                        if let Some(e) = &error {
                            crate::dialogs::error_note(ui, e);
                        }
                    },
                    |ui| {
                        right(ui, |ui| {
                            if kit::button_ex(ui, Kind::DangerFilled, Some(icon::TRASH), tr("Delete folder"), false).clicked() {
                                ok.set(true);
                            }
                            if cancel_button(ui).clicked() {
                                cancel.set(true);
                            }
                        });
                    },
                );
                let mut close = cancel.get() || resp.close;
                if ok.get() {
                    match cx.conns.store.delete_folder(&path) {
                        Ok(_) => {
                            cx.push(AppCommand::SaveConnections);
                            close = true;
                        }
                        Err(e) => d.error = Some(e.to_string()),
                    }
                }
                close
            }
            action => {
                let (title, parent) = match &action {
                    FolderAction::New { parent } => (tr("New folder"), parent.clone()),
                    FolderAction::Rename(path) => (tr("Rename folder"), split_folder(path).0.map(str::to_string)),
                    FolderAction::Delete(_) => unreachable!(),
                };
                let folders = cx.conns.folders().to_vec();
                let path_of = |name: &str| match &parent {
                    Some(p) => format!("{p}/{name}"),
                    None => name.to_string(),
                };
                let problem_of = |name: &str| {
                    let path = path_of(name);
                    if name.is_empty() {
                        Some(tr("Type a name."))
                    } else if name.contains('/') {
                        Some(tr("The name cannot contain \"/\"."))
                    } else if folders.iter().any(|f| f == &path) && action != FolderAction::Rename(path.clone()) {
                        Some(tr("A folder with this name already exists."))
                    } else {
                        None
                    }
                };
                let enter = Cell::new(false);
                let valid = Cell::new(false);
                let (label, glyph) = if matches!(action, FolderAction::New { .. }) { (tr("Create"), icon::PLUS) } else { (tr("Rename"), icon::CHECK) };
                let place = parent.as_deref().map(|p| trf("In {}", &[&p.replace('/', " › ")]));
                let mut dialog = Dialog::new(title).icon(icon::FOLDER).width(WIDTH_S).body_padding(BODY);
                if let Some(place) = &place {
                    dialog = dialog.subtitle(place);
                }
                let (resp, _) = dialog.show_modal(
                    ctx,
                    Id::new("ternarygrid_folder"),
                    |ui| {
                        form_spacing(ui, 10.0);
                        // A problem is said once there is a name (an empty field only disables Create).
                        let shown_of = |name: &str| problem_of(name).filter(|_| !name.is_empty());
                        let outlined = shown_of(d.name.trim()).is_some();
                        let edit = add_field_ex(ui, text_field(&mut d.name, f32::INFINITY).hint_text(hint(ui, tr("Folder name"))), outlined);
                        if !d.focused {
                            edit.request_focus();
                            d.focused = true;
                        }
                        enter.set(edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                        let name = d.name.trim().to_string();
                        let (problem, shown) = (problem_of(&name), shown_of(&name));
                        if shown.is_some() != outlined {
                            ui.ctx().request_repaint();
                        }
                        valid.set(problem.is_none());
                        if let Some(p) = shown {
                            field_error_at(ui, p, 0.0);
                        }
                        if let Some(e) = &d.error {
                            crate::dialogs::error_note(ui, e);
                        }
                    },
                    |ui| {
                        right(ui, |ui| {
                            if ui.add_enabled_ui(valid.get(), |ui| kit::primary_icon(ui, glyph, label)).inner.clicked() {
                                ok.set(true);
                            }
                            if cancel_button(ui).clicked() {
                                cancel.set(true);
                            }
                        });
                    },
                );
                let name = d.name.trim().to_string();
                let mut close = cancel.get() || resp.close;
                if (ok.get() || enter.get()) && problem_of(&name).is_none() {
                    let path = path_of(&name);
                    let result = match &action {
                        FolderAction::New { .. } => {
                            cx.conns.store.add_folder(&path);
                            Ok(())
                        }
                        FolderAction::Rename(old) => cx.conns.store.rename_folder(old, &path),
                        FolderAction::Delete(_) => Ok(()),
                    };
                    match result {
                        Ok(()) => {
                            cx.push(AppCommand::SaveConnections);
                            close = true;
                        }
                        Err(e) => d.error = Some(e.to_string()),
                    }
                }
                close
            }
        };
        if close {
            self.folder = None;
        }
    }

    fn show_delete(&mut self, ctx: &egui::Context, cx: &mut AppContext) {
        let Some(d) = &mut self.delete else { return };
        // Disconnected first, the usual way: open transactions are asked about, the
        // editors' sessions close and the after-disconnect command runs.
        let connected = cx.conns.is_connected(&d.conn_id) || cx.conns.status(&d.conn_id) == crate::conn::ConnStatus::Connecting;
        let delete = Cell::new(false);
        let cancel = Cell::new(false);
        let waiting = d.disconnecting.is_some() && connected;
        let (resp, _) = Dialog::new(tr("Delete connection")).icon(icon::TRASH).tone(Tone::Error).subtitle(&d.name).width(WIDTH_S).body_padding(BODY).show_modal(
            ctx,
            Id::new("delete_conn"),
            |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                body_text_lh(ui, &trf("Delete the connection \"{}\" and its saved passwords? This cannot be undone.", &[&d.name]), 1.5);
                if connected {
                    note(ui, Tone::Warning, &trf("{} is connected; it will be disconnected first.", &[&d.name]));
                }
            },
            |ui| {
                if waiting {
                    parts::spinner_label(ui, tr("Disconnecting…"));
                }
                right(ui, |ui| {
                    let label = if connected { tr("Disconnect and delete") } else { tr("Delete connection") };
                    if ui.add_enabled_ui(!waiting, |ui| kit::button_ex(ui, Kind::DangerFilled, Some(icon::TRASH), label, false)).inner.clicked() {
                        delete.set(true);
                    }
                    if cancel_button(ui).clicked() {
                        cancel.set(true);
                    }
                });
            },
        );
        let mut close = cancel.get() || resp.close;
        let now = ctx.cumulative_pass_nr();
        if delete.get() && connected {
            cx.push(AppCommand::Disconnect(d.conn_id.clone()));
            d.disconnecting = Some(now);
        } else if (delete.get() || d.disconnecting.is_some()) && !connected {
            cx.conns.remove(cx.backend, &d.conn_id);
            cx.push(AppCommand::SaveConnections);
            close = true;
        } else if let Some(asked) = d.disconnecting {
            // Still connected once the question about its transactions is gone (this dialog is
            // the top modal again): the disconnect was cancelled, and so is the delete.
            let top = ctx.memory(|m| m.top_modal_layer());
            if now > asked + 3 && top.is_some() && top == resp.layer {
                d.disconnecting = None;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if close {
            self.delete = None;
        }
    }
}

/// `user@host:port via ssh-host` (or the file) of a connection, for the password prompt.
fn connection_target(cfg: &ternarygrid_core::config::ConnectionConfig) -> String {
    if cfg.kind.is_file_based() {
        return cfg.file_path.clone().unwrap_or_default();
    }
    let user = cfg.user.as_deref().map(|u| format!("{u}@")).unwrap_or_default();
    let mut s = format!("{user}{}:{}", cfg.host, cfg.effective_port());
    if let Some(ssh) = cfg.ssh.as_ref().filter(|s| s.enabled && !s.host.is_empty()) {
        s.push_str(&format!(" {} {}", tr("via"), ssh.host));
    }
    s
}

/// The body of About: the app's tile, name, version and the promises.
fn about_body(ui: &mut Ui) {
    let p = style::pal(ui);
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(16), p.accent);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon::TABLE, style::icon_font(34.0), Color32::WHITE);
        ui.add_space(8.0);
        kit::semibold_label(ui, "TernaryGrid", 22.0, p.text);
        ui.label(RichText::new(format!("{} {}", tr("Version"), env!("CARGO_PKG_VERSION"))).size(12.5).color(p.text_muted));
        ui.label(RichText::new(tr("A fast, lightweight database client, written in Rust.")).color(p.text));
        ui.label(RichText::new(tr("License: MIT OR Apache-2.0")).size(12.0).color(p.text_muted));
        ui.label(RichText::new(tr("No telemetry. Only connects to the servers you configure.")).size(12.0).color(p.text_muted));
    });
}

/// The capability table of the built-in drivers.
fn drivers_body(ui: &mut Ui, cx: &mut AppContext) {
    let p = style::pal(ui);
    ui.spacing_mut().item_spacing.y = 12.0;
    muted(ui, tr("Drivers are built into TernaryGrid (no downloads) and written in Rust; SQLite is an embedded engine."));
    // In the order of the new connection dialog's list.
    let kinds: Vec<_> = ternarygrid_core::DbKind::ALL.into_iter().filter(|k| cx.conns.registry.get(*k).is_some()).collect();
    let cols = [
        Column::new(tr("Database"), W::Fr(1.3)),
        Column::new(tr("Driver"), W::Fr(1.4)),
        Column::new(tr("Transactions"), W::Fr(0.9)),
        Column::new(tr("Explain"), W::Fr(1.0)),
        Column::new(tr("Sessions"), W::Fr(0.8)),
        Column::new(tr("Locks"), W::Fr(0.7)),
        Column::new(tr("Backup"), W::Fr(0.7)),
        Column::new(tr("Debugger"), W::Fr(0.8)),
        Column::new(tr("Cancel"), W::Fr(0.7)),
    ];
    let max_h = (ui.ctx().content_rect().height() - 320.0).max(160.0);
    table(ui, "drivers_table", &cols, kinds.len(), 30.0, max_h, |ui, row, col| {
        let Some(row) = row else { return };
        let k = kinds[row];
        let Some(d) = cx.conns.registry.get(k) else { return };
        let info = d.info();
        let c = &info.capabilities;
        let yes = |ui: &mut Ui, b: bool| {
            if b {
                ui.label(RichText::new("✓").color(p.ok));
            } else {
                ui.label(RichText::new("—").color(p.text_faint));
            }
        };
        match col {
            0 => {
                db_logo(ui, k, 15.0);
                ui.label(RichText::new(k.display_name()).font(style::medium(ui.ctx(), 13.0)).color(p.text));
            }
            1 => {
                ui.label(RichText::new(driver_library(k).unwrap_or(info.name)).monospace().size(12.0).color(p.text_muted)).on_hover_text(info.description);
            }
            2 => yes(ui, c.transactions),
            3 => {
                if c.explain_analyze {
                    ui.label(RichText::new("✓ + ANALYZE").size(12.5).color(p.ok));
                } else {
                    yes(ui, c.explain);
                }
            }
            4 => yes(ui, c.session_manager),
            5 => yes(ui, c.lock_manager),
            6 => yes(ui, c.backup),
            7 => yes(ui, c.debugger),
            _ => yes(ui, c.cancel),
        }
    });
    // The database types without a built-in driver, and the well-known ones TernaryGrid has no type for.
    let mut missing: Vec<&str> = ternarygrid_core::DbKind::ALL.into_iter().filter(|k| cx.conns.registry.get(*k).is_none()).map(|k| k.display_name()).collect();
    missing.extend(["Db2", "Firebird", "H2", "Trino"]);
    let list = match missing.split_last() {
        Some((last, rest)) if !rest.is_empty() => trf("{} and {}", &[&rest.join(", "), last]),
        _ => missing.join(""),
    };
    muted(ui, &trf("Not available yet: {}.", &[&list]));
}

/// The Rust library (or protocol) a built-in driver speaks through.
fn driver_library(kind: ternarygrid_core::DbKind) -> Option<&'static str> {
    use ternarygrid_core::DbKind as K;
    Some(match kind {
        K::Postgres | K::CockroachDb => "tokio-postgres",
        K::MySql | K::MariaDb => "mysql_async",
        K::Sqlite => "rusqlite (embedded)",
        K::SqlServer => "tiberius",
        K::Oracle => "oracledb (thin)",
        K::ClickHouse | K::Elasticsearch => "HTTP",
        K::Cassandra => "scylla",
        K::MongoDb => "mongodb",
        K::Redis => "redis",
        _ => return None,
    })
}

/// The groups of shortcuts, as in the settings.
const SHORTCUT_GROUPS: &[ternarygrid_config::ActionCategory] = {
    use ternarygrid_config::ActionCategory as C;
    &[C::Sql, C::Editor, C::Data, C::Transactions, C::Connections, C::Navigation, C::View, C::Tools, C::Application, C::Help]
};

/// Every bound shortcut in two columns of groups, with a filter.
fn shortcuts_body(ui: &mut Ui, cx: &mut AppContext, filter: &mut String) {
    use crate::keys::{Action, IS_MAC, action_label};
    let p = style::pal(ui);
    ui.spacing_mut().item_spacing.y = 14.0;
    let w = ui.available_width();
    kit::search_field(ui, Id::new("shortcuts_filter"), filter, tr("Filter shortcuts"), w);
    let needle = filter.trim().to_lowercase();
    let groups: Vec<(&str, Vec<(&str, Vec<String>)>)> = SHORTCUT_GROUPS
        .iter()
        .filter_map(|cat| {
            let group = tr(cat.label());
            let rows: Vec<(&str, Vec<String>)> = Action::ALL
                .iter()
                .copied()
                .filter(|a| a.category() == *cat && !cx.keymap.shortcuts(*a).is_empty())
                .map(|a| (action_label(a), cx.keymap.shortcuts(a).iter().map(|s| s.format(IS_MAC)).collect::<Vec<_>>()))
                .filter(|(label, keys)| {
                    needle.is_empty() || label.to_lowercase().contains(&needle) || group.to_lowercase().contains(&needle) || keys.iter().any(|k| k.to_lowercase().contains(&needle))
                })
                .collect();
            (!rows.is_empty()).then_some((group, rows))
        })
        .collect();
    if groups.is_empty() {
        muted(ui, tr("No shortcut matches the filter."));
        return;
    }
    let max_h = (ui.ctx().content_rect().height() - 300.0).max(200.0);
    // The columns as wide as the design's; the scroll bar in the body's right padding.
    let full = ui.available_rect_before_wrap();
    let mut area = ui.new_child(UiBuilder::new().max_rect(full.with_max_x(full.right() + 14.0)).layout(Layout::top_down(Align::Min)));
    egui::ScrollArea::vertical().id_salt("shortcuts_scroll").max_height(max_h).auto_shrink([false, true]).show(&mut area, |ui| {
        const COLUMNS: usize = 3;
        let gap = 28.0;
        let col_w = ((full.width() - gap * (COLUMNS - 1) as f32) / COLUMNS as f32).floor();
        // Each group under the shortest column so far (masonry): the columns end about level.
        let heights: Vec<usize> = groups.iter().map(|(_, rows)| rows.len() + 2).collect();
        let columns = masonry(&heights, COLUMNS);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for column in columns {
                ui.allocate_ui_with_layout(vec2(col_w, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(col_w);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (i, (group, rows)) in column.into_iter().map(|g| &groups[g]).enumerate() {
                        if i > 0 {
                            ui.add_space(18.0);
                        }
                        // The design's rows: 28 points and a hairline, 4 apart.
                        ui.spacing_mut().item_spacing.y = 4.0;
                        kit::semibold_label(ui, group, 12.0, p.text_muted);
                        for (label, keys) in rows {
                            let (rect, resp) = ui.allocate_exact_size(vec2(col_w, 29.0), Sense::hover());
                            ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, p.line));
                            let mut x = rect.right();
                            for k in keys.iter().rev() {
                                x = kit::paint_kbd_text(ui.painter(), ui.ctx(), k, x, rect.center().y - 0.5, &p).left() - 6.0;
                            }
                            // The name in what the keys leave, cut with … (the whole name in the tooltip).
                            let mut job = egui::text::LayoutJob::single_section(label.to_string(), egui::TextFormat::simple(egui::FontId::proportional(13.0), p.text));
                            job.wrap = egui::text::TextWrapping::truncate_at_width((x - rect.left() - 6.0).max(20.0));
                            let galley = ui.painter().layout_job(job);
                            let cut = galley.elided;
                            ui.painter().galley(egui::pos2(rect.left(), rect.center().y - 0.5 - galley.size().y / 2.0), galley, p.text);
                            if cut {
                                resp.on_hover_text(*label);
                            }
                        }
                        ui.spacing_mut().item_spacing.y = 0.0;
                    }
                });
            }
        });
    });
    ui.allocate_exact_size(vec2(full.width(), area.min_rect().height()), Sense::hover());
}

/// The items of `heights` in `n` columns, each (in order) under the shortest column so far
/// (the leftmost of equal ones): their indexes, column by column.
fn masonry(heights: &[usize], n: usize) -> Vec<Vec<usize>> {
    let mut columns: Vec<(usize, Vec<usize>)> = vec![(0, Vec::new()); n.max(1)];
    for (i, &h) in heights.iter().enumerate() {
        let shortest = columns.iter_mut().min_by_key(|(total, _)| *total).expect("a column");
        shortest.0 += h;
        shortest.1.push(i);
    }
    columns.into_iter().map(|(_, items)| items).filter(|c| !c.is_empty()).collect()
}

// ---------------------------------------------------------------------------------------------
// The pieces dialogs share
// ---------------------------------------------------------------------------------------------

/// Width of the label column in the dialogs' forms.
pub const LABEL_W: f32 = 150.0;

/// The widths of dialogs: small (a question, a name, a password, a progress), medium (a form,
/// an SQL preview, notes) and large (a table, a list beside a form).
pub const WIDTH_S: f32 = 440.0;
pub const WIDTH_M: f32 = 600.0;
pub const WIDTH_L: f32 = 900.0;

/// The padding of a dialog's body (the design's `18px 20px`).
pub const BODY: Margin = Margin { left: 20, right: 20, top: 18, bottom: 18 };

/// Controls as tall as the design kit's with `gap` between the rows.
pub fn form_spacing(ui: &mut Ui, gap: f32) {
    style::form_spacing(ui);
    ui.spacing_mut().item_spacing.y = gap;
}

/// A form row: the label (muted) in a column of [`LABEL_W`], centred on the first line of the
/// controls (an input, a row of check boxes or radio buttons, a value: see
/// [`parts::form_row`]), then the controls 16 points to its right, side by side.
pub fn form_row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    form_row_w(ui, label, LABEL_W, add)
}

/// [`form_row`] with a label column of `label_w`.
pub fn form_row_w<R>(ui: &mut Ui, label: &str, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut add = Some(add);
    let mut out = None;
    form_row_dyn(ui, label, label_w, &mut |ui| out = add.take().map(|a| a(ui)));
    out.expect("a form row runs once")
}

// Not generic: compiled once for every form row.
fn form_row_dyn(ui: &mut Ui, label: &str, label_w: f32, add: &mut dyn FnMut(&mut Ui)) {
    // The kit's row is at least `interact_size.y` high (30 in a form): a row of check boxes or
    // radio buttons is shorter in the design (17), so the row starts at no height.
    let control_h = ui.spacing().interact_size.y;
    ui.spacing_mut().interact_size.y = 0.0;
    let add = &mut |ui: &mut Ui| {
        ui.spacing_mut().interact_size.y = control_h;
        add(ui)
    };
    if label.is_empty() {
        // Without a label the design's row has no line of text in the label column either.
        ui.horizontal(|ui| {
            ui.add_space(label_w + 16.0);
            add(ui)
        });
    } else {
        parts::form_row(ui, label, label_w, add);
    }
    ui.spacing_mut().interact_size.y = control_h;
}

/// The frame of [`Dialog`] windows, for windows that are laid out by hand
/// (the pop-up tool windows, the operations): surface, hairline, large radius, shadow.
pub fn window_frame(ctx: &egui::Context) -> egui::Frame {
    let dark = ctx.global_style().visuals.dark_mode;
    let p = style::palette(dark);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(RADIUS_LG + 2))
        .shadow(ctx.global_style().visuals.window_shadow)
        .inner_margin(Margin::ZERO)
}

/// The id of the dimming behind dialog windows.
const SCRIM: &str = "ternarygrid_dialog_scrim";

/// As in the design, the app is dimmed behind a dialog window (`layer`): asks for it this
/// frame; [`dim_topmost`] dims behind the topmost one. Unlike a modal's backdrop, the app
/// still answers clicks.
pub fn dim_behind(ctx: &egui::Context, layer: Option<egui::LayerId>) {
    if let Some(layer) = layer {
        ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<egui::LayerId>>(Id::new(SCRIM)).push(layer));
    }
}

/// Dims the app under the topmost dialog window that asked for it this frame (once a
/// frame, after every dialog).
pub fn dim_topmost(ctx: &egui::Context) {
    let layers = ctx.data_mut(|d| d.remove_temp::<Vec<egui::LayerId>>(Id::new(SCRIM))).unwrap_or_default();
    let top = ctx.memory(|m| m.areas().top_layer_id(egui::Order::Middle));
    let Some(window) = top.filter(|t| layers.contains(t)).or_else(|| layers.last().copied()) else { return };
    let dark = ctx.global_style().visuals.dark_mode;
    let scrim = if dark { Color32::from_rgba_unmultiplied(8, 9, 12, 140) } else { Color32::from_rgba_unmultiplied(30, 30, 45, 71) };
    let rect = ctx.content_rect();
    let shown = egui::Area::new(Id::new(SCRIM)).order(egui::Order::Middle).interactable(false).fixed_pos(rect.min).show(ctx, |ui| ui.painter().rect_filled(rect, 0.0, scrim));
    ctx.move_to_top(shown.response.layer_id);
    ctx.set_sublayer(shown.response.layer_id, window);
}

/// Lay out `add` from the right edge (the main actions of a footer: primary first).
pub fn right<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.with_layout(Layout::right_to_left(Align::Center), add).inner
}

/// "Review transactions…" under the list of a question about uncommitted transactions (it
/// opens the pending transactions); true when clicked.
pub fn review_button(ui: &mut Ui, enabled: bool) -> bool {
    ui.add_enabled_ui(enabled, |ui| kit::button_ex(ui, Kind::Ghost, None, tr("Review transactions…"), true)).inner.clicked()
}

/// The quiet Cancel of a footer.
pub fn cancel_button(ui: &mut Ui) -> Response {
    kit::button_ex(ui, Kind::Ghost, None, tr("Cancel"), false)
}

/// A hint in the faint text colour (placeholders).
pub fn hint(ui: &Ui, text: &str) -> RichText {
    RichText::new(text).color(style::pal(ui).text_faint)
}

/// A hint in monospace (the placeholder of a monospace field).
pub fn mono_hint(ui: &Ui, text: &str) -> RichText {
    hint(ui, text).font(egui::FontId::monospace(12.5))
}

/// A single-line text field as tall as the other controls, `width` wide
/// (`f32::INFINITY`: the rest of the row). Add it with [`add_field`].
pub fn text_field(text: &mut dyn egui::TextBuffer, width: f32) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).margin(Margin { left: 13, right: 13, top: 5, bottom: 5 }).min_size(vec2(0.0, CONTROL_H)).vertical_align(Align::Center).desired_width(width)
}

/// Add a field (see [`text_field`]) with the focus halo of the design kit.
pub fn add_field(ui: &mut Ui, edit: egui::TextEdit<'_>) -> Response {
    let r = ui.add(edit);
    if r.has_focus() {
        style::focus_halo(ui, r.rect, RADIUS);
    }
    r
}

/// A field for an optional text: empty is `None`.
pub fn opt_field(ui: &mut Ui, value: &mut Option<String>, hint_text: &str, width: f32, mono: bool) -> Response {
    let mut text = value.clone().unwrap_or_default();
    let mut edit = text_field(&mut text, width);
    edit = if mono { edit.font(egui::FontId::monospace(12.5)).hint_text(mono_hint(ui, hint_text)) } else { edit.hint_text(hint(ui, hint_text)) };
    let r = add_field(ui, edit);
    if r.changed() {
        *value = Some(text).filter(|t| !t.is_empty());
    }
    r
}

/// A text field for a number: typed digits are kept while it has the focus,
/// the value follows when they parse (clamped to `range`).
pub fn number_field<T>(ui: &mut Ui, id_salt: impl std::hash::Hash + std::fmt::Debug, value: &mut T, range: std::ops::RangeInclusive<T>, width: f32) -> Response
where
    T: Copy + PartialOrd + std::str::FromStr + std::fmt::Display,
{
    let id = ui.make_persistent_id(id_salt);
    let mut text = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| value.to_string());
    let r = add_field(ui, text_field(&mut text, width).id(id));
    if r.changed()
        && let Ok(v) = text.trim().parse::<T>()
    {
        *value = if v < *range.start() {
            *range.start()
        } else if v > *range.end() {
            *range.end()
        } else {
            v
        };
    }
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, text));
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
    }
    r
}

/// The width left in the row after `reserve` points (other controls on it).
pub fn rest(ui: &Ui, reserve: f32) -> f32 {
    (ui.available_width() - reserve).max(60.0)
}

/// The width of `text` in the body font.
pub fn text_width(ui: &Ui, text: &str) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(style::body_size(ui.ctx())), Color32::PLACEHOLDER).size().x
}

/// Wrapping text at 13.5 points (the question of a confirmation).
pub fn body_text(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(13.5).color(p.text)).wrap())
}

/// [`body_text`] with lines `factor` times the text size apart.
pub fn body_text_lh(ui: &mut Ui, text: &str, factor: f32) -> Response {
    let p = style::pal(ui);
    label_lh(ui, RichText::new(text).color(p.text), egui::FontId::proportional(13.5), factor, false)
}

/// A wrapping label on CSS-style line heights ([`kit::label_lh`]).
pub fn label_lh(ui: &mut Ui, text: RichText, font: egui::FontId, factor: f32, selectable: bool) -> Response {
    ternarygrid_ui::kit::label_lh(ui, text, font, factor, selectable)
}

/// Wrapping muted text (explanations).
pub fn muted(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.text_muted)).wrap())
}

/// A semibold line (the headings of a confirmation's lists).
pub fn strong(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    semibold_text(ui, text, 13.0, p.text, None, false)
}

/// Text in the design's semibold (600: the medium weight painted twice, see
/// `kit::paint_semibold`), wrapping (`one_line`: cut with … instead), on lines `factor` times
/// its size apart as [`label_lh`] (`None`: the font's own).
pub fn semibold_text(ui: &mut Ui, text: &str, size: f32, color: Color32, factor: Option<f32>, one_line: bool) -> Response {
    let font = style::medium(ui.ctx(), size);
    let row = ui.fonts_mut(|f| f.row_height(&font));
    let lh = factor.map(|f| (size * f * 2.0).floor() / 2.0);
    let half = lh.map_or(0.0, |lh| ((lh - row) / 2.0).max(0.0));
    let mut job = egui::text::LayoutJob::single_section(text.to_string(), egui::TextFormat { font_id: font, color, line_height: lh, ..Default::default() });
    job.wrap =
        egui::text::TextWrapping { max_width: ui.available_width() - 0.5, max_rows: if one_line { 1 } else { usize::MAX }, break_anywhere: false, overflow_character: Some('…') };
    let galley = ui.painter().layout_job(job);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(0.5, 0.0), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    kit::paint_semibold(ui.painter(), rect.min + vec2(0.0, half), galley, color);
    resp
}

/// A value shown read-only in a form row (the row's label is centred on its line), semibold or
/// not; in the text colour, so it does not read as disabled.
pub fn value_text(ui: &mut Ui, text: &str, color: Color32, semibold: bool) -> Response {
    if semibold { semibold_text(ui, text, 13.0, color, None, true) } else { ui.label(RichText::new(text).color(color)) }
}

/// A validation message under the field of the form row above it (in the controls' column, 4
/// points under the control): [`kit::field_error`]. Mark the field with [`add_field_ex`].
pub fn field_error(ui: &mut Ui, text: &str) -> Response {
    field_error_at(ui, text, LABEL_W + 16.0)
}

/// [`field_error`] `indent` points from the left (a form without labels: 0).
pub fn field_error_at(ui: &mut Ui, text: &str, indent: f32) -> Response {
    ui.add_space(4.0 - ui.spacing().item_spacing.y);
    ui.horizontal(|ui| {
        ui.add_space(indent);
        kit::field_error(ui, text)
    })
    .inner
}

/// [`add_field`], outlined in the error colour while `invalid` (its value was refused: see
/// [`field_error`]).
pub fn add_field_ex(ui: &mut Ui, edit: egui::TextEdit<'_>, invalid: bool) -> Response {
    if !invalid {
        return add_field(ui, edit);
    }
    let r = ui.add(edit);
    let p = style::pal(ui);
    if r.has_focus() {
        let halo = style::soft(p.err, if ui.visuals().dark_mode { 0.30 } else { 0.22 });
        ui.painter().rect_stroke(r.rect, CornerRadius::same(RADIUS), Stroke::new(3.0, halo), egui::StrokeKind::Outside);
    }
    kit::error_outline(ui, r.rect);
    r
}

/// One bullet line per item.
pub fn bullets(ui: &mut Ui, items: impl IntoIterator<Item = String>, color: Color32) {
    for item in items {
        ui.add(egui::Label::new(RichText::new(format!("• {item}")).color(color)).wrap());
    }
}

/// A note on the soft fill of its tone: the tone's icon and the text (wrapping, selectable)
/// on the design's lines of 1.45 (`parts::callout` keeps the font's shorter lines), as tall
/// as its text, also in a form.
pub fn note(ui: &mut Ui, tone: Tone, text: &str) -> Response {
    let p = style::pal(ui);
    let color = tone.color(&p);
    egui::Frame::new()
        .fill(tone.fill(ui))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin { left: 12, right: 12, top: 10, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().interact_size.y = 0.0;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                // The icon level with the first line (the icon font sits a little low).
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
                ui.painter().text(egui::pos2(r.center().x, r.top() + 8.0), Align2::CENTER_CENTER, tone.icon(), style::icon_font(16.0), color);
                ui.vertical(|ui| label_lh(ui, RichText::new(text).color(p.text), egui::FontId::proportional(12.5), 1.45, true));
            });
        })
        .response
}

/// An error in a dialog's body.
pub fn error_note(ui: &mut Ui, text: &str) -> Response {
    note(ui, Tone::Error, text)
}

/// A row that opens and closes what follows it (a chevron and a muted label);
/// returns whether it is open.
pub fn disclosure(ui: &mut Ui, id: Id, label: &str, default_open: bool) -> bool {
    let p = style::pal(ui);
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(default_open);
    let g = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(12.5), p.text_muted);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 22.0, 16.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::CollapsingHeader, true, open, label));
    let color = if resp.hovered() { p.text } else { p.text_muted };
    let caret = if open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
    ui.painter().text(egui::pos2(rect.left() + 6.0, rect.center().y), Align2::CENTER_CENTER, caret, style::icon_font(12.0), p.text_faint);
    ui.painter().galley(egui::pos2(rect.left() + 20.0, rect.center().y - g.size().y / 2.0), g, color);
    if resp.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    open
}

/// The logo of a database type (its icon when there is no logo).
pub fn db_logo(ui: &mut Ui, kind: ternarygrid_core::DbKind, size: f32) -> Response {
    let color = crate::theme::logo_color(kind, ui.visuals().dark_mode);
    match crate::dblogo::image(ui.ctx(), kind, size, color) {
        Some(logo) => ui.add(logo.fit_to_exact_size(Vec2::splat(size))),
        None => ui.label(RichText::new(crate::nav::db_icon(kind)).size(size).color(color)),
    }
}

/// Read-only SQL in a bordered frame, highlighted in `dialect`, at most `max_h` high (see
/// [`code_block`]).
pub fn sql_preview(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, dialect: Option<std::sync::Arc<dyn ternarygrid_core::Dialect>>, text: &str, max_h: f32, gutter: bool) {
    let mut t = text;
    code_block(ui, Id::new(id), dialect, &mut t, gutter, 1, max_h);
}

/// Editable SQL in a bordered frame with line numbers, at least `min_lines` high.
pub fn code_editor(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    dialect: Option<std::sync::Arc<dyn ternarygrid_core::Dialect>>,
    text: &mut String,
    min_lines: usize,
) -> Response {
    code_block(ui, Id::new(id), dialect, text, true, min_lines, f32::INFINITY)
}

/// SQL as in the design's code blocks: a bordered frame (radius 8, 10 points above and
/// below), 12.5 point monospace on 20 point lines highlighted in `dialect`, and the line
/// numbers in a gutter (`gutter`) or 12 points of padding at the left.
fn code_block(
    ui: &mut Ui,
    id: Id,
    dialect: Option<std::sync::Arc<dyn ternarygrid_core::Dialect>>,
    text: &mut dyn egui::TextBuffer,
    gutter: bool,
    min_lines: usize,
    max_h: f32,
) -> Response {
    use ternarygrid_editor::EditorLanguage;
    let p = style::pal(ui);
    let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
    let font = egui::FontId::monospace(12.5);
    let lang = dialect.map(crate::sqllang::SqlLanguage::new);
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
        let code = buf.as_str();
        let tokens = lang.as_ref().map(|l| l.highlight(code)).unwrap_or_default();
        let mut job = crate::sqllang::highlight_job(code, &tokens, &theme, font.clone());
        if job.sections.is_empty() {
            job.append("", 0.0, egui::TextFormat { font_id: font.clone(), color: p.text, ..Default::default() });
        }
        for s in &mut job.sections {
            s.format.line_height = Some(20.0);
            s.format.valign = Align::Center;
        }
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let lines = text.as_str().split('\n').count().max(min_lines);
    // egui leaves a line's extra height under its text: the code moves down by half of it,
    // so it sits in the middle of its 20 points like the line numbers.
    let half = ((20.0 - ui.fonts_mut(|f| f.row_height(&font))) / 2.0).round() as i8;
    let (left, right) = if gutter { (0, 12) } else { (12, 12) };
    let margin = Margin { left, right, top: 10 + half, bottom: 10 - half };
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::both()
                .id_salt(id)
                .max_height(max_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if gutter {
                            // The numbers right-aligned in 32 points, 14 points before the code.
                            let (g, _) = ui.allocate_exact_size(vec2(46.0, lines as f32 * 20.0), Sense::hover());
                            for i in 0..lines {
                                let y = g.top() + 20.0 * i as f32 + 10.0 - half as f32;
                                ui.painter().text(egui::pos2(g.left() + 32.0, y), Align2::RIGHT_CENTER, (i + 1).to_string(), font.clone(), p.text_faint);
                            }
                        }
                        let edit = egui::TextEdit::multiline(text).id(id.with("text")).code_editor().frame(egui::Frame::NONE).margin(Margin::ZERO);
                        ui.add(edit.desired_rows(1).desired_width(f32::INFINITY).layouter(&mut layouter))
                    })
                    .inner
                })
                .inner
        })
        .inner
}

/// A line under a control (what an option means): 12 points, muted.
pub fn sub_text(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    label_lh(ui, RichText::new(text).color(p.text_muted), egui::FontId::proportional(12.0), 1.4, false)
}

/// The width of a table column.
#[derive(Clone, Copy, Debug)]
pub enum W {
    /// A share of what the fixed columns leave.
    Fr(f32),
    /// Fixed points.
    Px(f32),
}

/// A column of a [`table`].
pub struct Column<'a> {
    pub title: &'a str,
    pub width: W,
    pub right: bool,
    pub center: bool,
}

impl<'a> Column<'a> {
    pub fn new(title: &'a str, width: W) -> Self {
        Column { title, width, right: false, center: false }
    }

    /// Aligned to the right (numbers).
    pub fn right(mut self) -> Self {
        self.right = true;
        self
    }

    /// Centred (a column of check boxes).
    pub fn center(mut self) -> Self {
        self.center = true;
        self
    }
}

/// A bordered table as in the mockups: a header row (32 points and a hairline) on the
/// header colour, striped rows of `row_h` and a hairline (scrolling past `max_h`).
/// `cell(ui, Some(row), col)` draws a cell; `cell(ui, None, col)` the header of a
/// column without a title.
pub fn table(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    cols: &[Column<'_>],
    rows: usize,
    row_h: f32,
    max_h: f32,
    mut cell: impl FnMut(&mut Ui, Option<usize>, usize),
) {
    table_dyn(ui, Id::new(id), cols, rows, row_h, max_h, &mut cell);
}

// Not generic: compiled once for every table.
fn table_dyn(ui: &mut Ui, id: Id, cols: &[Column<'_>], rows: usize, row_h: f32, max_h: f32, cell: &mut dyn FnMut(&mut Ui, Option<usize>, usize)) {
    let p = style::pal(ui);
    parts::bordered(ui).show(ui, |ui| {
        let w = ui.available_width();
        ui.set_width(w);
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let fixed: f32 = cols.iter().map(|c| if let W::Px(px) = c.width { px } else { 0.0 }).sum();
        let fr: f32 = cols.iter().map(|c| if let W::Fr(f) = c.width { f } else { 0.0 }).sum::<f32>().max(0.001);
        let widths: Vec<f32> = cols
            .iter()
            .map(|c| match c.width {
                W::Px(px) => px,
                W::Fr(f) => (w - fixed).max(0.0) * f / fr,
            })
            .collect();
        let mut draw_row = |ui: &mut Ui, rect: Rect, row: Option<usize>| {
            let mut x = rect.left();
            for (c, col) in cols.iter().enumerate() {
                let r = Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x + widths[c], rect.bottom()));
                x += widths[c];
                let inner = if col.center { r } else { r.shrink2(vec2(10.0, 0.0)) };
                if row.is_none() && !col.title.is_empty() {
                    let g = ui.painter().layout_no_wrap(col.title.to_string(), style::medium(ui.ctx(), 12.0), p.text_muted);
                    let x = if col.right { inner.right() - g.size().x - 0.35 } else { inner.left() };
                    kit::paint_semibold(&ui.painter().with_clip_rect(r), egui::pos2(x, inner.center().y - g.size().y / 2.0), g, p.text_muted);
                    continue;
                }
                let layout = if col.right {
                    Layout::right_to_left(Align::Center)
                } else if col.center {
                    Layout::centered_and_justified(egui::Direction::LeftToRight)
                } else {
                    Layout::left_to_right(Align::Center)
                };
                let mut child = ui.new_child(UiBuilder::new().id_salt((id, row, c)).max_rect(inner).layout(layout));
                child.shrink_clip_rect(r);
                child.spacing_mut().item_spacing = vec2(6.0, 0.0);
                cell(&mut child, row, c);
            }
        };
        let (head, _) = ui.allocate_exact_size(vec2(w, 33.0), Sense::hover());
        ui.painter().rect_filled(head, CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, p.header);
        ui.painter().hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, p.border));
        draw_row(ui, head.with_max_y(head.bottom() - 1.0), None);
        egui::ScrollArea::vertical().id_salt(id.with("rows")).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for r in 0..rows {
                let (rect, _) = ui.allocate_exact_size(vec2(w, row_h + 1.0), Sense::hover());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                if r % 2 == 1 {
                    let radius = if r + 1 == rows { CornerRadius { nw: 0, ne: 0, sw: 7, se: 7 } } else { CornerRadius::ZERO };
                    ui.painter().rect_filled(rect, radius, p.stripe);
                }
                ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, p.line));
                draw_row(ui, rect.with_max_y(rect.bottom() - 1.0), Some(r));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_columns() {
        // SQL, Editor, Data, Navigation, View, Tools, Application, Help.
        assert_eq!(masonry(&[12, 17, 3, 7, 8, 4, 4, 3], 3), [vec![0, 5, 6], vec![1, 7], vec![2, 3, 4]]);
        assert_eq!(masonry(&[5, 5], 3), [vec![0], vec![1]]);
        assert!(masonry(&[], 3).is_empty());
    }

    #[test]
    fn folder_paths() {
        assert_eq!(split_folder("a/b/c"), (Some("a/b"), "c"));
        assert_eq!(split_folder("top"), (None, "top"));
    }

    #[test]
    fn prompt_targets() {
        let mut cfg = ternarygrid_core::config::ConnectionConfig::new(ternarygrid_core::DbKind::Postgres, "p");
        cfg.host = "10.0.4.12".into();
        cfg.user = Some("app".into());
        assert_eq!(connection_target(&cfg), "app@10.0.4.12:5432");
        cfg.ssh = Some(ternarygrid_core::config::SshConfig { enabled: true, host: "bastion".into(), ..Default::default() });
        assert_eq!(connection_target(&cfg), "app@10.0.4.12:5432 via bastion");
    }
}
