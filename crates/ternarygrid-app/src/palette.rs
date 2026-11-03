//! Command palette (⇧⌘P): quick access to actions, connections, objects and scripts.

use crate::backend::Loadable;
use crate::command::{AppCommand, ObjectPage};
use crate::cx::AppContext;
use egui::{Key, RichText};
use egui_phosphor::regular as icon;
use ternarygrid_core::tr;

#[derive(Clone, Default)]
struct PaletteState {
    query: String,
    selected: usize,
    focus_requested: bool,
    /// Scripts and bookmarks of the project, read once per opening.
    project: Option<std::sync::Arc<(Vec<(String, std::path::PathBuf)>, Vec<ternarygrid_config::Bookmark>)>>,
}

enum Target {
    Script(std::path::PathBuf),
    Bookmark(String, ternarygrid_core::ObjectRef),
    Action(crate::keys::Action),
    Command(fn() -> AppCommand),
    Connect(String),
    Editor(String),
    Object(String, ternarygrid_core::ObjectRef),
}

struct Item {
    icon: &'static str,
    label: String,
    /// At the right: the category (commands), the connection and schema (objects), the
    /// server (connections).
    detail: String,
    /// The command's shortcut, drawn as key caps at the right end.
    keys: String,
    target: Target,
}

/// The palette's groups, each under its caption.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Group {
    Commands,
    Connections,
    Scripts,
    Objects,
}

impl Group {
    fn caption(self) -> &'static str {
        match self {
            Group::Commands => tr("Commands"),
            Group::Connections => tr("Connections"),
            Group::Scripts => tr("Scripts"),
            Group::Objects => tr("Objects"),
        }
    }
}

impl Item {
    fn group(&self) -> Group {
        match self.target {
            Target::Action(_) | Target::Command(_) => Group::Commands,
            Target::Connect(_) | Target::Editor(_) => Group::Connections,
            Target::Script(_) => Group::Scripts,
            Target::Bookmark(..) | Target::Object(..) => Group::Objects,
        }
    }
}

/// How well `pattern` matches `text` (case-insensitive; higher is better) and which characters
/// of `text` matched (char indices, for the highlight). Matches typed in one piece come first
/// (at the start, then at a word's start, then anywhere), then the word starts typed in pieces
/// ("nse" or "new ed" for "New SQL editor"), then letters scattered over the text.
pub fn fuzzy_match(pattern: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    let pat: Vec<char> = pattern.trim().chars().map(lower).collect();
    if pat.is_empty() {
        return Some((0, Vec::new()));
    }
    let orig: Vec<char> = text.chars().collect();
    let t: Vec<char> = orig.iter().copied().map(lower).collect();
    let starts: Vec<bool> =
        (0..t.len()).map(|i| i == 0 || matches!(orig[i - 1], ' ' | '_' | '.' | '-' | ':' | '/' | '(' | '·') || (orig[i - 1].is_lowercase() && orig[i].is_uppercase())).collect();
    let penalty = t.len() as i32 / 4;
    // In one piece.
    let at = |i: usize| t.len() >= i + pat.len() && t[i..i + pat.len()] == pat[..];
    let hits: Vec<usize> = (0..t.len()).filter(|&i| at(i)).collect();
    if let Some(&i) = hits.iter().find(|&&i| starts[i]).or(hits.first()) {
        let tier = if i == 0 {
            1000
        } else if starts[i] {
            800
        } else {
            600
        };
        return Some((tier - penalty, (i..i + pat.len()).collect()));
    }
    // Word starts in pieces: each piece the beginning of a later word.
    let pc: Vec<char> = pat.iter().copied().filter(|c| !c.is_whitespace()).collect();
    let mut positions = Vec::new();
    let (mut pi, mut ti) = (0usize, 0usize);
    while pi < pc.len() && ti < t.len() {
        if starts[ti] && t[ti] == pc[pi] {
            while pi < pc.len() && ti < t.len() && t[ti] == pc[pi] {
                positions.push(ti);
                pi += 1;
                ti += 1;
            }
        } else {
            ti += 1;
        }
    }
    if pi == pc.len() {
        return Some((500 - penalty, positions));
    }
    // Scattered letters.
    let mut score = 100i32;
    let mut positions = Vec::new();
    let mut ti = 0usize;
    for &c in &pc {
        let i = (ti..t.len()).find(|&i| t[i] == c)?;
        score += 10;
        if starts[i] {
            score += 8;
        }
        if positions.last().is_some_and(|&l| l + 1 == i) {
            score += 6;
        }
        positions.push(i);
        ti = i + 1;
    }
    Some((score - penalty, positions))
}

