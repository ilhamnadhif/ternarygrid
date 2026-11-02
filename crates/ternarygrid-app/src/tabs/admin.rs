//! Administration views (FR-A1): session manager, lock manager and the
//! server health dashboard. Each view uses its own database session so that
//! a slow catalog query never blocks the navigator or an editor.

use crate::backend::{Job, Loadable};
use crate::command::AppCommand;
use crate::conn::session::SessionHandle;
use crate::cx::AppContext;
use crate::tabs::TabView;
use crate::tabs::dbtools::look::{self, Cell};
use crate::tabs::transfer::wizard::W;
use egui::{Align, Layout, Margin, RichText, Ui};
use egui_phosphor::regular as icon;
use std::time::{Duration, Instant};
use ternarygrid_core::admin::{KillMode, LockList, Metric, SessionList, TabularData};
use ternarygrid_core::value::FormatOptions;
use ternarygrid_core::{DbResult, tr, trf};
use ternarygrid_ui::{kit, parts, style};
use ternarygrid_visual::dashboard::{DashboardLayout, DashboardState, show_dashboard_with};

/// A dedicated session of a connection, opened on first use.
#[derive(Default)]
pub struct AdminSession {
    session: Loadable<SessionHandle>,
    runtime: Option<tokio::runtime::Handle>,
}

impl AdminSession {
    /// The session once open; `None` while connecting (shows a spinner) or
    /// when the connection is closed (shows a connect button).
    pub fn get(&mut self, ui: &mut Ui, cx: &mut AppContext, conn: &str, label: &str) -> Option<SessionHandle> {
        self.get_in(ui, cx, conn, label, None)
    }

    /// The session, connected to `database` (another database of the same
    /// server, e.g. a table of a database shown in the navigator).
    pub fn get_in(&mut self, ui: &mut Ui, cx: &mut AppContext, conn: &str, label: &str, database: Option<&str>) -> Option<SessionHandle> {
        self.session.poll();
        if let Loadable::Loaded(s) = &self.session {
            if !s.is_closed() {
                return Some(s.clone());
            }
            self.session = Loadable::NotLoaded;
        }
        let Some(live) = cx.conns.live(conn) else {
            kit::empty_state(ui, icon::PLUGS, tr("The connection is closed."), tr("Connect to see the server's state."), |ui| {
                if kit::primary_icon(ui, icon::PLUGS_CONNECTED, tr("Connect")).clicked() {
                    cx.commands.push(AppCommand::Connect(conn.to_string()));
                }
            });
            return None;
        };
        match &self.session {
            Loadable::NotLoaded => {
                self.runtime = Some(cx.backend.handle());
                let database = database.filter(|_| live.multiple_databases()).map(str::to_string);
                self.session = Loadable::Loading(live.open_session_job(cx.backend, format!("{} ({label})", live.config.name), database));
            }
            Loadable::Failed(e) => {
                let text = e.full_text();
                egui::Frame::new().inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
                    look::error(ui, &text);
                    ui.add_space(8.0);
                    if kit::secondary(ui, Some(icon::ARROW_CLOCKWISE), tr("Retry")).clicked() {
                        self.session = Loadable::NotLoaded;
                    }
                });
                return None;
            }
            _ => {}
        }
        egui::Frame::new().inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| parts::spinner_label(ui, tr("Opening a session…")));
        None
    }
}

impl Drop for AdminSession {
    fn drop(&mut self) {
        if let (Loadable::Loaded(s), Some(rt)) = (&self.session, &self.runtime) {
            let s = s.clone();
            rt.spawn(async move { s.close().await });
        }
    }
}

fn cells(data: &TabularData, fmt: &FormatOptions) -> Vec<Vec<String>> {
    data.rows.iter().map(|r| r.iter().map(|v| v.display(fmt).into_owned()).collect()).collect()
}

/// Shared refresh controls: manual refresh plus an auto-refresh interval.
struct Refresh {
    interval: u32,
    last: Option<Instant>,
}

