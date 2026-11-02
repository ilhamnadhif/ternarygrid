//! Query builder UI: canvas with table boxes (tick columns, drag a column
//! onto another table's column to join, right click a join to change its
//! type), grids for columns / conditions / grouping / ordering and a live
//! SQL preview.

use super::model::{
    Aggregate, Connector, JoinKind, OPERATORS, QB_FONT, QB_HEADER, QB_ROW, QbCondition, QbJoin,
    QbSelectItem, QueryModel,
};
use crate::common::paint::{self, font_px};
use crate::common::view::{ViewTransform, distance_to_polyline, handle_pan_zoom, round_pos};
use crate::er::scene::route;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Id, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    Ui, Vec2, pos2, vec2,
};
use egui_phosphor::regular as icon;
use std::fmt;
use std::sync::Arc;
use ternarygrid_core::dialect::GenericDialect;
use ternarygrid_core::{Dialect, tr, trf};
use ternarygrid_ui::{kit, parts, style};

/// Bottom tabs of the builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum QbTab {
    /// Select list.
    #[default]
    Columns,
    /// WHERE conditions.
    Conditions,
    /// GROUP BY and HAVING.
    Grouping,
    /// ORDER BY.
    Order,
}

impl QbTab {
    const ALL: [QbTab; 4] = [
        QbTab::Columns,
        QbTab::Conditions,
        QbTab::Grouping,
        QbTab::Order,
    ];
    fn label(self) -> &'static str {
        match self {
            QbTab::Columns => "Columns",
            QbTab::Conditions => "Conditions",
            QbTab::Grouping => "Grouping",
            QbTab::Order => "Order",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum QbDrag {
    Pan,
    MoveTable(String),
    Join {
        alias: String,
        column: String,
        current: Pos2,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum QbHit {
    Header(String),
    Close(String),
    Row(String, String),
    Join(usize),
}

#[derive(Debug, Clone, PartialEq)]
enum QbContext {
    Join(usize),
    Table(String),
    Background,
}

/// View state of the query builder.
pub struct QbViewState {
    /// Dialect used for quoting and LIMIT syntax in the generated SQL.
    pub dialect: Arc<dyn Dialect>,
    /// Canvas zoom.
    pub zoom: f32,
    /// Canvas pan offset.
    pub pan: Vec2,
    /// Active bottom tab.
    pub tab: QbTab,
    /// Show the toolbar.
    pub show_toolbar: bool,
    drag: Option<QbDrag>,
    context: Option<QbContext>,
    alias_edit: String,
    last_canvas: Rect,
}

impl fmt::Debug for QbViewState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QbViewState")
            .field("dialect", &self.dialect.kind())
            .field("zoom", &self.zoom)
            .field("pan", &self.pan)
            .field("tab", &self.tab)
            .finish_non_exhaustive()
    }
}

impl Default for QbViewState {
    fn default() -> Self {
        Self::new(Arc::new(GenericDialect::new()))
    }
}

impl QbViewState {
    /// State generating SQL for `dialect`.
    pub fn new(dialect: Arc<dyn Dialect>) -> Self {
        QbViewState {
            dialect,
            zoom: 1.0,
            pan: vec2(10.0, 10.0),
            tab: QbTab::Columns,
            show_toolbar: true,
            drag: None,
            context: None,
            alias_edit: String::new(),
            last_canvas: Rect::NOTHING,
        }
    }
}

/// Something the application has to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QbEvent {
    /// The user wants to add a table: show a picker and call
    /// [`QueryModel::add_table`].
    AddTableRequested,
    /// Run the generated SQL.
    RunQuery(String),
    /// Open the generated SQL in a SQL editor.
    OpenInEditor(String),
    /// The model changed (save / mark dirty).
    Changed,
}

/// Result of [`show_query_builder`].
#[derive(Debug, Clone, Default)]
pub struct QbResponse {
    /// Events produced this frame.
    pub events: Vec<QbEvent>,
    /// SQL of the current model.
    pub sql: String,
}

/// Shows the query builder filling the available space.
pub fn show_query_builder(
    ui: &mut Ui,
    model: &mut QueryModel,
    state: &mut QbViewState,
) -> QbResponse {
    let id = ui.id().with("ternarygrid_qb");
    let before = model.clone();
    let mut events = Vec::new();
    let sql_now = model.to_sql(state.dialect.as_ref());
    let p = style::pal(ui);
    ui.spacing_mut().item_spacing.y = 0.0;
    if state.show_toolbar {
        toolbar(ui, model, &sql_now, &mut events);
    }
    // The canvas 270 high as in the design.
    let bottom = (ui.available_height() - 270.0).max(240.0);
    egui::Panel::bottom(id.with("bottom"))
        .resizable(true)
        .default_size(bottom)
        .frame(egui::Frame::new().fill(p.canvas))
        .show(ui, |ui| {
            egui::Panel::right(id.with("sql"))
                .resizable(true)
                .default_size(440.0)
                .frame(egui::Frame::new().fill(p.surface))
                .show(ui, |ui| {
                    sql_preview(ui, &sql_now, &mut events);
                });
            tabs(ui, model, state);
        });
    canvas(ui, model, state, id, &mut events);
    if *model != before {
        events.push(QbEvent::Changed);
    }
    QbResponse {
        events,
        sql: model.to_sql(state.dialect.as_ref()),
    }
}

/// A check box like the design's: an accent box with a white check when on.
fn check(ui: &mut Ui, on: &mut bool, label: &str) -> egui::Response {
    let p = style::pal(ui);
    let enabled = ui.is_enabled();
    let galley = (!label.is_empty()).then(|| {
        ui.painter().layout_no_wrap(
            label.to_string(),
            egui::FontId::proportional(style::body_size(ui.ctx())),
            Color32::PLACEHOLDER,
        )
    });
    let w = 16.0 + galley.as_ref().map_or(0.0, |g| g.size().x + 8.0);
    let h = galley.as_ref().map_or(16.0, |g| g.size().y).max(20.0);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, *on, label)
    });
    let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
    let b = Rect::from_center_size(pos2(rect.left() + 8.0, rect.center().y), Vec2::splat(16.0));
    paint_check(ui.painter(), b, *on, enabled && resp.hovered(), &p, fade);
    if resp.has_focus() {
        style::focus_halo(ui, b, 4);
    }
    if let Some(g) = galley {
        ui.painter().galley(
            pos2(b.right() + 8.0, rect.center().y - g.size().y / 2.0),
            g,
            fade(p.text),
        );
    }
    resp
}