/// [`fuzzy_match`]'s score.
pub fn fuzzy(pattern: &str, text: &str) -> Option<i32> {
    fuzzy_match(pattern, text).map(|m| m.0)
}

/// How `it` matches `query`: by its label (the characters to highlight), else, ranked lower, by
/// its detail typed in one piece or by word starts (a connection's or a schema's name).
fn score(query: &str, it: &Item) -> Option<(i32, Vec<usize>)> {
    if let Some(m) = fuzzy_match(query, &it.label) {
        return Some(m);
    }
    match fuzzy_match(query, &it.detail) {
        Some((s, _)) if s >= 400 => Some((s - 450, Vec::new())),
        _ => None,
    }
}

fn collect_project(cx: &AppContext, state: &mut PaletteState) -> Vec<Item> {
    let data = state
        .project
        .get_or_insert_with(|| {
            let p = cx.services.projects.as_ref().and_then(|m| m.default_project().ok());
            let scripts = p.as_ref().and_then(|p| p.list_scripts().ok()).unwrap_or_default().into_iter().map(|s| (s.relative, s.path)).collect();
            let bookmarks = p.as_ref().and_then(|p| p.bookmarks().ok()).unwrap_or_default();
            std::sync::Arc::new((scripts, bookmarks))
        })
        .clone();
    let mut items = Vec::new();
    for (name, path) in &data.0 {
        items.push(Item { icon: icon::FILE_CODE, label: name.clone(), detail: String::new(), keys: String::new(), target: Target::Script(path.clone()) });
    }
    for b in &data.1 {
        items.push(Item {
            icon: icon::BOOKMARK_SIMPLE,
            label: b.label.clone(),
            detail: tr("Bookmark").to_string(),
            keys: String::new(),
            target: Target::Bookmark(b.connection_id.clone(), b.object.clone()),
        });
    }
    items
}

fn collect(cx: &AppContext) -> Vec<Item> {
    use crate::keys::ActionCategory as C;
    let command = |icon: &'static str, label: &str, category: C, target: fn() -> AppCommand| Item {
        icon,
        label: tr(label).into(),
        detail: tr(category.label()).into(),
        keys: String::new(),
        target: Target::Command(target),
    };
    let mut items = vec![
        command(icon::PLUS, "New connection…", C::Connections, || AppCommand::NewConnection { kind: None, folder: None }),
        command(icon::FILE_CODE, "New SQL editor", C::Sql, || AppCommand::NewSqlEditor { connection: None, schema: None, text: None }),
        command(icon::GEAR_SIX, "Settings", C::Application, || AppCommand::OpenSettings),
        command(icon::CLOCK_COUNTER_CLOCKWISE, "Query history", C::Tools, || AppCommand::OpenHistory),
        command(icon::LIST_CHECKS, "Tasks", C::Tools, || AppCommand::OpenTasks),
        command(icon::GIT_DIFF, "Compare schemas…", C::Tools, || AppCommand::OpenSchemaCompare { connection: None, schema: None }),
        command(icon::ARROWS_LEFT_RIGHT, "Compare data…", C::Tools, || AppCommand::OpenDataCompare { connection: None, table: None }),
        command(icon::SPARKLE, "AI assistant", C::Tools, || AppCommand::OpenAiAssistant { connection: None }),
        command(icon::DOWNLOAD_SIMPLE, "Import connections…", C::Connections, || AppCommand::ImportConnections),
        command(icon::UPLOAD_SIMPLE, "Export connections…", C::Connections, || AppCommand::ExportConnections(None)),
        command(icon::SIDEBAR, "Toggle navigator", C::View, || AppCommand::ToggleNavigator),
        command(icon::ACTIVITY, "Background operations", C::Tools, || AppCommand::ToggleOperations),
        command(icon::KEYBOARD, "Keyboard shortcuts", C::Help, || AppCommand::ShowShortcuts),
        command(icon::PLUGS, "Disconnect all", C::Connections, || AppCommand::DisconnectAll),
    ];
    // "New connection…" and "New connection" are one command.
    let key = |s: &str| s.trim_end_matches('…').to_lowercase();
    for a in crate::keys::Action::ALL {
        let label = crate::keys::action_label(*a).to_string();
        let (detail, keys) = (tr(a.category().label()).to_string(), crate::keys::label(cx.keymap, *a));
        // The same command listed above: it gets the category and shortcut.
        if let Some(it) = items.iter_mut().find(|it| key(&it.label) == key(&label)) {
            (it.detail, it.keys) = (detail, keys);
            continue;
        }
        items.push(Item { icon: crate::keys::action_icon(*a), label, detail, keys, target: Target::Action(*a) });
    }
    for c in cx.conns.configs() {
        let connected = cx.conns.is_connected(&c.id);
        if !connected {
            items.push(Item {
                icon: icon::PLUGS_CONNECTED,
                label: format!("{} {}", tr("Connect:"), c.name),
                detail: c.summary(),
                keys: String::new(),
                target: Target::Connect(c.id.clone()),
            });
        }
        items.push(Item {
            icon: icon::FILE_CODE,
            label: format!("{} {}", tr("SQL editor:"), c.name),
            detail: c.summary(),
            keys: String::new(),
            target: Target::Editor(c.id.clone()),
        });
    }
    for id in cx.conns.live_ids() {
        let Some(live) = cx.conns.live(&id) else { continue };
        for ((schema, kind), list) in &live.meta.objects {
            if let Loadable::Loaded(objs) = list {
                for o in objs.iter().take(5000) {
                    let detail = match &schema.schema {
                        Some(s) => format!("{} · {s}", live.config.name),
                        None => live.config.name.clone(),
                    };
                    items.push(Item {
                        icon: crate::nav::kind_icon(*kind),
                        label: o.obj.name.clone(),
                        detail,
                        keys: String::new(),
                        target: Target::Object(id.clone(), o.obj.clone()),
                    });
                }
            }
        }
    }
    items
}

