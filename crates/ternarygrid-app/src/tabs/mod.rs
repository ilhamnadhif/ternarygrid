//! Central tab area: SQL editors, object editors, diagrams, admin views ...
//! Tabs live in an `egui_dock` tree so they can be moved, split and detached.

pub mod admin;
pub mod ai;
pub mod ai_agent;
pub mod ai_history;
pub mod catalog;
pub mod compare;
pub mod data_compare;
pub mod dbtools;
pub mod er;
pub mod errorlog;
pub mod history;
pub mod object;
pub mod query_builder;
pub mod search;
pub mod settings;
pub mod sql_editor;
pub mod table_designer;
pub mod tasks;
pub mod transfer;
pub mod welcome;

use crate::cx::AppContext;
use egui::{Id, Ui, WidgetText};
use std::any::Any;
use std::time::{Duration, Instant};
use ternarygrid_core::SchemaRef;

/// Live background or foreground process activity reported by a [`TabView`].
#[derive(Debug, Clone, Default, PartialEq)]
pub enum TabActivity {
    /// No active process, pending prompt, or unacknowledged error.
    #[default]
    Idle,
    /// A process is running (`progress` is `Some(0.0..=1.0)` when determinate).
    Running { detail: Option<String>, progress: Option<f32> },
    /// Waiting on user action (parameter prompt, dangerous SQL confirm, AI approval).
    Attention { detail: String },
    /// Most recent operation failed (stays visible until resolved or re-run).
    Error { detail: String },
    /// Most recent operation finished with warnings or partial/truncated results.
    Warning { detail: String },
    /// Most recent operation was cancelled by the user.
    Cancelled { detail: String },
}

/// Visual icon painted in the tab's 14×14 left icon slot (Opsi A — Pure Logo Only).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum TabBadge {
    /// Normal object icon.
    #[default]
    None,
    /// Animated teal arc spinner (indeterminate running).
    Running,
    /// Determinate circular progress ring (`0.0..=1.0`).
    Progress(f32),
    /// Green check-circle: finished in the background (auto-clears on visit).
    Done,
    /// Red error-circle: failed / error.
    Error,
    /// Amber warning triangle: warning / partial / truncated.
    Warning,
    /// Amber exclamation-circle: waiting for confirmation / parameters / approval.
    Attention,
    /// Muted slash-circle: cancelled by user.
    Cancelled,
}

/// Grace period before an indeterminate spinner shows (prevents micro-flicker on sub-250 ms queries).
const SPINNER_DELAY: Duration = Duration::from_millis(250);

/// Single-line compact summary of an error or status message for tab hover tooltips.
pub(crate) fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut chars = line.chars();
    let short: String = chars.by_ref().take(96).collect();
    if chars.next().is_some() { format!("{short}…") } else { short }
}

/// Tracks a tab's live activity, background completion ("unread ✓"), anti-flicker grace period,
/// and hover tooltip detail.
#[derive(Debug, Default)]
pub struct TabStatusTracker {
    activity: TabActivity,
    running_since: Option<Instant>,
    last_running_detail: Option<String>,
    /// Unread background outcome (`Done`, `Warning`, `Cancelled`) + tooltip detail,
    /// cleared automatically as soon as the tab becomes open/active.
    unread: Option<(TabBadge, String)>,
}

impl TabStatusTracker {
    /// Update state from the tab's live [`TabActivity`] and whether the tab is currently open.
    pub fn update(&mut self, activity: TabActivity, is_open: bool, now: Instant) {
        if is_open {
            self.unread = None;
        }
        match &activity {
            TabActivity::Running { detail, .. } => {
                self.running_since.get_or_insert(now);
                if detail.is_some() {
                    self.last_running_detail = detail.clone();
                }
                self.unread = None;
            }
            other => {
                if let Some(started) = self.running_since.take() {
                    let elapsed = now.saturating_duration_since(started);
                    let running_label = self.last_running_detail.take();
                    if !is_open {
                        match other {
                            TabActivity::Idle => {
                                let dur = crate::widgets::format_duration(elapsed);
                                let tip = match running_label {
                                    Some(d) => ternarygrid_core::trf("Completed ({}) · {}", &[&dur, &d.trim_end_matches('…')]),
                                    None => ternarygrid_core::trf("Completed in {}", &[&dur]),
                                };
                                self.unread = Some((TabBadge::Done, tip));
                            }
                            TabActivity::Warning { detail } => {
                                self.unread = Some((TabBadge::Warning, detail.clone()));
                            }
                            TabActivity::Cancelled { detail } => {
                                self.unread = Some((TabBadge::Cancelled, detail.clone()));
                            }
                            TabActivity::Error { .. } | TabActivity::Attention { .. } | TabActivity::Running { .. } => {}
                        }
                    }
                }
            }
        }
        self.activity = activity;
    }

