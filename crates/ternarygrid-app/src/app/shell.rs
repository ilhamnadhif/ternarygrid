//! The window's chrome: the top bar (menus where there is no system menu
//! bar, New, where the active tab works, the search / command box, the AI
//! assistant, theme and settings), the icon rail at the left and the status
//! bar. Every action that used to have a toolbar button is in one of these,
//! the menus, the context menus or the command palette.

use super::*;
use ternarygrid_ui::kit;

/// Height of the top bar: 44 points and the hairline under it.
pub const TOP_BAR_H: f32 = 45.0;
/// Width of the icon rail, its hairline included.
pub const RAIL_W: f32 = 48.0;
/// Height of the status bar: the hairline over it and 26 points.
pub const STATUS_H: f32 = 27.0;

/// What leads a crumb of the top bar: the database's logo or an icon.
enum Lead<'a> {
    Logo(ternarygrid_core::DbKind),
    Icon(&'a str),
}

/// A flat button of the top bar: the lead (16-point logo or 14-point muted icon), the text in
/// the medium weight and a faint chevron, 28 points high, padded by 8; `framed`: on the surface
/// with the strong hairline, padded by 10 and the hairline, the chevron muted (New).
fn bar_button(ui: &mut Ui, lead: Lead, text: &str, tip: &str, framed: bool) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let size = ternarygrid_ui::style::body_size(ui.ctx());
    let painter = ui.painter().clone();
    let galley = painter.layout_no_wrap(text.to_string(), ternarygrid_ui::style::medium(ui.ctx(), size), p.text);
    let chevron_color = if framed { p.text_muted } else { p.text_faint };
    let chevron = painter.layout_no_wrap(icon::CARET_DOWN.to_string(), ternarygrid_ui::style::icon_font(12.0), chevron_color);
    let (lead_w, pad) = match lead {
        Lead::Logo(_) => (16.0, 8.0),
        Lead::Icon(_) if framed => (14.0, 11.0),
        Lead::Icon(_) => (14.0, 8.0),
    };
    let gap = 6.0;
    // The chevron in a 12-point slot (its glyph is narrower).
    let width = pad + lead_w + gap + galley.size().x + gap + 12.0 + pad;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 28.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, if framed { text } else { tip }));
    let resp = if tip.is_empty() || framed { resp } else { resp.on_hover_text(tip) };
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = resp.hovered() || resp.is_pointer_button_down_on();
    let radius = egui::CornerRadius::same(ternarygrid_ui::style::RADIUS);
    if framed {
        painter.rect(rect, radius, if hovered { p.hover } else { p.surface }, egui::Stroke::new(1.0, p.border_strong), egui::StrokeKind::Inside);
    } else if hovered {
        painter.rect_filled(rect, radius, p.hover);
    }
    if resp.has_focus() {
        painter.rect_stroke(rect, radius, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
    }
    let cy = rect.center().y;
    let mut x = rect.left() + pad;
    match lead {
        Lead::Logo(kind) => {
            let r = egui::Rect::from_min_size(egui::pos2(x, cy - 8.0), egui::vec2(16.0, 16.0));
            let dark = ui.visuals().dark_mode;
            if !crate::dblogo::paint(ui, kind, r, crate::theme::logo_color(kind, dark)) {
                painter.text(r.center(), egui::Align2::CENTER_CENTER, crate::nav::db_icon(kind), ternarygrid_ui::style::icon_font(16.0), p.text_muted);
            }
        }
        Lead::Icon(glyph) => {
            let color = if framed { p.text } else { p.text_muted };
            painter.text(egui::pos2(x + 7.0, cy), egui::Align2::CENTER_CENTER, glyph, ternarygrid_ui::style::icon_font(14.0), color);
        }
    }
    x += lead_w + gap;
    let w = galley.size().x;
    painter.galley(egui::pos2(x, cy - galley.size().y / 2.0), galley, p.text);
    x += w + gap;
    painter.galley(egui::pos2(x + (12.0 - chevron.size().x) / 2.0, cy - chevron.size().y / 2.0), chevron, chevron_color);
    resp
}