/// The box of a check box in `b`.
fn paint_check(
    painter: &egui::Painter,
    b: Rect,
    on: bool,
    hovered: bool,
    p: &style::Palette,
    fade: impl Fn(Color32) -> Color32,
) {
    let r = CornerRadius::same((b.width() * 0.25).round() as u8);
    if on {
        painter.rect_filled(b, r, fade(p.accent));
        let c = b.center();
        let s = b.width() / 16.0;
        let pts = vec![
            pos2(c.x - 3.6 * s, c.y + 0.2 * s),
            pos2(c.x - 1.0 * s, c.y + 2.8 * s),
            pos2(c.x + 3.8 * s, c.y - 2.6 * s),
        ];
        painter.add(egui::Shape::line(
            pts,
            Stroke::new(2.0 * s.max(0.6), fade(Color32::WHITE)),
        ));
    } else {
        let border = if hovered { p.accent } else { p.border_strong };
        painter.rect(
            b,
            r,
            p.surface,
            Stroke::new(1.5, fade(border)),
            StrokeKind::Inside,
        );
    }
}

fn toolbar(ui: &mut Ui, model: &mut QueryModel, sql: &str, events: &mut Vec<QbEvent>) {
    parts::toolbar(ui, |ui| {
        if kit::button_ex(
            ui,
            kit::Kind::Secondary,
            Some(icon::PLUS),
            tr("Add table"),
            true,
        )
        .clicked()
        {
            events.push(QbEvent::AddTableRequested);
        }
        let has_sql = !sql.is_empty();
        ui.add_enabled_ui(has_sql, |ui| {
            if kit::button_ex(ui, kit::Kind::Primary, Some(icon::PLAY), tr("Run"), true).clicked() {
                events.push(QbEvent::RunQuery(sql.to_string()));
            }
            if kit::button_ex(
                ui,
                kit::Kind::Ghost,
                Some(icon::ARROW_SQUARE_OUT),
                tr("Open in editor"),
                true,
            )
            .clicked()
            {
                events.push(QbEvent::OpenInEditor(sql.to_string()));
            }
        });
        kit::vdivider(ui, 18.0);
        ui.spacing_mut().item_spacing.x = 12.0;
        check(ui, &mut model.distinct, "DISTINCT");
        let mut limited = model.limit.is_some();
        if check(ui, &mut limited, tr("Limit")).changed() {
            model.limit = if limited { Some(100) } else { None };
        }
        if let Some(l) = model.limit.as_mut() {
            ui.add_sized(
                [70.0, 24.0],
                egui::DragValue::new(l).range(1..=u64::MAX).speed(1.0),
            );
        }
    });
}

/// SQL keywords coloured in the preview.
const KEYWORDS: &[&str] = &[
    "select", "from", "where", "join", "left", "right", "full", "inner", "outer", "cross", "on",
    "group", "by", "order", "limit", "offset", "as", "and", "or", "not", "null", "is", "in",
    "having", "distinct", "asc", "desc", "union", "all", "like", "between", "exists", "case",
    "when", "then", "else", "end", "top", "fetch", "first", "rows", "only", "next", "count", "sum",
    "avg", "min", "max", "true", "false", "with", "ilike",
];

/// A highlighted layout of the generated SQL (keywords, strings, numbers, comments).
fn sql_job(ui: &Ui, text: &str, font: egui::FontId) -> egui::text::LayoutJob {
    let p = style::pal(ui);
    let dark = ui.visuals().dark_mode;
    let keyword = if dark {
        Color32::from_rgb(0x7a, 0xa7, 0xff)
    } else {
        Color32::from_rgb(0x25, 0x54, 0xc7)
    };
    let string = if dark {
        Color32::from_rgb(0xe6, 0xa9, 0x6b)
    } else {
        Color32::from_rgb(0xa5, 0x53, 0x1a)
    };
    let number = if dark {
        Color32::from_rgb(0xc3, 0x9b, 0xf2)
    } else {
        Color32::from_rgb(0x7a, 0x3f, 0xc4)
    };
    let mut job = egui::text::LayoutJob::default();
    let fmt = |c: Color32, italics: bool| egui::TextFormat {
        font_id: font.clone(),
        color: c,
        italics,
        ..Default::default()
    };
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let c = rest.chars().next().unwrap_or(' ');
        let (len, color, italics) = if rest.starts_with("--") {
            (rest.find('\n').unwrap_or(rest.len()), p.text_faint, true)
        } else if c == '\'' {
            let end = rest[1..].find('\'').map(|e| e + 2).unwrap_or(rest.len());
            (end, string, false)
        } else if c.is_ascii_digit() {
            let end = rest
                .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
                .unwrap_or(rest.len());
            (end, number, false)
        } else if c.is_alphanumeric() || c == '_' {
            let end = rest
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .unwrap_or(rest.len());
            let word = &rest[..end];
            let prev_dot = i > 0 && bytes[i - 1] == b'.';
            let kw = !prev_dot && KEYWORDS.contains(&word.to_ascii_lowercase().as_str());
            (end, if kw { keyword } else { p.text }, false)
        } else {
            (c.len_utf8(), p.text, false)
        };
        let len = len.max(c.len_utf8()).min(rest.len());
        job.append(&rest[..len], 0.0, fmt(color, italics));
        i += len;
    }
    job
}

