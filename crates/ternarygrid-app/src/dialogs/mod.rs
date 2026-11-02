//! Modal and floating dialogs owned by the application, and the pieces they
//! share on top of `ternarygrid_ui` (form fields, a red filled button, tables,
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
use egui::{Align, Align2, Color32, CornerRadius, Id, Layout, Margin, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType, vec2};
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
            self.delete = Some(DeleteConfirm { conn_id: id.to_string(), name: cfg.name.clone() });
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
            let (resp, _) = Dialog::new(tr("About TernaryGrid")).width(420.0).body_padding(Margin { left: 24, right: 24, top: 28, bottom: 24 }).show_window(
                ctx,
                Id::new("ternarygrid_about"),
                &mut open,
                None,
                about_body,
                None,
            );
            self.about_open = open && !crate::widgets::escape_closes(ctx, resp.layer);
        }
        if self.drivers_open {
            let mut open = true;
            let (resp, _) = Dialog::new(tr("Database drivers")).icon(icon::PLUGS).width(900.0).body_padding(Margin::symmetric(20, 16)).show_window(
                ctx,
                Id::new("ternarygrid_drivers"),
                &mut open,
                None,
                |ui| drivers_body(ui, cx),
                None,
            );
            self.drivers_open = open && !crate::widgets::escape_closes(ctx, resp.layer);
        }
        if self.shortcuts_open {
            let mut open = true;
            let customize = Cell::new(false);
            let preset = trf("Preset: {}", &[&cx.keymap.preset().label()]);
            let filter = &mut self.shortcuts_filter;
            let (resp, _) = Dialog::new(tr("Keyboard shortcuts")).icon(icon::KEYBOARD).width(720.0).show_window(
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
                        ui.label(RichText::new(&preset).size(12.0).color(style::pal(ui).text_faint));
                    });
                }),
            );
            if customize.get() {
                cx.push(AppCommand::OpenSettings);
                open = false;
            }
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
            let (resp, _) = Dialog::new(&title).icon(icon::KEY).subtitle(&target).width(460.0).show_window(
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
                        let r = parts::form_row(ui, label, LABEL_W, |ui| add_field(ui, text_field(value, f32::INFINITY).password(true)));
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
                    parts::form_row(ui, "", LABEL_W, |ui| ternarygrid_ui::kit::checkbox(ui, &mut p.save, tr("Save password")));
                },
                Some(&mut |ui: &mut Ui| {
                    right(ui, |ui| {
                        if kit::primary_icon(ui, icon::PLUGS_CONNECTED, tr("Connect")).clicked() {
                            submit.set(true);
                        }
                        if cancel_button(ui).clicked() {
                            cancel.set(true);
                        }
                    });
                }),
            );
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
                let title = trf("Delete the folder \"{}\"?", &[&split_folder(&path).1]);
                let error = d.error.clone();
                let (resp, _) = Dialog::new(&title).icon(icon::FOLDER).width(480.0).show_modal(
                    ctx,
                    Id::new("ternarygrid_folder"),
                    |ui| {
                        body_text(ui, &trf("Its {} connection(s) and its subfolders move to {}. No connection is deleted.", &[&n, &target]));
                        if let Some(e) = &error {
                            crate::dialogs::error_note(ui, e);
                        }
                    },
                    |ui| {
                        right(ui, |ui| {
                            if danger_button(ui, Some(icon::TRASH), tr("Delete folder")).clicked() {
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
                        Some(tr("Type a name"))
                    } else if name.contains('/') {
                        Some(tr("The name cannot contain \"/\""))
                    } else if folders.iter().any(|f| f == &path) && action != FolderAction::Rename(path.clone()) {
                        Some(tr("A folder with this name already exists"))
                    } else {
                        None
                    }
                };
                let enter = Cell::new(false);
                let valid = Cell::new(false);
                let label = if matches!(action, FolderAction::New { .. }) { tr("Create") } else { tr("Rename") };
                let (resp, _) = Dialog::new(title).icon(icon::FOLDER).width(420.0).show_modal(
                    ctx,
                    Id::new("ternarygrid_folder"),
                    |ui| {
                        form_spacing(ui, 10.0);
                        if let Some(p) = &parent {
                            ui.label(RichText::new(trf("In {}", &[p])).size(12.5).color(style::pal(ui).text_muted));
                        }
                        let edit = add_field(ui, text_field(&mut d.name, f32::INFINITY).hint_text(hint(ui, tr("Folder name"))));
                        if !d.focused {
                            edit.request_focus();
                            d.focused = true;
                        }
                        enter.set(edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                        let name = d.name.trim().to_string();
                        let problem = problem_of(&name);
                        valid.set(problem.is_none());
                        if let Some(p) = problem.filter(|_| !name.is_empty()) {
                            error_text(ui, p);
                        }
                        if let Some(e) = &d.error {
                            crate::dialogs::error_note(ui, e);
                        }
                    },
                    |ui| {
                        right(ui, |ui| {
                            if ui.add_enabled_ui(valid.get(), |ui| kit::primary_icon(ui, icon::CHECK, label)).inner.clicked() {
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
        let Some(d) = &self.delete else { return };
        // Disconnected first, the usual way: open transactions are asked about, the
        // editors' sessions close and the after-disconnect command runs.
        let connected = cx.conns.is_connected(&d.conn_id) || cx.conns.status(&d.conn_id) == crate::conn::ConnStatus::Connecting;
        let disconnect = Cell::new(false);
        let delete = Cell::new(false);
        let cancel = Cell::new(false);
        let (resp, _) = Dialog::new(tr("Delete connection")).icon(icon::TRASH).width(480.0).show_modal(
            ctx,
            Id::new("delete_conn"),
            |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                body_text(ui, &trf("Delete the connection \"{}\" and its saved passwords?", &[&d.name]));
                if connected {
                    note(ui, Tone::Warning, tr("It is connected: disconnect it first."));
                }
            },
            |ui| {
                if connected && kit::secondary(ui, None, tr("Disconnect")).clicked() {
                    disconnect.set(true);
                }
                right(ui, |ui| {
                    if ui.add_enabled_ui(!connected, |ui| danger_button(ui, Some(icon::TRASH), tr("Delete"))).inner.clicked() {
                        delete.set(true);
                    }
                    if cancel_button(ui).clicked() {
                        cancel.set(true);
                    }
                });
            },
        );
        if disconnect.get() {
            cx.push(AppCommand::Disconnect(d.conn_id.clone()));
        }
        let mut close = cancel.get() || resp.close;
        if delete.get() && !connected {
            cx.conns.remove(cx.backend, &d.conn_id);
            cx.push(AppCommand::SaveConnections);
            close = true;
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

/// The body of About: the app icon, name, version and the promises.
fn about_body(ui: &mut Ui) {
    let p = style::pal(ui);
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
        match app_icon(ui.ctx()) {
            Some(tex) => {
                egui::Image::new((tex.id(), Vec2::splat(64.0))).corner_radius(16).paint_at(ui, rect);
            }
            None => {
                ui.painter().rect_filled(rect, CornerRadius::same(16), p.accent);
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon::GRID_FOUR, style::icon_font(34.0), Color32::WHITE);
            }
        }
        ui.add_space(8.0);
        ui.label(RichText::new("TernaryGrid").font(style::semibold(ui.ctx(), 22.0)).color(p.text));
        ui.label(RichText::new(format!("{} {}", tr("Version"), env!("CARGO_PKG_VERSION"))).size(12.5).color(p.text_muted));
        ui.label(RichText::new(tr("A fast, lightweight database client, written in Rust.")).color(p.text));
        ui.label(RichText::new(tr("License: MIT OR Apache-2.0")).size(12.0).color(p.text_faint));
        ui.label(RichText::new(tr("No telemetry. Only connects to the servers you configure.")).size(12.0).color(p.text_faint));
    });
}

/// The app's icon as a texture (loaded once).
fn app_icon(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let id = Id::new("ternarygrid_app_icon");
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return Some(t);
    }
    let img = image::load_from_memory(include_bytes!("../../assets/icon-256.png")).ok()?.into_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let tex = ctx.load_texture("ternarygrid_app_icon", egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    Some(tex)
}

/// The capability table of the built-in drivers.
fn drivers_body(ui: &mut Ui, cx: &mut AppContext) {
    let p = style::pal(ui);
    ui.spacing_mut().item_spacing.y = 12.0;
    muted(ui, tr("Drivers are built into TernaryGrid (no downloads) and written in Rust; SQLite is an embedded engine."));
    let mut kinds = cx.conns.registry.kinds();
    kinds.sort_by_key(|k| k.display_name());
    let kinds: Vec<_> = kinds.into_iter().filter(|k| cx.conns.registry.get(*k).is_some()).collect();
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
                ui.label(RichText::new(info.name).monospace().size(12.0).color(p.text_muted)).on_hover_text(info.description);
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
    ui.label(RichText::new(tr("Not available: Oracle, Db2, Firebird, H2 and Trino (no Rust network driver yet).")).size(12.0).color(p.text_faint));
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
    let (line, _) = table_colors(ui);
    let max_h = (ui.ctx().content_rect().height() - 300.0).max(200.0);
    egui::ScrollArea::vertical().id_salt("shortcuts_scroll").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
        let gap = 28.0;
        // Room at the right for the scroll bar over the second column.
        let col_w = ((ui.available_width() - gap - 14.0) / 2.0).floor();
        for pair in groups.chunks(2) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (group, rows) in pair {
                    ui.allocate_ui_with_layout(vec2(col_w, 0.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(col_w);
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.label(RichText::new(*group).font(style::semibold(ui.ctx(), 12.0)).color(p.text_faint));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (label, keys) in rows {
                            let (rect, _) = ui.allocate_exact_size(vec2(col_w, 33.0), Sense::hover());
                            ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, line));
                            ui.painter().text(egui::pos2(rect.left(), rect.center().y), Align2::LEFT_CENTER, *label, egui::FontId::proportional(13.0), p.text);
                            let mut x = rect.right();
                            for k in keys.iter().rev() {
                                let g = ui.painter().layout_no_wrap(k.clone(), style::medium(ui.ctx(), style::SMALL), p.text_muted);
                                let r = Rect::from_min_size(egui::pos2(x - g.size().x - 10.0, rect.center().y - 9.0), vec2(g.size().x + 10.0, 18.0));
                                ui.painter().rect(r, CornerRadius::same(4), p.surface, Stroke::new(1.0, p.border_strong), StrokeKind::Inside);
                                ui.painter().galley(r.center() - g.size() / 2.0, g, p.text_muted);
                                x = r.left() - 6.0;
                            }
                        }
                    });
                }
            });
            ui.add_space(18.0);
        }
    });
}