impl Refresh {
    fn ui(&mut self, ui: &mut Ui) -> bool {
        // The toolbar's pickers are 28 high.
        ui.spacing_mut().interact_size.y = 28.0;
        let mut now = look::small(ui, kit::Kind::Secondary, icon::ARROW_CLOCKWISE, tr("Refresh")).clicked();
        egui::ComboBox::from_id_salt(ui.next_auto_id())
            .icon(kit::combo_caret)
            .width(120.0)
            .selected_text(if self.interval == 0 { tr("Auto-refresh off").to_string() } else { trf("Every {} s", &[&self.interval]) })
            .show_ui(ui, |ui| {
                kit::menu_look(ui, 120.0);
                for s in [0u32, 1, 2, 5, 10, 30, 60] {
                    let label = if s == 0 { tr("Off").to_string() } else { trf("{} s", &[&s]) };
                    if crate::tabs::transfer::wizard::choice(ui, self.interval == s, &label) {
                        self.interval = s;
                    }
                }
            });
        if self.interval > 0 {
            let due = self.last.is_none_or(|t| t.elapsed() >= Duration::from_secs(self.interval as u64));
            now |= due;
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        if self.last.is_none() {
            now = true;
        }
        now
    }
}

/// A false-like value of a "granted" column.
fn is_false(v: &str) -> bool {
    matches!(v.to_lowercase().as_str(), "false" | "f" | "0" | "no" | "☐")
}

/// How a cell of an administration table is drawn, from its column's name.
fn styled(column: &str, text: String, p: &style::Palette, null_text: &str) -> Cell {
    let c = column.to_ascii_lowercase();
    // Nothing to show (no wait event, no client address): an empty cell as in the design.
    if text == null_text {
        return Cell::new("");
    }
    if c == "pid" || c == "id" || c.ends_with("_id") || c == "spid" || c == "session_id" || c.contains("duration") || c == "query" || c == "sql" || c == "info" || c == "mode" {
        return Cell::new(text).mono();
    }
    if c.contains("addr") || c == "host" || c == "client" {
        return Cell::new(text).mono().color(p.text_muted);
    }
    if c.starts_with("wait") {
        return Cell::new(text).color(p.text_muted);
    }
    Cell::new(text)
}

/// The design's width of a column of the session and lock lists (`None`: a share of the rest).
fn column_width(column: &str) -> W {
    match column.to_ascii_lowercase().as_str() {
        "pid" => W::Px(70.0),
        "user" | "database" | "granted" | "blocked_by" => W::Px(90.0),
        "application_name" | "locktype" => W::Px(120.0),
        "client_addr" => W::Px(100.0),
        "state" => W::Px(140.0),
        "wait_event_type" | "relation" | "query_duration" => W::Px(110.0),
        "mode" => W::Px(160.0),
        _ => W::Fr(1.0),
    }
}

/// The colour of a session state.
fn state_color(state: &str, p: &style::Palette) -> egui::Color32 {
    let s = state.to_lowercase();
    if s == "active" || s == "running" || s == "query" || s == "execute" {
        p.ok
    } else if s.starts_with("idle in transaction") {
        p.warn
    } else {
        p.text_muted
    }
}

/// A table that is loading, failed, or loaded: the spinner, the error with Retry, or `false`.
fn load_note<T>(ui: &mut Ui, data: &Loadable<T>, shown: bool, retry: &mut bool) {
    match data {
        Loadable::Failed(e) => {
            look::error(ui, &e.full_text());
            ui.add_space(8.0);
            if kit::secondary(ui, Some(icon::ARROW_CLOCKWISE), tr("Retry")).clicked() {
                *retry = true;
            }
        }
        Loadable::NotLoaded | Loadable::Loading(_) if !shown => {
            parts::spinner_label(ui, tr("Loading…"));
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        _ => {}
    }
}

/// The confirmation of Cancel query / Terminate session.
fn kill_dialog(ui: &Ui, id: egui::Id, session: &str, mode: KillMode) -> (bool, bool) {
    let (title, glyph, explain) = match mode {
        KillMode::Terminate => (tr("Terminate session"), icon::X_CIRCLE, tr("The client is disconnected and its open transaction is rolled back.")),
        KillMode::CancelQuery => (tr("Cancel query"), icon::STOP, tr("The running statement is cancelled; the session stays connected.")),
    };
    let question = trf("Session {} — continue?", &[&session]);
    let (mut yes, mut no) = (false, false);
    let (resp, _) = parts::Dialog::new(title).icon(glyph).width(440.0).body_padding(egui::Margin::symmetric(20, 18)).show_modal(
        ui.ctx(),
        id,
        |ui| {
            let p = style::pal(ui);
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(RichText::new(&question).size(13.5));
            ui.label(RichText::new(explain).size(12.5).color(p.text_muted));
        },
        |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if kit::button_ex(ui, kit::Kind::DangerFilled, None, tr("Yes"), false).clicked() {
                    yes = true;
                }
                if kit::button_ex(ui, kit::Kind::Ghost, None, tr("No"), false).clicked() {
                    no = true;
                }
            });
        },
    );
    (yes, no || resp.close)
}