fn sql_preview(ui: &mut Ui, sql: &str, events: &mut Vec<QbEvent>) {
    let p = style::pal(ui);
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(14, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(tr("SQL"))
                        .font(style::semibold(ui.ctx(), 13.0))
                        .color(p.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::icon_button_ex(
                        ui,
                        icon::ARROW_SQUARE_OUT,
                        tr("Open in editor"),
                        false,
                        26.0,
                    )
                    .clicked()
                        && !sql.is_empty()
                    {
                        events.push(QbEvent::OpenInEditor(sql.to_string()));
                    }
                    if kit::button_ex(ui, kit::Kind::Ghost, Some(icon::COPY), tr("Copy"), true)
                        .clicked()
                    {
                        ui.ctx().copy_text(sql.to_string());
                    }
                });
            });
        });
    parts::hline(ui);
    let font = egui::FontId::monospace(13.0);
    egui::ScrollArea::both()
        .id_salt("ternarygrid_qb_sql")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.add_space(10.0);
            if sql.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(tr("Add tables to build a query")).color(p.text_faint),
                    );
                });
                return;
            }
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                let lines = sql.lines().count().max(1);
                ui.vertical(|ui| {
                    ui.set_width(44.0);
                    for i in 1..=lines {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            ui.add_space(16.0);
                            ui.label(
                                egui::RichText::new(i.to_string())
                                    .font(font.clone())
                                    .color(p.text_faint),
                            );
                        });
                    }
                });
                let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
                    ui.fonts_mut(|f| f.layout_job(sql_job(ui, buf.as_str(), font.clone())))
                };
                let mut text = sql;
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .font(font.clone())
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::ZERO)
                        .desired_width(f32::INFINITY)
                        .layouter(&mut layouter),
                );
            });
        });
}

// ---------------------------------------------------------------- canvas

fn join_points(model: &QueryModel, j: &QbJoin) -> Option<Vec<Pos2>> {
    let l = model.table(&j.left_alias)?;
    let r = model.table(&j.right_alias)?;
    let y1 = l.column_y(&j.left_column)?;
    let y2 = r.column_y(&j.right_column)?;
    Some(route(l.rect(), y1, r.rect(), y2, 0.0).points)
}

fn join_label_pos(points: &[Pos2]) -> Option<Pos2> {
    match points.len() {
        0 => None,
        1 => points.first().copied(),
        2 | 3 => Some(points[0].lerp(points[points.len() - 1], 0.5)),
        _ => Some(points[1].lerp(points[2], 0.5)),
    }
}

fn kind_symbol(k: JoinKind) -> &'static str {
    match k {
        JoinKind::Inner => "INNER",
        JoinKind::Left => "LEFT",
        JoinKind::Right => "RIGHT",
        JoinKind::Full => "FULL",
    }
}

fn hit_test(model: &QueryModel, world: Pos2, zoom: f32) -> Option<QbHit> {
    for t in model.tables.iter().rev() {
        let r = t.rect();
        if !r.contains(world) {
            continue;
        }
        if world.y < r.top() + QB_HEADER {
            if world.x > r.right() - 22.0 {
                return Some(QbHit::Close(t.alias.clone()));
            }
            return Some(QbHit::Header(t.alias.clone()));
        }
        let i = ((world.y - r.top() - QB_HEADER) / QB_ROW).floor();
        if i >= 0.0
            && let Some(c) = t.columns.get(i as usize)
        {
            return Some(QbHit::Row(t.alias.clone(), c.name.clone()));
        }
        return Some(QbHit::Header(t.alias.clone()));
    }
    let tol = 6.0 / zoom.max(0.1);
    for (i, j) in model.joins.iter().enumerate() {
        if let Some(pts) = join_points(model, j) {
            let near_label = join_label_pos(&pts).is_some_and(|p| (p - world).length() < 18.0);
            if near_label || distance_to_polyline(world, &pts) <= tol {
                return Some(QbHit::Join(i));
            }
        }
    }
    None
}

