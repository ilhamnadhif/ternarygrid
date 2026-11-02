//! The window's chrome: the top bar (menus where there is no system menu
//! bar, New, where the active tab works, the search / command box, the AI
//! assistant, theme and settings), the icon rail at the left and the status
//! bar. Every action that used to have a toolbar button is in one of these,
//! the menus, the context menus or the command palette.

use super::*;
use ternarygrid_ui::kit;

/// Height of the top bar.
pub const TOP_BAR_H: f32 = 44.0;
/// Width of the icon rail.
pub const RAIL_W: f32 = 48.0;
/// Height of the status bar.
pub const STATUS_H: f32 = 26.0;

impl TernaryGridApp {
    /// The top bar. `inset`: space for the window buttons at its left.
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

        // Right: update, AI, theme, settings (laid out first: the search box goes between).
        let mut right = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
        right.spacing_mut().item_spacing.x = 2.0;
        if kit::icon_button_ex(&mut right, icon::GEAR_SIX, tr("Settings"), false, 30.0).clicked() {
            self.commands.push(AppCommand::OpenSettings);
        }
        let dark = right.visuals().dark_mode;
        let (theme_icon, theme_tip) = if dark { (icon::SUN, tr("Switch to the light theme")) } else { (icon::MOON, tr("Switch to the dark theme")) };
        if kit::icon_button_ex(&mut right, theme_icon, theme_tip, false, 30.0).clicked() {
            self.toggle_theme(right.ctx());
        }
        if kit::icon_button_ex(&mut right, icon::SPARKLE, tr("AI assistant"), self.ai_open, 30.0).clicked() {
            self.commands.push(AppCommand::Action(crate::keys::Action::AiAssistant));
        }
        right.add_space(4.0);
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
        kit::vdivider(&mut left, 18.0);
        self.location_crumbs(&mut left, &p);
        let left_end = left.min_rect().right();