    /// Current icon badge to render in the tab's left icon slot.
    pub fn badge(&self, now: Instant) -> TabBadge {
        match &self.activity {
            TabActivity::Running { progress: Some(p), .. } => TabBadge::Progress(p.clamp(0.0, 1.0)),
            TabActivity::Running { progress: None, .. } => {
                let elapsed = self.running_since.map(|t| now.saturating_duration_since(t)).unwrap_or_default();
                if elapsed >= SPINNER_DELAY { TabBadge::Running } else { TabBadge::None }
            }
            TabActivity::Attention { .. } => TabBadge::Attention,
            TabActivity::Error { .. } => TabBadge::Error,
            TabActivity::Warning { .. } => TabBadge::Warning,
            TabActivity::Cancelled { .. } => TabBadge::Cancelled,
            TabActivity::Idle => self.unread.as_ref().map_or(TabBadge::None, |(b, _)| *b),
        }
    }

    /// Optional status summary appended to the tab's hover tooltip.
    pub fn tooltip_line(&self, now: Instant) -> Option<String> {
        match &self.activity {
            TabActivity::Running { detail, progress } => {
                let elapsed = self.running_since.map(|t| now.saturating_duration_since(t)).unwrap_or_default();
                let label = detail.as_deref().unwrap_or_else(|| ternarygrid_core::tr("Running…"));
                if let Some(p) = progress {
                    let pct = (p.clamp(0.0, 1.0) * 100.0).round() as u32;
                    Some(format!("{label} ({pct}%)"))
                } else if elapsed >= Duration::from_secs(1) {
                    Some(format!("{label} ({})", crate::widgets::format_duration(elapsed)))
                } else {
                    Some(label.to_string())
                }
            }
            TabActivity::Attention { detail } | TabActivity::Error { detail } | TabActivity::Warning { detail } | TabActivity::Cancelled { detail } => Some(detail.clone()),
            TabActivity::Idle => self.unread.as_ref().map(|(_, tip)| tip.clone()),
        }
    }

    /// Repaint delay needed by the tab status icon (`None` when completely static/idle).
    pub fn repaint_after(&self, now: Instant) -> Option<Duration> {
        match &self.activity {
            TabActivity::Running { progress: Some(_), .. } => Some(Duration::from_millis(80)),
            TabActivity::Running { progress: None, .. } => {
                let elapsed = self.running_since.map(|t| now.saturating_duration_since(t)).unwrap_or_default();
                if elapsed >= SPINNER_DELAY { Some(Duration::from_millis(33)) } else { Some(SPINNER_DELAY - elapsed) }
            }
            _ => None,
        }
    }
}

/// Behaviour shared by every tab kind.
pub trait TabView: Any {
    /// Title shown on the tab button.
    fn title(&self) -> String;

    /// Phosphor icon glyph.
    fn icon(&self) -> &'static str {
        egui_phosphor::regular::FILE
    }

    /// Tooltip for the tab button.
    fn tooltip(&self) -> Option<String> {
        None
    }

    /// Connection this tab belongs to (status bar, colour marker, palette).
    fn connection_id(&self) -> Option<&str> {
        None
    }

    /// Database and schema the tab works in, as far as it knows them (the
    /// connection's current ones fill the gaps, see [`tab_location`]).
    fn schema_context(&self) -> Option<SchemaRef> {
        None
    }

    /// Stable key used to focus an existing tab instead of opening a duplicate
    /// (e.g. "object:<conn>:<schema>.<table>").
    fn dedupe_key(&self) -> Option<String> {
        None
    }

    /// The database object this tab shows (Link with editor).
    fn navigator_object(&self) -> Option<(String, ternarygrid_core::ObjectRef)> {
        None
    }

    /// Whether the tab can run `action` now (toolbar buttons are disabled
    /// otherwise); `None` when the tab does not handle the action.
    fn action_enabled(&self, action: crate::keys::Action) -> Option<bool> {
        let _ = action;
        None
    }

    /// The session of the tab's own transaction (Pending transactions).
    fn transaction_session(&self) -> Option<crate::conn::session::SessionHandle> {
        None
    }

    /// The result the tab shows, for the status bar ("1,204 rows · 42 ms").
    fn status_summary(&self) -> Option<String> {
        None
    }

    /// Unsaved changes (editor text, pending data edits).
    fn is_dirty(&self) -> bool {
        false
    }

    /// Live process / attention / error state shown in the tab's icon slot.
    fn tab_activity(&self) -> TabActivity {
        TabActivity::Idle
    }

    /// Poll already-started background jobs while the tab is inactive so processes finish
    /// and update the tab's status icon without requiring the user to switch back.
    fn poll_background(&mut self, _ctx: &egui::Context, _cx: &mut AppContext) {}

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext);

    /// Handle an action (shortcut, menu, palette) routed to the active tab.
    /// Return true if handled.
    fn on_action(&mut self, action: crate::keys::Action, cx: &mut AppContext) -> bool {
        let _ = (action, cx);
        false
    }

    /// Which part of the tab had keyboard focus last frame (selects the
    /// shortcut context: SQL editor, results grid ...).
    fn focus_context(&self) -> crate::keys::ActionContext {
        crate::keys::ActionContext::Global
    }

    /// True while the tab itself is capturing raw keystrokes (e.g. shortcut
    /// recorder in Settings), so global and tab shortcuts must not fire.
    fn captures_keyboard(&self) -> bool {
        false
    }

    /// Opens in a pop-up window instead of a tab (wizards and one-off actions: export,
    /// import, backup …) of this size: the design's window inside its 1-point border (the
    /// modal's CSS width × height, header included); the window adds the border.
    fn popup_size(&self) -> Option<egui::Vec2> {
        None
    }

    /// The connection a pop-up writes to, when it is not its own (a migration's target): the
    /// header then reads "source → target".
    fn popup_target(&self) -> Option<&str> {
        None
    }

    /// True once after its job finished successfully: a pop-up then closes
    /// (the result was reported by a toast). Failures keep it open.
    fn take_done(&mut self) -> bool {
        false
    }

    /// The tab asks about its unsaved changes itself in [`on_close`](Self::on_close);
    /// for the others the application asks before closing a dirty tab.
    fn prompts_on_close(&self) -> bool {
        false
    }

    /// Called before closing; return false to keep the tab open (e.g. to ask
    /// about unsaved changes — the tab then shows its own confirmation).
    fn on_close(&mut self, cx: &mut AppContext) -> bool {
        let _ = cx;
        true
    }

    /// State persisted to restore the tab on the next start (FR-S1).
    fn session_state(&self) -> Option<serde_json::Value> {
        None
    }

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// A tab in the dock tree.
pub struct Tab {
    pub id: u64,
    pub view: Box<dyn TabView>,
    /// The user chose to discard its unsaved changes: close without asking.
    pub force_close: bool,
    /// Process / background-completion status rendered in the tab's left icon slot.
    pub status: TabStatusTracker,
}