fn canvas(
    ui: &mut Ui,
    model: &mut QueryModel,
    state: &mut QbViewState,
    id: Id,
    events: &mut Vec<QbEvent>,
) {
    let rect = ui.available_rect_before_wrap();
    let rect = Rect::from_min_size(rect.min, rect.size().max(vec2(64.0, 64.0)));
    let response = ui.interact(rect, id.with("canvas"), Sense::click_and_drag());
    ui.advance_cursor_after_rect(rect);
    state.last_canvas = rect;
    let origin = rect.min;
    let mut view = ViewTransform::new(state.zoom, state.pan);
    if handle_pan_zoom(ui, &response, &mut view, origin, (0.4, 2.0), false).changed {
        state.zoom = view.zoom;
        state.pan = view.pan;
    }
    let view = ViewTransform::new(state.zoom, state.pan);
    let pointer = ui
        .input(|i| i.pointer.latest_pos())
        .filter(|p| rect.contains(*p));
    let hover = pointer.and_then(|p| hit_test(model, view.to_world(origin, p), view.zoom));

    if response.drag_started_by(egui::PointerButton::Primary) {
        let press = ui.input(|i| i.pointer.press_origin()).unwrap_or(origin);
        let world = view.to_world(origin, press);
        state.drag = Some(match hit_test(model, world, view.zoom) {
            Some(QbHit::Header(a)) | Some(QbHit::Close(a)) => QbDrag::MoveTable(a),
            Some(QbHit::Row(a, c)) => QbDrag::Join {
                alias: a,
                column: c,
                current: world,
            },
            _ => QbDrag::Pan,
        });
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        let d = response.drag_delta();
        match &mut state.drag {
            Some(QbDrag::Pan) => state.pan += d,
            Some(QbDrag::MoveTable(a)) => {
                if let Some(t) = model.table_mut(a) {
                    t.pos += d / state.zoom.max(0.1);
                }
            }
            Some(QbDrag::Join { current, .. }) => {
                if let Some(p) = pointer {
                    *current = view.to_world(origin, p);
                }
            }
            None => {}
        }
    }
    if response.drag_stopped() {
        match state.drag.take() {
            Some(QbDrag::MoveTable(a)) => {
                if let Some(t) = model.table_mut(&a) {
                    t.pos = round_pos(t.pos);
                }
            }
            Some(QbDrag::Join {
                alias,
                column,
                current,
            }) => {
                if let Some(QbHit::Row(a2, c2)) = hit_test(model, current, view.zoom)
                    && a2 != alias
                {
                    model.add_join(QbJoin {
                        left_alias: alias,
                        left_column: column,
                        right_alias: a2,
                        right_column: c2,
                        kind: JoinKind::Inner,
                    });
                }
            }
            _ => {}
        }
    }
    // MySQL and MariaDB have no FULL JOIN: it is not offered there.
    let full_join = !matches!(
        state.dialect.kind(),
        ternarygrid_core::DbKind::MySql | ternarygrid_core::DbKind::MariaDb
    );
    if response.clicked() {
        match hover.clone() {
            Some(QbHit::Row(a, c)) => {
                let selected = model
                    .table(&a)
                    .and_then(|t| t.columns.iter().find(|x| x.name == c))
                    .is_some_and(|x| x.selected);
                model.set_column_selected(&a, &c, !selected);
            }
            Some(QbHit::Close(a)) => model.remove_table(&a),
            Some(QbHit::Join(i)) => {
                // quick toggle; the full menu is on right click
                if let Some(j) = model.joins.get_mut(i) {
                    j.kind = match j.kind {
                        JoinKind::Inner => JoinKind::Left,
                        JoinKind::Left => JoinKind::Right,
                        JoinKind::Right if full_join => JoinKind::Full,
                        JoinKind::Right | JoinKind::Full => JoinKind::Inner,
                    };
                }
            }
            _ => {}
        }
    }
    if response.secondary_clicked() {
        state.context = Some(match hover.clone() {
            Some(QbHit::Join(i)) => QbContext::Join(i),
            Some(QbHit::Header(a)) | Some(QbHit::Close(a)) | Some(QbHit::Row(a, _)) => {
                state.alias_edit = a.clone();
                QbContext::Table(a)
            }
            None => QbContext::Background,
        });
    }
    if let Some(ctx_target) = state.context.clone() {
        let mut alias_edit = std::mem::take(&mut state.alias_edit);
        let menu = response.context_menu(|ui| match &ctx_target {
            QbContext::Join(i) => {
                if let Some(j) = model.joins.get_mut(*i) {
                    ui.label(
                        RichText::new(format!(
                            "{}.{} = {}.{}",
                            j.left_alias, j.left_column, j.right_alias, j.right_column
                        ))
                        .strong(),
                    );
                    ui.separator();
                    for k in JoinKind::ALL {
                        if k == JoinKind::Full && !full_join && j.kind != k {
                            continue;
                        }
                        if ternarygrid_ui::kit::radio(ui, j.kind == k, tr(k.label())).clicked() {
                            j.kind = k;
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui
                        .button((ternarygrid_icons::ic(icon::TRASH), tr("Remove join")))
                        .clicked()
                    {
                        model.joins.remove(*i);
                        ui.close();
                    }
                }
            }
            QbContext::Table(a) => {
                ui.label(RichText::new(a.as_str()).strong());
                ui.separator();
                if ui
                    .button(format!(
                        "{} {}",
                        icon::CHECK_SQUARE,
                        tr("Select all columns")
                    ))
                    .clicked()
                {
                    let cols: Vec<String> = model
                        .table(a)
                        .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
                        .unwrap_or_default();
                    for c in cols {
                        model.set_column_selected(a, &c, true);
                    }
                    ui.close();
                }
                if ui
                    .button((ternarygrid_icons::ic(icon::SQUARE), tr("Clear selection")))
                    .clicked()
                {
                    let cols: Vec<String> = model
                        .table(a)
                        .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
                        .unwrap_or_default();
                    for c in cols {
                        model.set_column_selected(a, &c, false);
                    }
                    ui.close();
                }
                ui.horizontal(|ui| {
                    ui.label(tr("Alias"));
                    let r = ui.add(egui::TextEdit::singleline(&mut alias_edit).desired_width(90.0));
                    let apply = ui
                        .button(ternarygrid_icons::ic(icon::CHECK))
                        .on_hover_text(tr("Rename alias"))
                        .clicked()
                        || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if apply && model.rename_alias(a, &alias_edit) {
                        ui.close();
                    }
                });
                if ui
                    .button((ternarygrid_icons::ic(icon::TRASH), tr("Remove table")))
                    .clicked()
                {
                    model.remove_table(a);
                    ui.close();
                }
            }
            QbContext::Background => {
                if ui
                    .button((ternarygrid_icons::ic(icon::PLUS), tr("Add table…")))
                    .clicked()
                {
                    events.push(QbEvent::AddTableRequested);
                    ui.close();
                }
                if ui
                    .button((ternarygrid_icons::ic(icon::ARROWS_IN), tr("Reset view")))
                    .clicked()
                {
                    state.zoom = 1.0;
                    state.pan = vec2(10.0, 10.0);
                    ui.close();
                }
            }
        });
        state.alias_edit = alias_edit;
        // The menu closed: the join is no longer highlighted (and an index may now name another one).
        if menu.is_none() {
            state.context = None;
        }
    }

    // painting
    let painter = ui.painter_at(rect);
    let p = style::pal(ui);
    let zoom = state.zoom;
    painter.rect_filled(rect, 0.0, p.surface);
    // a dotted grid that pans and zooms with the tables
    let step = 20.0 * zoom;
    if step >= 6.0 {
        let o = view.to_screen(origin, pos2(0.0, 0.0));
        let x0 = rect.left() + (o.x - rect.left()).rem_euclid(step);
        let y0 = rect.top() + (o.y - rect.top()).rem_euclid(step);
        let dot = (zoom * 1.0).clamp(0.6, 1.4);
        let mut y = y0;
        while y < rect.bottom() {
            let mut x = x0;
            while x < rect.right() {
                painter.circle_filled(pos2(x, y), dot, p.border);
                x += step;
            }
            y += step;
        }
    }
    for (i, j) in model.joins.iter().enumerate() {
        let Some(pts) = join_points(model, j) else {
            continue;
        };
        let hot = hover == Some(QbHit::Join(i)) || state.context == Some(QbContext::Join(i));
        let color = if hot { p.accent_hover } else { p.accent };
        let stroke = Stroke::new(if hot { 2.5 } else { 2.0 }, color);
        let screen: Vec<Pos2> = pts.iter().map(|p| view.to_screen(origin, *p)).collect();
        painter.line(screen.clone(), stroke);
        if let (Some(pos), Some(px)) = (join_label_pos(&pts), font_px(10.5, zoom)) {
            let galley = painter.layout_no_wrap(
                kind_symbol(j.kind).to_string(),
                style::semibold(ui.ctx(), px),
                p.on_accent,
            );
            let sp = view.to_screen(origin, pos);
            let r = Rect::from_center_size(
                sp,
                vec2(
                    galley.size().x + 14.0 * zoom,
                    (18.0 * zoom).max(galley.size().y),
                ),
            );
            painter.rect_filled(r, r.height() * 0.5, color);
            painter.galley(r.center() - galley.size() / 2.0, galley, p.on_accent);
        }
    }
    for t in &model.tables {
        let srect = view.rect_to_screen(origin, t.rect());
        if !srect.intersects(rect) {
            continue;
        }
        let radius = (6.0 * zoom).clamp(0.0, 10.0);
        let r8 = radius.round() as u8;
        painter.rect_filled(srect, r8, p.surface);
        let header = Rect::from_min_size(srect.min, vec2(srect.width(), QB_HEADER * zoom));
        painter.rect_filled(
            header,
            CornerRadius {
                nw: r8,
                ne: r8,
                sw: 0,
                se: 0,
            },
            p.accent_soft,
        );
        let hovered_header =
            matches!(&hover, Some(QbHit::Header(a)) | Some(QbHit::Close(a)) if *a == t.alias);
        if let Some(px) = font_px(QB_FONT, zoom) {
            let x = header.left() + 8.0 * zoom;
            let r1 = painter.text(
                pos2(x, header.center().y),
                Align2::LEFT_CENTER,
                icon::TABLE,
                style::icon_font(px + 0.5),
                p.accent_text,
            );
            let name_max = (header.width() * 0.62).max(20.0);
            let name = crate::common::text::truncate_to_width(&t.obj.name, name_max, px, true);
            let r2 = painter.text(
                pos2(r1.right() + 6.0 * zoom, header.center().y),
                Align2::LEFT_CENTER,
                name,
                style::semibold(ui.ctx(), px),
                p.text,
            );
            paint::text(
                &painter,
                pos2(r2.right() + 6.0 * zoom, header.center().y),
                Align2::LEFT_CENTER,
                &t.alias,
                px - 1.5,
                p.text_faint,
                false,
                f32::INFINITY,
            );
            let close_color = if matches!(&hover, Some(QbHit::Close(a)) if *a == t.alias) {
                p.err
            } else {
                p.text_faint
            };
            painter.text(
                pos2(header.right() - 8.0 * zoom, header.center().y),
                Align2::RIGHT_CENTER,
                icon::X,
                style::icon_font(px - 1.0),
                close_color,
            );
        }
        for (i, c) in t.columns.iter().enumerate() {
            let top = srect.top() + (QB_HEADER + i as f32 * QB_ROW) * zoom;
            let row =
                Rect::from_min_size(pos2(srect.left(), top), vec2(srect.width(), QB_ROW * zoom));
            let hovered_row =
                matches!(&hover, Some(QbHit::Row(a, col)) if *a == t.alias && *col == c.name);
            let join_source = matches!(&state.drag, Some(QbDrag::Join { alias, column, .. }) if *alias == t.alias && *column == c.name);
            if hovered_row || join_source {
                painter.rect_filled(row.shrink2(vec2(2.0, 0.0)), 3.0, p.hover);
            }
            let Some(px) = font_px(QB_FONT - 1.0, zoom) else {
                continue;
            };
            let cb = Rect::from_center_size(
                pos2(row.left() + 15.5 * zoom, row.center().y),
                Vec2::splat(14.0 * zoom),
            );
            paint_check(&painter, cb, c.selected, false, &p, |c| c);
            let name_x = row.left() + 30.0 * zoom;
            let tr_rect = paint::text(
                &painter,
                pos2(row.right() - 8.0 * zoom, row.center().y),
                Align2::RIGHT_CENTER,
                &c.data_type,
                px - 1.0,
                p.text_faint,
                true,
                row.width() * 0.45,
            );
            paint::text(
                &painter,
                pos2(name_x, row.center().y),
                Align2::LEFT_CENTER,
                &c.name,
                px,
                p.text,
                false,
                (tr_rect.left() - name_x - 6.0).max(10.0),
            );
        }
        let stroke = if hovered_header {
            Stroke::new(1.5, p.accent)
        } else {
            Stroke::new(1.0, p.border_strong)
        };
        painter.rect_stroke(srect, r8, stroke, StrokeKind::Inside);
    }
    if let Some(QbDrag::Join {
        alias,
        column,
        current,
    }) = &state.drag
        && let Some(t) = model.table(alias)
        && let Some(y) = t.column_y(column)
    {
        let r = t.rect();
        let start = if current.x > r.center().x {
            pos2(r.right(), y)
        } else {
            pos2(r.left(), y)
        };
        paint::dashed_polyline(
            &painter,
            &[
                view.to_screen(origin, start),
                view.to_screen(origin, *current),
            ],
            Stroke::new(2.0, p.accent),
            6.0,
            4.0,
        );
    }
    if model.tables.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            tr("Add tables with the + button, then tick columns and drag columns onto each other to join"),
            egui::FontId::proportional(13.0),
            p.text_muted,
        );
    }
    match &hover {
        Some(QbHit::Row(..)) | Some(QbHit::Close(..)) | Some(QbHit::Join(..)) => {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand)
        }
        Some(QbHit::Header(..)) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
        None => {}
    }
    if let Some(QbHit::Row(..)) = &hover {
        response.on_hover_text_at_pointer(tr(
            "Click to select the column, drag onto a column of another table to join",
        ));
    }
}