// ---------------------------------------------------------------------------------------------
// The pieces dialogs share
// ---------------------------------------------------------------------------------------------

/// Width of the label column in the dialogs' forms.
pub const LABEL_W: f32 = 150.0;

/// Controls as tall as the design kit's (28 px) with `gap` between the rows.
pub fn form_spacing(ui: &mut Ui, gap: f32) {
    style::form_spacing(ui);
    ui.spacing_mut().item_spacing.y = gap;
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

/// Lay out `add` from the right edge (the main actions of a footer: primary first).
pub fn right<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.with_layout(Layout::right_to_left(Align::Center), add).inner
}

/// The quiet Cancel of a footer.
pub fn cancel_button(ui: &mut Ui) -> Response {
    kit::button_ex(ui, Kind::Ghost, None, tr("Cancel"), false)
}

/// A hint in the faint text colour (placeholders).
pub fn hint(ui: &Ui, text: &str) -> RichText {
    RichText::new(text).color(style::pal(ui).text_faint)
}

/// A single-line text field as tall as the other controls, `width` wide
/// (`f32::INFINITY`: the rest of the row). Add it with [`add_field`].
pub fn text_field(text: &mut dyn egui::TextBuffer, width: f32) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).margin(Margin::symmetric(10, 5)).min_size(vec2(0.0, CONTROL_H)).vertical_align(Align::Center).desired_width(width)
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
    let mut edit = text_field(&mut text, width).hint_text(hint(ui, hint_text));
    if mono {
        edit = edit.font(egui::TextStyle::Monospace);
    }
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

