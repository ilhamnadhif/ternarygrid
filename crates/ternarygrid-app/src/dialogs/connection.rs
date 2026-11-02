//! Connection wizard (FR-C1..C8): create, edit, test connections.

use super::{LABEL_W, add_field, cancel_button, caption, error_text, hint, muted, number_field, opt_field, rest, right, status_line, text_field, text_width};
use crate::backend::Job;
use crate::conn::{ConnectionManager, Credentials};
use crate::cx::AppContext;
use egui::{Align, Align2, CornerRadius, Id, Layout, Margin, RichText, Sense, Stroke, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType, vec2};
use egui_phosphor::regular as icon;
use std::cell::Cell;
use ternarygrid_core::config::*;
use ternarygrid_core::{DbKind, DbResult, Secret, tr, trf};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::{self, Dialog, Tone};
use ternarygrid_ui::style;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Main,
    Ssh,
    Proxy,
    Ssl,
    Init,
    Properties,
    Filters,
    General,
}

/// The field a validation message belongs to (shown under it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    File,
    Host,
    Ssh,
}

/// What the footer was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FooterAction {
    Test,
    Save,
    SaveAndConnect,
    Cancel,
}

/// What the sessions of a connection are opened with: `cfg` without its name,
/// colour, navigator settings and other settings a live connection applies as
/// they are; `auto_commit` resolves the connection type's default.
fn session_settings(cfg: &ConnectionConfig, auto_commit: bool) -> ConnectionConfig {
    let mut c = cfg.clone();
    c.auto_commit = Some(auto_commit);
    c.connection_type.clear();
    c.name.clear();
    c.folder = None;
    c.sort_index = None;
    c.description = None;
    c.color = None;
    c.save_password = false;
    c.last_connected = None;
    c.filters = Default::default();
    c.navigator_view = Default::default();
    c.init.show_all_databases = false;
    c.init.show_system_objects = false;
    c.init.fetch_size = None;
    c.init.query_timeout_secs = 0;
    c.init.shell_after_disconnect = None;
    c
}

pub struct ConnectionDialog {
    pub cfg: ConnectionConfig,
    pub is_new: bool,
    page: Page,
    password: String,
    ssh_password: String,
    ssh_passphrase: String,
    proxy_password: String,
    password_loaded: bool,
    /// The saved password, SSH password, SSH passphrase and proxy password as loaded.
    saved_secrets: [String; 4],
    /// Saved secrets that could not be read (locked vault, keychain denied): left as they are on save.
    unreadable: Vec<SecretSlot>,
    auto_commit_choice: u8, // 0 = type default, 1 = on, 2 = off
    new_prop: (String, String),
    test: Option<Job<DbResult<String>>>,
    test_result: Option<Result<String, String>>,
    url: String,
    /// The compatible product picked in the database list (TiDB, Supabase ...).
    preset: Option<&'static str>,
    /// The filter of the database list.
    kind_filter: String,
    /// The name the edited connection had (the title).
    edit_name: String,
    /// A required field is missing (after Test or Save): its message under it.
    invalid: Option<(Field, String)>,
    /// The URL of "Connect with a URL" did not parse.
    url_error: Option<String>,
    error: Option<String>,
    pub open: bool,
}

impl ConnectionDialog {
    pub fn new(kind: Option<DbKind>, folder: Option<String>) -> Self {
        let kind = kind.unwrap_or(DbKind::Postgres);
        let mut cfg = ConnectionConfig::new(kind, "");
        cfg.folder = folder;
        Self::from_config(cfg, true)
    }

    pub fn edit(cfg: ConnectionConfig) -> Self {
        Self::from_config(cfg, false)
    }

    fn from_config(cfg: ConnectionConfig, is_new: bool) -> Self {
        let auto_commit_choice = match cfg.auto_commit {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        };
        ConnectionDialog {
            edit_name: cfg.name.clone(),
            cfg,
            is_new,
            page: Page::Main,
            password: String::new(),
            ssh_password: String::new(),
            ssh_passphrase: String::new(),
            proxy_password: String::new(),
            password_loaded: false,
            saved_secrets: Default::default(),
            unreadable: Vec::new(),
            auto_commit_choice,
            new_prop: Default::default(),
            test: None,
            test_result: None,
            url: String::new(),
            preset: None,
            kind_filter: String::new(),
            invalid: None,
            url_error: None,
            error: None,
            open: true,
        }
    }

    /// Switch the new connection to another database or compatible product,
    /// keeping its name, folder and id.
    fn choose(&mut self, mut cfg: ConnectionConfig, preset: Option<&'static str>) {
        cfg.name = std::mem::take(&mut self.cfg.name);
        cfg.folder = self.cfg.folder.take();
        cfg.id = std::mem::take(&mut self.cfg.id);
        self.cfg = cfg;
        self.preset = preset;
    }

    fn load_saved_secrets(&mut self, conns: &ConnectionManager) {
        if self.password_loaded || self.is_new {
            self.password_loaded = true;
            return;
        }
        self.password_loaded = true;
        let id = self.cfg.id.clone();
        let mut unreadable = Vec::new();
        let mut get = |slot| match conns.secrets.get(&secret_key(&id, slot)) {
            Ok(s) => s.map(|s| s.expose().to_string()).unwrap_or_default(),
            Err(_) => {
                unreadable.push(slot);
                String::new()
            }
        };
        self.password = get(SecretSlot::Password);
        self.ssh_password = get(SecretSlot::SshPassword);
        self.ssh_passphrase = get(SecretSlot::SshPassphrase);
        self.proxy_password = get(SecretSlot::ProxyPassword);
        self.unreadable = unreadable;
        self.saved_secrets = self.secrets();
    }

    fn secrets(&self) -> [String; 4] {
        [self.password.clone(), self.ssh_password.clone(), self.ssh_passphrase.clone(), self.proxy_password.clone()]
    }

    fn credentials(&self) -> Credentials {
        let opt = |s: &str| if s.is_empty() { None } else { Some(Secret::new(s)) };
        Credentials { password: opt(&self.password), ssh_password: opt(&self.ssh_password), ssh_passphrase: opt(&self.ssh_passphrase), proxy_password: opt(&self.proxy_password) }
    }