// ---------------------------------------------------------------- grids

#[derive(Clone, Copy)]
enum RowAction {
    Up(usize),
    Down(usize),
    Delete(usize),
}

fn apply_row_action<T>(list: &mut Vec<T>, action: Option<RowAction>) {
    match action {
        Some(RowAction::Up(i)) if i > 0 && i < list.len() => list.swap(i, i - 1),
        Some(RowAction::Down(i)) if i + 1 < list.len() => list.swap(i, i + 1),
        Some(RowAction::Delete(i)) if i < list.len() => {
            list.remove(i);
        }
        _ => {}
    }
}

/// Width of the move up / move down / remove buttons of a row.
const ROW_BUTTONS_W: f32 = 92.0;

fn row_buttons(ui: &mut Ui, i: usize, action: &mut Option<RowAction>) {
    ui.spacing_mut().item_spacing.x = 2.0;
    if kit::icon_button_ex(ui, icon::ARROW_UP, tr("Move up"), false, 24.0).clicked() {
        *action = Some(RowAction::Up(i));
    }
    if kit::icon_button_ex(ui, icon::ARROW_DOWN, tr("Move down"), false, 24.0).clicked() {
        *action = Some(RowAction::Down(i));
    }
    if kit::icon_button_ex(ui, icon::TRASH, tr("Remove"), false, 24.0).clicked() {
        *action = Some(RowAction::Delete(i));
    }
}