// --------------------------------------------------------------- sessions

pub struct SessionManagerTab {
    conn: String,
    admin: AdminSession,
    data: Loadable<SessionList>,
    /// The last list, shown while the next one loads.
    shown: Option<SessionList>,
    refresh: Refresh,
    /// Id of the selected session (an index would move when the list refreshes).
    selected: Option<String>,
    filter: String,
    hide_idle: bool,
    kill: Option<(String, KillMode)>,
    kill_job: Option<Job<DbResult<()>>>,
}

impl SessionManagerTab {
    pub fn new(conn: String) -> Self {
        SessionManagerTab {
            conn,
            admin: AdminSession::default(),
            data: Loadable::NotLoaded,
            shown: None,
            refresh: Refresh { interval: 5, last: None },
            selected: None,
            filter: String::new(),
            hide_idle: false,
            kill: None,
            kill_job: None,
        }
    }

    /// Show `list` as if the server had sent it (no refresh for an hour), with the session
    /// `selected` and, with `kill`, its Terminate confirmation.
    #[doc(hidden)]
    pub fn show_sample(&mut self, list: SessionList, hide_idle: bool, selected: Option<&str>, kill: bool) {
        self.data = Loadable::Loaded(list.clone());
        self.shown = Some(list);
        self.refresh.last = Some(Instant::now() + Duration::from_secs(3600));
        self.hide_idle = hide_idle;
        self.selected = selected.map(str::to_string);
        self.kill = selected.filter(|_| kill).map(|s| (s.to_string(), KillMode::Terminate));
    }
}