/// Forget the previous query and selection (the palette is being opened).
pub fn reset(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<PaletteState>(egui::Id::new("ternarygrid_palette")));
}

/// One full-width row: the coloured icon, the label with the characters that matched in the
/// accent, and on the right the detail (category, connection, server) in small faint text and
/// the shortcut as key caps.
fn row(ui: &mut egui::Ui, it: &Item, hits: &[usize], selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
    let p = ternarygrid_ui::pal(ui);
    if selected {
        ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, p.accent_soft);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, p.hover);
    }
    let painter = ui.painter().clone();
    let cy = rect.center().y;
    let icon_color = if selected { p.accent_text } else { ternarygrid_icons::color(it.icon).unwrap_or(p.text_muted) };
    // As in the design: the icon 12 points in, the label 12 after it (13.5).
    painter.text(rect.left_center() + egui::vec2(19.5, 0.0), egui::Align2::CENTER_CENTER, it.icon, ternarygrid_ui::style::icon_font(15.0), icon_color);
    let label_x = rect.left() + 39.0;
    let mut right = rect.right() - 12.0;
    if !it.keys.is_empty() {
        right = ternarygrid_ui::kit::paint_kbd_text(&painter, ui.ctx(), &it.keys, right, cy, &p).left() - 10.0;
    }
    if !it.detail.is_empty() {
        let width = (rect.width() * 0.45).min(right - label_x - 80.0).max(20.0);
        let g = egui::WidgetText::from(RichText::new(&it.detail).size(12.0).color(p.text_faint)).into_galley(ui, Some(egui::TextWrapMode::Truncate), width, egui::TextStyle::Small);
        let w = g.size().x;
        painter.galley(egui::pos2(right - w, cy - g.size().y / 2.0), g, p.text_faint);
        right -= w + 16.0;
    }
    let mut job = egui::text::LayoutJob::default();
    let (plain, matched) = (egui::FontId::proportional(13.5), ternarygrid_ui::style::medium(ui.ctx(), 13.5));
    for (i, c) in it.label.chars().enumerate() {
        let hit = hits.contains(&i);
        let format = egui::TextFormat::simple(if hit { matched.clone() } else { plain.clone() }, if hit { p.accent_text } else { p.text });
        job.append(c.encode_utf8(&mut [0; 4]), 0.0, format);
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - label_x).max(40.0));
    let label = painter.layout_job(job);
    painter.galley(egui::pos2(label_x, cy - label.size().y / 2.0), label, p.text);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &it.label));
    resp
}