/// Wrapping muted text (explanations).
pub fn muted(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.text_muted)).wrap())
}

/// A semibold line (the headings of a confirmation's lists).
pub fn strong(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).font(style::semibold(ui.ctx(), 13.0)).color(p.text)).wrap())
}

/// A validation message under a field.
pub fn error_text(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.err)).wrap())
}

/// A small caption in capitals (DATABASE, COMPATIBLE).
pub fn caption(ui: &mut Ui, text: &str) -> Response {
    let p = style::pal(ui);
    ui.label(RichText::new(text.to_uppercase()).font(style::semibold(ui.ctx(), 11.0)).color(p.text_faint))
}

/// One bullet line per item.
pub fn bullets(ui: &mut Ui, items: impl IntoIterator<Item = String>, color: Color32) {
    for item in items {
        ui.add(egui::Label::new(RichText::new(format!("•  {item}")).color(color)).wrap());
    }
}

/// A note on the soft fill of its tone: the tone's icon and the text (wrapping,
/// selectable). As tall as its text, also in a modal.
pub fn note(ui: &mut Ui, tone: Tone, text: &str) -> Response {
    let p = style::pal(ui);
    let color = tone.color(&p);
    egui::Frame::new()
        .fill(tone.fill(ui))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin { left: 12, right: 12, top: 10, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new(tone.icon()).font(style::icon_font(16.0)).color(color));
                ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.text)).wrap().selectable(true));
            });
        })
        .response
}

