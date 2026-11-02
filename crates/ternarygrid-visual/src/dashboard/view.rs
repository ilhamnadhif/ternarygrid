//! Server health dashboard: a grid of small time series per metric group
//! with current values, pause/resume and window length controls.

use super::history::{DashboardState, MetricHistory};
use crate::common::format::with_unit;
use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui,
    pos2, vec2,
};
use egui_phosphor::regular as icon;
use serde::{Deserialize, Serialize};
use ternarygrid_core::{tr, trf};
use ternarygrid_ui::{kit, parts, style};

/// Visible time window of the dashboard charts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowLength {
    /// Last minute.
    OneMinute,
    /// Last five minutes.
    #[default]
    FiveMinutes,
    /// Last fifteen minutes.
    FifteenMinutes,
    /// Last hour.
    OneHour,
}

impl WindowLength {
    /// All windows in menu order.
    pub const ALL: [WindowLength; 4] = [
        WindowLength::OneMinute,
        WindowLength::FiveMinutes,
        WindowLength::FifteenMinutes,
        WindowLength::OneHour,
    ];

    /// Length in seconds.
    pub fn secs(self) -> f64 {
        match self {
            WindowLength::OneMinute => 60.0,
            WindowLength::FiveMinutes => 300.0,
            WindowLength::FifteenMinutes => 900.0,
            WindowLength::OneHour => 3600.0,
        }
    }

    /// Short label ("1 min").
    pub fn label(self) -> &'static str {
        match self {
            WindowLength::OneMinute => "1 min",
            WindowLength::FiveMinutes => "5 min",
            WindowLength::FifteenMinutes => "15 min",
            WindowLength::OneHour => "60 min",
        }
    }
}

/// How the dashboard is arranged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardLayout {
    /// Unique id (several dashboards can be open at once).
    pub id: String,
    /// Title shown above the charts.
    pub title: Option<String>,
    /// Charts per row (`None`: as many as fit, at least 240 points wide).
    pub columns: Option<usize>,
    /// Height of one chart.
    pub chart_height: f32,
    /// Groups to show, in this order (`None`: all, in first-seen order).
    pub groups: Option<Vec<String>>,
    /// Window used until the user picks another one.
    pub default_window: WindowLength,
}

impl Default for DashboardLayout {
    fn default() -> Self {
        DashboardLayout {
            id: "server".into(),
            title: None,
            columns: None,
            chart_height: 70.0,
            groups: None,
            default_window: WindowLength::FiveMinutes,
        }
    }
}

/// Controls of a dashboard (kept in egui memory, returned to the app).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DashboardResponse {
    /// The user paused the display; the app may stop polling.
    pub paused: bool,
    /// Selected window length in seconds.
    pub window_secs: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Controls {
    paused_at: Option<f64>,
    window: WindowLength,
}

/// Shows the server health dashboard.
pub fn show_dashboard(
    ui: &mut Ui,
    state: &DashboardState,
    layout: &DashboardLayout,
) -> DashboardResponse {
    show_dashboard_with(ui, state, layout, |_| {}, |_| {})
}