/// A bordered table of widget rows like the design's: the header on the header colour,
/// striped rows `row_h` high; `widths` are the column widths, the last one takes the rest.
fn grid(
    ui: &mut Ui,
    names: &[&str],
    widths: &[f32],
    rows: usize,
    row_h: f32,
    mut cell: impl FnMut(&mut Ui, usize, usize),
) {
    let p = style::pal(ui);
    let n = widths.len();
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(8)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let total = ui.available_width();
            let fixed: f32 = widths[..n.saturating_sub(1)].iter().sum();
            let col_w = |i: usize| {
                if i + 1 == n {
                    (total - fixed).max(widths[i])
                } else {
                    widths[i]
                }
            };
            let (head, _) = ui.allocate_exact_size(vec2(total, 32.0), Sense::hover());
            ui.painter().rect_filled(
                head,
                CornerRadius {
                    nw: 8,
                    ne: 8,
                    sw: 0,
                    se: 0,
                },
                p.header,
            );
            ui.painter().hline(
                head.x_range(),
                head.bottom() - 0.5,
                Stroke::new(1.0, p.border),
            );
            let mut x = head.left();
            for (i, name) in names.iter().enumerate() {
                ui.painter().text(
                    pos2(x + 10.0, head.center().y),
                    Align2::LEFT_CENTER,
                    *name,
                    style::semibold(ui.ctx(), 12.0),
                    p.text_muted,
                );
                x += col_w(i);
            }
            for r in 0..rows {
                let (row, _) = ui.allocate_exact_size(vec2(total, row_h), Sense::hover());
                if r % 2 == 1 {
                    ui.painter().rect_filled(
                        row.shrink2(vec2(1.0, 0.0)),
                        0.0,
                        style::soft(p.hover, 0.5),
                    );
                }
                let mut x = row.left();
                for c in 0..n {
                    let cr = Rect::from_min_size(pos2(x, row.top()), vec2(col_w(c), row_h));
                    let mut cu = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(cr.shrink2(vec2(8.0, 0.0)))
                            .layout(egui::Layout::left_to_right(egui::Align::Center))
                            .id_salt(("qb_cell", r, c)),
                    );
                    cell(&mut cu, r, c);
                    x += col_w(c);
                }
            }
        });
}

/// Text field with a menu of column references next to it.
fn expr_field(ui: &mut Ui, value: &mut String, refs: &[String], width: f32) {
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width((width - 30.0).max(60.0))
            .font(egui::FontId::monospace(12.5))
            .margin(egui::Margin::symmetric(8, 4))
            .hint_text(tr("column or expression")),
    );
    ui.menu_button(icon::LIST, |ui| {
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                for r in refs {
                    if ui.button(r.as_str()).clicked() {
                        *value = r.clone();
                        ui.close();
                    }
                }
            });
    })
    .response
    .on_hover_text(tr("Pick a column"));
}

/// A small text field of a grid cell.
fn cell_field(ui: &mut Ui, value: &mut String, hint: &str, mono: bool) -> egui::Response {
    let mut edit = egui::TextEdit::singleline(value)
        .desired_width(ui.available_width())
        .margin(egui::Margin::symmetric(8, 4))
        .hint_text(hint);
    if mono {
        edit = edit.font(egui::FontId::monospace(12.5));
    }
    ui.add(edit)
}

fn add_button(ui: &mut Ui, label: &str) -> bool {
    ui.add_space(8.0);
    kit::button_ex(ui, kit::Kind::Ghost, Some(icon::PLUS), label, true).clicked()
}

fn tabs(ui: &mut Ui, model: &mut QueryModel, state: &mut QbViewState) {
    let p = style::pal(ui);
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.add_space(8.0);
        for t in QbTab::ALL {
            let count = match t {
                QbTab::Columns => model.select.len(),
                QbTab::Conditions => model.conditions.len(),
                QbTab::Grouping => model.group_by.len() + model.having.len(),
                QbTab::Order => model.order_by.len(),
            };
            let label = if count > 0 {
                format!("{} ({count})", tr(t.label()))
            } else {
                tr(t.label()).to_string()
            };
            if kit::underline_tab(ui, None, &label, None, state.tab == t, 34.0).clicked() {
                state.tab = t;
            }
        }
    });
    ui.painter().hline(
        ui.max_rect().x_range(),
        row.response.rect.bottom() - 0.5,
        Stroke::new(1.0, p.border),
    );
    let refs = model.column_refs();
    egui::ScrollArea::vertical()
        .id_salt("ternarygrid_qb_tabs")
        .auto_shrink(false)
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    match state.tab {
                        QbTab::Columns => columns_grid(ui, model, &refs),
                        QbTab::Conditions => {
                            conditions_grid(
                                ui,
                                "ternarygrid_qb_where",
                                &mut model.conditions,
                                &refs,
                            );
                        }
                        QbTab::Grouping => grouping(ui, model, &refs, state.dialect.as_ref()),
                        QbTab::Order => order_grid(ui, model, &refs),
                    }
                });
        });
}