impl TernaryGridApp {
    /// The top bar, as in the design: [window buttons] [menus] + New ▾ │ connection ▾ ›
    /// database ▾ › schema ▾ PRODUCTION, the search box in the middle, the AI assistant,
    /// theme and settings at the right. `inset`: space for the window buttons at its left.
    pub(super) fn top_bar(&mut self, ui: &mut Ui, inset: f32, menus: bool) {
        let rect = ui.max_rect();
        // An empty spot of the bar moves the window (its title bar is hidden on macOS);
        // registered first, so the widgets on the bar take their clicks.
        if inset > 0.0 {
            let bg = ui.interact(rect, egui::Id::new("tg_topbar_drag"), egui::Sense::click_and_drag());
            if bg.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if bg.double_clicked() {
                let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }
        }
        let p = ternarygrid_ui::pal(ui);

        // Right: update, AI, theme (laid out first: the search box goes between). The settings
        // are on the rail, which shows when they are open.
        let mut right = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
        right.spacing_mut().item_spacing.x = 2.0;
        let dark = right.visuals().dark_mode;
        let (theme_icon, theme_tip) = if dark { (icon::SUN, tr("Switch to the light theme")) } else { (icon::MOON, tr("Switch to the dark theme")) };
        if kit::icon_button_sized(&mut right, theme_icon, theme_tip, false, 30.0, 16.0).clicked() {
            self.toggle_theme(right.ctx());
        }
        if kit::icon_button_sized(&mut right, icon::SPARKLE, tr("AI assistant"), self.ai_open, 30.0, 16.0).clicked() {
            self.commands.push(AppCommand::Action(crate::keys::Action::AiAssistant));
        }
        right.add_space(6.0);
        self.updates.toolbar_button(&mut right);
        let right_start = right.min_rect().left();

        // Left: menus (no system menu bar), New, where the active tab works.
        let mut left = ui.new_child(egui::UiBuilder::new().max_rect(rect.with_min_x(rect.left() + inset)).layout(egui::Layout::left_to_right(egui::Align::Center)));
        left.spacing_mut().item_spacing.x = 4.0;
        if menus {
            // egui's menu bar takes the whole width: give it none, it grows to its menus.
            let h = left.spacing().interact_size.y;
            left.allocate_ui_with_layout(egui::vec2(0.0, h), egui::Layout::left_to_right(egui::Align::Center), |ui| self.menu_bar(ui));
            kit::vdivider(&mut left, 18.0);
        }
        self.new_menu(&mut left);
        // The design's divider after New: 6 points, a hairline, 6 points and the 4-point gap (drawn as a gap).
        left.add_space(17.0);
        // The connection type's pill only where it does not run under the search box.
        let pill_limit = rect.center().x - 210.0 - 16.0;
        self.location_crumbs(&mut left, &p, pill_limit);
        let left_end = left.min_rect().right();

        // Centre: the search / command box, centred in the bar while it fits (420 points).
        let room = right_start - left_end - 32.0;
        if room >= 180.0 {
            let width = room.min(420.0);
            let x = (rect.center().x - width / 2.0).clamp(left_end + 16.0, right_start - 16.0 - width);
            let r = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 15.0), egui::vec2(width, 30.0));
            self.search_box(ui, r, &p);
        }
    }

    /// "+ New ▾": what can be created.
    fn new_menu(&mut self, ui: &mut Ui) {
        use crate::menu::MenuAction as M;
        let resp = bar_button(ui, Lead::Icon(icon::PLUS), tr("New"), "", true);
        let state = self.menu_state();
        let mut chosen = None;
        let mut console = false;
        let keys = |a: crate::keys::Action| crate::keys::label(&self.keymap, a);
        let (editor_keys, conn_keys) = (keys(crate::keys::Action::NewSqlEditor), keys(crate::keys::Action::NewConnection));
        let (database_why, schema_why) = (self.cannot_create(false), self.cannot_create(true));
        egui::Popup::menu(&resp).gap(4.0).show(|ui| {
            kit::menu_look(ui, 230.0);
            let mut item = |ui: &mut Ui, why: Option<&str>, glyph: &str, text: &str, keys: &str, action: Option<M>| {
                let r = ui.add_enabled_ui(why.is_none(), |ui| kit::menu_item(ui, Some(glyph), text, keys)).inner;
                let r = match why {
                    Some(why) => r.on_disabled_hover_text(why),
                    None => r,
                };
                if r.clicked() {
                    match action {
                        Some(a) => chosen = Some(a),
                        None => console = true,
                    }
                    ui.close();
                }
            };
            item(ui, None, icon::FILE_CODE, tr("SQL editor"), &editor_keys, Some(M::NewSqlEditor));
            item(ui, None, icon::TERMINAL_WINDOW, tr("SQL console"), "", None);
            kit::menu_separator(ui);
            item(ui, None, icon::PLUG, tr("Connection…"), &conn_keys, Some(M::NewConnection));
            item(ui, database_why.as_deref().filter(|_| !state.new_database), icon::DATABASE, tr("Database…"), "", Some(M::NewDatabase));
            item(ui, schema_why.as_deref().filter(|_| !state.new_schema), icon::STACK, tr("Schema…"), "", Some(M::NewSchema));
            kit::menu_separator(ui);
            item(ui, None, icon::DOWNLOAD_SIMPLE, tr("Import connections…"), "", Some(M::ImportConnections));
            item(ui, None, crate::tabs::welcome::SAMPLE_ICON, tr("Sample database"), "", Some(M::CreateSampleDatabase));
        });
        if console {
            self.commands.push(AppCommand::Action(crate::keys::Action::OpenSqlConsole));
        }
        if let Some(a) = chosen {
            let ctx = ui.ctx().clone();
            self.run_menu_action(&ctx, a);
        }
    }

    /// Why New › Database… (`schema`: Schema…) is off, for its tooltip: no connection selected,
    /// not connected, or a server that has none to create.
    pub(super) fn cannot_create(&self, schema: bool) -> Option<String> {
        let Some(conn) = self.selected_connection() else {
            return Some(tr("Select a connection in the navigator first.").to_string());
        };
        let Some(live) = self.conns.live(&conn) else {
            let name = self.conns.config(&conn).map(|c| c.name.clone()).unwrap_or_default();
            return Some(ternarygrid_core::trf("Connect to {} first.", &[&name]));
        };
        let product = live.config.kind.display_name();
        Some(if schema { ternarygrid_core::trf("{} has no schemas to create.", &[&product]) } else { ternarygrid_core::trf("{} has no databases to create.", &[&product]) })
    }

    /// Connection › database › schema of the active tab (in a SQL editor a
    /// click chooses another; elsewhere it shows the place in the navigator),
    /// and the connection's type when it has a colour (Production).
    fn location_crumbs(&mut self, ui: &mut Ui, p: &ternarygrid_ui::Palette, pill_limit: f32) {
        let active = self.dock.find_active_focused();
        let object = active.as_ref().and_then(|(_, t)| t.view.navigator_object());
        let in_editor = active.as_ref().is_some_and(|(_, t)| t.view.as_any().is::<crate::tabs::sql_editor::SqlEditorTab>());
        // A tab that works nowhere (the start page, the settings): nothing, or the connection
        // selected in the navigator, which New and the other commands use.
        let Some(loc) = self.location() else { return };
        let conn_tip = format!("{}: {}", tr("Connection"), loc.name);
        let mut clicked_place = false;
        let clicked_conn = bar_button(ui, Lead::Logo(loc.kind), &loc.name, &conn_tip, false).clicked();
        if self.conns.is_connected(&loc.conn) {
            for (kind, name) in [(ternarygrid_core::meta::ObjectKind::Database, &loc.database), (ternarygrid_core::meta::ObjectKind::Schema, &loc.schema)] {
                let Some(name) = name else { continue };
                ui.label(RichText::new("›").size(13.0).color(p.text_faint));
                let tip = format!("{}: {name}", kind.label());
                clicked_place |= bar_button(ui, Lead::Icon(crate::nav::kind_icon(kind)), name, &tip, false).clicked();
            }
        }
        let ty = self.conns.type_of(&loc.conn);
        let name = ty.name.to_uppercase();
        let pill_w = ui.painter().layout_no_wrap(name.clone(), ternarygrid_ui::style::medium(ui.ctx(), ternarygrid_ui::style::SMALL), p.text).size().x + 14.0;
        if let Some(c) = crate::theme::marker(ty.color, ui.visuals().dark_mode)
            && ui.cursor().left() + 6.0 + pill_w <= pill_limit
        {
            ui.add_space(6.0);
            crate::theme::marker_pill(ui, &name, c).on_hover_text(tr("Connection type"));
        }
        if in_editor && clicked_conn {
            self.commands.push(AppCommand::Action(crate::keys::Action::SelectConnection));
        } else if in_editor && clicked_place {
            self.commands.push(AppCommand::Action(crate::keys::Action::SelectSchema));
        } else if clicked_conn || clicked_place {
            self.show_nav = true;
            self.show_project = false;
            match object {
                Some((conn, obj)) => self.nav.reveal(&conn, &obj, &self.conns),
                None => self.nav.select(crate::nav::NavNode::Connection(loc.conn)),
            }
        }
    }

    /// Where the active tab works; for a tab without a connection, the connection selected in
    /// the navigator (what New and the menus act on).
    pub(super) fn location(&mut self) -> Option<crate::tabs::TabLocation> {
        let active = self.dock.find_active_focused();
        if let Some(loc) = active.as_ref().and_then(|(_, t)| crate::tabs::tab_location(t.view.as_ref(), &self.conns)) {
            return Some(loc);
        }
        // A tab on a connection that is gone, or an editor without one (its toolbar says so).
        if active.as_ref().is_some_and(|(_, t)| t.view.connection_id().is_some() || t.view.as_any().is::<crate::tabs::sql_editor::SqlEditorTab>()) {
            return None;
        }
        let conn = self.nav.selected.as_ref().and_then(|n| n.connection())?;
        let cfg = self.conns.config(conn)?;
        Some(crate::tabs::TabLocation { conn: conn.to_string(), name: cfg.name.clone(), kind: cfg.kind, place: None, database: None, schema: None })
    }

    /// The box that opens the command palette (objects, scripts, commands): 30 points high,
    /// on the hover fill, the magnifier, the hint and the palette's shortcut.
    fn search_box(&mut self, ui: &mut Ui, r: egui::Rect, p: &ternarygrid_ui::Palette) {
        let label = tr("Search or run a command");
        let resp = ui.interact(r, egui::Id::new("tg_search_box"), egui::Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        let painter = ui.painter();
        let radius = egui::CornerRadius::same(8);
        let hovered = resp.hovered();
        painter.rect(r, radius, if hovered { p.surface } else { p.hover }, egui::Stroke::new(1.0, if hovered { p.border_strong } else { p.border }), egui::StrokeKind::Inside);
        painter.text(egui::pos2(r.left() + 17.0, r.center().y), egui::Align2::CENTER_CENTER, icon::MAGNIFYING_GLASS, ternarygrid_ui::style::icon_font(14.0), p.text_faint);
        let keys = crate::keys::label(&self.keymap, crate::keys::Action::CommandPalette);
        let hint_right = if keys.is_empty() { r.right() - 10.0 } else { kit::paint_kbd_text(painter, ui.ctx(), &keys, r.right() - 6.0, r.center().y, p).left() - 8.0 };
        let hint = egui::WidgetText::from(RichText::new(tr("Search tables, scripts, commands…")).size(13.0).color(p.text_faint)).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (hint_right - r.left() - 32.0).max(10.0),
            egui::TextStyle::Body,
        );
        painter.galley(egui::pos2(r.left() + 32.0, r.center().y - hint.size().y / 2.0), hint, p.text_faint);
        if resp.clicked() {
            self.dialogs.palette_open = true;
        }
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    /// The icon rail: two groups with a selection each, the side panel that is open (the
    /// navigator, projects) and the workspace tab that is active (history, tasks); background
    /// operations and settings at the bottom (34-point buttons 4 apart, 8 points from the ends).
    pub(super) fn rail(&mut self, ui: &mut Ui) {
        use crate::keys::Action;
        let (side, glyph) = (34.0, 20.0);
        #[derive(PartialEq)]
        enum Place {
            History,
            Tasks,
            Settings,
        }
        let place = match self.dock.find_active_focused().map(|(_, t)| t.view.as_any()) {
            Some(v) if v.is::<crate::tabs::history::HistoryTab>() => Some(Place::History),
            Some(v) if v.is::<crate::tabs::tasks::TasksTab>() => Some(Place::Tasks),
            Some(v) if v.is::<crate::tabs::settings::SettingsTab>() => Some(Place::Settings),
            _ => None,
        };
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.add_space(8.0);
            let nav_on = self.show_nav && !self.show_project;
            if kit::icon_button_sized(ui, icon::DATABASE, tr("Database navigator"), nav_on, side, glyph).clicked() {
                if nav_on {
                    self.show_nav = false;
                } else {
                    self.show_nav = true;
                    self.show_project = false;
                }
            }
            let projects_on = self.show_nav && self.show_project;
            if kit::icon_button_sized(ui, icon::FILE_TEXT, tr("Projects and scripts"), projects_on, side, glyph).clicked() {
                if projects_on {
                    self.show_nav = false;
                } else {
                    self.show_nav = true;
                    self.show_project = true;
                }
            }
            // A hairline between the groups, 6 points clear of the buttons.
            ui.add_space(2.0);
            let (line, _) = ui.allocate_exact_size(egui::vec2(20.0, 1.0), egui::Sense::hover());
            ui.painter().hline(line.x_range(), line.center().y, egui::Stroke::new(1.0, ternarygrid_ui::pal(ui).border_strong));
            ui.add_space(2.0);
            if kit::icon_button_sized(ui, icon::CLOCK_COUNTER_CLOCKWISE, tr("Query history"), place == Some(Place::History), side, glyph).clicked() {
                self.commands.push(AppCommand::Action(Action::OpenHistory));
            }
            if kit::icon_button_sized(ui, icon::LIST_CHECKS, tr("Tasks"), place == Some(Place::Tasks), side, glyph).clicked() {
                self.commands.push(AppCommand::Action(Action::OpenTasks));
            }
        });
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.add_space(8.0);
            if kit::icon_button_sized(ui, icon::GEAR_SIX, tr("Settings"), place == Some(Place::Settings), side, glyph).clicked() {
                self.commands.push(AppCommand::OpenSettings);
            }
            let running = self.ops.running();
            let tip = if running > 0 { ternarygrid_core::trf("Background operations ({} running)", &[&running]) } else { tr("Background operations").to_string() };
            let ops = kit::icon_button_sized(ui, icon::ACTIVITY, &tip, self.ops.panel_open, side, glyph);
            if running > 0 {
                let p = ternarygrid_ui::pal(ui);
                ui.painter().circle_filled(ops.rect.right_top() + egui::vec2(-9.5, 9.5), 3.5, p.accent_text);
            }
            if ops.clicked() {
                self.ops.panel_open = !self.ops.panel_open;
            }
        });
    }

    /// The status bar: the state of the connection the active tab works on (the top bar names
    /// it, the results' footer counts the rows): a dot, the server, the transaction mode; the
    /// time zone and memory.
    pub(super) fn status_bar(&mut self, ui: &mut Ui) {
        let p = ternarygrid_ui::pal(ui);
        let small = egui::FontId::proportional(11.5);
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let sep = |ui: &mut Ui| {
                ui.label(RichText::new("·").font(small.clone()).color(p.border_strong));
            };
            let location = self.location();
            let active = self.dock.find_active_focused();
            match location {
                Some(loc) => {
                    let (dot, server) = match self.conns.live(&loc.conn) {
                        // `version()` texts repeat the product ("PostgreSQL 16.4 on …"): show the number.
                        Some(live) => {
                            let number = live.server.version.split_whitespace().find(|w| w.starts_with(|c: char| c.is_ascii_digit())).unwrap_or("");
                            (p.ok, format!("{} {number}", live.server.product).trim().to_string())
                        }
                        None => match self.conns.status(&loc.conn) {
                            crate::conn::ConnStatus::Connecting => (p.warn, tr("Connecting…").to_string()),
                            crate::conn::ConnStatus::Failed(_) => (p.err, tr("Connection failed").to_string()),
                            _ => (p.text_faint, tr("Not connected").to_string()),
                        },
                    };
                    let tip = match &loc.place {
                        Some(place) => format!("{} · {place}", loc.name),
                        None => loc.name.clone(),
                    };
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let (r, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                        ui.painter().circle_filled(r.center(), 3.5, dot);
                        ui.label(RichText::new(&server).font(small.clone()).color(p.text_muted));
                    })
                    .response
                    .on_hover_text(tip);
                    // Transaction mode of the tab's session (Manual: what is pending).
                    if let Some(session) = active.as_ref().and_then(|(_, t)| t.view.transaction_session()) {
                        let tx = session.tx_state();
                        sep(ui);
                        let (text, color) = match tx.mode {
                            ternarygrid_core::TransactionMode::Auto => (tr("Auto-commit").to_string(), p.text_muted),
                            ternarygrid_core::TransactionMode::Manual if tx.in_transaction && !tx.log.is_empty() => {
                                (ternarygrid_core::trf("Manual commit · {} pending", &[&tx.log.len()]), p.warn)
                            }
                            ternarygrid_core::TransactionMode::Manual => (tr("Manual commit").to_string(), p.text_muted),
                        };
                        let r = ui.add(egui::Label::new(RichText::new(text).font(small.clone()).color(color)).sense(egui::Sense::click()));
                        if r.on_hover_text(tr("Pending transactions")).clicked() {
                            self.commands.push(AppCommand::Action(crate::keys::Action::PendingTransactions));
                        }
                    }
                }
                None => {
                    ui.add_space(-2.0);
                    ui.label(RichText::new(tr("No connection")).font(small.clone()).color(p.text_faint));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let mem = ternarygrid_core::rowstore::global_resident_bytes();
                ui.label(RichText::new(format!("{} {}", tr("Memory"), crate::widgets::format_bytes(mem as i64))).font(small.clone()).color(p.text_muted))
                    .on_hover_text(tr("Memory used by result sets"));
                // Time zone ("WIB (UTC+7)"): a click chooses another.
                let mut changed = false;
                let tz = self.settings.general.time_zone;
                let tz_label = time_zone_text(tz);
                let tz_button = ui.add(egui::Label::new(RichText::new(tz_label).font(small.clone()).color(p.text_muted)).sense(egui::Sense::click()));
                let tz_tip = crate::conn::time_zone_choices().into_iter().find(|(c, _)| *c == tz).map(|(_, l)| l).unwrap_or_default();
                let tz_button = tz_button.on_hover_text(format!("{}: {tz_tip}", tr("Time zone")));
                egui::Popup::menu(&tz_button).show(|ui| {
                    kit::menu_look(ui, 200.0);
                    let mut v = tz;
                    for (choice, label) in crate::conn::time_zone_choices() {
                        let mut on = v == choice;
                        if kit::menu_check(ui, &mut on, &label).clicked() {
                            v = choice;
                            changed = true;
                            ui.close();
                        }
                    }
                    self.settings.general.time_zone = v;
                });
                if changed {
                    self.commands.push(AppCommand::SettingsChanged);
                    self.save_settings();
                }
            });
        });
    }
}

/// The status bar's time zone: the computer's zone with its offset ("WIB (UTC+7)"), UTC or
/// the server's.
fn time_zone_text(tz: ternarygrid_config::settings::TimeZoneChoice) -> String {
    let label = crate::conn::time_zone_label(tz);
    if tz != ternarygrid_config::settings::TimeZoneChoice::System || label.starts_with(['+', '-']) {
        return label;
    }
    let secs = chrono::Local::now().offset().local_minus_utc();
    let (sign, m) = (if secs < 0 { '-' } else { '+' }, secs.unsigned_abs() / 60);
    let offset = if m % 60 == 0 { format!("{}", m / 60) } else { format!("{}:{:02}", m / 60, m % 60) };
    format!("{label} (UTC{sign}{offset})")
}
