//! Connection wizard (FR-C1..C8): create, edit, test connections.

use super::{LABEL_W, add_field, cancel_button, error_text, form_row, hint, mono_hint, muted, number_field, opt_field, rest, right, status_line, sub_text, text_field, text_width};
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
        let (resp, _) = Dialog::new(&title).icon(icon::PLUG).width(width).body_padding(Margin::ZERO).show_window(
            ctx,
            id,
            &mut open,
            None,
            |ui| self.body(ui, cx, body_h),
            Some(&mut |ui: &mut Ui| {
                let main = if is_new { tr("Save and connect") } else { tr("Save") };
                let right_w = kit::button_width(ui, Some(icon::PLUG), main, false)
                    + if is_new { kit::button_width(ui, None, tr("Save"), false) + 8.0 } else { 0.0 }
                    + kit::button_width(ui, None, tr("Cancel"), false)
                    + 24.0;
                if ui.add_enabled_ui(!testing, |ui| kit::secondary(ui, Some(icon::PLUG), tr("Test connection"))).inner.clicked() {
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
                        if kit::primary_icon(ui, icon::PLUG, main).clicked() {
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
        super::dim_behind(ctx, resp.layer);
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
        pui.spacing_mut().item_spacing.y = 0.0;
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
                    Page::Init => self.init_page(ui, cx.conns.registry.get(self.cfg.kind).map(|d| d.dialect())),
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
        ui.spacing_mut().item_spacing.y = 0.0;
        kit::search_field(ui, Id::new(("conn_kind_filter", &self.cfg.id)), &mut self.kind_filter, tr("Filter databases"), w);
        let needle = self.kind_filter.trim().to_lowercase();
        let matches = |name: &str| needle.is_empty() || name.to_lowercase().contains(&needle);
        egui::ScrollArea::vertical().id_salt("conn_kinds_scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let kinds: Vec<DbKind> = DbKind::ALL.into_iter().filter(|k| cx.conns.registry.get(*k).is_some() && matches(k.display_name())).collect();
            if !kinds.is_empty() {
                list_caption(ui, tr("Database"), 24.5);
            }
            for kind in kinds {
                let sel = self.cfg.kind == kind && self.preset.is_none();
                if kind_row(ui, kind, kind.display_name(), sel).clicked() && !sel {
                    self.choose(ConnectionConfig::new(kind, ""), None);
                }
            }
            let mut presets: Vec<_> = PRESETS.iter().filter(|p| cx.conns.registry.get(p.kind).is_some() && matches(p.name)).collect();
            // The design's five first, the others after them.
            const FIRST: [&str; 5] = ["supabase", "neon", "timescaledb", "planetscale", "atlas"];
            presets.sort_by_key(|p| FIRST.iter().position(|f| *f == p.id).unwrap_or(FIRST.len()));
            if !presets.is_empty() {
                list_caption(ui, tr("Compatible"), 24.0);
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
            // The design's tabs: 16 apart, 16 in from the edge.
            ui.spacing_mut().item_spacing.x = 16.0;
            ui.add_space(16.0);
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
        form_row(ui, tr("Connection name"), |ui| add_field(ui, text_field(&mut self.cfg.name, f32::INFINITY).hint_text(hint(ui, kind.display_name()))));
        if let Some(m) = self.invalid(Field::Name) {
            error_text(ui, m);
        }
        if kind.is_file_based() {
            form_row(ui, tr("Database file"), |ui| {
                let reserve = kit::button_width(ui, Some(icon::FOLDER_OPEN), tr("Browse…"), false) + kit::button_width(ui, None, tr("New…"), false) + 16.0;
                let w = rest(ui, reserve);
                let mut path = self.cfg.file_path.clone().unwrap_or_default();
                if add_field(ui, text_field(&mut path, w).font(egui::FontId::monospace(12.5))).changed() {
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
            form_row(ui, tr("Host"), |ui| {
                let w = rest(ui, 80.0 + text_width(ui, tr("Port")) + 16.0);
                add_field(ui, text_field(&mut self.cfg.host, w));
                ui.label(RichText::new(tr("Port")).size(12.5).color(style::pal(ui).text_muted));
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
                form_row(ui, tr("Connect by"), |ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    let before = sid;
                    kit::radio_value(ui, &mut sid, false, tr("Service name"));
                    kit::radio_value(ui, &mut sid, true, "SID");
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
                form_row(ui, "", |ui| {
                    if ternarygrid_ui::kit::checkbox(ui, &mut srv, tr("DNS seed list (mongodb+srv, Atlas)")).changed() {
                        if srv {
                            self.cfg.properties.insert("srv".into(), "true".into());
                        } else {
                            self.cfg.properties.remove("srv");
                        }
                    }
                });
            }
            let sid = kind == DbKind::Oracle && self.cfg.properties.contains_key("sid");
            form_row(ui, if sid { "SID" } else { tr(kind.database_label()) }, |ui| {
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
            form_row(ui, tr("Authentication"), |ui| {
                egui::ComboBox::from_id_salt("auth_method").icon(kit::combo_caret).width(200.0).selected_text(auth_label(&self.cfg.auth)).show_ui(ui, |ui| {
                    kit::menu_look(ui, 200.0);
                    let mut options = vec![AuthMethod::Password, AuthMethod::Prompt];
                    if kind.is_postgres_family() {
                        options.push(AuthMethod::Pgpass);
                    }
                    options.push(AuthMethod::Command { command: String::new() });
                    options.push(AuthMethod::None);
                    for o in options {
                        let mut same = std::mem::discriminant(&o) == std::mem::discriminant(&self.cfg.auth);
                        if kit::menu_check(ui, &mut same, auth_label(&o)).clicked() && same {
                            self.cfg.auth = o;
                        }
                    }
                });
            });
            if !matches!(self.cfg.auth, AuthMethod::None) {
                form_row(ui, tr("User"), |ui| opt_field(ui, &mut self.cfg.user, "", f32::INFINITY, false));
            }
            match &mut self.cfg.auth {
                AuthMethod::Password => {
                    let save = &mut self.cfg.save_password;
                    let password = &mut self.password;
                    form_row(ui, tr("Password"), |ui| {
                        let w = rest(ui, text_width(ui, tr("Save password")) + 34.0);
                        add_field(ui, text_field(password, w).password(true));
                        ternarygrid_ui::kit::checkbox(ui, save, tr("Save password"));
                    });
                }
                AuthMethod::Command { command } => {
                    form_row(ui, tr("Password command"), |ui| {
                        add_field(ui, text_field(command, f32::INFINITY).font(egui::FontId::monospace(12.5)).hint_text(mono_hint(ui, "security find-generic-password -w -s db")))
                    });
                }
                AuthMethod::Pgpass => {
                    form_row(ui, "", |ui| muted(ui, tr("The password is read from ~/.pgpass (or PGPASSFILE).")));
                }
                _ => {}
            }
            if cx.conns.registry.get(kind).is_some_and(|d| d.info().capabilities.multiple_databases) {
                form_row(ui, "", |ui| ternarygrid_ui::kit::checkbox(ui, &mut self.cfg.init.show_all_databases, tr("Show all databases")));
            }
        }
        form_row(ui, tr("Connection type"), |ui| {
            let types = cx.conns.connection_types();
            let current = types.iter().find(|t| t.id == self.cfg.connection_type).map(|t| t.name.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("conn_type").icon(kit::combo_caret).width(180.0).selected_text(tr(&current)).show_ui(ui, |ui| {
                kit::menu_look(ui, 180.0);
                for t in &types {
                    let mut on = self.cfg.connection_type == t.id;
                    let r = kit::menu_check(ui, &mut on, tr(&t.name));
                    // The type's colour at the right, as its marker in the navigator.
                    if let Some(c) = crate::theme::connection_marker(t.color) {
                        ui.painter().circle_filled(egui::pos2(r.rect.right() - 14.0, r.rect.center().y), 4.0, c);
                    }
                    if r.clicked() {
                        self.cfg.connection_type = t.id.clone();
                    }
                }
            });
            ternarygrid_ui::kit::checkbox(ui, &mut self.cfg.read_only, tr("Read-only"));
        });
        form_row(ui, tr("Auto-commit"), |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            kit::radio_value(ui, &mut self.auto_commit_choice, 0, tr("Connection type default"));
            kit::radio_value(ui, &mut self.auto_commit_choice, 1, tr("On"));
            kit::radio_value(ui, &mut self.auto_commit_choice, 2, tr("Off (manual commit)"));
        });
        if self.cfg.kind.is_file_based() {
            return;
        }
        let open = form_row(ui, "", |ui| super::disclosure(ui, Id::new(("conn_url", &self.cfg.id)), tr("Connect with a URL"), false));
        if !open {
            return;
        }
        form_row(ui, "", |ui| {
            let w = rest(ui, kit::button_width(ui, None, tr("Apply"), false) + 8.0);
            add_field(ui, text_field(&mut self.url, w).font(egui::FontId::monospace(12.5)).hint_text(mono_hint(ui, url_example(self.cfg.kind))));
            if kit::secondary(ui, None, tr("Apply")).clicked() {
                self.apply_url();
            }
        });
        if let Some(e) = &self.url_error {
            form_row(ui, "", |ui| error_text(ui, e));
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
        form_row(ui, tr("Network profile"), |ui| {
            let current = self
                .cfg
                .network_profile
                .as_ref()
                .and_then(|id| profiles.iter().find(|(i, _)| i == id))
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| tr("(none — settings below)").to_string());
            egui::ComboBox::from_id_salt("net_profile").icon(kit::combo_caret).width(280.0).selected_text(current).show_ui(ui, |ui| {
                kit::menu_look(ui, 280.0);
                let none = tr("(none — settings below)");
                for (id, name) in std::iter::once((None, none)).chain(profiles.iter().map(|(id, n)| (Some(id), n.as_str()))) {
                    let mut on = self.cfg.network_profile.as_ref() == id;
                    if kit::menu_check(ui, &mut on, name).clicked() {
                        self.cfg.network_profile = id.cloned();
                    }
                }
            });
        });
        if self.cfg.network_profile.is_some() {
            form_row(ui, "", |ui| muted(ui, tr("SSH and proxy settings come from the network profile (Settings → Network profiles).")));
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
        form_row(ui, tr("SSL mode"), |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                let items: Vec<(SslMode, &str)> = SslMode::ALL.iter().map(|m| (*m, m.label())).collect();
                parts::segmented(ui, &mut ssl.mode, &items);
                sub_text(
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
        });
        for (label, field, h) in [
            (tr("CA certificate"), &mut ssl.ca_cert, ""),
            (tr("Client certificate"), &mut ssl.client_cert, tr("optional")),
            (tr("Client key"), &mut ssl.client_key, tr("optional")),
        ] {
            form_row(ui, label, |ui| {
                let w = rest(ui, 36.0);
                opt_field(ui, field, h, w, true);
                if kit::icon_button(ui, icon::FOLDER_OPEN, tr("Choose file")).clicked()
                    && let Some(p) = rfd::FileDialog::new().add_filter("PEM", &["pem", "crt", "cer", "key"]).pick_file()
                {
                    *field = Some(p.display().to_string());
                }
            });
        }
        form_row(ui, "", |ui| ternarygrid_ui::kit::checkbox(ui, &mut ssl.only_custom_ca, tr("Trust only this CA (ignore system certificates)")));
    }

    fn init_page(&mut self, ui: &mut Ui, dialect: Option<std::sync::Arc<dyn ternarygrid_core::Dialect>>) {
        let init = &mut self.cfg.init;
        let gap = 28.0;
        // The columns share the width as in the design (the left one a little wider).
        let left_w = ((ui.available_width() - gap) * 0.513).floor();
        let right_w = ui.available_width() - gap - left_w;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.allocate_ui_with_layout(vec2(left_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(left_w);
                ui.spacing_mut().item_spacing.y = 12.0;
                // 200 wide, short of the right column.
                let schema_w = (left_w + gap - LABEL_W - 16.0 - 6.0).min(200.0);
                form_row(ui, tr("Default schema"), |ui| opt_field(ui, &mut init.default_schema, "", schema_w, false));
                form_row(ui, tr("Keep-alive (s)"), |ui| number_field(ui, "init_keepalive", &mut init.keep_alive_secs, 0..=3600, 90.0));
                form_row(ui, tr("Connect timeout (s)"), |ui| number_field(ui, "init_connect_timeout", &mut init.connect_timeout_secs, 1..=600, 90.0));
                form_row(ui, tr("Query timeout (s)"), |ui| number_field(ui, "init_query_timeout", &mut init.query_timeout_secs, 0..=86_400, 90.0));
                form_row(ui, tr("Fetch size"), |ui| {
                    let mut fs = init.fetch_size.map(|f| f.to_string()).unwrap_or_default();
                    if add_field(ui, text_field(&mut fs, 140.0).hint_text(hint(ui, tr("global setting")))).changed() {
                        init.fetch_size = fs.trim().parse().ok();
                    }
                });
            });
            ui.allocate_ui_with_layout(vec2(right_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(right_w);
                ui.spacing_mut().item_spacing.y = 12.0;
                ternarygrid_ui::kit::checkbox(ui, &mut init.separate_editor_connection, tr("Separate connection for each SQL editor"));
                ternarygrid_ui::kit::checkbox(ui, &mut init.show_system_objects, tr("Show system objects"));
                form_row(ui, tr("Before connect"), |ui| {
                    opt_field(ui, &mut init.shell_before_connect, tr("Shell command"), f32::INFINITY, true).on_hover_text(tr("Shell command before connect"))
                });
                form_row(ui, tr("After disconnect"), |ui| {
                    opt_field(ui, &mut init.shell_after_disconnect, tr("Shell command"), f32::INFINITY, true).on_hover_text(tr("Shell command after disconnect"))
                });
            });
        });
        ui.label(RichText::new(tr("Initialization SQL (run after every connect)")).color(style::pal(ui).text_muted));
        super::code_editor(ui, "init_sql", dialect, &mut init.init_sql, 3);
    }

    fn properties_page(&mut self, ui: &mut Ui) {
        muted(ui, tr("Driver specific properties, e.g. application_name, options, charset."));
        let keys: Vec<String> = self.cfg.properties.keys().cloned().collect();
        let mut remove = None;
        let mut add = false;
        let cols = [super::Column::new(tr("Property"), super::W::Px(220.0)), super::Column::new(tr("Value"), super::W::Fr(1.0)), super::Column::new("", super::W::Px(44.0))];
        let props = &mut self.cfg.properties;
        let new_prop = &mut self.new_prop;
        super::table(ui, "conn_props", &cols, keys.len() + 1, 36.0, f32::INFINITY, |ui, row, col| {
            let Some(row) = row else { return };
            match (keys.get(row), col) {
                (Some(k), 0) => {
                    ui.label(RichText::new(k).monospace().size(12.0));
                }
                (Some(k), 1) => {
                    if let Some(v) = props.get_mut(k) {
                        add_field(ui, text_field(v, f32::INFINITY).font(egui::FontId::monospace(12.5)).min_size(vec2(0.0, 26.0)));
                    }
                }
                (Some(k), _) => {
                    if kit::icon_button_ex(ui, icon::TRASH, tr("Remove"), false, 24.0).clicked() {
                        remove = Some(k.clone());
                    }
                }
                (None, 0) => {
                    add_field(ui, text_field(&mut new_prop.0, f32::INFINITY).min_size(vec2(0.0, 26.0)).hint_text(hint(ui, tr("name"))));
                }
                (None, 1) => {
                    add_field(ui, text_field(&mut new_prop.1, f32::INFINITY).min_size(vec2(0.0, 26.0)).hint_text(hint(ui, tr("value"))));
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
        let used = |f: &NameFilter| f.enabled || !f.include.is_empty() || !f.exclude.is_empty();
        // Databases, routines and other objects: one line until one is used or asked for.
        let more_id = Id::new(("conn_filter_more", &id));
        let more = used(&f.databases) || used(&f.routines) || used(&f.other) || ui.data(|d| d.get_temp::<bool>(more_id)).unwrap_or(false);
        for (i, (label, filter)) in [
            (tr("Schemas"), &mut f.schemas),
            (tr("Tables"), &mut f.tables),
            (tr("Views"), &mut f.views),
            (tr("Databases"), &mut f.databases),
            (tr("Procedures and functions"), &mut f.routines),
            (tr("Other objects"), &mut f.other),
        ]
        .into_iter()
        .enumerate()
        {
            if i >= 3 && !more {
                continue;
            }
            let open_id = Id::new(("conn_filter_open", &id, label));
            let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(true);
            kit::card(ui).corner_radius(CornerRadius::same(style::RADIUS_LG)).inner_margin(Margin::same(12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 10.0;
                // The title row is as tall as the switch (19 points), not a control.
                let control_h = ui.spacing().interact_size.y;
                ui.spacing_mut().interact_size.y = 19.0;
                ui.horizontal(|ui| {
                    let title = ui.painter().layout_no_wrap(label.to_string(), style::medium(ui.ctx(), 13.0), p.text);
                    let (r, resp) = ui.allocate_exact_size(vec2(title.size().x + 20.0, 19.0), Sense::click());
                    resp.widget_info(|| WidgetInfo::selected(WidgetType::CollapsingHeader, true, open, label));
                    let caret = if open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
                    ui.painter().text(egui::pos2(r.left() + 6.0, r.center().y), Align2::CENTER_CENTER, caret, style::icon_font(12.0), p.text_faint);
                    kit::paint_semibold(ui.painter(), egui::pos2(r.left() + 20.0, r.center().y - title.size().y / 2.0), title, p.text);
                    if resp.clicked() {
                        open = !open;
                        ui.data_mut(|d| d.insert_temp(open_id, open));
                    }
                    if !open && !used(filter) {
                        ui.label(RichText::new(tr("(no filter)")).size(12.0).color(p.text_faint));
                    }
                    // The switch at the right edge (laid out left to right: its label after it).
                    let switch_w = 40.0 + text_width(ui, tr("Enable filter"));
                    ui.add_space((ui.available_width() - switch_w).max(0.0));
                    parts::switch(ui, &mut filter.enabled, tr("Enable filter"), None);
                });
                ui.spacing_mut().interact_size.y = control_h;
                if open {
                    ui.horizontal(|ui| {
                        let w = ((ui.available_width() - 8.0) / 2.0).floor();
                        let mut inc = filter.include.join(", ");
                        let mut exc = filter.exclude.join(", ");
                        if add_field(ui, text_field(&mut inc, w).font(egui::FontId::monospace(12.5)).hint_text(mono_hint(ui, tr("Include, e.g. app_*"))))
                            .on_hover_text(tr("Include"))
                            .changed()
                        {
                            filter.include = split_patterns(&inc);
                        }
                        if add_field(ui, text_field(&mut exc, w).font(egui::FontId::monospace(12.5)).hint_text(mono_hint(ui, tr("Exclude, e.g. *_old, tmp_*"))))
                            .on_hover_text(tr("Exclude"))
                            .changed()
                        {
                            filter.exclude = split_patterns(&exc);
                        }
                    });
                }
            });
        }
        if !more {
            ui.horizontal(|ui| {
                let names = [tr("Databases"), tr("Procedures and functions"), tr("Other objects")].join(" · ");
                let r = ui.add(egui::Label::new(RichText::new(names).size(12.5).color(p.text_muted)).sense(Sense::click())).on_hover_text(tr("Show these filters"));
                ui.label(RichText::new(tr("(no filter)")).size(12.0).color(p.text_faint));
                if r.clicked() {
                    ui.data_mut(|d| d.insert_temp(more_id, true));
                }
            });
        }
    }

    fn general_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        form_row(ui, tr("Folder"), |ui| {
            let mut folder = self.cfg.folder.clone().unwrap_or_default();
            let w = rest(ui, 52.0);
            if add_field(ui, text_field(&mut folder, w).hint_text(hint(ui, tr("none")))).changed() {
                self.cfg.folder = if folder.trim().is_empty() { None } else { Some(folder.trim().trim_matches('/').to_string()) };
            }
            egui::ComboBox::from_id_salt("folders").icon(kit::combo_caret).selected_text("").width(44.0).show_ui(ui, |ui| {
                kit::menu_look(ui, 220.0);
                for f in cx.conns.folders() {
                    let mut on = self.cfg.folder.as_deref() == Some(f);
                    if kit::menu_check(ui, &mut on, f).clicked() {
                        self.cfg.folder = Some(f.clone());
                    }
                }
            });
        });
        form_row(ui, tr("Description"), |ui| {
            let mut d = self.cfg.description.clone().unwrap_or_default();
            let edit = egui::TextEdit::multiline(&mut d)
                .margin(Margin::symmetric(13, 6))
                .min_size(vec2(0.0, 56.0))
                .vertical_align(Align::Center)
                .desired_rows(2)
                .desired_width(f32::INFINITY);
            if add_field(ui, edit).changed() {
                self.cfg.description = if d.is_empty() { None } else { Some(d) };
            }
        });
        form_row(ui, tr("Colour"), |ui| {
            let mut use_color = self.cfg.color.is_some();
            // The check box centred on the swatch.
            ui.set_min_height(28.0);
            if ternarygrid_ui::kit::checkbox(ui, &mut use_color, tr("Custom colour")).changed() {
                self.cfg.color = if use_color { Some([120, 160, 220]) } else { None };
            }
            if let Some(c) = &mut self.cfg.color {
                ui.spacing_mut().interact_size = vec2(28.0, 28.0);
                ui.color_edit_button_srgb(c);
            }
        });
        form_row(ui, tr("Navigator view"), |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            kit::radio_value(ui, &mut self.cfg.navigator_view, NavigatorView::Advanced, tr("Advanced"));
            kit::radio_value(ui, &mut self.cfg.navigator_view, NavigatorView::Simple, tr("Simple"));
        });
    }
}

fn split_patterns(s: &str) -> Vec<String> {
    s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// The height of an entry of the database list: the design's 30 as its page renders them,
/// squeezed to fit the list (CSS `flex-shrink`), 1 apart.
const KIND_ROW_H: f32 = 23.5;

/// A caption of the database list, `h` high, its capitals 7.5 above the row under it.
fn list_caption(ui: &mut Ui, text: &str, h: f32) {
    let p = style::pal(ui);
    let g = ui.painter().layout_no_wrap(text.to_uppercase(), style::medium(ui.ctx(), 11.0), p.text_faint);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    kit::paint_semibold(ui.painter(), egui::pos2(rect.left() + 10.0, rect.bottom() - 17.5), g, p.text_faint);
}

/// One entry of the database list: logo and name, the chosen one on a soft accent.
fn kind_row(ui: &mut Ui, kind: DbKind, name: &str, selected: bool) -> egui::Response {
    let p = style::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), KIND_ROW_H), Sense::click());
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
    ui.label(RichText::new(tr("Port")).size(12.5).color(style::pal(ui).text_muted));
    number_field(ui, (id, "port"), port, 1..=65535, 80.0);
}

/// SSH tunnel settings form (connection dialog and network profiles).
pub fn ssh_form(ui: &mut Ui, id: &str, ssh: &mut SshConfig, password: &mut String, passphrase: &mut String) {
    let narrow = |ui: &Ui| 280f32.min(rest(ui, 0.0));
    form_row(ui, "", |ui| parts::switch(ui, &mut ssh.enabled, tr("Use SSH tunnel"), None));
    // Off: only the switch (the settings stay and come back when it is turned on).
    if !ssh.enabled {
        return;
    }
    ui.scope(|ui| {
        form_row(ui, tr("SSH host"), |ui| host_port(ui, id, &mut ssh.host, &mut ssh.port));
        form_row(ui, tr("User"), |ui| {
            let w = narrow(ui);
            add_field(ui, text_field(&mut ssh.user, w))
        });
        form_row(ui, tr("Authentication"), |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            let is_pw = matches!(ssh.auth, SshAuth::Password);
            let is_key = matches!(ssh.auth, SshAuth::PublicKey { .. });
            let is_agent = matches!(ssh.auth, SshAuth::Agent);
            if kit::radio(ui, is_pw, tr("Password")).clicked() {
                ssh.auth = SshAuth::Password;
            }
            if kit::radio(ui, is_key, tr("Private key")).clicked() && !is_key {
                ssh.auth = SshAuth::PublicKey { key_path: default_key_path() };
            }
            if kit::radio(ui, is_agent, tr("SSH agent")).clicked() {
                ssh.auth = SshAuth::Agent;
            }
        });
        match &mut ssh.auth {
            SshAuth::Password => {
                form_row(ui, tr("Password"), |ui| {
                    let w = narrow(ui);
                    add_field(ui, text_field(password, w).password(true))
                });
            }
            SshAuth::PublicKey { key_path } => {
                form_row(ui, tr("Private key file"), |ui| {
                    let w = rest(ui, 36.0);
                    add_field(ui, text_field(key_path, w).font(egui::FontId::monospace(12.5)));
                    if kit::icon_button(ui, icon::FOLDER_OPEN, tr("Choose file")).clicked()
                        && let Some(p) = rfd::FileDialog::new().pick_file()
                    {
                        *key_path = p.display().to_string();
                    }
                });
                form_row(ui, tr("Passphrase"), |ui| {
                    let w = narrow(ui);
                    add_field(ui, text_field(passphrase, w).password(true))
                });
            }
            SshAuth::Agent => {}
        }
        form_row(ui, "", |ui| ternarygrid_ui::kit::checkbox(ui, &mut ssh.save_credentials, tr("Save credentials")));
        form_row(ui, tr("Local port"), |ui| {
            let mut lp = ssh.local_port.map(|p| p.to_string()).unwrap_or_default();
            if add_field(ui, text_field(&mut lp, 140.0).hint_text(hint(ui, tr("automatic")))).changed() {
                ssh.local_port = lp.trim().parse().ok();
            }
        });
        form_row(ui, tr("Keep-alive (s)"), |ui| number_field(ui, (id, "keepalive"), &mut ssh.keepalive_secs, 0..=3600, 90.0));
        form_row(ui, tr("Host key check"), |ui| {
            egui::ComboBox::from_id_salt((id, "hostkey")).icon(kit::combo_caret).width(240.0).selected_text(host_key_label(ssh.host_key_policy)).show_ui(ui, |ui| {
                kit::menu_look(ui, 240.0);
                for p in [HostKeyPolicy::AcceptNew, HostKeyPolicy::Strict, HostKeyPolicy::AcceptAll] {
                    let mut on = ssh.host_key_policy == p;
                    if kit::menu_check(ui, &mut on, host_key_label(p)).clicked() {
                        ssh.host_key_policy = p;
                    }
                }
            });
        });
    });
}

/// Proxy settings form (connection dialog and network profiles).
pub fn proxy_form(ui: &mut Ui, id: &str, proxy: &mut ProxyConfig, password: &mut String) {
    let narrow = |ui: &Ui| 280f32.min(rest(ui, 0.0));
    form_row(ui, "", |ui| parts::switch(ui, &mut proxy.enabled, tr("Use proxy"), None));
    if !proxy.enabled {
        return;
    }
    ui.scope(|ui| {
        form_row(ui, tr("Type"), |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            kit::radio_value(ui, &mut proxy.kind, ProxyKind::Socks5, "SOCKS5");
            kit::radio_value(ui, &mut proxy.kind, ProxyKind::Http, "HTTP CONNECT");
        });
        form_row(ui, tr("Proxy host"), |ui| host_port(ui, id, &mut proxy.host, &mut proxy.port));
        form_row(ui, tr("User"), |ui| {
            let w = narrow(ui);
            opt_field(ui, &mut proxy.user, "", w, false)
        });
        form_row(ui, tr("Password"), |ui| {
            let w = narrow(ui);
            add_field(ui, text_field(password, w).password(true))
        });
        form_row(ui, "", |ui| ternarygrid_ui::kit::checkbox(ui, &mut proxy.save_credentials, tr("Save credentials")));
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