    fn validate(&self) -> Option<(Field, String)> {
        if self.cfg.name.trim().is_empty() {
            return Some((Field::Name, tr("Enter a connection name.").to_string()));
        }
        if self.cfg.kind.is_file_based() {
            if self.cfg.file_path.as_deref().is_none_or(|p| p.trim().is_empty()) {
                return Some((Field::File, tr("Choose a database file.").to_string()));
            }
        } else if self.cfg.host.trim().is_empty() {
            return Some((Field::Host, tr("Enter a host name.").to_string()));
        }
        if let Some(ssh) = self.cfg.ssh.as_ref().filter(|s| s.enabled)
            && (ssh.host.trim().is_empty() || ssh.user.trim().is_empty())
        {
            return Some((Field::Ssh, tr("SSH tunnel needs a host and a user.").to_string()));
        }
        None
    }

    /// The validation message of `field`, if it is the one missing.
    fn invalid(&self, field: Field) -> Option<&str> {
        self.invalid.as_ref().filter(|(f, _)| *f == field).map(|(_, m)| m.as_str())
    }

    fn sync_auto_commit(&mut self) {
        self.cfg.auto_commit = match self.auto_commit_choice {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
    }

    /// Store secrets and the configuration. Returns true when saved.
    fn save(&mut self, cx: &mut AppContext) -> bool {
        self.sync_auto_commit();
        self.error = None;
        if let Some(e) = self.validate() {
            self.invalid = Some(e);
            return false;
        }
        let id = self.cfg.id.clone();
        let unreadable = self.unreadable.clone();
        let store = |slot, value: &str, keep: bool| -> DbResult<()> {
            let key = secret_key(&id, slot);
            if keep && !value.is_empty() {
                cx.conns.secrets.set(&key, &Secret::new(value))
            } else if keep && unreadable.contains(&slot) {
                // Not read, not retyped: the saved one stays.
                Ok(())
            } else {
                cx.conns.secrets.delete(&key)
            }
        };
        let ssh_save = self.cfg.ssh.as_ref().is_some_and(|s| s.enabled && s.save_credentials);
        let proxy_save = self.cfg.proxy.as_ref().is_some_and(|p| p.enabled && p.save_credentials);
        let results = [
            store(SecretSlot::Password, &self.password, self.cfg.save_password && matches!(self.cfg.auth, AuthMethod::Password)),
            store(SecretSlot::SshPassword, &self.ssh_password, ssh_save),
            store(SecretSlot::SshPassphrase, &self.ssh_passphrase, ssh_save),
            store(SecretSlot::ProxyPassword, &self.proxy_password, proxy_save),
        ];
        if let Some(Err(e)) = results.into_iter().find(|r| r.is_err()) {
            crate::dialogs::vault::unlock_after_failed_write(cx, &e);
            self.error = Some(trf("Cannot store the password securely: {}", &[&e]));
            return false;
        }
        // A live connection reconnects when its sessions were opened with what changed.
        let auto_commit = |c: &ConnectionConfig| c.auto_commit.unwrap_or(cx.conns.connection_type(&c.connection_type).auto_commit);
        let reconnect = cx.conns.is_connected(&id)
            && (self.secrets() != self.saved_secrets
                || cx.conns.config(&id).is_none_or(|old| session_settings(old, auto_commit(old)) != session_settings(&self.cfg, auto_commit(&self.cfg))));
        cx.conns.upsert(self.cfg.clone());
        cx.push(crate::command::AppCommand::SaveConnections);
        if reconnect {
            cx.push(crate::command::AppCommand::Reconnect(id));
            cx.toasts.info(tr("Reconnecting to apply the changed settings."));
        }
        true
    }

    pub fn show(&mut self, ctx: &egui::Context, cx: &mut AppContext) {
        self.load_saved_secrets(cx.conns);
        if let Some(job) = self.test.as_mut()
            && let Some(r) = job.poll()
        {
            self.test_result = Some(r.map_err(|e| e.full_text()));
            self.test = None;
        }
        // A missing field asked for: the message goes once it is filled in.
        if self.invalid.is_some() {
            self.invalid = self.validate();
        }
        let title = if self.is_new { tr("New connection").to_string() } else { trf("Edit connection · {}", &[&self.edit_name]) };
        let (width, is_new, testing) = (if self.is_new { 900.0 } else { 720.0 }, self.is_new, self.test.is_some());
        // Fixed body height: the dialog fits the screen and does not grow with its pages.
        let body_h = (ctx.content_rect().height() - 220.0).clamp(300.0, 520.0);
        // The footer's status: an error first, a missing field on another page, the test result.
        let shown_here = |f: Field| matches!((f, self.page), (Field::Name | Field::File | Field::Host, Page::Main) | (Field::Ssh, Page::Ssh));
        let status: Option<(Tone, String)> = match (&self.error, &self.invalid, &self.test_result) {
            (Some(e), _, _) => Some((Tone::Error, e.clone())),
            (None, Some((f, m)), _) if !shown_here(*f) => Some((Tone::Error, m.clone())),
            (None, _, Some(Ok(info))) => Some((Tone::Success, format!("{} {info}", tr("Connected:")))),
            (None, _, Some(Err(e))) => Some((Tone::Error, e.clone())),
            _ => None,
        };
        let action = Cell::new(None::<FooterAction>);
        let id = Id::new(("conn_dlg", &self.cfg.id));
        let mut open = self.open;
        let (resp, _) = Dialog::new(&title).icon(icon::PLUGS_CONNECTED).width(width).body_padding(Margin::ZERO).show_window(
            ctx,
            id,
            &mut open,
            None,
            |ui| self.body(ui, cx, body_h),
            Some(&mut |ui: &mut Ui| {
                let main = if is_new { tr("Save and connect") } else { tr("Save") };
                let right_w = kit::button_width(ui, Some(icon::PLUGS_CONNECTED), main, false)
                    + if is_new { kit::button_width(ui, None, tr("Save"), false) + 8.0 } else { 0.0 }
                    + kit::button_width(ui, None, tr("Cancel"), false)
                    + 24.0;
                if ui.add_enabled_ui(!testing, |ui| kit::secondary(ui, Some(icon::PLUGS_CONNECTED), tr("Test connection"))).inner.clicked() {
                    action.set(Some(FooterAction::Test));
                }
                if testing {
                    ui.add(egui::Spinner::new().size(14.0));
                }
                if let Some((tone, text)) = &status {
                    let w = rest(ui, right_w);
                    status_line(ui, *tone, text, w);
                }
                right(ui, |ui| {
                    if is_new {
                        if kit::primary_icon(ui, icon::PLUGS_CONNECTED, main).clicked() {
                            action.set(Some(FooterAction::SaveAndConnect));
                        }
                        if kit::secondary(ui, None, tr("Save")).clicked() {
                            action.set(Some(FooterAction::Save));
                        }
                    } else if kit::primary(ui, main).clicked() {
                        action.set(Some(FooterAction::Save));
                    }
                    if cancel_button(ui).clicked() {
                        action.set(Some(FooterAction::Cancel));
                    }
                });
            }),
        );
        let mut close = crate::widgets::escape_closes(ctx, resp.layer);
        match action.get() {
            Some(FooterAction::Test) => {
                self.sync_auto_commit();
                self.test_result = None;
                self.error = None;
                match self.validate() {
                    Some(e) => self.invalid = Some(e),
                    None => {
                        self.invalid = None;
                        self.test = Some(cx.conns.test_connection(cx.backend, self.cfg.clone(), self.credentials()));
                    }
                }
            }
            Some(FooterAction::SaveAndConnect) if self.save(cx) => {
                cx.push(crate::command::AppCommand::Connect(self.cfg.id.clone()));
                close = true;
            }
            Some(FooterAction::Save) if self.save(cx) => close = true,
            Some(FooterAction::Cancel) => close = true,
            _ => {}
        }
        self.open = open && !close;
    }

    /// The database list (new connections), the page tabs and the page.
    fn body(&mut self, ui: &mut Ui, cx: &mut AppContext, h: f32) {
        let p = style::pal(ui);
        let full = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(full, h), Sense::hover());
        let mut x = rect.left();
        if self.is_new {
            let list = egui::Rect::from_min_size(rect.min, vec2(200.0, h));
            ui.painter().vline(list.right() - 0.5, list.y_range(), Stroke::new(1.0, p.border));
            let mut lui = ui.new_child(UiBuilder::new().id_salt("conn_kinds").max_rect(list.shrink2(vec2(8.0, 12.0))).layout(Layout::top_down(Align::Min)));
            self.kind_list(&mut lui, cx);
            x = list.right();
        }
        let page_rect = egui::Rect::from_min_max(egui::pos2(x, rect.top()), rect.max);
        let mut pui = ui.new_child(UiBuilder::new().id_salt("conn_page").max_rect(page_rect).layout(Layout::top_down(Align::Min)));
        self.page_tabs(&mut pui);
        let page = self.page;
        egui::ScrollArea::vertical().id_salt(("conn_page_scroll", page as u8)).auto_shrink([false, false]).show(&mut pui, |ui| {
            egui::Frame::new().inner_margin(Margin::symmetric(22, 18)).show(ui, |ui| {
                super::form_spacing(ui, 14.0);
                ui.set_width(ui.available_width());
                match page {
                    Page::Main => self.main_page(ui, cx),
                    Page::Ssh => self.ssh_page(ui, cx),
                    Page::Proxy => self.proxy_page(ui, cx),
                    Page::Ssl => self.ssl_page(ui),
                    Page::Init => self.init_page(ui),
                    Page::Properties => self.properties_page(ui),
                    Page::Filters => self.filters_page(ui),
                    Page::General => self.general_page(ui, cx),
                }
            });
        });
    }