impl TabView for SessionManagerTab {
    fn title(&self) -> String {
        tr("Sessions").to_string()
    }
    fn icon(&self) -> &'static str {
        icon::USERS
    }
    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }
    fn dedupe_key(&self) -> Option<String> {
        Some(format!("sessions:{}", self.conn))
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let conn = self.conn.clone();
        let Some(session) = self.admin.get(ui, cx, &conn, "sessions") else { return };
        if self.data.poll()
            && let Loadable::Loaded(l) = &self.data
        {
            self.shown = Some(l.clone());
        }
        if let Some(j) = self.kill_job.as_mut()
            && let Some(r) = j.poll()
        {
            self.kill_job = None;
            match r {
                Ok(()) => cx.toasts.success(tr("Done")),
                Err(e) => cx.toasts.error(tr("Cannot end the session"), Some(e.full_text())),
            }
            self.refresh.last = None;
        }
        let p = style::pal(ui);
        let fmt = cx.settings.results.format.clone();
        // The visible rows of the last list.
        let mut index = Vec::new();
        let mut rows: Vec<Vec<String>> = Vec::new();
        if let Some(list) = &self.shown {
            let filter = self.filter.to_lowercase();
            for (i, r) in cells(&list.data, &fmt).into_iter().enumerate() {
                let matches = filter.is_empty() || r.iter().any(|c| c.to_lowercase().contains(&filter));
                // As in the design, sessions idle in a transaction stay (they hold locks).
                let idle = list.state_column.and_then(|c| r.get(c)).is_some_and(|s| s.eq_ignore_ascii_case("idle") || s.eq_ignore_ascii_case("sleep"));
                if matches && !(self.hide_idle && idle) {
                    index.push(i);
                    rows.push(r);
                }
            }
        }
        let row_id = |list: &SessionList, i: usize| list.data.rows.get(i).and_then(|r| r.get(list.id_column)).map(|v| v.to_text().into_owned());
        let blocked = self
            .shown
            .as_ref()
            .and_then(|l| l.blocked_by_column)
            .map(|c| rows.iter().filter(|r| r.get(c).is_some_and(|v| !v.is_empty() && v != &fmt.null_text)).count())
            .unwrap_or(0);
        let sel_id = match (&self.shown, &self.selected) {
            (Some(l), Some(id)) => l.data.rows.iter().any(|r| r.get(l.id_column).is_some_and(|v| v.to_text() == id.as_str())).then(|| id.clone()),
            _ => None,
        };
        ui.spacing_mut().item_spacing.y = 0.0;
        parts::toolbar(ui, |ui| {
            let now = self.refresh.ui(ui);
            if now && !self.data.is_loading() {
                self.refresh.last = Some(Instant::now());
                let s = session.clone();
                self.data = Loadable::Loading(cx.backend.spawn(async move { s.lock().await.list_sessions().await }));
            }
            kit::search_field(ui, egui::Id::new(("sessions_filter", &self.conn)), &mut self.filter, tr("Filter…"), 200.0);
            look::checkbox(ui, &mut self.hide_idle, tr("Hide idle"));
            kit::vdivider(ui, 18.0);
            ui.add_enabled_ui(sel_id.is_some(), |ui| {
                if look::small(ui, kit::Kind::Ghost, icon::STOP, tr("Cancel query")).clicked() {
                    self.kill = sel_id.clone().map(|i| (i, KillMode::CancelQuery));
                }
                if look::small(ui, kit::Kind::Danger, icon::X_CIRCLE, tr("Terminate session")).clicked() {
                    self.kill = sel_id.clone().map(|i| (i, KillMode::Terminate));
                }
            });
            if self.shown.is_some() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(trf("{} sessions, {} waiting for a lock", &[&rows.len(), &blocked])).size(12.0).color(p.text_faint));
                });
            }
        });
        let mut retry = false;
        egui::Frame::new().inner_margin(Margin::symmetric(16, 10)).show(ui, |ui| {
            load_note(ui, &self.data, self.shown.is_some(), &mut retry);
            let Some(list) = &self.shown else { return };
            let headers = look::headers(&list.data.columns.iter().map(String::as_str).collect::<Vec<_>>());
            let styled_rows: Vec<Vec<Cell>> = rows
                .iter()
                .map(|r| {
                    r.iter()
                        .enumerate()
                        .map(|(c, v)| {
                            let name = list.data.columns.get(c).map(String::as_str).unwrap_or("");
                            if Some(c) == list.state_column {
                                Cell::new(v.clone()).color(state_color(v, &p))
                            } else if Some(c) == list.blocked_by_column && !v.is_empty() && v != &fmt.null_text {
                                Cell::new(v.clone()).mono().color(p.err)
                            } else if Some(c) == list.blocked_by_column || (Some(c) == list.query_column && v == &fmt.null_text) {
                                Cell::new(if v == &fmt.null_text { String::new() } else { v.clone() })
                            } else if c == list.id_column || Some(c) == list.query_column {
                                Cell::new(v.clone()).mono()
                            } else {
                                styled(name, v.clone(), &p, &fmt.null_text)
                            }
                        })
                        .collect()
                })
                .collect();
            let selected_visible = self.selected.as_ref().and_then(|s| index.iter().position(|i| row_id(list, *i).as_ref() == Some(s)));
            let widths: Vec<W> = list.data.columns.iter().map(|c| column_width(c)).collect();
            let r = look::table_w(ui, ("sessions", &self.conn), &headers, &styled_rows, selected_visible, &widths);
            if let Some(i) = r.clicked.or(r.context_row) {
                self.selected = index.get(i).and_then(|i| row_id(list, *i));
            }
            if let Some(i) = r.double_clicked
                && let (Some(q), Some(row)) = (list.query_column, index.get(i).and_then(|i| list.data.rows.get(*i)))
            {
                let sql = row.get(q).map(|v| v.to_text().into_owned()).unwrap_or_default();
                if !sql.trim().is_empty() {
                    cx.push(AppCommand::NewSqlEditor { connection: Some(self.conn.clone()), schema: None, text: Some(sql) });
                }
            }
        });
        if retry {
            self.refresh.last = None;
        }
        if let Some((id, mode)) = self.kill.clone() {
            let (yes, close) = kill_dialog(ui, egui::Id::new(("kill", &self.conn)), &id, mode);
            if yes {
                let s = session.clone();
                let id = id.clone();
                self.kill_job = Some(cx.backend.spawn(async move { s.lock().await.kill_session(&id, mode).await }));
            }
            if yes || close {
                self.kill = None;
            }
        }
    }

    crate::impl_tab_any!();
}