impl Tab {
    pub fn new(id: u64, view: Box<dyn TabView>) -> Self {
        Tab { id, view, force_close: false, status: TabStatusTracker::default() }
    }

    /// Sync [`Self::status`] from the view's live activity.
    pub fn sync_status(&mut self, is_open: bool, now: Instant) {
        let activity = self.view.tab_activity();
        self.status.update(activity, is_open, now);
    }

    /// Closing it would lose unsaved changes the tab does not ask about itself.
    pub fn needs_close_confirm(&self) -> bool {
        !self.force_close && !self.view.prompts_on_close() && self.view.is_dirty()
    }

    /// Closing it rolls back writes its own session made in an open transaction
    /// (a data tab saved in manual commit), and it does not ask about that itself.
    pub fn close_rolls_back(&self, conns: &crate::conn::ConnectionManager) -> bool {
        !self.force_close && !self.view.prompts_on_close() && self.view.transaction_session().is_some_and(|s| s.has_uncommitted_writes() && !conns.is_shared_session(&s))
    }
}

/// egui_dock adapter that renders tabs with access to the application context.
pub struct DockViewer<'a, 'b> {
    pub cx: &'a mut AppContext<'b>,
    /// Tab ids that the user asked to close but which vetoed (dirty).
    pub close_vetoed: Vec<u64>,
    /// Dirty tabs whose closing the application must confirm.
    pub close_confirm: Vec<u64>,
    pub active_tab: Option<u64>,
    /// For the tab titles (a coloured icon and the title).
    pub style: std::sync::Arc<egui::Style>,
    /// Where each tab's button was drawn (filled while the tabs are shown).
    pub tab_rects: std::collections::HashMap<u64, egui::Rect>,
    /// The open tab of each tab's leaf, as it was before the tabs were shown: a click on a
    /// tab makes it the open one before [`egui_dock::TabViewer::on_tab_button`] runs.
    pub leaf_open: std::collections::HashMap<u64, u64>,
}

/// Height of the tab bar: 41 points and the hairline under it (two lines of text).
pub const TAB_BAR_H: f32 = 42.0;
/// Room at the bar's right end for "+" and the list of tabs while tabs overflow (a 26-point
/// button each, 2 apart, 8 from the edge, 2 from the tabs; the 8 at the end is the bar's own).
pub const TAB_BAR_END: f32 = 56.0;
/// Room kept at the right of a tab's text for its × (8, the 20-point button, 10).
const TAB_CLOSE_ROOM: f32 = 30.0;
/// The tab's icon slot and the gap after it: where the text starts after the leading room.
const TAB_ICON_ROOM: f32 = 22.0;

/// How far to scroll a tab bar (added to its scroll, which is negative or 0) to bring the tab
/// at `rect` between `left` and `right`; the left end wins for a tab wider than that.
pub fn tab_reveal_delta(rect: egui::Rect, left: f32, right: f32) -> f32 {
    if rect.left() < left || rect.width() > right - left {
        left - rect.left()
    } else if rect.right() > right {
        right - rect.right()
    } else {
        0.0
    }
}

/// Where a tab works: its connection and database / schema (main toolbar,
/// status bar, tab tooltip).
pub struct TabLocation {
    pub conn: String,
    pub name: String,
    pub kind: ternarygrid_core::DbKind,
    /// `schema@database` (DBeaver's notation), or the one level the database has.
    pub place: Option<String>,
    /// The database, where databases hold schemas (PostgreSQL, SQL Server).
    pub database: Option<String>,
    /// The schema, or the one level the database has.
    pub schema: Option<String>,
}