    /// The databases and the compatible products, with a filter.
    fn kind_list(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let w = ui.available_width();
        kit::search_field(ui, Id::new(("conn_kind_filter", &self.cfg.id)), &mut self.kind_filter, tr("Filter databases"), w);
        ui.add_space(8.0);
        let needle = self.kind_filter.trim().to_lowercase();
        let matches = |name: &str| needle.is_empty() || name.to_lowercase().contains(&needle);
        egui::ScrollArea::vertical().id_salt("conn_kinds_scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let kinds: Vec<DbKind> = DbKind::ALL.into_iter().filter(|k| cx.conns.registry.get(*k).is_some() && matches(k.display_name())).collect();
            if !kinds.is_empty() {
                list_caption(ui, tr("Database"), 4.0);
            }
            for kind in kinds {
                let sel = self.cfg.kind == kind && self.preset.is_none();
                if kind_row(ui, kind, kind.display_name(), sel).clicked() && !sel {
                    self.choose(ConnectionConfig::new(kind, ""), None);
                }
            }
            let presets: Vec<_> = PRESETS.iter().filter(|p| cx.conns.registry.get(p.kind).is_some() && matches(p.name)).collect();
            if !presets.is_empty() {
                list_caption(ui, tr("Compatible"), 10.0);
            }
            for preset in presets {
                let sel = self.preset == Some(preset.id);
                let r = kind_row(ui, preset.kind, preset.name, sel).on_hover_text(trf("Connects with the {} driver", &[&preset.kind.display_name()]));
                if r.clicked() && !sel {
                    self.choose(preset.config(""), Some(preset.id));
                }
            }
        });
    }

    /// The underlined page tabs (file databases have no SSH, proxy or SSL).
    fn page_tabs(&mut self, ui: &mut Ui) {
        let p = style::pal(ui);
        let row = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.add_space(10.0);
            for (page, label) in [
                (Page::Main, tr("Main")),
                (Page::Ssh, tr("SSH")),
                (Page::Proxy, tr("Proxy")),
                (Page::Ssl, tr("SSL")),
                (Page::Init, tr("Initialization")),
                (Page::Properties, tr("Driver properties")),
                (Page::Filters, tr("Filters")),
                (Page::General, tr("General")),
            ] {
                if self.cfg.kind.is_file_based() && matches!(page, Page::Ssh | Page::Proxy | Page::Ssl) {
                    continue;
                }
                if kit::underline_tab(ui, None, label, None, self.page == page, 34.0).clicked() {
                    self.page = page;
                }
            }
        });
        ui.painter().hline(ui.max_rect().x_range(), row.response.rect.bottom() - 0.5, Stroke::new(1.0, p.border));
        if self.cfg.kind.is_file_based() && matches!(self.page, Page::Ssh | Page::Proxy | Page::Ssl) {
            self.page = Page::Main;
        }
    }

    fn main_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let kind = self.cfg.kind;
        parts::form_row(ui, tr("Connection name"), LABEL_W, |ui| add_field(ui, text_field(&mut self.cfg.name, f32::INFINITY).hint_text(hint(ui, kind.display_name()))));
        if let Some(m) = self.invalid(Field::Name) {
            error_text(ui, m);
        }
        if kind.is_file_based() {
            parts::form_row(ui, tr("Database file"), LABEL_W, |ui| {
                let reserve = kit::button_width(ui, Some(icon::FOLDER_OPEN), tr("Browse…"), false) + kit::button_width(ui, None, tr("New…"), false) + 16.0;
                let w = rest(ui, reserve);
                let mut path = self.cfg.file_path.clone().unwrap_or_default();
                if add_field(ui, text_field(&mut path, w).font(egui::TextStyle::Monospace)).changed() {
                    self.cfg.file_path = Some(path);
                }
                if kit::secondary(ui, Some(icon::FOLDER_OPEN), tr("Browse…")).on_hover_text(tr("Open existing file")).clicked()
                    && let Some(p) = rfd::FileDialog::new().pick_file()
                {
                    self.cfg.file_path = Some(p.display().to_string());
                    if self.cfg.name.is_empty() {
                        self.cfg.name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    }
                }
                if kit::button_ex(ui, Kind::Ghost, None, tr("New…"), false).on_hover_text(tr("Create new database file")).clicked()
                    && let Some(p) = rfd::FileDialog::new().set_file_name("database.db").save_file()
                {
                    self.cfg.file_path = Some(p.display().to_string());
                    if self.cfg.name.is_empty() {
                        self.cfg.name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    }
                }
            });
            if let Some(m) = self.invalid(Field::File) {
                error_text(ui, m);
            }
        } else {
            parts::form_row(ui, tr("Host"), LABEL_W, |ui| {
                let w = rest(ui, 80.0 + text_width(ui, tr("Port")) + 16.0);
                add_field(ui, text_field(&mut self.cfg.host, w));
                ui.label(RichText::new(tr("Port")).color(style::pal(ui).text_muted));
                let mut port = self.cfg.port.map(|p| p.to_string()).unwrap_or_default();
                let default = kind.default_port().map(|p| p.to_string()).unwrap_or_default();
                if add_field(ui, text_field(&mut port, 80.0).hint_text(hint(ui, &default))).changed() {
                    self.cfg.port = port.trim().parse().ok();
                }
            });
            if let Some(m) = self.invalid(Field::Host) {
                error_text(ui, m);
            }
            if kind == DbKind::Oracle {
                // A service name (the usual) or the SID of an older database.
                let mut sid = self.cfg.properties.contains_key("sid");
                parts::form_row(ui, tr("Connect by"), LABEL_W, |ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    let before = sid;
                    ui.radio_value(&mut sid, false, tr("Service name"));
                    ui.radio_value(&mut sid, true, "SID");
                    if sid != before {
                        if sid {
                            let v = self.cfg.database.take().unwrap_or_default();
                            self.cfg.properties.insert("sid".into(), v);
                        } else {
                            self.cfg.database = self.cfg.properties.remove("sid").filter(|v| !v.is_empty());
                        }
                    }
                });
            }
            if kind == DbKind::MongoDb {
                // Atlas and other hosted clusters: `mongodb+srv://` (hosts from DNS, port unused).
                let mut srv = self.cfg.properties.get("srv").is_some_and(|v| v == "true");
                parts::form_row(ui, "", LABEL_W, |ui| {
                    if ui.checkbox(&mut srv, tr("DNS seed list (mongodb+srv, Atlas)")).changed() {
                        if srv {
                            self.cfg.properties.insert("srv".into(), "true".into());
                        } else {
                            self.cfg.properties.remove("srv");
                        }
                    }
                });
            }
            let sid = kind == DbKind::Oracle && self.cfg.properties.contains_key("sid");
            parts::form_row(ui, if sid { "SID" } else { tr(kind.database_label()) }, LABEL_W, |ui| {
                let mut db = if sid { self.cfg.properties.get("sid").cloned().unwrap_or_default() } else { self.cfg.database.clone().unwrap_or_default() };
                let h = if sid { "ORCL" } else { kind.default_database().unwrap_or("") };
                if add_field(ui, text_field(&mut db, f32::INFINITY).hint_text(hint(ui, h))).changed() {
                    if sid {
                        self.cfg.properties.insert("sid".into(), db);
                    } else {
                        self.cfg.database = if db.is_empty() { None } else { Some(db) };
                    }
                }
            });
            parts::form_row(ui, tr("Authentication"), LABEL_W, |ui| {
                egui::ComboBox::from_id_salt("auth_method").icon(kit::combo_caret).width(200.0).selected_text(auth_label(&self.cfg.auth)).show_ui(ui, |ui| {
                    let mut options = vec![AuthMethod::Password, AuthMethod::Prompt];
                    if kind.is_postgres_family() {
                        options.push(AuthMethod::Pgpass);
                    }
                    options.push(AuthMethod::Command { command: String::new() });
                    options.push(AuthMethod::None);
                    for o in options {
                        let label = auth_label(&o);
                        let same = std::mem::discriminant(&o) == std::mem::discriminant(&self.cfg.auth);
                        if ui.selectable_label(same, label).clicked() && !same {
                            self.cfg.auth = o;
                        }
                    }
                });
            });
            if !matches!(self.cfg.auth, AuthMethod::None) {
                parts::form_row(ui, tr("User"), LABEL_W, |ui| opt_field(ui, &mut self.cfg.user, "", f32::INFINITY, false));
            }
            match &mut self.cfg.auth {
                AuthMethod::Password => {
                    let save = &mut self.cfg.save_password;
                    let password = &mut self.password;
                    parts::form_row(ui, tr("Password"), LABEL_W, |ui| {
                        let w = rest(ui, text_width(ui, tr("Save password")) + 34.0);
                        add_field(ui, text_field(password, w).password(true));
                        ui.checkbox(save, tr("Save password"));
                    });
                }
                AuthMethod::Command { command } => {
                    parts::form_row(ui, tr("Password command"), LABEL_W, |ui| {
                        add_field(ui, text_field(command, f32::INFINITY).font(egui::TextStyle::Monospace).hint_text(hint(ui, "security find-generic-password -w -s db")))
                    });
                }
                AuthMethod::Pgpass => {
                    parts::form_row(ui, "", LABEL_W, |ui| muted(ui, tr("The password is read from ~/.pgpass (or PGPASSFILE).")));
                }
                _ => {}
            }
            if cx.conns.registry.get(kind).is_some_and(|d| d.info().capabilities.multiple_databases) {
                parts::form_row(ui, "", LABEL_W, |ui| ui.checkbox(&mut self.cfg.init.show_all_databases, tr("Show all databases")));
            }
        }
        parts::form_row(ui, tr("Connection type"), LABEL_W, |ui| {
            let types = cx.conns.connection_types();
            let current = types.iter().find(|t| t.id == self.cfg.connection_type).map(|t| t.name.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("conn_type").icon(kit::combo_caret).width(180.0).selected_text(tr(&current)).show_ui(ui, |ui| {
                for t in &types {
                    let marker = crate::theme::connection_marker(t.color).map(|c| RichText::new("■ ").color(c)).unwrap_or_else(|| RichText::new("□ "));
                    ui.horizontal(|ui| {
                        ui.label(marker);
                        ui.selectable_value(&mut self.cfg.connection_type, t.id.clone(), tr(&t.name));
                    });
                }
            });
            ui.add_space(4.0);
            ui.checkbox(&mut self.cfg.read_only, tr("Read-only"));
        });
        parts::form_row(ui, tr("Auto-commit"), LABEL_W, |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            ui.radio_value(&mut self.auto_commit_choice, 0, tr("Connection type default"));
            ui.radio_value(&mut self.auto_commit_choice, 1, tr("On"));
            ui.radio_value(&mut self.auto_commit_choice, 2, tr("Off (manual commit)"));
        });
        if self.cfg.kind.is_file_based() {
            return;
        }
        let open = parts::form_row(ui, "", LABEL_W, |ui| super::disclosure(ui, Id::new(("conn_url", &self.cfg.id)), tr("Connect with a URL"), false));
        if !open {
            return;
        }
        parts::form_row(ui, "", LABEL_W, |ui| {
            let w = rest(ui, kit::button_width(ui, None, tr("Apply"), false) + 8.0);
            add_field(ui, text_field(&mut self.url, w).font(egui::TextStyle::Monospace).hint_text(hint(ui, url_example(self.cfg.kind))));
            if kit::secondary(ui, None, tr("Apply")).clicked() {
                self.apply_url();
            }
        });
        if let Some(e) = &self.url_error {
            parts::form_row(ui, "", LABEL_W, |ui| error_text(ui, e));
        }
    }

    /// Take host, port, database, user, TLS and the password from the URL field.
    fn apply_url(&mut self) {
        match crate::urls::parse(&self.url) {
            Ok((cfg, pw)) => {
                // What a URL says; SSH, colours, filters and the other settings stay.
                if self.is_new || cfg.kind == self.cfg.kind {
                    self.cfg.kind = cfg.kind;
                }
                if self.cfg.name.is_empty() {
                    self.cfg.name = if cfg.name.is_empty() { cfg.host.clone() } else { cfg.name.clone() };
                }
                self.cfg.host = cfg.host;
                self.cfg.port = cfg.port;
                self.cfg.database = cfg.database;
                if cfg.user.is_some() {
                    self.cfg.user = cfg.user;
                }
                if cfg.file_path.is_some() {
                    self.cfg.file_path = cfg.file_path;
                }
                if cfg.ssl != ternarygrid_core::config::SslConfig::default() {
                    self.cfg.ssl = cfg.ssl;
                }
                self.cfg.read_only |= cfg.read_only;
                self.cfg.properties.extend(cfg.properties);
                if let Some(pw) = pw {
                    self.password = pw.expose().to_string();
                }
                self.url_error = None;
            }
            Err(e) => self.url_error = Some(e.to_string()),
        }
    }

    fn network_profile_selector(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let profiles: Vec<(String, String)> = cx.conns.store.network_profiles.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
        if profiles.is_empty() && self.cfg.network_profile.is_none() {
            return false;
        }
        parts::form_row(ui, tr("Network profile"), LABEL_W, |ui| {
            let current = self
                .cfg
                .network_profile
                .as_ref()
                .and_then(|id| profiles.iter().find(|(i, _)| i == id))
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| tr("(none — settings below)").to_string());
            egui::ComboBox::from_id_salt("net_profile").icon(kit::combo_caret).width(280.0).selected_text(current).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.cfg.network_profile, None, tr("(none — settings below)"));
                for (id, name) in &profiles {
                    ui.selectable_value(&mut self.cfg.network_profile, Some(id.clone()), name);
                }
            });
        });
        if self.cfg.network_profile.is_some() {
            parts::form_row(ui, "", LABEL_W, |ui| muted(ui, tr("SSH and proxy settings come from the network profile (Settings → Network profiles).")));
            true
        } else {
            false
        }
    }

    fn ssh_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if self.network_profile_selector(ui, cx) {
            return;
        }
        let ssh = self.cfg.ssh.get_or_insert_with(SshConfig::default);
        ssh_form(ui, "ssh_grid", ssh, &mut self.ssh_password, &mut self.ssh_passphrase);
        if let Some(m) = self.invalid(Field::Ssh) {
            error_text(ui, m);
        }
    }

    fn proxy_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if self.network_profile_selector(ui, cx) {
            return;
        }
        let proxy = self.cfg.proxy.get_or_insert_with(ProxyConfig::default);
        proxy_form(ui, "proxy_grid", proxy, &mut self.proxy_password);
    }

    fn ssl_page(&mut self, ui: &mut Ui) {
        let ssl = &mut self.cfg.ssl;
        form_row_top(ui, tr("SSL mode"), |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let items: Vec<(SslMode, &str)> = SslMode::ALL.iter().map(|m| (*m, m.label())).collect();
            parts::segmented(ui, &mut ssl.mode, &items);
            muted(
                ui,
                match ssl.mode {
                    SslMode::Disable => tr("No encryption."),
                    SslMode::Prefer => tr("Encrypt when the server supports it; the certificate is not verified."),
                    SslMode::Require => tr("Always encrypt; the certificate is not verified."),
                    SslMode::VerifyCa => tr("Always encrypt and verify the certificate chain."),
                    SslMode::VerifyFull => tr("Always encrypt and verify the certificate chain and host name."),
                },
            );
        });
        for (label, field, h) in [
            (tr("CA certificate"), &mut ssl.ca_cert, ""),
            (tr("Client certificate"), &mut ssl.client_cert, tr("optional")),
            (tr("Client key"), &mut ssl.client_key, tr("optional")),
        ] {
            parts::form_row(ui, label, LABEL_W, |ui| {
                let w = rest(ui, 36.0);
                opt_field(ui, field, h, w, true);
                if kit::icon_button(ui, icon::FOLDER_OPEN, tr("Choose file")).clicked()
                    && let Some(p) = rfd::FileDialog::new().add_filter("PEM", &["pem", "crt", "cer", "key"]).pick_file()
                {
                    *field = Some(p.display().to_string());
                }
            });
        }
        parts::form_row(ui, "", LABEL_W, |ui| ui.checkbox(&mut ssl.only_custom_ca, tr("Trust only this CA (ignore system certificates)")));
    }

    fn init_page(&mut self, ui: &mut Ui) {
        let init = &mut self.cfg.init;
        let gap = 28.0;
        let col_w = ((ui.available_width() - gap) / 2.0).floor();
        let label_w = 130.0;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.allocate_ui_with_layout(vec2(col_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(col_w);
                ui.spacing_mut().item_spacing.y = 12.0;
                let field_w = |ui: &Ui, w: f32| w.min(ui.available_width());
                parts::form_row(ui, tr("Default schema"), label_w, |ui| {
                    let w = field_w(ui, 200.0);
                    opt_field(ui, &mut init.default_schema, "", w, false)
                });
                parts::form_row(ui, tr("Keep-alive (s)"), label_w, |ui| number_field(ui, "init_keepalive", &mut init.keep_alive_secs, 0..=3600, 90.0));
                parts::form_row(ui, tr("Connect timeout (s)"), label_w, |ui| number_field(ui, "init_connect_timeout", &mut init.connect_timeout_secs, 1..=600, 90.0));
                parts::form_row(ui, tr("Query timeout (s)"), label_w, |ui| number_field(ui, "init_query_timeout", &mut init.query_timeout_secs, 0..=86_400, 90.0));
                parts::form_row(ui, tr("Fetch size"), label_w, |ui| {
                    let mut fs = init.fetch_size.map(|f| f.to_string()).unwrap_or_default();
                    let w = field_w(ui, 140.0);
                    if add_field(ui, text_field(&mut fs, w).hint_text(hint(ui, tr("global setting")))).changed() {
                        init.fetch_size = fs.trim().parse().ok();
                    }
                });
            });
            ui.allocate_ui_with_layout(vec2(col_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(col_w);
                ui.spacing_mut().item_spacing.y = 12.0;
                ui.checkbox(&mut init.separate_editor_connection, tr("Separate connection for each SQL editor"));
                ui.checkbox(&mut init.show_system_objects, tr("Show system objects"));
                parts::form_row(ui, tr("Before connect"), 110.0, |ui| {
                    opt_field(ui, &mut init.shell_before_connect, tr("Shell command"), f32::INFINITY, true).on_hover_text(tr("Shell command before connect"))
                });
                parts::form_row(ui, tr("After disconnect"), 110.0, |ui| {
                    opt_field(ui, &mut init.shell_after_disconnect, tr("Shell command"), f32::INFINITY, true).on_hover_text(tr("Shell command after disconnect"))
                });
            });
        });
        ui.label(RichText::new(tr("Initialization SQL (run after every connect)")).color(style::pal(ui).text_muted));
        parts::code_frame()(ui).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut init.init_sql).code_editor().frame(egui::Frame::NONE).margin(Margin::ZERO).desired_rows(5).desired_width(f32::INFINITY));
        });
    }

    fn properties_page(&mut self, ui: &mut Ui) {
        muted(ui, tr("Driver specific properties, e.g. application_name, options, charset."));
        let keys: Vec<String> = self.cfg.properties.keys().cloned().collect();
        let mut remove = None;
        let mut add = false;
        let cols = [super::Column::new(tr("Property"), super::W::Px(220.0)), super::Column::new(tr("Value"), super::W::Fr(1.0)), super::Column::new("", super::W::Px(44.0))];
        let props = &mut self.cfg.properties;
        let new_prop = &mut self.new_prop;
        super::table(ui, "conn_props", &cols, keys.len() + 1, 38.0, f32::INFINITY, |ui, row, col| {
            let Some(row) = row else { return };
            match (keys.get(row), col) {
                (Some(k), 0) => {
                    ui.label(RichText::new(k).monospace().size(12.0));
                }
                (Some(k), 1) => {
                    if let Some(v) = props.get_mut(k) {
                        add_field(ui, text_field(v, f32::INFINITY).font(egui::TextStyle::Monospace));
                    }
                }
                (Some(k), _) => {
                    if kit::icon_button_ex(ui, icon::TRASH, tr("Remove"), false, 24.0).clicked() {
                        remove = Some(k.clone());
                    }
                }
                (None, 0) => {
                    add_field(ui, text_field(&mut new_prop.0, f32::INFINITY).hint_text(hint(ui, tr("name"))));
                }
                (None, 1) => {
                    add_field(ui, text_field(&mut new_prop.1, f32::INFINITY).hint_text(hint(ui, tr("value"))));
                }
                (None, _) => {
                    if kit::icon_button_ex(ui, icon::PLUS, tr("Add"), false, 24.0).clicked() {
                        add = true;
                    }
                }
            }
        });
        if add && !self.new_prop.0.trim().is_empty() {
            let (k, v) = std::mem::take(&mut self.new_prop);
            self.cfg.properties.insert(k.trim().to_string(), v);
        }
        if let Some(k) = remove {
            self.cfg.properties.remove(&k);
        }
    }

    fn filters_page(&mut self, ui: &mut Ui) {
        muted(ui, tr("Patterns use * and ? wildcards, separated by commas."));
        let p = style::pal(ui);
        let id = self.cfg.id.clone();
        let f = &mut self.cfg.filters;
        ui.spacing_mut().item_spacing.y = 10.0;
        for (label, filter) in [
            (tr("Schemas"), &mut f.schemas),
            (tr("Tables"), &mut f.tables),
            (tr("Views"), &mut f.views),
            (tr("Databases"), &mut f.databases),
            (tr("Procedures and functions"), &mut f.routines),
            (tr("Other objects"), &mut f.other),
        ] {
            let open_id = Id::new(("conn_filter_open", &id, label));
            let used = filter.enabled || !filter.include.is_empty() || !filter.exclude.is_empty();
            let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(used);
            kit::card(ui).corner_radius(CornerRadius::same(style::RADIUS_LG)).inner_margin(Margin::same(12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    let title = ui.painter().layout_no_wrap(label.to_string(), style::semibold(ui.ctx(), 13.0), p.text);
                    let (r, resp) = ui.allocate_exact_size(vec2(title.size().x + 22.0, 24.0), Sense::click());
                    resp.widget_info(|| WidgetInfo::selected(WidgetType::CollapsingHeader, true, open, label));
                    let caret = if open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
                    ui.painter().text(egui::pos2(r.left() + 6.0, r.center().y), Align2::CENTER_CENTER, caret, style::icon_font(12.0), p.text_faint);
                    ui.painter().galley(egui::pos2(r.left() + 20.0, r.center().y - title.size().y / 2.0), title, p.text);
                    if resp.clicked() {
                        open = !open;
                        ui.data_mut(|d| d.insert_temp(open_id, open));
                    }
                    if !open && !used {
                        ui.label(RichText::new(tr("(no filter)")).size(12.0).color(p.text_faint));
                    }
                    // The switch at the right edge (laid out left to right: its label after it).
                    let switch_w = 40.0 + text_width(ui, tr("Enable filter"));
                    ui.add_space((ui.available_width() - switch_w).max(0.0));
                    parts::switch(ui, &mut filter.enabled, tr("Enable filter"), None);
                });
                if open {
                    ui.add_enabled_ui(filter.enabled, |ui| {
                        ui.horizontal(|ui| {
                            let w = ((ui.available_width() - 8.0) / 2.0).floor();
                            let mut inc = filter.include.join(", ");
                            let mut exc = filter.exclude.join(", ");
                            if add_field(ui, text_field(&mut inc, w).font(egui::TextStyle::Monospace).hint_text(hint(ui, tr("Include, e.g. app_*"))))
                                .on_hover_text(tr("Include"))
                                .changed()
                            {
                                filter.include = split_patterns(&inc);
                            }
                            if add_field(ui, text_field(&mut exc, w).font(egui::TextStyle::Monospace).hint_text(hint(ui, tr("Exclude, e.g. *_old, tmp_*"))))
                                .on_hover_text(tr("Exclude"))
                                .changed()
                            {
                                filter.exclude = split_patterns(&exc);
                            }
                        });
                    });
                }
            });
        }
    }

    fn general_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        parts::form_row(ui, tr("Folder"), LABEL_W, |ui| {
            let mut folder = self.cfg.folder.clone().unwrap_or_default();
            let w = rest(ui, 52.0);
            if add_field(ui, text_field(&mut folder, w).hint_text(hint(ui, tr("none")))).changed() {
                self.cfg.folder = if folder.trim().is_empty() { None } else { Some(folder.trim().trim_matches('/').to_string()) };
            }
            egui::ComboBox::from_id_salt("folders").icon(kit::combo_caret).selected_text("").width(44.0).show_ui(ui, |ui| {
                for f in cx.conns.folders() {
                    if ui.selectable_label(self.cfg.folder.as_deref() == Some(f), f).clicked() {
                        self.cfg.folder = Some(f.clone());
                    }
                }
            });
        });
        form_row_top(ui, tr("Description"), |ui| {
            let mut d = self.cfg.description.clone().unwrap_or_default();
            if add_field(ui, egui::TextEdit::multiline(&mut d).margin(Margin::symmetric(10, 6)).desired_rows(2).desired_width(f32::INFINITY)).changed() {
                self.cfg.description = if d.is_empty() { None } else { Some(d) };
            }
        });
        parts::form_row(ui, tr("Colour"), LABEL_W, |ui| {
            let mut use_color = self.cfg.color.is_some();
            if ui.checkbox(&mut use_color, tr("Custom colour")).changed() {
                self.cfg.color = if use_color { Some([120, 160, 220]) } else { None };
            }
            if let Some(c) = &mut self.cfg.color {
                ui.spacing_mut().interact_size = vec2(28.0, 28.0);
                ui.color_edit_button_srgb(c);
            }
        });
        parts::form_row(ui, tr("Navigator view"), LABEL_W, |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            ui.radio_value(&mut self.cfg.navigator_view, NavigatorView::Advanced, tr("Advanced"));
            ui.radio_value(&mut self.cfg.navigator_view, NavigatorView::Simple, tr("Simple"));
        });
    }
}

