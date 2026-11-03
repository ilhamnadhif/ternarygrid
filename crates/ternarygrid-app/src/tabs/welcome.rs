//! Start page: the editor area when no tab is open.

use crate::command::AppCommand;
use crate::cx::AppContext;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use ternarygrid_core::tr;

/// Most recently modified scripts of the default project (cached for a few seconds).
fn recent_scripts(ui: &Ui, cx: &AppContext) -> Vec<ternarygrid_config::project::ScriptInfo> {
    let id = egui::Id::new("welcome_recent_scripts");
    let now = ui.input(|i| i.time);
    if let Some((t, list)) = ui.data(|d| d.get_temp::<(f64, std::sync::Arc<Vec<ternarygrid_config::project::ScriptInfo>>)>(id))
        && now - t < 5.0
    {
        return (*list).clone();
    }
    let mut list = cx.services.projects.as_ref().and_then(|p| p.default_project().ok()).and_then(|p| p.list_scripts().ok()).unwrap_or_default();
    list.sort_by_key(|s| std::cmp::Reverse(s.modified));
    list.truncate(8);
    ui.data_mut(|d| d.insert_temp(id, (now, std::sync::Arc::new(list.clone()))));
    list
}

/// A clickable two-line row (46 points): the logo or icon, the name, a faint detail line (cut
/// with "…") and an optional pill at the right (the connection type).
fn recent_row(
    ui: &mut Ui,
    icon: &str,
    logo: Option<ternarygrid_core::DbKind>,
    icon_color: egui::Color32,
    name: &str,
    detail: &str,
    tag: Option<(String, egui::Color32)>,
) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 46.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    if resp.hovered() {
        ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, p.hover);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(rect, ternarygrid_ui::style::RADIUS, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
    }
    let logo_rect = egui::Rect::from_center_size(egui::pos2(rect.left() + 20.0, rect.center().y), egui::vec2(20.0, 20.0));
    if !logo.is_some_and(|kind| crate::dblogo::paint(ui, kind, logo_rect, icon_color)) {
        ui.painter().text(logo_rect.center(), egui::Align2::CENTER_CENTER, icon, ternarygrid_ui::style::icon_font(18.0), icon_color);
    }
    let mut right = rect.right() - 10.0;
    if let Some((text, color)) = &tag {
        let g = ui.painter().layout_no_wrap(text.clone(), ternarygrid_ui::style::medium(ui.ctx(), ternarygrid_ui::style::SMALL), *color);
        let size = egui::vec2(g.size().x + 14.0, 18.0);
        let r = egui::Rect::from_min_size(egui::pos2(right - size.x, rect.center().y - size.y / 2.0), size);
        let fill = if *color == p.err { p.prod_soft } else { ternarygrid_ui::soft(*color, if ui.visuals().dark_mode { 0.18 } else { 0.11 }) };
        ui.painter().rect_filled(r, (size.y / 2.0) as u8, fill);
        ui.painter().galley(r.center() - g.size() / 2.0, g, *color);
        right = r.left() - 8.0;
    }
    let left = rect.left() + 42.0;
    let max_w = (right - left).max(20.0);
    let size = ternarygrid_ui::style::body_size(ui.ctx()) + 0.5;
    let line = |text: &str, font: egui::FontId, color: egui::Color32| {
        egui::WidgetText::from(RichText::new(text).font(font).color(color)).into_galley(ui, Some(egui::TextWrapMode::Truncate), max_w, egui::TextStyle::Body)
    };
    let top = line(name, ternarygrid_ui::style::medium(ui.ctx(), size), p.text);
    let bottom = line(detail, egui::FontId::proportional(11.5), p.text_faint);
    let y = rect.center().y - (top.size().y + bottom.size().y + 2.0) / 2.0;
    let top_h = top.size().y;
    ui.painter().galley(egui::pos2(left, y), top, p.text);
    ui.painter().galley(egui::pos2(left, y + top_h + 2.0), bottom, p.text_faint);
    resp
}

/// A full-width action (38 points): its icon, its name and its shortcut, framed only when hovered.
fn action_row(ui: &mut Ui, ic: &str, label: &str, keys: &str) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if resp.hovered() {
            painter.rect_filled(rect, ternarygrid_ui::style::RADIUS, p.hover);
        }
        if resp.has_focus() {
            painter.rect_stroke(rect, ternarygrid_ui::style::RADIUS, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
        }
        let cy = rect.center().y;
        painter.text(egui::pos2(rect.left() + 20.0, cy), egui::Align2::CENTER_CENTER, ic, ternarygrid_ui::style::icon_font(16.0), p.text_muted);
        let size = ternarygrid_ui::style::body_size(ui.ctx()) + 0.5;
        painter.text(egui::pos2(rect.left() + 40.0, cy), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(size), p.text);
        if !keys.is_empty() {
            let g = painter.layout_no_wrap(keys.to_string(), egui::FontId::proportional(ternarygrid_ui::style::SMALL), p.text_muted);
            let r = egui::Rect::from_min_size(egui::pos2(rect.right() - 12.0 - g.size().x - 10.0, cy - (g.size().y + 4.0) / 2.0), g.size() + egui::vec2(10.0, 4.0));
            ternarygrid_ui::kit::paint_kbd(painter, r, &p);
            painter.galley(r.center() - g.size() / 2.0, g, p.text_muted);
        }
    }
    resp
}