pub fn tab_location(view: &dyn TabView, conns: &crate::conn::ConnectionManager) -> Option<TabLocation> {
    location_at(view.connection_id()?, view.schema_context().unwrap_or_default(), conns)
}

/// [`tab_location`] of something working at `at` on the connection `conn`.
pub fn location_at(conn: &str, at: SchemaRef, conns: &crate::conn::ConnectionManager) -> Option<TabLocation> {
    let cfg = conns.config(conn)?;
    let live = conns.live(conn);
    // PostgreSQL, SQL Server: databases hold schemas. Elsewhere one level.
    let two_levels = live.is_some_and(|l| l.multiple_databases());
    let current = live.map(|l| SchemaRef::new(l.current_database.clone(), l.current_schema.clone())).unwrap_or_default();
    let place = place_label(at.clone(), &current, two_levels);
    let (database, schema) = place_parts(at, &current, two_levels);
    Some(TabLocation { conn: conn.to_string(), name: cfg.name.clone(), kind: cfg.kind, place, database, schema })
}

/// Database and schema of `at`, their gaps filled from the session's
/// `current` database / schema; where the database has a single level, that
/// level as the schema.
fn place_parts(mut at: SchemaRef, current: &SchemaRef, two_levels: bool) -> (Option<String>, Option<String>) {
    if two_levels && at.database.is_none() {
        at.database = current.database.clone();
    }
    if at.schema.is_none() && (!two_levels || at.database == current.database) {
        at.schema = current.schema.clone();
    }
    if !two_levels && at.schema.is_none() {
        at.schema = current.database.clone();
    }
    if two_levels { (at.database, at.schema) } else { (None, at.schema.or(at.database)) }
}

/// `schema@database` of `at` (see [`place_parts`]); one name where the
/// database has a single level.
fn place_label(at: SchemaRef, current: &SchemaRef, two_levels: bool) -> Option<String> {
    match place_parts(at, current, two_levels) {
        (Some(d), Some(s)) if s != d => Some(format!("{s}@{d}")),
        (d, s) => s.or(d),
    }
}

impl DockViewer<'_, '_> {
    /// Is the tab the one shown in its leaf?
    fn is_open(&self, id: u64) -> bool {
        self.leaf_open.get(&id) == Some(&id)
    }
}