// ------------------------------------------------------------------ locks

pub struct LockManagerTab {
    conn: String,
    admin: AdminSession,
    data: Loadable<LockList>,
    /// The last list, shown while the next one loads.
    shown: Option<LockList>,
    refresh: Refresh,
    only_waiting: bool,
}

impl LockManagerTab {
    pub fn new(conn: String) -> Self {
        LockManagerTab { conn, admin: AdminSession::default(), data: Loadable::NotLoaded, shown: None, refresh: Refresh { interval: 5, last: None }, only_waiting: false }
    }

    /// Show `list` as if the server had sent it (no refresh for an hour).
    #[doc(hidden)]
    pub fn show_sample(&mut self, list: LockList, only_waiting: bool) {
        self.data = Loadable::Loaded(list.clone());
        self.shown = Some(list);
        self.refresh.last = Some(Instant::now() + Duration::from_secs(3600));
        self.only_waiting = only_waiting;
    }
}

impl TabView for LockManagerTab {
    fn title(&self) -> String {
        tr("Locks").to_string()
    }
    fn icon(&self) -> &'static str {
        icon::LOCK
    }
    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }
    fn dedupe_key(&self) -> Option<String> {
        Some(format!("locks:{}", self.conn))
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let conn = self.conn.clone();
        let Some(session) = self.admin.get(ui, cx, &conn, "locks") else { return };
        if self.data.poll()
            && let Loadable::Loaded(l) = &self.data
        {
            self.shown = Some(l.clone());
        }
        let p = style::pal(ui);
        let fmt = cx.settings.results.format.clone();
        let rows: Vec<Vec<String>> = match &self.shown {
            Some(list) => cells(&list.data, &fmt)
                .into_iter()
                .filter(|r| {
                    !self.only_waiting
                        || list.granted_column.and_then(|c| r.get(c)).is_some_and(|g| is_false(g))
                        || list.blocked_by_column.and_then(|c| r.get(c)).is_some_and(|b| !b.is_empty() && b != &fmt.null_text)
                })
                .collect(),
            None => Vec::new(),
        };
        ui.spacing_mut().item_spacing.y = 0.0;
        parts::toolbar(ui, |ui| {
            if self.refresh.ui(ui) && !self.data.is_loading() {
                self.refresh.last = Some(Instant::now());
                let s = session.clone();
                self.data = Loadable::Loading(cx.backend.spawn(async move { s.lock().await.list_locks().await }));
            }
            look::checkbox(ui, &mut self.only_waiting, tr("Only waiting locks"));
            if look::small(ui, kit::Kind::Ghost, icon::USERS, tr("Sessions")).clicked() {
                cx.push(AppCommand::OpenSessionManager { connection: self.conn.clone() });
            }
            if self.shown.is_some() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(trf("{} locks", &[&rows.len()])).size(12.0).color(p.text_faint));
                });
            }
        });
        let mut retry = false;
        egui::Frame::new().inner_margin(Margin::symmetric(16, 10)).show(ui, |ui| {
            load_note(ui, &self.data, self.shown.is_some(), &mut retry);
            let Some(list) = &self.shown else { return };
            let headers = look::headers(&list.data.columns.iter().map(String::as_str).collect::<Vec<_>>());
            let styled_rows: Vec<Vec<Cell>> = rows
                .iter()
                .map(|r| {
                    r.iter()
                        .enumerate()
                        .map(|(c, v)| {
                            let name = list.data.columns.get(c).map(String::as_str).unwrap_or("");
                            if Some(c) == list.granted_column {
                                if is_false(v) { Cell::new(tr("waiting")).color(p.warn) } else { Cell::new("").glyph(icon::CHECK, p.ok).hover(v.clone()) }
                            } else if Some(c) == list.blocked_by_column {
                                if v.is_empty() || v == &fmt.null_text { Cell::new("") } else { Cell::new(v.clone()).mono().color(p.err) }
                            } else if c == list.session_column {
                                Cell::new(v.clone()).mono()
                            } else {
                                styled(name, v.clone(), &p, &fmt.null_text)
                            }
                        })
                        .collect()
                })
                .collect();
            let widths: Vec<W> = list.data.columns.iter().map(|c| column_width(c)).collect();
            look::table_w(ui, ("locks", &self.conn), &headers, &styled_rows, None, &widths);
        });
        if retry {
            self.refresh.last = None;
        }
    }

    crate::impl_tab_any!();
}