/// The start page's main action: a full-width primary button, 36 points high.
fn new_connection_button(ui: &mut Ui) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("New connection")));
    if ui.is_rect_visible(rect) {
        let fill = if resp.hovered() { p.accent_hover } else { p.accent };
        ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, fill);
        if resp.has_focus() {
            ui.painter().rect_stroke(rect.expand(2.0), ternarygrid_ui::style::RADIUS + 2, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Outside);
        }
        // The plus, 8 points, the text (13.5, medium), centred.
        let size = ternarygrid_ui::style::body_size(ui.ctx()) + 0.5;
        let plus = ui.painter().layout_no_wrap(icon::PLUS.to_string(), ternarygrid_ui::style::icon_font(15.0), p.on_accent);
        let g = ui.painter().layout_no_wrap(tr("New connection").to_string(), ternarygrid_ui::style::medium(ui.ctx(), size), p.on_accent);
        let w = plus.size().x + 8.0 + g.size().x;
        let x = rect.center().x - w / 2.0;
        let plus_w = plus.size().x;
        ui.painter().galley(egui::pos2(x, rect.center().y - plus.size().y / 2.0), plus, p.on_accent);
        ui.painter().galley(egui::pos2(x + plus_w + 8.0, rect.center().y - g.size().y / 2.0), g, p.on_accent);
    }
    resp
}

/// The app's mark: the grid on a rounded accent square.
fn app_mark(ui: &mut Ui) {
    let p = ternarygrid_ui::pal(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 14, p.accent);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon::TABLE, ternarygrid_ui::style::icon_font(30.0), p.on_accent);
}