/// [`show_dashboard`] with the application's controls at the start of the
/// toolbar (`lead`) and content between the toolbar and the charts (`above`,
/// e.g. an error).
pub fn show_dashboard_with(
    ui: &mut Ui,
    state: &DashboardState,
    layout: &DashboardLayout,
    lead: impl FnOnce(&mut Ui),
    above: impl FnOnce(&mut Ui),
) -> DashboardResponse {
    let p = style::pal(ui);
    let id = Id::new(("ternarygrid_dashboard", layout.id.as_str()));
    let mut controls = ui.data(|d| d.get_temp::<Controls>(id)).unwrap_or(Controls {
        paused_at: None,
        window: layout.default_window,
    });
    let latest = state.latest_time();

    ui.spacing_mut().item_spacing.y = 0.0;
    parts::toolbar(ui, |ui| {
        if let Some(t) = &layout.title {
            kit::semibold_label(ui, t, 13.0, p.text);
            kit::vdivider(ui, 18.0);
        }
        lead(ui);
        let paused = controls.paused_at.is_some();
        let (glyph, label) = if paused {
            (icon::PLAY, tr("Resume"))
        } else {
            (icon::PAUSE, tr("Pause"))
        };
        if kit::button_ex(ui, kit::Kind::Secondary, Some(glyph), label, true).clicked() {
            controls.paused_at = if paused {
                None
            } else {
                Some(latest.unwrap_or(0.0))
            };
        }
        ui.add_space(4.0);
        ui.label(RichText::new(tr("Window")).color(p.text_muted));
        let items: Vec<(WindowLength, &str)> = WindowLength::ALL
            .iter()
            .map(|w| (*w, tr(w.label())))
            .collect();
        parts::segmented_small(ui, &mut controls.window, &items);
        if paused {
            ui.add_space(4.0);
            ui.label(RichText::new(tr("Paused")).color(p.warn));
        }
    });
    ui.data_mut(|d| d.insert_temp(id, controls));
    let response = DashboardResponse {
        paused: controls.paused_at.is_some(),
        window_secs: controls.window.secs(),
    };

    egui::ScrollArea::vertical()
        .id_salt(id.with("scroll"))
        .auto_shrink(false)
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 14))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    above(ui);
                    let Some(now) = controls.paused_at.or(latest) else {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new(tr("Waiting for the first metrics…"))
                                    .color(p.text_muted),
                            )
                        });
                        return;
                    };
                    let window = controls.window.secs();
                    let groups = state.groups();
                    let ordered: Vec<(String, Vec<&MetricHistory>)> = match &layout.groups {
                        Some(order) => order
                            .iter()
                            .filter_map(|g| groups.iter().find(|(name, _)| name == g).cloned())
                            .collect(),
                        None => groups,
                    };
                    for (i, (group, metrics)) in ordered.into_iter().enumerate() {
                        if i > 0 {
                            ui.add_space(10.0);
                        }
                        kit::semibold_label(ui, &group, 13.0, p.text);
                        let avail = ui.available_width();
                        let gap = 12.0;
                        let cols = layout
                            .columns
                            // Four to a row like the design, fewer when they do not fit.
                            .unwrap_or(
                                ((avail + gap) / (200.0 + gap)).floor().clamp(1.0, 4.0) as usize
                            )
                            .max(1);
                        let card_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).max(120.0);
                        // Rows 12 apart (the spacing and 4), groups 18 (the spacing and 10).
                        for (r, row) in metrics.chunks(cols).enumerate() {
                            if r > 0 {
                                ui.add_space(4.0);
                            }
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                for m in row {
                                    metric_card(ui, m, now, window, card_w, layout.chart_height);
                                }
                            });
                        }
                    }
                });
        });
    response
}

/// The tone of a metric: warnings and errors for the ones that tell of trouble.
fn metric_color(m: &MetricHistory, p: &style::Palette) -> Color32 {
    let k = format!("{} {}", m.key, m.label).to_ascii_lowercase();
    if k.contains("deadlock") || k.contains("waiting") || k.contains("blocked") {
        p.err
    } else if k.contains("idle_in_transaction")
        || k.contains("idle in transaction")
        || k.contains("rollback")
    {
        p.warn
    } else if k.contains("hit_ratio") || k.contains("hit ratio") {
        p.ok
    } else {
        p.accent_text
    }
}

/// A count as in the design: whole numbers with thousands separators ("48,900"), one decimal
/// below 10 ("1.6", "0").
fn count_text(v: f64) -> String {
    if v.abs() < 10.0 {
        let r = (v * 10.0).round() / 10.0;
        return if r.fract() == 0.0 {
            format!("{r:.0}")
        } else {
            format!("{r:.1}")
        };
    }
    let n = v.round().abs() as u64;
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3 + 1);
    if v < 0.0 {
        out.push('-');
    }
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A value with its unit split for display ("1.2K", "/s").
fn split_unit(text: &str) -> (String, String) {
    let end = text
        .char_indices()
        .find(|(_, c)| {
            !(c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | 'K' | 'M' | 'B' | 'T' | '–'))
        })
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let (v, u) = text.split_at(end);
    if v.is_empty() {
        (text.to_string(), String::new())
    } else {
        (v.to_string(), u.trim().to_string())
    }
}