fn split_patterns(s: &str) -> Vec<String> {
    s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// A form row whose control is taller than one line: the label at its top.
fn form_row_top<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = style::pal(ui);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(LABEL_W, style::CONTROL_H), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            ui.add(egui::Label::new(RichText::new(label).color(p.text_muted)).truncate());
        });
        ui.vertical(add).inner
    })
    .inner
}

/// A caption of the database list.
fn list_caption(ui: &mut Ui, text: &str, top: f32) {
    ui.add_space(top);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        caption(ui, text);
    });
    ui.add_space(3.0);
}

/// One entry of the database list: logo and name, the chosen one on a soft accent.
fn kind_row(ui: &mut Ui, kind: DbKind, name: &str, selected: bool) -> egui::Response {
    let p = style::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, name));
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same(style::RADIUS);
        if selected {
            ui.painter().rect_filled(rect, radius, p.accent_soft);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, radius, p.hover);
        }
        if resp.has_focus() {
            ui.painter().rect_stroke(rect, radius, Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
        }
        let logo = egui::Rect::from_center_size(egui::pos2(rect.left() + 18.0, rect.center().y), Vec2::splat(16.0));
        let color = crate::theme::logo_color(kind, ui.visuals().dark_mode);
        if !crate::dblogo::paint(ui, kind, logo, color) {
            ui.painter().text(logo.center(), Align2::CENTER_CENTER, crate::nav::db_icon(kind), style::icon_font(15.0), color);
        }
        let size = style::body_size(ui.ctx());
        let font = if selected { style::medium(ui.ctx(), size) } else { egui::FontId::proportional(size) };
        ui.painter().with_clip_rect(rect).text(egui::pos2(rect.left() + 36.0, rect.center().y), Align2::LEFT_CENTER, name, font, p.text);
    }
    resp
}