/// Draw the start page in `ui` (new connection, recent connections and scripts).
pub fn show(ui: &mut Ui, cx: &mut AppContext) {
    use crate::keys::Action;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let p = ternarygrid_ui::pal(ui);
        // As in the design: a first start (no connections) shows the version, 70 points
        // down; with connections the cards come up a little (60).
        let first_start = cx.conns.configs().is_empty();
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add_space(if first_start { 70.0 } else { 60.0 });
        ui.vertical_centered(|ui| {
            app_mark(ui);
            ui.add_space(12.0);
            ternarygrid_ui::kit::semibold_label(ui, "TernaryGrid", 28.0, p.text);
            ui.add_space(6.0);
            ui.label(RichText::new(tr("A fast, lightweight database client")).size(14.0).color(p.text_muted));
            if first_start {
                ui.add_space(6.0);
                ui.label(RichText::new(format!("{} {}", tr("Version"), env!("CARGO_PKG_VERSION"))).size(12.0).color(p.text_faint));
            }
        });
        ui.add_space(if first_start { 30.0 } else { 28.0 });
        // Two cards side by side when there is room (as tall as each other), else stacked:
        // 380 and 400 points of content, padded by 18, 24 apart.
        let (start_w, recent_w) = (380.0, if first_start { 380.0 } else { 400.0 });
        let wide = ui.available_width() >= start_w + recent_w + 2.0 * 38.0 + 48.0;
        let total = if wide { start_w + recent_w + 2.0 * 38.0 + 24.0 } else { start_w.max(recent_w) + 38.0 };
        let margin = ((ui.available_width() - total) / 2.0).max(8.0);
        let height_id = egui::Id::new("welcome_card_height");
        let min_h: f32 = if wide { ui.data(|d| d.get_temp(height_id)).unwrap_or(0.0) } else { 0.0 };
        let tallest = std::cell::Cell::new(0.0f32);
        let card = |ui: &mut Ui, title: &str, width: f32, gap: f32, add: &mut dyn FnMut(&mut Ui)| {
            let shown = ternarygrid_ui::kit::card(ui).inner_margin(18).show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.set_min_height(min_h);
                    ui.spacing_mut().item_spacing.y = gap;
                    let size = ternarygrid_ui::style::body_size(ui.ctx()) + 0.5;
                    ternarygrid_ui::kit::semibold_label(ui, title, size, p.text);
                    add(ui);
                })
                .response
                .rect
                .height()
            });
            tallest.set(tallest.get().max(shown.inner));
        };
        let keymap = cx.keymap;
        let shortcut = |a: Action| crate::keys::label(keymap, a);
        // The cards only read the context; their commands run afterwards.
        let cmds = std::cell::RefCell::new(Vec::new());
        let push = |c: AppCommand| cmds.borrow_mut().push(c);
        let ro: &AppContext = cx;
        let mut start = |ui: &mut Ui| {
            if new_connection_button(ui).clicked() {
                push(AppCommand::NewConnection { kind: None, folder: None });
            }
            let items: [(&str, &str, Option<Action>, Option<&str>, AppCommand); 4] = [
                (icon::FILE_CODE, tr("New SQL editor"), Some(Action::NewSqlEditor), None, AppCommand::NewSqlEditor { connection: None, schema: None, text: None }),
                (icon::MAGNIFYING_GLASS, tr("Search or run a command"), Some(Action::CommandPalette), None, AppCommand::ShowPalette),
                (icon::DOWNLOAD_SIMPLE, tr("Import connections…"), None, Some(tr("From DBeaver, ~/.pgpass or a connection URL")), AppCommand::ImportConnections),
                (icon::DATABASE, tr("Create sample database"), None, Some(tr("A small SQLite shop database to try every feature")), AppCommand::CreateSampleDatabase),
            ];
            for (ic, label, action, tip, cmd) in items {
                let keys = action.map(shortcut).unwrap_or_default();
                let mut r = action_row(ui, ic, label, &keys);
                if let Some(t) = tip {
                    r = r.on_hover_text(t);
                }
                if r.clicked() {
                    push(cmd);
                }
            }
        };
        let mut recent_ui = |ui: &mut Ui| {
            let mut recent: Vec<_> = ro.conns.configs().iter().filter(|c| c.last_connected.is_some()).cloned().collect();
            recent.sort_by_key(|c| std::cmp::Reverse(c.last_connected));
            let pal = ternarygrid_ui::pal(ui);
            ui.label(RichText::new(tr("Connections")).size(11.5).color(pal.text_faint));
            if recent.is_empty() {
                ui.label(RichText::new(tr("Connections you open appear here.")).color(pal.text_muted));
            }
            for c in recent.into_iter().take(6) {
                let detail = crate::nav::connection_detail(&c);
                let ty = ro.conns.type_of(&c.id);
                let tag = crate::theme::marker(ty.color, ui.visuals().dark_mode).map(|m| (ty.name.to_uppercase(), m));
                if recent_row(ui, crate::nav::db_icon(c.kind), Some(c.kind), crate::theme::logo_color(c.kind, ui.visuals().dark_mode), &c.name, &detail, tag)
                    .on_hover_text(format!("{detail}\n{}", tr("Connect and open a SQL editor")))
                    .clicked()
                {
                    push(AppCommand::Connect(c.id.clone()));
                    push(AppCommand::NewSqlEditor { connection: Some(c.id.clone()), schema: None, text: None });
                }
            }
            let scripts = recent_scripts(ui, ro);
            if !scripts.is_empty() {
                ui.label(RichText::new(tr("Scripts")).size(11.5).color(pal.text_faint));
                // Where a script is: from its project's folder ("General/Scripts/reports").
                let root = ro.services.projects.as_ref().and_then(|m| m.default_project().ok()).and_then(|p| p.path().parent().map(|r| r.to_path_buf()));
                for s in scripts.into_iter().take(6) {
                    let parent = s.path.parent().unwrap_or(&s.path);
                    let dir = root.as_ref().and_then(|r| parent.strip_prefix(r).ok()).unwrap_or(parent).display().to_string();
                    let name = s.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| s.relative.clone());
                    let r = recent_row(ui, icon::FILE_CODE, None, ternarygrid_icons::SKY, &name, &dir, None);
                    if r.on_hover_text(s.path.display().to_string()).clicked() {
                        push(AppCommand::OpenScript(s.path.clone()));
                    }
                }
            }
        };
        ui.horizontal_top(|ui| {
            ui.add_space(margin);
            if wide {
                ui.spacing_mut().item_spacing.x = 0.0;
                card(ui, tr("Start"), start_w, 6.0, &mut start);
                ui.add_space(24.0);
                card(ui, tr("Recent"), recent_w, 4.0, &mut recent_ui);
            } else {
                ui.vertical(|ui| {
                    card(ui, tr("Start"), start_w, 6.0, &mut start);
                    ui.add_space(16.0);
                    card(ui, tr("Recent"), recent_w, 4.0, &mut recent_ui);
                });
            }
        });
        if (tallest.get() - min_h).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(height_id, tallest.get()));
            ui.ctx().request_repaint();
        }
        for c in cmds.take() {
            cx.push(c);
        }
    });
}
