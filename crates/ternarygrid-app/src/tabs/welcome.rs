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

/// A clickable two-line row: icon, name, and a detail line (cut with "…").
fn recent_row(ui: &mut Ui, icon: &str, logo: Option<ternarygrid_core::DbKind>, icon_color: egui::Color32, name: &str, detail: &str) -> egui::Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 42.0), egui::Sense::click());
    let v = ui.visuals();
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, v.widgets.hovered.weak_bg_fill);
    }
    let text_color = v.text_color();
    let weak = v.weak_text_color();
    let painter = ui.painter_at(rect);
    let logo_rect = egui::Rect::from_center_size(egui::pos2(rect.left() + 18.0, rect.center().y), egui::vec2(18.0, 18.0));
    if !logo.is_some_and(|kind| crate::dblogo::paint(ui, kind, logo_rect, icon_color)) {
        painter.text(egui::pos2(rect.left() + 10.0, rect.center().y), egui::Align2::LEFT_CENTER, icon, egui::TextStyle::Body.resolve(ui.style()), icon_color);
    }
    let left = rect.left() + 34.0;
    let max_w = (rect.right() - left - 8.0).max(20.0);
    let line = |text: &str, style: egui::TextStyle, color: egui::Color32| {
        egui::WidgetText::from(RichText::new(text).color(color)).into_galley(ui, Some(egui::TextWrapMode::Truncate), max_w, style)
    };
    let top = line(name, egui::TextStyle::Body, text_color);
    let bottom = line(detail, egui::TextStyle::Small, weak);
    let y = rect.center().y - (top.size().y + bottom.size().y + 2.0) / 2.0;
    let top_h = top.size().y;
    painter.galley(egui::pos2(left, y), top, text_color);
    painter.galley(egui::pos2(left, y + top_h + 2.0), bottom, weak);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    resp
}

/// A full-width action: its icon, its name and its shortcut, framed only when hovered.
fn action_row(ui: &mut Ui, ic: &str, label: &str, keys: &str) -> egui::Response {
    let p = ternarygrid_ui::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
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
        let size = ternarygrid_ui::style::body_size(ui.ctx());
        painter.text(egui::pos2(rect.left() + 40.0, cy), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(size), p.text);
        if !keys.is_empty() {
            let g = painter.layout_no_wrap(keys.to_string(), egui::FontId::proportional(ternarygrid_ui::style::SMALL), p.text_muted);
            let r = egui::Rect::from_min_size(egui::pos2(rect.right() - 10.0 - g.size().x - 10.0, cy - (g.size().y + 4.0) / 2.0), g.size() + egui::vec2(10.0, 4.0));
            ternarygrid_ui::kit::paint_kbd(painter, r, &p);
            painter.galley(r.center() - g.size() / 2.0, g, p.text_muted);
        }
    }
    resp
}

/// Draw the start page in `ui` (new connection, recent connections and scripts).
pub fn show(ui: &mut Ui, cx: &mut AppContext) {
    use crate::keys::Action;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let p = ternarygrid_ui::pal(ui);
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("TernaryGrid").font(ternarygrid_ui::style::semibold(ui.ctx(), 30.0)).color(p.text));
            ui.add_space(2.0);
            ui.label(RichText::new(tr("A fast, lightweight database client")).color(p.text_muted));
            ui.label(RichText::new(format!("{} {}", tr("Version"), env!("CARGO_PKG_VERSION"))).size(ternarygrid_ui::style::SMALL).color(p.text_faint));
        });
        ui.add_space(28.0);
        // Two cards side by side when there is room, else stacked.
        let card_w = 340.0;
        let wide = ui.available_width() >= card_w * 2.0 + 48.0;
        let total = if wide { card_w * 2.0 + 24.0 } else { card_w };
        let margin = ((ui.available_width() - total) / 2.0).max(8.0);
        let card = |ui: &mut Ui, title: &str, add: &mut dyn FnMut(&mut Ui)| {
            ternarygrid_ui::kit::card(ui).corner_radius(ternarygrid_ui::style::RADIUS_LG).inner_margin(16).show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(card_w - 34.0);
                    ui.label(ternarygrid_ui::kit::section_title(ui, title));
                    ui.add_space(10.0);
                    add(ui);
                });
            });
        };
        let keymap = cx.keymap;
        let shortcut = |a: Action| crate::keys::label(keymap, a);
        // The cards only read the context; their commands run afterwards.
        let cmds = std::cell::RefCell::new(Vec::new());
        let push = |c: AppCommand| cmds.borrow_mut().push(c);
        let ro: &AppContext = cx;
        let mut start = |ui: &mut Ui| {
            let full = egui::vec2(ui.available_width(), 34.0);
            let pal = ternarygrid_ui::pal(ui);
            let label =
                RichText::new(format!("{}  {}", icon::PLUS, tr("New connection"))).font(ternarygrid_ui::style::medium(ui.ctx(), ternarygrid_ui::style::body_size(ui.ctx())));
            let primary = egui::Button::new(label.color(pal.on_accent)).fill(pal.accent).stroke(egui::Stroke::NONE).corner_radius(ternarygrid_ui::style::RADIUS).min_size(full);
            if ui.add(primary).clicked() {
                push(AppCommand::NewConnection { kind: None, folder: None });
            }
            ui.add_space(6.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            let items: [(&str, &str, Option<Action>, Option<&str>, AppCommand); 4] = [
                (icon::FILE_SQL, tr("New SQL editor"), Some(Action::NewSqlEditor), None, AppCommand::NewSqlEditor { connection: None, schema: None, text: None }),
                (icon::COMMAND, tr("Command palette"), Some(Action::CommandPalette), None, AppCommand::ShowPalette),
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
            ui.label(RichText::new(tr("Connections")).size(ternarygrid_ui::style::SMALL).color(pal.text_faint));
            if recent.is_empty() {
                ui.label(RichText::new(tr("Connections you open appear here.")).color(pal.text_muted));
            }
            for c in recent.into_iter().take(6) {
                let detail = crate::nav::connection_detail(&c);
                if recent_row(ui, crate::nav::db_icon(c.kind), Some(c.kind), crate::theme::logo_color(c.kind, ui.visuals().dark_mode), &c.name, &detail)
                    .on_hover_text(format!("{detail}\n{}", tr("Connect and open a SQL editor")))
                    .clicked()
                {
                    push(AppCommand::Connect(c.id.clone()));
                    push(AppCommand::NewSqlEditor { connection: Some(c.id.clone()), schema: None, text: None });
                }
            }
            let scripts = recent_scripts(ui, ro);
            if !scripts.is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new(tr("Scripts")).size(ternarygrid_ui::style::SMALL).color(pal.text_faint));
                for s in scripts.into_iter().take(6) {
                    let dir = s.path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                    let r = recent_row(ui, icon::FILE_SQL, None, ternarygrid_icons::SKY, &s.relative, &dir);
                    if r.on_hover_text(s.path.display().to_string()).clicked() {
                        push(AppCommand::OpenScript(s.path.clone()));
                    }
                }
            }
        };
        ui.horizontal_top(|ui| {
            ui.add_space(margin);
            if wide {
                card(ui, tr("Start"), &mut start);
                ui.add_space(24.0);
                card(ui, tr("Recent"), &mut recent_ui);
            } else {
                ui.vertical(|ui| {
                    card(ui, tr("Start"), &mut start);
                    ui.add_space(16.0);
                    card(ui, tr("Recent"), &mut recent_ui);
                });
            }
        });
        for c in cmds.take() {
            cx.push(c);
        }
    });
}