impl egui_dock::TabViewer for DockViewer<'_, '_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> Id {
        Id::new(("ternarygrid_tab", tab.id))
    }

    /// The tab in two lines, never cut (a tab bar that is too short scrolls): the icon and the
    /// title (the medium weight on the open tab), under it where the tab works, the schema and
    /// the database, so that the same table of two databases can be told apart. Tabs of no
    /// connection have the first line only. Every tab keeps room for its × at the right.
    fn title(&mut self, tab: &mut Tab) -> WidgetText {
        let dirty = tab.view.is_dirty();
        let title = tab.view.title();
        let p = ternarygrid_ui::palette(self.style.visuals.dark_mode);
        let open = self.is_open(tab.id);
        let badge = tab.status.badge(Instant::now());
        let size = self.style.text_styles.get(&egui::TextStyle::Body).map_or(13.0, |f| f.size);
        let format = |font: egui::FontId, color: egui::Color32| egui::TextFormat { font_id: font, color, valign: egui::Align::Center, ..Default::default() };
        let location = tab_location(tab.view.as_ref(), self.cx.conns);
        // An object's icon in its hue on the tab's background; other tabs stay quiet.
        // When a process status badge is active, keep the exact glyph slot transparent so
        // `on_tab_button` can paint the vector logo in place with zero tab-width jump.
        let quiet = if open { p.text_muted } else { p.text_faint };
        let icon_color = if badge != TabBadge::None {
            egui::Color32::TRANSPARENT
        } else {
            location.as_ref().and_then(|_| crate::theme::object_icon_color(tab.view.icon(), if open { p.canvas } else { p.chrome })).unwrap_or(quiet)
        };
        let mut job = egui::text::LayoutJob::default();
        // egui_dock pads the title by 8 points: 6 more on the left (7 inside the open tab's
        // hairline), the icon in a 14-point slot, 8 points, the title, then the × room.
        let lead = if open { 7.0 } else { 6.0 };
        let end = "\u{200b}";
        job.append(tab.view.icon(), lead, format(ternarygrid_ui::style::icon_font(14.0), icon_color));
        let font = if open { egui::FontId::new(size, egui::FontFamily::Name(ternarygrid_ui::style::MEDIUM.into())) } else { egui::FontId::proportional(size) };
        let after_icon = TAB_ICON_ROOM - 14.0 * ternarygrid_ui::style::ICON_SCALE;
        if dirty {
            let dot_font = egui::FontId::proportional((size - 2.5).max(9.0));
            job.append("● ", after_icon, format(dot_font, p.warn));
            job.append(&title, 0.0, format(font, egui::Color32::PLACEHOLDER));
        } else {
            job.append(&title, after_icon, format(font, egui::Color32::PLACEHOLDER));
        }
        let close_room = TAB_CLOSE_ROOM + 1.0;
        job.append(end, close_room, format(egui::FontId::proportional(size), egui::Color32::PLACEHOLDER));
        if let Some(place) = location.and_then(|l| l.place) {
            let small = egui::FontId::proportional(size - 2.0);
            job.append("\n", 0.0, format(small.clone(), quiet));
            job.append(&place, lead + TAB_ICON_ROOM, format(small.clone(), quiet));
            job.append(end, close_room, format(small, quiet));
        }
        job.into()
    }

    /// A tab of a connection whose type has a colour (Production: red) is
    /// tinted in it, as in DBeaver: work on that server stands out.
    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        self.cx.tab_active = self.active_tab == Some(tab.id);
        self.cx.current_tab = Some(tab.id);
        tab.view.ui(ui, self.cx);
        self.cx.current_tab = None;
        // Sync immediately after UI so processes started on this frame are tracked right away.
        tab.sync_status(true, Instant::now());
    }

    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        // The usual pointer over a tab: egui_dock's open hand reads as a link. Dragging one
        // still shows the closed hand.
        if response.hovered() && response.ctx.dragged_id().is_none() {
            response.ctx.set_cursor_icon(egui::CursorIcon::Default);
        }
        let now = Instant::now();
        let loc_info = tab_location(tab.view.as_ref(), self.cx.conns);
        let has_place = loc_info.as_ref().is_some_and(|l| l.place.is_some());
        // The tab's own text, its connection/schema, then any live/unread process detail.
        let location = loc_info.map(|l| match l.place {
            Some(p) => format!("{} · {p}", l.name),
            None => l.name,
        });
        let tip: Vec<String> = tab.view.tooltip().into_iter().chain(location).chain(tab.status.tooltip_line(now)).collect();
        if !tip.is_empty() {
            response.clone().on_hover_text(tip.join("\n"));
        }
        if response.middle_clicked() {
            self.cx.commands.push(crate::command::AppCommand::CloseTab(tab.id));
        }
        self.tab_rects.insert(tab.id, response.rect);
        let p = ternarygrid_ui::palette(self.style.visuals.dark_mode);
        let icons = ternarygrid_ui::style::icon_palette(self.style.visuals.dark_mode);
        let r = response.rect;
        // The tab reaches up to the top bar's hairline: what it paints starts a point below. Only
        // the part of the tab inside the bar: a tab scrolled away paints nothing over the panels.
        let painter = response.ctx.layer_painter(response.layer_id).with_clip_rect(response.interact_rect.with_min_y(r.top() + 1.0));
        // Paint the Opsi A pure-logo status icon in the exact 14×14 left icon slot.
        let badge = tab.status.badge(now);
        if badge != TabBadge::None {
            let open = self.is_open(tab.id);
            let lead = if open { 7.0 } else { 6.0 };
            let size = self.style.text_styles.get(&egui::TextStyle::Body).map_or(13.0, |f| f.size);
            let cx = r.left() + 8.0 + lead + 6.5;
            let cy = if has_place { r.center().y - (size - 2.0) * 0.54 } else { r.center().y };
            let time = response.ctx.input(|i| i.time);
            paint_tab_status_icon(&painter, egui::pos2(cx, cy), badge, &p, &icons, time);
        }
        if let Some(delay) = tab.status.repaint_after(now) {
            response.ctx.request_repaint_after(delay);
        }
        // Every tab's ×: 20 points, 10 from its right edge.
        let x = egui::Rect::from_center_size(egui::pos2(r.right() - 21.0, r.center().y + 0.5), egui::vec2(20.0, 20.0));
        let hover = response.ctx.input(|i| i.pointer.hover_pos()).is_some_and(|q| x.contains(q)) && response.hovered();
        if hover {
            painter.rect_filled(x, ternarygrid_ui::style::RADIUS, p.hover);
        }
        painter.text(x.center(), egui::Align2::CENTER_CENTER, egui_phosphor::regular::X, ternarygrid_ui::style::icon_font(12.0), if hover { p.text } else { p.text_muted });
        if response.clicked() && response.interact_pointer_pos().is_some_and(|q| x.contains(q)) {
            // The click also opened the tab: closing one in the background keeps what was open.
            let command = match self.leaf_open.get(&tab.id).copied().filter(|keep| *keep != tab.id) {
                Some(keep_open) => crate::command::AppCommand::CloseBackgroundTab { id: tab.id, keep_open, keep_focused: self.active_tab },
                None => crate::command::AppCommand::CloseTab(tab.id),
            };
            self.cx.commands.push(command);
        }
    }

    fn context_menu(&mut self, ui: &mut Ui, tab: &mut Tab, _path: egui_dock::NodePath) {
        use crate::command::AppCommand;
        use ternarygrid_core::tr;
        ternarygrid_ui::kit::menu_look(ui, 180.0);
        if ternarygrid_ui::kit::menu_item(ui, Some(egui_phosphor::regular::X), tr("Close"), "").clicked() {
            self.cx.commands.push(AppCommand::CloseTab(tab.id));
            ui.close();
        }
        if ternarygrid_ui::kit::menu_item(ui, None, tr("Close others"), "").clicked() {
            self.cx.commands.push(AppCommand::CloseOtherTabs(tab.id));
            ui.close();
        }
        if ternarygrid_ui::kit::menu_item(ui, None, tr("Close all"), "").clicked() {
            self.cx.commands.push(AppCommand::CloseAllTabs);
            ui.close();
        }
    }

    fn on_close(&mut self, tab: &mut Tab) -> egui_dock::tab_viewer::OnCloseResponse {
        if tab.needs_close_confirm() || tab.close_rolls_back(self.cx.conns) {
            self.close_confirm.push(tab.id);
            return egui_dock::tab_viewer::OnCloseResponse::Ignore;
        }
        if tab.view.on_close(self.cx) {
            egui_dock::tab_viewer::OnCloseResponse::Close
        } else {
            self.close_vetoed.push(tab.id);
            egui_dock::tab_viewer::OnCloseResponse::Ignore
        }
    }

    fn scroll_bars(&self, _tab: &Tab) -> [bool; 2] {
        [false, false]
    }
}