/// Example URL for the "Connect with a URL" field.
fn url_example(kind: DbKind) -> &'static str {
    match kind {
        DbKind::MySql | DbKind::MariaDb => "mysql://user@host:3306/db",
        DbKind::SqlServer => "sqlserver://user@host:1433/db",
        DbKind::Oracle => "oracle://user@host:1521/service",
        DbKind::ClickHouse => "clickhouse://user@host:8123/db",
        DbKind::Cassandra => "cassandra://user@host:9042/keyspace",
        DbKind::Redis => "redis://user@host:6379/0",
        DbKind::MongoDb => "mongodb://user@host:27017/db",
        DbKind::Elasticsearch => "elasticsearch+https://user@host:9200",
        DbKind::Neo4j => "neo4j://user@host:7687",
        _ => "postgres://user@host:5432/db",
    }
}

fn auth_label(a: &AuthMethod) -> &'static str {
    match a {
        AuthMethod::Password => tr("Password"),
        AuthMethod::Prompt => tr("Ask every time"),
        AuthMethod::Pgpass => tr("pgpass file"),
        AuthMethod::Command { .. } => tr("Shell command"),
        AuthMethod::None => tr("No authentication"),
    }
}

fn host_key_label(p: HostKeyPolicy) -> &'static str {
    match p {
        HostKeyPolicy::AcceptNew => tr("Trust on first use"),
        HostKeyPolicy::Strict => tr("Only known hosts"),
        HostKeyPolicy::AcceptAll => tr("Accept any key (insecure)"),
    }
}

