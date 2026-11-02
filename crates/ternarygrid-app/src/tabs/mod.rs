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
use ternarygrid_core::SchemaRef;

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

    /// Opens in a pop-up window of this size instead of a tab: wizards and
    /// one-off actions (export, import, backup …).
    fn popup_size(&self) -> Option<egui::Vec2> {
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
}

impl Tab {
    pub fn new(id: u64, view: Box<dyn TabView>) -> Self {
        Tab { id, view, force_close: false }
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
    /// Titles that several tabs share, with what tells them apart (see [`tab_suffixes`]).
    pub suffixes: std::collections::HashMap<u64, String>,
    /// Where each tab's button was drawn (filled while the tabs are shown).
    pub tab_rects: std::collections::HashMap<u64, egui::Rect>,
    /// The tabs shown in their leaf (each leaf's open tab).
    pub open_tabs: std::collections::HashSet<u64>,
}

/// Height of the tab bar: 37 points and the hairline under it.
pub const TAB_BAR_H: f32 = 38.0;
/// Room kept at the right of the open tab's title for its × (8, the 20-point button, 10).
const TAB_CLOSE_ROOM: f32 = 30.0;

/// Tabs with the same title ("product" of two connections) get what tells them
/// apart after it: the connection's name, or schema@database within one connection.
pub fn tab_suffixes<'t>(tabs: impl Iterator<Item = &'t Tab>, conns: &crate::conn::ConnectionManager) -> std::collections::HashMap<u64, String> {
    let mut by_title: std::collections::HashMap<String, Vec<(u64, Option<TabLocation>)>> = std::collections::HashMap::new();
    for t in tabs {
        by_title.entry(t.view.title()).or_default().push((t.id, tab_location(t.view.as_ref(), conns)));
    }
    let mut out = std::collections::HashMap::new();
    for same in by_title.into_values().filter(|v| v.len() > 1) {
        let conns_differ = same.iter().map(|(_, l)| l.as_ref().map(|l| l.conn.clone())).collect::<std::collections::HashSet<_>>().len() > 1;
        for (id, loc) in same {
            let suffix = loc.and_then(|l| if conns_differ { Some(l.name) } else { l.place });
            if let Some(s) = suffix {
                out.insert(id, s);
            }
        }
    }
    out
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
    let conn = view.connection_id()?;
    let cfg = conns.config(conn)?;
    let live = conns.live(conn);
    // PostgreSQL, SQL Server: databases hold schemas. Elsewhere one level.
    let two_levels = live.is_some_and(|l| l.multiple_databases());
    let current = live.map(|l| SchemaRef::new(l.current_database.clone(), l.current_schema.clone())).unwrap_or_default();
    let at = view.schema_context().unwrap_or_default();
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

impl egui_dock::TabViewer for DockViewer<'_, '_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> Id {
        Id::new(("ternarygrid_tab", tab.id))
    }

    /// The tab's icon and title as in the design: 14 points from the tab's left edge, the
    /// icon (muted on the open tab, faint on the others), 8 points, the title (the medium
    /// weight on the open tab); the open tab keeps room for its × at the right.
    fn title(&mut self, tab: &mut Tab) -> WidgetText {
        let dirty = if tab.view.is_dirty() { "● " } else { "" };
        let mut title = format!("{dirty}{}", tab.view.title());
        if let Some(s) = self.suffixes.get(&tab.id) {
            title = format!("{title} · {s}");
        }
        if title.chars().count() > 46 {
            title = title.chars().take(44).collect::<String>() + "…";
        }
        let p = ternarygrid_ui::palette(self.style.visuals.dark_mode);
        let open = self.open_tabs.contains(&tab.id);
        let size = self.style.text_styles.get(&egui::TextStyle::Body).map_or(13.0, |f| f.size);
        let format = |font: egui::FontId, color: egui::Color32| egui::TextFormat { font_id: font, color, valign: egui::Align::Center, ..Default::default() };
        let mut job = egui::text::LayoutJob::default();
        // egui_dock pads the title by 8 points: 6 more on the left, the rest on the right.
        job.append(tab.view.icon(), 6.0, format(ternarygrid_ui::style::icon_font(14.0), if open { p.text_muted } else { p.text_faint }));
        let font = if open { egui::FontId::new(size, egui::FontFamily::Name(ternarygrid_ui::style::MEDIUM.into())) } else { egui::FontId::proportional(size) };
        job.append(&title, 8.0, format(font, egui::Color32::PLACEHOLDER));
        job.append("\u{200b}", if open { TAB_CLOSE_ROOM } else { 4.0 }, format(egui::FontId::proportional(size), egui::Color32::PLACEHOLDER));
        job.into()
    }

    /// A tab of a connection whose type has a colour (Production: red) is
    /// tinted in it, as in DBeaver: work on that server stands out.
    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        self.cx.tab_active = self.active_tab == Some(tab.id);
        self.cx.current_tab = Some(tab.id);
        tab.view.ui(ui, self.cx);
        self.cx.current_tab = None;
    }

    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        // The tab's own text, then its connection and schema@database.
        let location = tab_location(tab.view.as_ref(), self.cx.conns).map(|l| match l.place {
            Some(p) => format!("{} · {p}", l.name),
            None => l.name,
        });
        let tip: Vec<String> = tab.view.tooltip().into_iter().chain(location).collect();
        if !tip.is_empty() {
            response.clone().on_hover_text(tip.join("\n"));
        }
        if response.middle_clicked() {
            self.cx.commands.push(crate::command::AppCommand::CloseTab(tab.id));
        }
        self.tab_rects.insert(tab.id, response.rect);
        let p = ternarygrid_ui::palette(self.style.visuals.dark_mode);
        let open = self.open_tabs.contains(&tab.id);
        let r = response.rect;
        // The tab reaches up to the top bar's hairline: what it paints starts a point below.
        let painter = response.ctx.layer_painter(response.layer_id).with_clip_rect(r.with_min_y(r.top() + 1.0));
        // A line along the top: the connection type's colour on every tab of a marked
        // connection (Production stays visible), else the accent on the focused tab.
        let marker = tab.view.connection_id().and_then(|c| self.cx.conns.color_of(c)).and_then(|c| crate::theme::marker(c, self.style.visuals.dark_mode));
        let focused = self.active_tab == Some(tab.id);
        if let Some(color) = marker.or_else(|| focused.then_some(p.accent)) {
            let (top, thick) = if open { (r.top() + 1.0, 3.0) } else { (r.top() + 2.0, 2.0) };
            let line = egui::Rect::from_min_max(egui::pos2(r.left() + 8.0, top), egui::pos2(r.right() - 8.0, top + thick));
            painter.rect_filled(line, egui::CornerRadius { nw: 0, ne: 0, sw: 2, se: 2 }, color);
        }
        // The open tab's ×: 20 points, 10 from its right edge.
        if open {
            let x = egui::Rect::from_center_size(egui::pos2(r.right() - 20.0, r.center().y + 0.5), egui::vec2(20.0, 20.0));
            let hover = response.ctx.input(|i| i.pointer.hover_pos()).is_some_and(|q| x.contains(q)) && response.hovered();
            if hover {
                painter.rect_filled(x, ternarygrid_ui::style::RADIUS, p.hover);
            }
            painter.text(x.center(), egui::Align2::CENTER_CENTER, egui_phosphor::regular::X, ternarygrid_ui::style::icon_font(12.0), if hover { p.text } else { p.text_muted });
            if response.clicked() && response.interact_pointer_pos().is_some_and(|q| x.contains(q)) {
                self.cx.commands.push(crate::command::AppCommand::CloseTab(tab.id));
            }
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

/// Data editor for a table or view (filled in by the results module).
pub fn make_table_data_view(cx: &mut AppContext, conn: &str, obj: &ternarygrid_core::ObjectRef) -> Option<Box<dyn EmbeddedView>> {
    let _ = cx;
    Some(Box::new(crate::results::data_tab::TableDataView::new(conn.to_string(), obj.clone(), None)))
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
    use super::place_label;
    use ternarygrid_core::SchemaRef;

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
}