/// An error in a dialog's body.
pub fn error_note(ui: &mut Ui, text: &str) -> Response {
    note(ui, Tone::Error, text)
}

/// A red filled button for the destructive action of a confirmation.
pub fn danger_button(ui: &mut Ui, icon: Option<&str>, text: &str) -> Response {
    let p = style::pal(ui);
    let dark = ui.visuals().dark_mode;
    let base = if dark { Color32::from_rgb(0xc4, 0x35, 0x2b) } else { p.err };
    let size = style::body_size(ui.ctx());
    let width = kit::button_width(ui, icon, text, false);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, CONTROL_H), Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, text));
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let fill = if hovered { crate::theme::darken(base, 0.12) } else { base };
        let fill = if enabled { fill } else { fill.gamma_multiply(0.5) };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(RADIUS), fill);
        if resp.has_focus() {
            painter.rect_stroke(rect.expand(2.0), CornerRadius::same(RADIUS + 2), Stroke::new(1.5, base), StrokeKind::Outside);
        }
        let color = if enabled { Color32::WHITE } else { Color32::from_white_alpha(170) };
        let text_g = painter.layout_no_wrap(text.to_string(), style::medium(ui.ctx(), size), color);
        let icon_g = icon.map(|i| painter.layout_no_wrap(i.to_string(), style::icon_font(size + 2.0), color));
        let gap = if icon_g.is_some() { 6.0 } else { 0.0 };
        let content = text_g.size().x + icon_g.as_ref().map_or(0.0, |g| g.size().x) + gap;
        let mut x = rect.center().x - content / 2.0;
        if let Some(g) = icon_g {
            let w = g.size().x;
            painter.galley(egui::pos2(x, rect.center().y - g.size().y / 2.0), g, color);
            x += w + gap;
        }
        painter.galley(egui::pos2(x, rect.center().y - text_g.size().y / 2.0), text_g, color);
    }
    resp
}

/// A status line (test result, error) in a footer: its icon and text in at
/// most two lines of `max_w`, the whole text in its tooltip.
pub fn status_line(ui: &mut Ui, tone: Tone, text: &str, max_w: f32) -> Response {
    let p = style::pal(ui);
    let c = tone.color(&p);
    ui.allocate_ui_with_layout(vec2(max_w.max(60.0), CONTROL_H), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(tone.icon()).font(style::icon_font(15.0)).color(c));
        let first = text.lines().next().unwrap_or_default().to_string();
        let mut job = egui::text::LayoutJob::single_section(first, egui::TextFormat { font_id: egui::FontId::proportional(12.5), color: c, ..Default::default() });
        job.wrap = egui::text::TextWrapping { max_width: ui.available_width(), max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
        let galley = ui.painter().layout_job(job);
        let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::hover());
        ui.painter().galley(rect.min, galley, c);
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
        resp.on_hover_text(text)
    })
    .inner
}

/// A row that opens and closes what follows it (a chevron and a muted label);
/// returns whether it is open.
pub fn disclosure(ui: &mut Ui, id: Id, label: &str, default_open: bool) -> bool {
    let p = style::pal(ui);
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(default_open);
    let g = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(12.5), p.text_muted);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 22.0, 24.0), Sense::click());
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