/// A group's caption over its rows: small, medium and faint, as the menus' captions.
fn caption(ui: &mut egui::Ui, text: &str, first: bool) {
    let p = ternarygrid_ui::pal(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), if first { 22.0 } else { 30.0 }), egui::Sense::hover());
    let font = ternarygrid_ui::style::medium(ui.ctx(), ternarygrid_ui::style::SMALL);
    ui.painter().text(egui::pos2(rect.left() + 12.0, rect.bottom() - 6.0), egui::Align2::LEFT_BOTTOM, text, font, p.text_faint);
}

pub fn show(ctx: &egui::Context, cx: &mut AppContext, open: &mut bool) {
    let id = egui::Id::new("ternarygrid_palette");
    let mut state: PaletteState = ctx.data_mut(|d| d.get_temp(id)).unwrap_or_default();
    let mut items = collect(cx);
    items.extend(collect_project(cx, &mut state));
    let mut scored: Vec<(i32, Vec<usize>, &Item)> = items.iter().filter_map(|it| score(&state.query, it).map(|(s, hits)| (s, hits, it))).collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    scored.truncate(60);
    // In groups under their captions: the group of the best match first (before typing:
    // commands, connections, scripts, objects), each sorted by score.
    let best = |g: Group| scored.iter().filter(|s| s.2.group() == g).map(|s| s.0).max();
    let mut groups: Vec<Group> = [Group::Commands, Group::Connections, Group::Scripts, Group::Objects].into_iter().filter(|g| best(*g).is_some()).collect();
    if !state.query.trim().is_empty() {
        groups.sort_by_key(|g| std::cmp::Reverse(best(*g)));
    }
    scored.sort_by_key(|s| (groups.iter().position(|g| *g == s.2.group()), std::cmp::Reverse(s.0)));
    if state.selected >= scored.len() {
        state.selected = scored.len().saturating_sub(1);
    }
    let mut chosen: Option<usize> = None;
    let (up, down, enter, esc) = ctx.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown), i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    if esc {
        *open = false;
    }
    if down {
        state.selected = (state.selected + 1).min(scored.len().saturating_sub(1));
    }
    if up {
        state.selected = state.selected.saturating_sub(1);
    }
    if enter && !scored.is_empty() {
        chosen = Some(state.selected);
    }
    // 620 points wide, 59 below the window's top, as in the design.
    let area = egui::Area::new(id.with("area")).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 59.0)).order(egui::Order::Foreground).show(ctx, |ui| {
        let p = ternarygrid_ui::pal(ui);
        let frame =
            egui::Frame::new().fill(p.surface).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(ternarygrid_ui::style::RADIUS_LG + 2).shadow(ui.visuals().window_shadow);
        frame.show(ui, |ui| {
            ui.set_width(620.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            let line = egui::Stroke::new(1.0, p.border);
            // The search: a large frameless field with a magnifier painted in its margin.
            let r = ui.add(
                egui::TextEdit::singleline(&mut state.query)
                    .hint_text(RichText::new(tr("Type a command, connection or object…")).color(p.text_faint))
                    .font(egui::FontId::proportional(15.0))
                    // A custom frame brings its own margin (the field's `margin` is then unused).
                    .frame(egui::Frame::new().inner_margin(egui::Margin { left: 45, right: 16, top: 0, bottom: 0 }))
                    .min_size(egui::vec2(0.0, 52.0))
                    .vertical_align(egui::Align::Center)
                    .desired_width(f32::INFINITY),
            );
            let glass = egui::pos2(r.rect.left() + 24.5, r.rect.center().y);
            ui.painter().text(glass, egui::Align2::CENTER_CENTER, icon::MAGNIFYING_GLASS, ternarygrid_ui::style::icon_font(17.0), p.text_faint);
            if !state.focus_requested {
                r.request_focus();
                state.focus_requested = true;
            }
            if r.changed() {
                state.selected = 0;
            }
            let y = ui.cursor().top();
            ui.painter().hline(ui.max_rect().x_range(), y + 0.5, line);
            ui.add_space(1.0);
            egui::Frame::new().inner_margin(6).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                    let mut group = None;
                    for (i, (_, hits, it)) in scored.iter().enumerate() {
                        if group != Some(it.group()) {
                            caption(ui, it.group().caption(), group.is_none());
                            group = Some(it.group());
                        }
                        let sel = i == state.selected;
                        let resp = row(ui, it, hits, sel);
                        if sel {
                            resp.scroll_to_me(None);
                        }
                        if resp.clicked() {
                            chosen = Some(i);
                        }
                    }
                    if scored.is_empty() {
                        ui.add_space(8.0);
                        ui.label(RichText::new(tr("No matches")).color(p.text_muted));
                        ui.add_space(8.0);
                    }
                });
            });
            // How to use it, quietly.
            let y = ui.cursor().top();
            ui.painter().hline(ui.max_rect().x_range(), y + 0.5, line);
            ui.add_space(1.0);
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (keys, what) in [("↑↓", tr("move")), ("↵", tr("open")), ("esc", tr("close"))] {
                        ternarygrid_ui::kit::kbd(ui, keys);
                        ui.label(RichText::new(what).size(11.5).color(p.text_faint));
                    }
                });
            });
        });
    });
    // A click anywhere else closes the palette.
    if ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p))) {
        *open = false;
    }
    if let Some(i) = chosen {
        if let Some((_, _, it)) = scored.get(i) {
            match &it.target {
                Target::Action(a) => cx.push(AppCommand::Action(*a)),
                Target::Script(p) => cx.push(AppCommand::OpenScript(p.clone())),
                Target::Bookmark(c, o) => {
                    if !cx.conns.is_connected(c) {
                        cx.push(AppCommand::Connect(c.clone()));
                    }
                    cx.push(AppCommand::OpenObject { connection: c.clone(), object: o.clone(), page: ObjectPage::Properties });
                }
                Target::Command(f) => cx.push(f()),
                Target::Connect(c) => cx.push(AppCommand::Connect(c.clone())),
                Target::Editor(c) => {
                    cx.push(AppCommand::Connect(c.clone()));
                    cx.push(AppCommand::NewSqlEditor { connection: Some(c.clone()), schema: None, text: None });
                }
                Target::Object(c, o) => cx.push(AppCommand::OpenObject { connection: c.clone(), object: o.clone(), page: ObjectPage::Properties }),
            }
        }
        *open = false;
    }
    if *open {
        ctx.data_mut(|d| d.insert_temp(id, state));
    } else {
        ctx.data_mut(|d| d.remove::<PaletteState>(id));
    }
}