/// Paint the pure-logo status glyph in the tab's 14×14 left icon slot.
fn paint_tab_status_icon(painter: &egui::Painter, c: egui::Pos2, badge: TabBadge, p: &ternarygrid_ui::style::Palette, icons: &ternarygrid_ui::style::IconPalette, time: f64) {
    use egui::{Stroke, pos2, vec2};
    use std::f32::consts::{FRAC_PI_2, TAU};
    let r = 5.6_f32;
    let arc_points =
        |start: f32, sweep: f32, steps: usize| -> Vec<egui::Pos2> { (0..=steps).map(|k| c + r * egui::Vec2::angled(start + sweep * (k as f32 / steps as f32))).collect() };
    match badge {
        TabBadge::None => {}
        TabBadge::Running => {
            painter.circle_stroke(c, r, Stroke::new(1.45, p.border_strong));
            let angle = (time * 5.2) as f32;
            painter.add(egui::epaint::PathShape::line(arc_points(angle, TAU * 0.65, 20), Stroke::new(1.75, icons.teal)));
        }
        TabBadge::Progress(frac) => {
            painter.circle_stroke(c, r, Stroke::new(1.5, p.border_strong));
            let sweep = frac.clamp(0.05, 1.0) * TAU;
            painter.add(egui::epaint::PathShape::line(arc_points(-FRAC_PI_2, sweep, 24), Stroke::new(1.8, icons.teal)));
        }
        TabBadge::Done => {
            painter.circle_filled(c, r, ternarygrid_ui::soft(p.ok, 0.16));
            painter.circle_stroke(c, r, Stroke::new(1.3, p.ok));
            let s = Stroke::new(1.55, p.ok);
            let a = c + vec2(-2.4, 0.2);
            let m = c + vec2(-0.5, 2.1);
            let z = c + vec2(2.8, -1.8);
            painter.line_segment([a, m], s);
            painter.line_segment([m, z], s);
        }
        TabBadge::Error => {
            painter.circle_filled(c, r, ternarygrid_ui::soft(p.err, 0.16));
            painter.circle_stroke(c, r, Stroke::new(1.3, p.err));
            let s = Stroke::new(1.5, p.err);
            let d = 2.15;
            painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], s);
            painter.line_segment([c + vec2(d, -d), c + vec2(-d, d)], s);
        }
        TabBadge::Warning => {
            let pts = vec![pos2(c.x, c.y - 5.3), pos2(c.x + 5.6, c.y + 4.4), pos2(c.x - 5.6, c.y + 4.4)];
            painter.add(egui::epaint::PathShape::convex_polygon(pts, ternarygrid_ui::soft(p.warn, 0.16), Stroke::new(1.25, p.warn)));
            painter.line_segment([c + vec2(0.0, -1.9), c + vec2(0.0, 1.1)], Stroke::new(1.4, p.warn));
            painter.circle_filled(c + vec2(0.0, 2.8), 0.8, p.warn);
        }
        TabBadge::Attention => {
            painter.circle_filled(c, r, ternarygrid_ui::soft(p.warn, 0.16));
            painter.circle_stroke(c, r, Stroke::new(1.3, p.warn));
            painter.line_segment([c + vec2(0.0, -2.8), c + vec2(0.0, 0.7)], Stroke::new(1.5, p.warn));
            painter.circle_filled(c + vec2(0.0, 2.55), 0.85, p.warn);
        }
        TabBadge::Cancelled => {
            painter.circle_stroke(c, r, Stroke::new(1.3, p.text_faint));
            let d = r * 0.68;
            painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], Stroke::new(1.3, p.text_faint));
        }
    }
}

/// Implements the `as_any` boilerplate of [`TabView`].
#[macro_export]
macro_rules! impl_tab_any {
    () => {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    };
}