fn metric_card(ui: &mut Ui, m: &MetricHistory, now: f64, window: f64, width: f32, chart_h: f32) {
    let p = style::pal(ui);
    let (pad, head_h, foot_h) = (12.0, 22.0, 14.0);
    let (rect, resp) = ui.allocate_exact_size(
        vec2(width, pad * 2.0 + head_h + 6.0 + chart_h + 6.0 + foot_h),
        Sense::hover(),
    );
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect.expand(1.0));
    painter.rect(
        rect,
        CornerRadius::same(style::RADIUS_LG),
        p.surface,
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
    let rate = m.is_rate();
    let color = metric_color(m, &p);
    let inner = rect.shrink(pad);
    let head_y = inner.top() + head_h / 2.0;
    // label, value and unit
    let current = m.window(now - window, now).last().map(|(_, v)| v);
    let (value, unit) = match current {
        Some(c) if m.unit.is_empty() => (
            count_text(c),
            if rate {
                "/s".to_string()
            } else {
                String::new()
            },
        ),
        Some(c) => split_unit(&with_unit(c, &m.unit, rate)),
        None => ("–".to_string(), String::new()),
    };
    let unit_g = painter.layout_no_wrap(unit.clone(), FontId::proportional(12.0), p.text_faint);
    let unit_w = if unit.is_empty() {
        0.0
    } else {
        unit_g.size().x + 8.0
    };
    painter.galley(
        pos2(
            inner.right() - unit_g.size().x,
            head_y - unit_g.size().y / 2.0 + 2.0,
        ),
        unit_g,
        p.text_faint,
    );
    let value_g = painter.layout_no_wrap(value, style::semibold(ui.ctx(), 17.0), p.text);
    let value_r =
        Align2::RIGHT_CENTER.anchor_size(pos2(inner.right() - unit_w, head_y), value_g.size());
    kit::paint_semibold(&painter, value_r.min, value_g, p.text);
    let label = crate::common::text::truncate_to_width(
        m.label.as_str(),
        (value_r.left() - inner.left() - 8.0).max(20.0),
        12.5,
        false,
    );
    painter.text(
        pos2(inner.left(), head_y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        p.text_muted,
    );

    let chart = Rect::from_min_size(
        pos2(inner.left(), inner.top() + head_h + 6.0),
        vec2(inner.width(), chart_h),
    );
    let points: Vec<(f64, f64)> = m.window(now - window, now).collect();
    let stats = m.stats(now - window, now);
    let (lo, hi) = match stats {
        Some(s) => {
            let lo = if s.min >= 0.0 { 0.0 } else { s.min };
            // 10 % head room above the maximum, also for negative values.
            let hi = if s.max <= lo {
                lo + 1.0
            } else {
                s.max + (s.max - lo) * 0.1
            };
            (lo, hi)
        }
        None => (0.0, 1.0),
    };
    let sx = |t: f64| chart.left() + ((t - (now - window)) / window) as f32 * chart.width();
    let sy = |y: f64| chart.bottom() - 1.0 - ((y - lo) / (hi - lo)) as f32 * (chart.height() - 2.0);
    painter.hline(
        chart.x_range(),
        chart.bottom() - 0.5,
        Stroke::new(1.0, p.border),
    );
    painter.text(
        pos2(inner.left(), inner.bottom()),
        Align2::LEFT_BOTTOM,
        format!("-{}", short_duration(window)),
        FontId::proportional(11.0),
        p.text_faint,
    );
    painter.text(
        inner.right_bottom(),
        Align2::RIGHT_BOTTOM,
        tr("now"),
        FontId::proportional(11.0),
        p.text_faint,
    );
    if points.len() >= 2 {
        let line: Vec<Pos2> = points.iter().map(|&(t, y)| pos2(sx(t), sy(y))).collect();
        // area wash (~14 %) under the line
        let mut mesh = egui::Mesh::default();
        let fill = style::soft(color, 0.14);
        for p in &line {
            mesh.colored_vertex(*p, fill);
            mesh.colored_vertex(pos2(p.x, chart.bottom() - 1.0), fill);
        }
        for k in 0..(line.len() as u32 - 1) {
            let (a, b, c, d) = (2 * k, 2 * k + 1, 2 * k + 2, 2 * k + 3);
            mesh.add_triangle(a, b, d);
            mesh.add_triangle(a, d, c);
        }
        painter.add(Shape::mesh(mesh));
        painter.add(Shape::line(line, Stroke::new(2.0, color)));
    } else if let Some(&(t, y)) = points.first() {
        painter.circle_filled(pos2(sx(t), sy(y)), 3.0, color);
    } else {
        painter.text(
            chart.center(),
            Align2::CENTER_CENTER,
            tr("no data"),
            FontId::proportional(11.0),
            p.text_faint,
        );
    }
    // hover crosshair
    if let Some(pos) = resp
        .hover_pos()
        .filter(|pos| chart.expand2(vec2(0.0, 8.0)).contains(*pos))
        && let Some(&(t, y)) = points
            .iter()
            .min_by(|a, b| (sx(a.0) - pos.x).abs().total_cmp(&(sx(b.0) - pos.x).abs()))
    {
        let x = sx(t);
        painter.vline(x, chart.y_range(), Stroke::new(1.0, p.text_faint));
        painter.circle(pos2(x, sy(y)), 4.0, color, Stroke::new(2.0, p.surface));
        let ago = now - t;
        let text = trf(
            "{} · {} ago",
            &[&with_unit(y, &m.unit, rate), &short_duration(ago)],
        );
        resp.clone().on_hover_ui_at_pointer(|ui| {
            ui.strong(m.label.as_str());
            ui.label(text);
            if let Some(s) = stats {
                ui.weak(trf(
                    "min {} · avg {} · max {}",
                    &[
                        &with_unit(s.min, &m.unit, rate),
                        &with_unit(s.avg, &m.unit, rate),
                        &with_unit(s.max, &m.unit, rate),
                    ],
                ));
            }
            if m.resets() > 0 {
                ui.weak(trf("{} counter resets", &[&m.resets()]));
            }
        });
    }
}

fn short_duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as i64;
    if s < 60 {
        trf("{}s", &[&s])
    } else if s < 3600 {
        if s % 60 == 0 {
            trf("{}m", &[&(s / 60)])
        } else {
            trf("{}m{}s", &[&(s / 60), &(s % 60)])
        }
    } else {
        trf("{}h{}m", &[&(s / 3600), &((s % 3600) / 60)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(short_duration(5.2), "5s");
        assert_eq!(short_duration(300.0), "5m");
        assert_eq!(short_duration(95.0), "1m35s");
        assert_eq!(short_duration(3700.0), "1h1m");
        assert_eq!(WindowLength::FifteenMinutes.secs(), 900.0);
    }

    #[test]
    fn dashboards_render() {
        use crate::chart::{ChartConfig, ChartKind};
        use crate::dashboard::{
            CustomDashboard, CustomDashboardEvent, CustomDashboardState, DashboardItem,
            show_custom_dashboard,
        };
        use crate::test_harness::Harness;
        use ternarygrid_core::admin::Metric;
        let mut state = DashboardState::new();
        let layout = DashboardLayout {
            title: Some("server".into()),
            ..Default::default()
        };
        let mut h = Harness::new(1000.0, 800.0);
        let mut resp = None;
        let mut ui_fn = |ui: &mut Ui| resp = Some(show_dashboard(ui, &state, &layout));
        h.idle(2, &mut ui_fn); // empty state
        drop(ui_fn);
        for t in 0..120 {
            state.push(
                t as f64,
                &[
                    Metric::gauge("Connections", "c", "Active", (t % 7) as f64, ""),
                    Metric::counter("I/O", "r", "Read", t as f64 * 4096.0, "bytes"),
                    Metric::gauge("I/O", "h", "Hit ratio", 97.5, "%"),
                ],
            );
        }
        let mut ui_fn = |ui: &mut Ui| resp = Some(show_dashboard(ui, &state, &layout));
        h.idle(2, &mut ui_fn);
        h.hover(egui::pos2(150.0, 120.0), &mut ui_fn);
        drop(ui_fn);
        let r = resp.expect("response");
        assert!(!r.paused);
        assert_eq!(r.window_secs, 300.0);

        let mut custom = CustomDashboard::default();
        custom.add_item(DashboardItem {
            title: "a".into(),
            sql: "select 1".into(),
            chart: ChartConfig {
                kind: ChartKind::Bar,
                x_column: Some(0),
                y_columns: vec![1],
                ..Default::default()
            },
            ..Default::default()
        });
        let mut cs = CustomDashboardState::default();
        let mut events = Vec::new();
        let mut ui_fn =
            |ui: &mut Ui| events.extend(show_custom_dashboard(ui, &mut custom, &mut cs, 1.0));
        h.idle(2, &mut ui_fn);
        drop(ui_fn);
        assert_eq!(events, vec![CustomDashboardEvent::Refresh(1)]);
    }
}