        // Centre: the search / command box, centred in the window while it fits.
        let room = right_start - left_end - 24.0;
        if room >= 180.0 {
            let width = room.min(420.0);
            let x = (rect.center().x - width / 2.0).clamp(left_end + 12.0, right_start - 12.0 - width);
            let r = egui::Rect::from_center_size(egui::pos2(x + width / 2.0, rect.center().y), egui::vec2(width, 30.0));
            self.search_box(ui, r, &p);
        }
    }

    /// "+ New": what can be created.
    fn new_menu(&mut self, ui: &mut Ui) {
        use crate::menu::MenuAction as M;
        let resp = kit::button_ex(ui, kit::Kind::Secondary, Some(icon::PLUS), tr("New"), true);
        let state = self.menu_state();
        let mut chosen = None;
        let mut console = false;
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(220.0);
            let mut item = |ui: &mut Ui, enabled: bool, glyph: &str, text: &str, action: Option<M>| {
                let clicked = ui.add_enabled(enabled, egui::Button::new(kit::with_icon(glyph, text))).clicked();
                if clicked {
                    match action {
                        Some(a) => chosen = Some(a),
                        None => console = true,
                    }
                    ui.close();
                }
            };
            item(ui, true, icon::FILE_SQL, tr("SQL editor"), Some(M::NewSqlEditor));
            item(ui, true, icon::TERMINAL, tr("SQL console"), None);
            ui.separator();
            item(ui, true, icon::PLUGS, tr("Connection…"), Some(M::NewConnection));
            item(ui, state.new_database, icon::DATABASE, tr("Database…"), Some(M::NewDatabase));
            item(ui, state.new_schema, icon::STACK, tr("Schema…"), Some(M::NewSchema));
            ui.separator();
            item(ui, true, icon::DOWNLOAD_SIMPLE, tr("Import connections…"), Some(M::ImportConnections));
            item(ui, true, icon::FLASK, tr("Sample database"), Some(M::CreateSampleDatabase));
        });
        if console {
            self.commands.push(AppCommand::Action(crate::keys::Action::OpenSqlConsole));
        }
        if let Some(a) = chosen {
            let ctx = ui.ctx().clone();
            self.run_menu_action(&ctx, a);
        }
    }

    /// Connection › database › schema of the active tab (in a SQL editor a
    /// click chooses another; elsewhere it shows the place in the navigator),
    /// and the connection's type when it has a colour (Production).
    fn location_crumbs(&mut self, ui: &mut Ui, p: &ternarygrid_ui::Palette) {
        let active = self.dock.find_active_focused();
        let location = active.as_ref().and_then(|(_, t)| crate::tabs::tab_location(t.view.as_ref(), &self.conns));
        let object = active.as_ref().and_then(|(_, t)| t.view.navigator_object());
        let in_editor = active.as_ref().is_some_and(|(_, t)| t.view.as_any().is::<crate::tabs::sql_editor::SqlEditorTab>());
        let Some(loc) = location else {
            ui.label(RichText::new(tr("No active connection")).color(p.text_faint));
            return;
        };
        let dark = ui.visuals().dark_mode;
        let crumb = |ui: &mut Ui, lead: egui::Image<'static>, text: &str, tip: &str| -> bool {
            let b = egui::Button::new((lead, RichText::new(text).strong(), RichText::new(icon::CARET_DOWN).size(10.0).color(p.text_faint))).frame_when_inactive(false);
            ui.add(b).on_hover_text(tip).clicked()
        };
        let logo = crate::dblogo::image(ui.ctx(), loc.kind, 16.0, crate::theme::logo_color(loc.kind, dark));
        let conn_tip = format!("{}: {}", tr("Connection"), loc.name);
        let mut clicked_place = false;
        let clicked_conn = match logo {
            Some(img) => crumb(ui, img, &loc.name, &conn_tip),
            None => {
                let b = egui::Button::new((RichText::new(crate::nav::db_icon(loc.kind)).color(p.text_muted), RichText::new(&loc.name).strong())).frame_when_inactive(false);
                ui.add(b).on_hover_text(&conn_tip).clicked()
            }
        };
        if self.conns.is_connected(&loc.conn) {
            for (kind, name) in [(ternarygrid_core::meta::ObjectKind::Database, &loc.database), (ternarygrid_core::meta::ObjectKind::Schema, &loc.schema)] {
                let Some(name) = name else { continue };
                ui.label(RichText::new(icon::CARET_RIGHT).size(11.0).color(p.text_faint));
                let tip = format!("{}: {name}", kind.label());
                let b = egui::Button::new((
                    RichText::new(crate::nav::kind_icon(kind)).color(p.text_muted),
                    name.as_str(),
                    RichText::new(icon::CARET_DOWN).size(10.0).color(p.text_faint),
                ))
                .frame_when_inactive(false);
                clicked_place |= ui.add(b).on_hover_text(tip).clicked();
            }
        }
        let ty = self.conns.type_of(&loc.conn);
        if let Some(c) = crate::theme::connection_marker(ty.color) {
            ui.add_space(4.0);
            kit::pill(ui, &ty.name.to_uppercase(), c).on_hover_text(tr("Connection type"));
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

    /// The box that opens the command palette (objects, scripts, commands).
    fn search_box(&mut self, ui: &mut Ui, r: egui::Rect, p: &ternarygrid_ui::Palette) {
        let label = tr("Search or run a command");
        let resp = ui.interact(r, egui::Id::new("tg_search_box"), egui::Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        let painter = ui.painter();
        let radius = egui::CornerRadius::same(8);
        painter.rect(r, radius, if resp.hovered() { p.surface } else { p.hover }, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
        painter.text(egui::pos2(r.left() + 14.0, r.center().y), egui::Align2::CENTER_CENTER, icon::MAGNIFYING_GLASS, ternarygrid_ui::style::icon_font(14.0), p.text_faint);
        painter.text(egui::pos2(r.left() + 28.0, r.center().y), egui::Align2::LEFT_CENTER, tr("Search tables, scripts, commands…"), egui::FontId::proportional(13.0), p.text_faint);
        let keys = crate::keys::label(&self.keymap, crate::keys::Action::CommandPalette);
        if !keys.is_empty() {
            let galley = painter.layout_no_wrap(keys, egui::FontId::proportional(11.0), p.text_muted);
            let kr = egui::Rect::from_min_size(egui::pos2(r.right() - galley.size().x - 18.0, r.center().y - 9.0), egui::vec2(galley.size().x + 10.0, 18.0));
            ternarygrid_ui::kit::paint_kbd(painter, kr, p);
            painter.galley(egui::pos2(kr.left() + 5.0, kr.center().y - galley.size().y / 2.0), galley, p.text_muted);
        }
        if resp.clicked() {
            self.dialogs.palette_open = true;
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    /// The icon rail: the navigator, projects, history, tasks; the AI assistant and settings.
    pub(super) fn rail(&mut self, ui: &mut Ui) {
        use crate::keys::Action;
        let side = 34.0;
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.add_space(6.0);
            let nav_on = self.show_nav && !self.show_project;
            if kit::icon_button_ex(ui, icon::DATABASE, tr("Database Navigator"), nav_on, side).clicked() {
                if nav_on {
                    self.show_nav = false;
                } else {
                    self.show_nav = true;
                    self.show_project = false;
                }
            }
            let projects_on = self.show_nav && self.show_project;
            if kit::icon_button_ex(ui, icon::FILES, tr("Projects and scripts"), projects_on, side).clicked() {
                if projects_on {
                    self.show_nav = false;
                } else {
                    self.show_nav = true;
                    self.show_project = true;
                }
            }
            if kit::icon_button_ex(ui, icon::CLOCK_COUNTER_CLOCKWISE, tr("Query history"), false, side).clicked() {
                self.commands.push(AppCommand::Action(Action::OpenHistory));
            }
            if kit::icon_button_ex(ui, icon::LIST_CHECKS, tr("Tasks"), false, side).clicked() {
                self.commands.push(AppCommand::Action(Action::OpenTasks));
            }
        });
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.add_space(6.0);
            if kit::icon_button_ex(ui, icon::GEAR_SIX, tr("Settings"), false, side).clicked() {
                self.commands.push(AppCommand::OpenSettings);
            }
            let running = self.ops.running();
            let tip = if running > 0 { ternarygrid_core::trf("Background operations ({} running)", &[&running]) } else { tr("Background operations").to_string() };
            let ops = kit::icon_button_ex(ui, icon::HOURGLASS, &tip, self.ops.panel_open, side);
            if running > 0 {
                let p = ternarygrid_ui::pal(ui);
                ui.painter().circle_filled(ops.rect.right_top() + egui::vec2(-8.0, 8.0), 4.0, p.accent);
            }
            if ops.clicked() {
                self.ops.panel_open = !self.ops.panel_open;
            }
        });
    }

    /// The status bar: where the active tab works (connection · schema ·
    /// transaction mode · result), the time zone and memory.
    pub(super) fn status_bar(&mut self, ui: &mut Ui) {
        let p = ternarygrid_ui::pal(ui);
        let small = egui::FontId::proportional(ternarygrid_ui::style::SMALL + 0.5);
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let sep = |ui: &mut Ui| {
                ui.label(RichText::new("·").font(small.clone()).color(p.border_strong));
            };
            let active = self.dock.find_active_focused();
            let location = active.as_ref().and_then(|(_, t)| crate::tabs::tab_location(t.view.as_ref(), &self.conns));
            match location {
                Some(loc) => {
                    let connected = self.conns.is_connected(&loc.conn);
                    let dot = if connected { p.ok } else { p.text_faint };
                    kit::dot(ui, dot, 3.5);
                    let server = match self.conns.live(&loc.conn) {
                        // `version()` texts repeat the product ("PostgreSQL 16.4 on …"): show the number.
                        Some(live) => {
                            let number = live.server.version.split_whitespace().find(|w| w.starts_with(|c: char| c.is_ascii_digit())).unwrap_or("");
                            format!("{} {number}", live.server.product)
                        }
                        None => tr("not connected").to_string(),
                    };
                    ui.label(RichText::new(&loc.name).font(small.clone()).color(p.text)).on_hover_text(server);
                    if let Some(place) = &loc.place {
                        sep(ui);
                        ui.label(RichText::new(place).font(small.clone()).color(p.text_muted));
                    }
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
                    if let Some(summary) = active.as_ref().and_then(|(_, t)| t.view.status_summary()) {
                        sep(ui);
                        ui.label(RichText::new(summary).font(small.clone()).color(p.text_muted));
                    }
                }
                None => {
                    ui.label(RichText::new(tr("No active connection")).font(small.clone()).color(p.text_faint));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let mem = ternarygrid_core::rowstore::global_resident_bytes();
                ui.label(RichText::new(format!("{} {}", tr("Memory"), crate::widgets::format_bytes(mem as i64))).font(small.clone()).color(p.text_muted))
                    .on_hover_text(tr("Memory used by result sets"));
                // Time zone ("WIB"): a click chooses another.
                let mut changed = false;
                let tz = self.settings.general.time_zone;
                let tz_button = ui.add(egui::Button::new(RichText::new(crate::conn::time_zone_label(tz)).font(small.clone()).color(p.text_muted)).frame_when_inactive(false));
                let tz_tip = crate::conn::time_zone_choices().into_iter().find(|(c, _)| *c == tz).map(|(_, l)| l).unwrap_or_default();
                let tz_button = tz_button.on_hover_text(format!("{}: {tz_tip}", tr("Time zone")));
                egui::Popup::menu(&tz_button).show(|ui| {
                    let mut v = tz;
                    for (choice, label) in crate::conn::time_zone_choices() {
                        changed |= ui.radio_value(&mut v, choice, label).changed();
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