/// A view embedded inside another tab (e.g. the data page of an object editor).
pub trait EmbeddedView {
    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext);
    fn is_dirty(&self) -> bool {
        false
    }
    fn on_action(&mut self, action: crate::keys::Action, cx: &mut AppContext) -> bool {
        let _ = (action, cx);
        false
    }
    fn focus_context(&self) -> crate::keys::ActionContext {
        crate::keys::ActionContext::Global
    }
    /// The view's own session (its open transaction is listed in Pending transactions).
    fn transaction_session(&self) -> Option<crate::conn::session::SessionHandle> {
        None
    }
    /// Controls the view puts in its host's header row (laid out right to left).
    fn header_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let _ = (ui, cx);
    }
}

/// Read-only SQL (DDL, generated scripts), highlighted in the dialect of `conn`.
pub fn code(ui: &mut Ui, cx: &mut AppContext, conn: &str, id: impl std::hash::Hash + std::fmt::Debug, text: &str) {
    use ternarygrid_editor::{EditorLanguage, TokenStyle};
    type Tokens = std::sync::Arc<Vec<(std::ops::Range<usize>, TokenStyle)>>;
    let key = Id::new(&id).with("sql_tokens");
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (conn, text).hash(&mut h);
        h.finish()
    };
    let cached = ui.data(|d| d.get_temp::<(u64, Tokens)>(key)).filter(|(h, _)| *h == hash).map(|(_, t)| t);
    let tokens = cached.unwrap_or_else(|| {
        let t: Tokens = std::sync::Arc::new(cx.conns.live(conn).map(|l| crate::sqllang::SqlLanguage::new(l.dialect.clone()).highlight(text)).unwrap_or_default());
        ui.data_mut(|d| d.insert_temp(key, (hash, t.clone())));
        t
    });
    crate::widgets::code_view(ui, id, text, &tokens);
}

#[cfg(test)]
mod tests {
    use super::{SPINNER_DELAY, TabActivity, TabBadge, TabStatusTracker, first_line, place_label, tab_reveal_delta};
    use std::time::{Duration, Instant};
    use ternarygrid_core::SchemaRef;

    #[test]
    fn a_focused_tab_scrolls_into_the_bar() {
        let tab = |left: f32, right: f32| egui::Rect::from_min_max(egui::pos2(left, 0.0), egui::pos2(right, 40.0));
        assert_eq!(tab_reveal_delta(tab(120.0, 220.0), 100.0, 500.0), 0.0, "in view");
        assert_eq!(tab_reveal_delta(tab(450.0, 550.0), 100.0, 500.0), -50.0, "past the right end");
        assert_eq!(tab_reveal_delta(tab(40.0, 140.0), 100.0, 500.0), 60.0, "past the left end");
        assert_eq!(tab_reveal_delta(tab(150.0, 750.0), 100.0, 500.0), -50.0, "wider than the bar: its start");
    }

    fn at(database: Option<&str>, schema: Option<&str>) -> SchemaRef {
        SchemaRef::new(database.map(Into::into), schema.map(Into::into))
    }

    #[test]
    fn place_as_in_dbeaver() {
        let pg = at(Some("zain_iq"), Some("public"));
        // A table of the current database, of another one, an editor without a schema.
        assert_eq!(place_label(at(None, Some("public")), &pg, true).as_deref(), Some("public@zain_iq"));
        assert_eq!(place_label(at(Some("reports"), Some("sales")), &pg, true).as_deref(), Some("sales@reports"));
        assert_eq!(place_label(at(Some("reports"), None), &pg, true).as_deref(), Some("reports"));
        assert_eq!(place_label(SchemaRef::default(), &pg, true).as_deref(), Some("public@zain_iq"));
        // MySQL: the database is the schema; SQLite: main.
        let mysql = at(Some("shop"), Some("shop"));
        assert_eq!(place_label(at(None, Some("crm")), &mysql, false).as_deref(), Some("crm"));
        assert_eq!(place_label(SchemaRef::default(), &mysql, false).as_deref(), Some("shop"));
        assert_eq!(place_label(at(None, Some("main")), &at(None, None), false).as_deref(), Some("main"));
    }

    #[test]
    fn tab_status_anti_flicker_and_determinate_progress() {
        let t0 = Instant::now();
        let mut tr = TabStatusTracker::default();
        assert_eq!(tr.badge(t0), TabBadge::None);
        assert_eq!(tr.repaint_after(t0), None);

        // Indeterminate run stays hidden during the 250 ms anti-flicker grace period.
        tr.update(TabActivity::Running { detail: Some("Executing SQL…".into()), progress: None }, true, t0);
        assert_eq!(tr.badge(t0 + Duration::from_millis(100)), TabBadge::None);
        assert!(tr.repaint_after(t0 + Duration::from_millis(100)).is_some());

        // After 250 ms it morphs into the animated spinner.
        let t_spin = t0 + SPINNER_DELAY;
        assert_eq!(tr.badge(t_spin), TabBadge::Running);
        assert_eq!(tr.repaint_after(t_spin), Some(Duration::from_millis(33)));

        // Determinate progress appears immediately without waiting 250 ms.
        let mut tr_prog = TabStatusTracker::default();
        tr_prog.update(TabActivity::Running { detail: Some("Fetching rows…".into()), progress: Some(0.64) }, false, t0);
        assert_eq!(tr_prog.badge(t0), TabBadge::Progress(0.64));
        assert_eq!(tr_prog.tooltip_line(t0).as_deref(), Some("Fetching rows… (64%)"));
    }