fn default_key_path() -> String {
    std::env::var("HOME").map(|h| format!("{h}/.ssh/id_ed25519")).unwrap_or_default()
}

/// A host and its port on one row.
fn host_port(ui: &mut Ui, id: &str, host: &mut String, port: &mut u16) {
    let w = rest(ui, 80.0 + text_width(ui, tr("Port")) + 16.0);
    add_field(ui, text_field(host, w));
    ui.label(RichText::new(tr("Port")).color(style::pal(ui).text_muted));
    number_field(ui, (id, "port"), port, 1..=65535, 80.0);
}

/// SSH tunnel settings form (connection dialog and network profiles).
pub fn ssh_form(ui: &mut Ui, id: &str, ssh: &mut SshConfig, password: &mut String, passphrase: &mut String) {
    let narrow = |ui: &Ui| 280f32.min(rest(ui, 0.0));
    parts::form_row(ui, "", LABEL_W, |ui| parts::switch(ui, &mut ssh.enabled, tr("Use SSH tunnel"), None));
    ui.add_enabled_ui(ssh.enabled, |ui| {
        parts::form_row(ui, tr("SSH host"), LABEL_W, |ui| host_port(ui, id, &mut ssh.host, &mut ssh.port));
        parts::form_row(ui, tr("User"), LABEL_W, |ui| {
            let w = narrow(ui);
            add_field(ui, text_field(&mut ssh.user, w))
        });
        parts::form_row(ui, tr("Authentication"), LABEL_W, |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            let is_pw = matches!(ssh.auth, SshAuth::Password);
            let is_key = matches!(ssh.auth, SshAuth::PublicKey { .. });
            let is_agent = matches!(ssh.auth, SshAuth::Agent);
            if ui.radio(is_pw, tr("Password")).clicked() {
                ssh.auth = SshAuth::Password;
            }
            if ui.radio(is_key, tr("Private key")).clicked() && !is_key {
                ssh.auth = SshAuth::PublicKey { key_path: default_key_path() };
            }
            if ui.radio(is_agent, tr("SSH agent")).clicked() {
                ssh.auth = SshAuth::Agent;
            }
        });
        match &mut ssh.auth {
            SshAuth::Password => {
                parts::form_row(ui, tr("Password"), LABEL_W, |ui| {
                    let w = narrow(ui);
                    add_field(ui, text_field(password, w).password(true))
                });
            }
            SshAuth::PublicKey { key_path } => {
                parts::form_row(ui, tr("Private key file"), LABEL_W, |ui| {
                    let w = rest(ui, 36.0);
                    add_field(ui, text_field(key_path, w).font(egui::TextStyle::Monospace));
                    if kit::icon_button(ui, icon::FOLDER_OPEN, tr("Choose file")).clicked()
                        && let Some(p) = rfd::FileDialog::new().pick_file()
                    {
                        *key_path = p.display().to_string();
                    }
                });
                parts::form_row(ui, tr("Passphrase"), LABEL_W, |ui| {
                    let w = narrow(ui);
                    add_field(ui, text_field(passphrase, w).password(true))
                });
            }
            SshAuth::Agent => {}
        }
        parts::form_row(ui, "", LABEL_W, |ui| ui.checkbox(&mut ssh.save_credentials, tr("Save credentials")));
        parts::form_row(ui, tr("Local port"), LABEL_W, |ui| {
            let mut lp = ssh.local_port.map(|p| p.to_string()).unwrap_or_default();
            if add_field(ui, text_field(&mut lp, 140.0).hint_text(hint(ui, tr("automatic")))).changed() {
                ssh.local_port = lp.trim().parse().ok();
            }
        });
        parts::form_row(ui, tr("Keep-alive (s)"), LABEL_W, |ui| number_field(ui, (id, "keepalive"), &mut ssh.keepalive_secs, 0..=3600, 90.0));
        parts::form_row(ui, tr("Host key check"), LABEL_W, |ui| {
            egui::ComboBox::from_id_salt((id, "hostkey")).icon(kit::combo_caret).width(240.0).selected_text(host_key_label(ssh.host_key_policy)).show_ui(ui, |ui| {
                for p in [HostKeyPolicy::AcceptNew, HostKeyPolicy::Strict, HostKeyPolicy::AcceptAll] {
                    ui.selectable_value(&mut ssh.host_key_policy, p, host_key_label(p));
                }
            });
        });
    });
}