/// Read-only SQL in a bordered frame, highlighted in `dialect`, at most `max_h` high.
pub fn sql_preview(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, dialect: Option<std::sync::Arc<dyn ternarygrid_core::Dialect>>, text: &str, max_h: f32) {
    use ternarygrid_editor::EditorLanguage;
    let tokens = dialect.map(|d| crate::sqllang::SqlLanguage::new(d).highlight(text)).unwrap_or_default();
    let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| ui.fonts_mut(|f| f.layout_job(crate::sqllang::highlight_job(buf.as_str(), &tokens, &theme, font.clone())));
    parts::code_frame()(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::both().id_salt(id).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            let mut t = text;
            ui.add(
                egui::TextEdit::multiline(&mut t).code_editor().frame(egui::Frame::NONE).margin(Margin::ZERO).desired_rows(1).desired_width(f32::INFINITY).layouter(&mut layouter),
            );
        });
    });
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
}

impl<'a> Column<'a> {
    pub fn new(title: &'a str, width: W) -> Self {
        Column { title, width, right: false }
    }

    /// Aligned to the right (numbers).
    pub fn right(mut self) -> Self {
        self.right = true;
        self
    }
}

/// The hairline between rows and the fill of every other row.
fn table_colors(ui: &Ui) -> (Color32, Color32) {
    if ui.visuals().dark_mode {
        (Color32::from_rgb(0x33, 0x35, 0x3e), Color32::from_rgb(0x26, 0x28, 0x30))
    } else {
        (Color32::from_rgb(0xed, 0xed, 0xf2), Color32::from_rgb(0xfa, 0xfa, 0xfb))
    }
}

/// A bordered table as in the mockups: a header row on the header colour, striped
/// rows of `row_h` (scrolling past `max_h`). `cell(ui, Some(row), col)` draws a
/// cell; `cell(ui, None, col)` the header of a column without a title.
pub fn table(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    cols: &[Column<'_>],
    rows: usize,
    row_h: f32,
    max_h: f32,
    mut cell: impl FnMut(&mut Ui, Option<usize>, usize),
) {
    let p = style::pal(ui);
    let (line, stripe) = table_colors(ui);
    let id = Id::new(id);
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
                W::Fr(f) => ((w - fixed).max(0.0) * f / fr).floor(),
            })
            .collect();
        let mut draw_row = |ui: &mut Ui, rect: Rect, row: Option<usize>| {
            let mut x = rect.left();
            for (c, col) in cols.iter().enumerate() {
                let r = Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x + widths[c], rect.bottom()));
                x += widths[c];
                let inner = r.shrink2(vec2(10.0, 0.0));
                if row.is_none() && !col.title.is_empty() {
                    let font = style::semibold(ui.ctx(), 12.0);
                    let (pos, align) = if col.right { (inner.right_center(), Align2::RIGHT_CENTER) } else { (inner.left_center(), Align2::LEFT_CENTER) };
                    ui.painter().with_clip_rect(r).text(pos, align, col.title, font, p.text_muted);
                    continue;
                }
                let layout = if col.right { Layout::right_to_left(Align::Center) } else { Layout::left_to_right(Align::Center) };
                let mut child = ui.new_child(UiBuilder::new().id_salt((id, row, c)).max_rect(inner).layout(layout));
                child.shrink_clip_rect(r);
                child.spacing_mut().item_spacing = vec2(6.0, 0.0);
                cell(&mut child, row, c);
            }
        };
        let (head, _) = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover());
        ui.painter().rect_filled(head, CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, p.header);
        ui.painter().hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, p.border));
        draw_row(ui, head, None);
        egui::ScrollArea::vertical().id_salt(id.with("rows")).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for r in 0..rows {
                let (rect, _) = ui.allocate_exact_size(vec2(w, row_h), Sense::hover());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                if r % 2 == 1 {
                    let radius = if r + 1 == rows { CornerRadius { nw: 0, ne: 0, sw: 7, se: 7 } } else { CornerRadius::ZERO };
                    ui.painter().rect_filled(rect, radius, stripe);
                }
                if r + 1 < rows {
                    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, line));
                }
                draw_row(ui, rect, Some(r));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