fn columns_grid(ui: &mut Ui, model: &mut QueryModel, refs: &[String]) {
    let mut action = None;
    let mut changed = false;
    let n = model.select.len();
    if n > 0 {
        let flex = (ui.available_width() - 64.0 - 160.0 - ROW_BUTTONS_W).max(240.0);
        let widths = [64.0, flex * 0.58, flex * 0.42, 160.0, ROW_BUTTONS_W];
        let names = [
            tr("Output"),
            tr("Expression"),
            tr("Alias"),
            tr("Aggregate"),
            "",
        ];
        let select = &mut model.select;
        grid(ui, &names, &widths, n, 36.0, |ui, r, c| {
            let item = &mut select[r];
            match c {
                0 => {
                    check(ui, &mut item.visible, "").on_hover_text(tr("Output"));
                }
                1 => {
                    let before = item.expr.clone();
                    let w = ui.available_width();
                    expr_field(ui, &mut item.expr, refs, w);
                    changed |= before != item.expr;
                }
                2 => {
                    cell_field(ui, &mut item.alias, "", false);
                }
                3 => {
                    egui::ComboBox::from_id_salt(("ternarygrid_qb_agg", r))
                        .icon(kit::combo_caret)
                        .width(140.0)
                        .selected_text(item.aggregate.map(|a| a.label()).unwrap_or("–"))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut item.aggregate, None, "–");
                            for a in Aggregate::ALL {
                                ui.selectable_value(&mut item.aggregate, Some(a), a.label());
                            }
                        });
                }
                _ => row_buttons(ui, r, &mut action),
            }
        });
    }
    if action.is_some() {
        changed = true;
    }
    apply_row_action(&mut model.select, action);
    if add_button(ui, tr("Add expression")) {
        model.select.push(QbSelectItem::default());
    }
    if changed {
        model.sync_selected_flags();
    }
}

fn conditions_grid(ui: &mut Ui, salt: &str, list: &mut Vec<QbCondition>, refs: &[String]) {
    let mut action = None;
    let p = style::pal(ui);
    let n = list.len();
    if n > 0 {
        let flex = (ui.available_width() - 90.0 - 120.0 - ROW_BUTTONS_W).max(240.0);
        let widths = [90.0, flex * 0.55, 120.0, flex * 0.45, ROW_BUTTONS_W];
        let names = ["", tr("Expression"), tr("Operator"), tr("Value"), ""];
        grid(ui, &names, &widths, n, 36.0, |ui, i, c| {
            let cond = &mut list[i];
            match c {
                0 if i == 0 => {
                    ui.label(egui::RichText::new(tr("where")).color(p.text_faint));
                }
                0 => {
                    egui::ComboBox::from_id_salt((salt, "conn", i))
                        .icon(kit::combo_caret)
                        .width(66.0)
                        .selected_text(cond.connector.sql())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut cond.connector, Connector::And, "AND");
                            ui.selectable_value(&mut cond.connector, Connector::Or, "OR");
                        });
                }
                1 => {
                    let w = ui.available_width();
                    expr_field(ui, &mut cond.expr, refs, w);
                }
                2 => {
                    egui::ComboBox::from_id_salt((salt, "op", i))
                        .icon(kit::combo_caret)
                        .width(100.0)
                        .selected_text(if cond.operator.is_empty() {
                            tr("(expression)")
                        } else {
                            cond.operator.as_str()
                        })
                        .show_ui(ui, |ui| {
                            for op in OPERATORS {
                                let label = if op.is_empty() {
                                    tr("(expression)")
                                } else {
                                    op
                                };
                                ui.selectable_value(&mut cond.operator, op.to_string(), label);
                            }
                        });
                }
                3 => {
                    let needs_value =
                        !matches!(cond.operator.as_str(), "IS NULL" | "IS NOT NULL" | "");
                    ui.add_enabled_ui(needs_value, |ui| {
                        cell_field(ui, &mut cond.value, tr("'text', 42, a.column"), true);
                    });
                }
                _ => row_buttons(ui, i, &mut action),
            }
        });
    }
    apply_row_action(list, action);
    if add_button(ui, tr("Add condition")) {
        list.push(QbCondition::new(
            refs.first().cloned().unwrap_or_default(),
            "=",
            "",
        ));
    }
}

/// The "Effective GROUP BY" hint, rendered with the builder's dialect so it
/// matches the generated SQL.
fn effective_group_by_hint(model: &QueryModel, dialect: &dyn Dialect) -> Option<String> {
    let effective = model.effective_group_by(dialect);
    if effective.is_empty() {
        None
    } else {
        Some(trf("Effective GROUP BY: {}", &[&effective.join(", ")]))
    }
}

fn grouping(ui: &mut Ui, model: &mut QueryModel, refs: &[String], dialect: &dyn Dialect) {
    let p = style::pal(ui);
    ui.label(
        egui::RichText::new(tr(
            "Columns without an aggregate are grouped automatically when the select list contains aggregates.",
        ))
        .color(p.text_muted),
    );
    let mut action = None;
    let n = model.group_by.len();
    if n > 0 {
        let widths = [
            (ui.available_width() - ROW_BUTTONS_W).max(200.0),
            ROW_BUTTONS_W,
        ];
        let group_by = &mut model.group_by;
        grid(ui, &[tr("Expression"), ""], &widths, n, 36.0, |ui, i, c| {
            if c == 0 {
                let w = ui.available_width();
                expr_field(ui, &mut group_by[i], refs, w.min(420.0));
            } else {
                row_buttons(ui, i, &mut action);
            }
        });
    }
    apply_row_action(&mut model.group_by, action);
    if add_button(ui, tr("Add group by expression")) {
        model
            .group_by
            .push(refs.first().cloned().unwrap_or_default());
    }
    if let Some(hint) = effective_group_by_hint(model, dialect) {
        ui.label(egui::RichText::new(hint).size(12.0).color(p.text_faint));
    }
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(tr("Having"))
            .font(style::semibold(ui.ctx(), 13.0))
            .color(p.text),
    );
    conditions_grid(ui, "ternarygrid_qb_having", &mut model.having, refs);
}