// -------------------------------------------------------------- dashboard

pub struct DashboardTab {
    conn: String,
    admin: AdminSession,
    state: DashboardState,
    layout: DashboardLayout,
    job: Option<Job<DbResult<Vec<Metric>>>>,
    started: Instant,
    last: Option<Instant>,
    interval: Duration,
    error: Option<String>,
    paused: bool,
}

impl DashboardTab {
    pub fn new(conn: String) -> Self {
        let layout = DashboardLayout { id: format!("dash-{conn}"), ..Default::default() };
        DashboardTab {
            conn,
            admin: AdminSession::default(),
            state: DashboardState::new(),
            layout,
            job: None,
            started: Instant::now(),
            last: None,
            interval: Duration::from_secs(1),
            error: None,
            paused: false,
        }
    }

    /// Show these samples (seconds, metrics) as if the server had sent them; no polling for
    /// an hour.
    #[doc(hidden)]
    pub fn show_sample(&mut self, samples: &[(f64, Vec<Metric>)]) {
        for (t, m) in samples {
            self.state.push(*t, m);
        }
        self.last = Some(Instant::now() + Duration::from_secs(3600));
        self.job = None;
        self.error = None;
    }
}

impl TabView for DashboardTab {
    fn title(&self) -> String {
        tr("Dashboard").to_string()
    }
    fn icon(&self) -> &'static str {
        icon::CHART_LINE
    }
    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }
    fn dedupe_key(&self) -> Option<String> {
        Some(format!("dashboard:{}", self.conn))
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let conn = self.conn.clone();
        let Some(session) = self.admin.get(ui, cx, &conn, "dashboard") else { return };
        if let Some(j) = self.job.as_mut()
            && let Some(r) = j.poll()
        {
            self.job = None;
            match r {
                Ok(m) => {
                    self.state.push(self.started.elapsed().as_secs_f64(), &m);
                    self.error = None;
                }
                Err(e) => self.error = Some(e.full_text()),
            }
        }
        let due = self.last.is_none_or(|t| t.elapsed() >= self.interval);
        if due && self.job.is_none() && !self.paused && ui.is_visible() {
            self.last = Some(Instant::now());
            let s = session.clone();
            self.job = Some(cx.backend.spawn(async move { s.lock().await.server_metrics().await }));
        }
        let interval = &mut self.interval;
        let error = self.error.clone();
        let r = show_dashboard_with(
            ui,
            &self.state,
            &self.layout,
            |ui| {
                look::muted(ui, tr("Sample every"));
                // "1 s", "0.5 s": no needless decimals; typed as "2" or "2 s".
                let shown = format!("{} s", (interval.as_secs_f32() * 10.0).round() / 10.0);
                crate::tabs::transfer::parsed_line(ui, shown, 70.0, 28.0, |t| {
                    if let Ok(secs) = t.trim().trim_end_matches('s').trim().parse::<f32>() {
                        *interval = Duration::from_secs_f32(secs.clamp(0.5, 60.0));
                    }
                });
                kit::vdivider(ui, 18.0);
            },
            |ui| {
                if let Some(e) = &error {
                    look::error(ui, e);
                }
            },
        );
        self.paused = r.paused;
        if !self.paused {
            ui.ctx().request_repaint_after(self.interval.saturating_sub(self.last.map(|t| t.elapsed()).unwrap_or_default()));
        }
    }

    crate::impl_tab_any!();
}