/// Proxy settings form (connection dialog and network profiles).
pub fn proxy_form(ui: &mut Ui, id: &str, proxy: &mut ProxyConfig, password: &mut String) {
    let narrow = |ui: &Ui| 280f32.min(rest(ui, 0.0));
    parts::form_row(ui, "", LABEL_W, |ui| parts::switch(ui, &mut proxy.enabled, tr("Use proxy"), None));
    ui.add_enabled_ui(proxy.enabled, |ui| {
        parts::form_row(ui, tr("Type"), LABEL_W, |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            ui.radio_value(&mut proxy.kind, ProxyKind::Socks5, "SOCKS5");
            ui.radio_value(&mut proxy.kind, ProxyKind::Http, "HTTP CONNECT");
        });
        parts::form_row(ui, tr("Proxy host"), LABEL_W, |ui| host_port(ui, id, &mut proxy.host, &mut proxy.port));
        parts::form_row(ui, tr("User"), LABEL_W, |ui| {
            let w = narrow(ui);
            opt_field(ui, &mut proxy.user, "", w, false)
        });
        parts::form_row(ui, tr("Password"), LABEL_W, |ui| {
            let w = narrow(ui);
            add_field(ui, text_field(password, w).password(true))
        });
        parts::form_row(ui, "", LABEL_W, |ui| ui.checkbox(&mut proxy.save_credentials, tr("Save credentials")));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_session_settings_need_a_reconnect() {
        let old = ConnectionConfig::new(DbKind::Postgres, "pg");
        let same = |new: &ConnectionConfig, ac: bool| session_settings(&old, true) == session_settings(new, ac);
        let mut new = old.clone();
        new.name = "renamed".into();
        new.color = Some([1, 2, 3]);
        new.connection_type = "prod".into();
        new.init.show_all_databases = true;
        assert!(same(&new, true), "name, colour, type with the same auto-commit, navigator");
        assert!(!same(&new, false), "the type's auto-commit changed");
        for change in [
            |c: &mut ConnectionConfig| c.host = "elsewhere".into(),
            |c: &mut ConnectionConfig| c.read_only = true,
            |c: &mut ConnectionConfig| c.init.init_sql = "SET x = 1".into(),
            |c: &mut ConnectionConfig| drop(c.properties.insert("sslmode".into(), "require".into())),
        ] {
            let mut new = old.clone();
            change(&mut new);
            assert!(!same(&new, true), "{new:?}");
        }
    }
}