fn order_grid(ui: &mut Ui, model: &mut QueryModel, refs: &[String]) {
    let mut action = None;
    let n = model.order_by.len();
    if n > 0 {
        let widths = [
            (ui.available_width() - 120.0 - ROW_BUTTONS_W).max(200.0),
            120.0,
            ROW_BUTTONS_W,
        ];
        let order_by = &mut model.order_by;
        grid(
            ui,
            &[tr("Expression"), tr("Direction"), ""],
            &widths,
            n,
            36.0,
            |ui, i, c| {
                let (expr, desc) = &mut order_by[i];
                match c {
                    0 => {
                        let w = ui.available_width();
                        expr_field(ui, expr, refs, w.min(420.0));
                    }
                    1 => {
                        let (glyph, label) = if *desc {
                            (icon::SORT_DESCENDING, "DESC")
                        } else {
                            (icon::SORT_ASCENDING, "ASC")
                        };
                        if kit::button_ex(ui, kit::Kind::Secondary, Some(glyph), label, true)
                            .on_hover_text(tr("Toggle sort direction"))
                            .clicked()
                        {
                            *desc = !*desc;
                        }
                    }
                    _ => row_buttons(ui, i, &mut action),
                }
            },
        );
    }
    apply_row_action(&mut model.order_by, action);
    if add_button(ui, tr("Add sort expression")) {
        model
            .order_by
            .push((refs.first().cloned().unwrap_or_default(), false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::model::tests::{customers, orders};
    use crate::test_harness::Harness;

    fn row_center(model: &QueryModel, state: &QbViewState, alias: &str, column: &str) -> Pos2 {
        let t = model.table(alias).expect("table");
        let y = t.column_y(column).expect("column");
        let view = ViewTransform::new(state.zoom, state.pan);
        view.to_screen(state.last_canvas.min, pos2(t.rect().center().x, y))
    }

    #[test]
    fn effective_group_by_hint_uses_the_dialect() {
        use crate::builder::sql::tests::dialect;
        use ternarygrid_core::DbKind;
        let mut model = QueryModel::default();
        model.add_table(&customers());
        model.add_table(&orders());
        assert_eq!(
            effective_group_by_hint(&model, &dialect(DbKind::MySql)),
            None
        );
        model.group_by.push("o.order".into()); // reserved word -> quoted
        let hint = effective_group_by_hint(&model, &dialect(DbKind::MySql)).unwrap();
        assert!(hint.ends_with("o.`order`"), "{hint}");
        let sql = model.to_sql(&dialect(DbKind::MySql));
        assert!(sql.contains("GROUP BY o.`order`"), "{sql}");
        // the grouping tab renders with a MySQL state
        let mut state = QbViewState::new(Arc::new(dialect(DbKind::MySql)));
        state.tab = QbTab::Grouping;
        let mut h = Harness::new(1000.0, 700.0);
        h.idle(2, &mut |ui: &mut Ui| {
            show_query_builder(ui, &mut model, &mut state);
        });
    }

    #[test]
    fn canvas_interactions_edit_the_model() {
        let mut model = QueryModel::default();
        model.add_table(&customers());
        model.add_table(&orders());
        model.joins.clear();
        let mut state = QbViewState::default();
        let mut h = Harness::new(1300.0, 900.0);
        let mut events = Vec::new();
        let mut sql = String::new();
        let mut ui_fn = |ui: &mut Ui| {
            let r = show_query_builder(ui, &mut model, &mut state);
            events.extend(r.events);
            sql = r.sql;
        };
        h.idle(3, &mut ui_fn);
        drop(ui_fn);
        assert!(sql.contains("CROSS JOIN"), "{sql}");

        // click a row: the column is selected
        let p = row_center(&model, &state, "c", "name");
        let mut ui_fn =
            |ui: &mut Ui| events.extend(show_query_builder(ui, &mut model, &mut state).events);
        h.click(p, &mut ui_fn);
        drop(ui_fn);
        assert!(model.table("c").unwrap().columns[1].selected);
        assert_eq!(model.select.len(), 1);
        assert!(events.contains(&QbEvent::Changed));

        // drag a column onto another table's column: join
        let from = row_center(&model, &state, "o", "customer_id");
        let to = row_center(&model, &state, "c", "id");
        let mut ui_fn =
            |ui: &mut Ui| events.extend(show_query_builder(ui, &mut model, &mut state).events);
        h.drag(from, to, &mut ui_fn);
        drop(ui_fn);
        assert_eq!(model.joins.len(), 1, "{:?}", model.joins);
        assert_eq!(
            (
                model.joins[0].left_alias.as_str(),
                model.joins[0].right_alias.as_str()
            ),
            ("o", "c")
        );
        let sql = model.to_sql(state.dialect.as_ref());
        assert!(sql.contains("INNER JOIN"), "{sql}");

        // click the join label: kind cycles to LEFT
        let pts = join_points(&model, &model.joins[0]).unwrap();
        let label = ViewTransform::new(state.zoom, state.pan)
            .to_screen(state.last_canvas.min, join_label_pos(&pts).unwrap());
        let mut ui_fn =
            |ui: &mut Ui| events.extend(show_query_builder(ui, &mut model, &mut state).events);
        h.click(label, &mut ui_fn);
        drop(ui_fn);
        assert_eq!(model.joins[0].kind, JoinKind::Left);

        // every tab renders
        for tab in QbTab::ALL {
            state.tab = tab;
            let mut ui_fn = |ui: &mut Ui| {
                show_query_builder(ui, &mut model, &mut state);
            };
            h.idle(2, &mut ui_fn);
        }

        // the close button removes a table
        let t = model.table("o").unwrap().rect();
        let close = ViewTransform::new(state.zoom, state.pan).to_screen(
            state.last_canvas.min,
            pos2(t.right() - 10.0, t.top() + 10.0),
        );
        let mut ui_fn =
            |ui: &mut Ui| events.extend(show_query_builder(ui, &mut model, &mut state).events);
        h.click(close, &mut ui_fn);
        drop(ui_fn);
        assert!(model.table("o").is_none());
        assert!(model.joins.is_empty());
    }
}