    #[test]
    fn tab_status_background_done_clears_on_visit_while_foreground_stays_clean() {
        let t0 = Instant::now();
        // Foreground run: active the entire time -> never shows unread Done badge.
        let mut fg = TabStatusTracker::default();
        fg.update(TabActivity::Running { detail: Some("Reading data…".into()), progress: None }, true, t0);
        fg.update(TabActivity::Idle, true, t0 + Duration::from_secs(2));
        assert_eq!(fg.badge(t0 + Duration::from_secs(2)), TabBadge::None);
        assert_eq!(fg.tooltip_line(t0 + Duration::from_secs(2)), None);

        // Background run: started active, user switches away, job completes while inactive -> green Done ✓.
        let mut bg = TabStatusTracker::default();
        bg.update(TabActivity::Running { detail: Some("Reading data…".into()), progress: None }, true, t0);
        bg.update(TabActivity::Running { detail: Some("Reading data…".into()), progress: None }, false, t0 + Duration::from_millis(400));
        assert_eq!(bg.badge(t0 + Duration::from_millis(400)), TabBadge::Running);

        let t_done = t0 + Duration::from_secs(3);
        bg.update(TabActivity::Idle, false, t_done);
        assert_eq!(bg.badge(t_done), TabBadge::Done);
        assert_eq!(bg.repaint_after(t_done), None, "Done badge is static and uses 0 idle CPU");
        let tip = bg.tooltip_line(t_done).expect("Done tooltip");
        assert!(tip.contains("Completed") && tip.contains("Reading data"), "unexpected tip: {tip}");

        // Switching back to the tab auto-clears the unread Done badge immediately.
        bg.update(TabActivity::Idle, true, t_done + Duration::from_millis(10));
        assert_eq!(bg.badge(t_done + Duration::from_millis(10)), TabBadge::None);
        assert_eq!(bg.tooltip_line(t_done + Duration::from_millis(10)), None);
    }

    #[test]
    fn tab_status_error_warning_attention_cancelled_and_restart() {
        let t0 = Instant::now();
        let mut tr = TabStatusTracker::default();

        // Background error stays visible until cleared by a new run or edit.
        tr.update(TabActivity::Running { detail: Some("Executing SQL…".into()), progress: None }, false, t0);
        tr.update(TabActivity::Error { detail: "syntax error at or near FROM".into() }, false, t0 + Duration::from_secs(1));
        assert_eq!(tr.badge(t0 + Duration::from_secs(1)), TabBadge::Error);
        assert_eq!(tr.tooltip_line(t0 + Duration::from_secs(1)).as_deref(), Some("syntax error at or near FROM"));
        assert_eq!(tr.repaint_after(t0 + Duration::from_secs(1)), None);

        // Attention (confirm / parameter prompt / AI approval).
        tr.update(TabActivity::Attention { detail: "Waiting for query parameters".into() }, false, t0 + Duration::from_secs(2));
        assert_eq!(tr.badge(t0 + Duration::from_secs(2)), TabBadge::Attention);

        // Warning.
        tr.update(TabActivity::Warning { detail: "Sync script was truncated".into() }, false, t0 + Duration::from_secs(3));
        assert_eq!(tr.badge(t0 + Duration::from_secs(3)), TabBadge::Warning);

        // Cancelled.
        tr.update(TabActivity::Cancelled { detail: "Cancelled after 1.2 s".into() }, false, t0 + Duration::from_secs(4));
        assert_eq!(tr.badge(t0 + Duration::from_secs(4)), TabBadge::Cancelled);

        // Starting a new run clears any previous unread Done badge.
        tr.update(TabActivity::Running { detail: Some("Fetching…".into()), progress: None }, false, t0 + Duration::from_secs(5));
        tr.update(TabActivity::Idle, false, t0 + Duration::from_secs(6));
        assert_eq!(tr.badge(t0 + Duration::from_secs(6)), TabBadge::Done);
        tr.update(TabActivity::Running { detail: Some("Fetching…".into()), progress: None }, false, t0 + Duration::from_secs(7));
        assert_eq!(tr.badge(t0 + Duration::from_secs(7)), TabBadge::None, "new run resets grace period and clears stale Done");
        assert_eq!(tr.badge(t0 + Duration::from_secs(7) + SPINNER_DELAY), TabBadge::Running);
    }

    #[test]
    fn first_line_compacts_multiline_and_long_messages() {
        assert_eq!(first_line("\n  ERROR: relation missing\nDETAIL: more info"), "ERROR: relation missing");
        let long = "x".repeat(120);
        let s = first_line(&long);
        assert!(s.ends_with('…') && s.chars().count() == 97);
    }
}