#[cfg(test)]
mod tests {
    use super::{fuzzy, fuzzy_match};

    #[test]
    fn fuzzy_ranks_prefix_higher() {
        let a = fuzzy("ne", "New connection").unwrap();
        let b = fuzzy("ne", "Open SQL editor: Neon").unwrap();
        assert!(a > b);
        assert!(fuzzy("xyz", "New connection").is_none());
        // A word typed in one piece beats letters spread over several words.
        assert!(fuzzy("set", "Settings").unwrap() > fuzzy("set", "Reset zoom").unwrap());
        assert!(fuzzy("set", "Settings").unwrap() > fuzzy("set", "Session manager Tools").unwrap());
        assert!(fuzzy("export", "Export data…").unwrap() > fuzzy("export", "Open recent export folder").unwrap());
    }

    #[test]
    fn fuzzy_prefers_word_starts_to_scattered_letters() {
        // "ord": the tables and the script named so, then letters spread over a label.
        let order = fuzzy("ord", "order_line").unwrap();
        let script = fuzzy("ord", "reports/orders_daily.sql").unwrap();
        let spread = fuzzy("ord", "Morph to delimited list").unwrap();
        let export = fuzzy("ord", "Export data…").unwrap();
        assert!(order > script && script > spread && script > export);
        // Word starts typed in pieces beat scattered letters.
        assert!(fuzzy("nse", "New SQL editor").unwrap() > fuzzy("nse", "Toggle line comment").unwrap_or(i32::MIN));
        // The matched characters, for the highlight.
        assert_eq!(fuzzy_match("ord", "reports/orders_daily.sql").unwrap().1, vec![8, 9, 10]);
        assert_eq!(fuzzy_match("nse", "New SQL editor").unwrap().1, vec![0, 4, 8]);
    }
}
