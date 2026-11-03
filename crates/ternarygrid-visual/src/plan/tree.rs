//! Explain tree table (explain.depesz style): collapsible node tree with
//! cost, estimated and actual rows, misestimate factor, time, loops and the
//! exclusive share as a heat bar; the slowest nodes are flagged.

use super::analysis::{FlatPlanNode, PlanMetric, flatten_plan};
use super::diagram::{PlanResponse, PlanViewState};
use crate::common::colors;
use crate::common::format::{compact, duration_ms, grouped};
use egui::{Align, Layout, RichText, Sense, Stroke, Ui, vec2};
use egui_phosphor::regular as icon;
use std::collections::BTreeSet;
use ternarygrid_core::plan::QueryPlan;
use ternarygrid_core::{tr, trf};

/// Number of slowest nodes that are flagged.
const HOT_NODES: usize = 3;

/// Shows the plan as a collapsible tree table. Selection is shared with
/// [`super::show_plan_diagram`] through `state`.
pub fn show_plan_tree(ui: &mut Ui, plan: &QueryPlan, state: &mut PlanViewState) -> PlanResponse {
    let flat = flatten_plan(plan);
    let metric = state.metric.resolve(plan);
    let before = state.selected.clone();
    let mut hottest: Vec<usize> = (0..flat.len())
        .filter(|&i| flat[i].share(metric).unwrap_or(0.0) > 0.0)
        .collect();
    hottest.sort_by(|&a, &b| {
        flat[b]
            .share(metric)
            .unwrap_or(0.0)
            .total_cmp(&flat[a].share(metric).unwrap_or(0.0))
    });
    let hot: BTreeSet<usize> = hottest.into_iter().take(HOT_NODES).collect();

    // visible rows: skip descendants of collapsed nodes
    let mut visible = Vec::new();
    let mut hidden_below: Option<usize> = None;
    for (i, f) in flat.iter().enumerate() {
        if let Some(d) = hidden_below {
            if f.depth > d {
                continue;
            }
            hidden_below = None;
        }
        visible.push(i);
        if state.collapsed.contains(&f.path) {
            hidden_below = Some(f.depth);
        }
    }

    // Inset as the results' other views (16 from the left; the table scrolls to the right edge).
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 16,
            right: 0,
            top: 8,
            bottom: 0,
        })
        .show(ui, |ui| {
            tree_body(ui, &flat, &visible, &hot, metric, state, before)
        })
        .inner
}

#[allow(clippy::too_many_arguments)]
fn tree_body(
    ui: &mut Ui,
    flat: &[FlatPlanNode<'_>],
    visible: &[usize],
    hot: &BTreeSet<usize>,
    metric: PlanMetric,
    state: &mut PlanViewState,
    before: Option<super::analysis::PlanNodePath>,
) -> PlanResponse {
    ui.horizontal(|ui| {
        use ternarygrid_ui::kit;
        if kit::button_full(
            ui,
            kit::Kind::Ghost,
            Some(icon::CARET_DOWN),
            tr("Expand all"),
            true,
            false,
        )
        .clicked()
        {
            state.collapsed.clear();
        }
        if kit::button_full(
            ui,
            kit::Kind::Ghost,
            Some(icon::CARET_RIGHT),
            tr("Collapse all"),
            true,
            false,
        )
        .clicked()
        {
            state.collapsed = flat
                .iter()
                .filter(|f| !f.children.is_empty())
                .map(|f| f.path.clone())
                .collect();
        }
        kit::vdivider(ui, 16.0);
        ui.label(RichText::new(tr("Color by")).color(ternarygrid_ui::pal(ui).text_muted));
        super::diagram::metric_combo(ui, &mut state.metric, "ternarygrid_plan_tree_metric");
    });
    ui.separator();

    let mut hovered = None;
    let mut toggles: Vec<Vec<usize>> = Vec::new();
    let mut clicked: Option<Vec<usize>> = None;
    let out = egui::ScrollArea::both()
        .auto_shrink(false)
        .id_salt("ternarygrid_plan_tree")
        .show(ui, |ui| {
            egui::Grid::new("ternarygrid_plan_tree_grid")
                .striped(true)
                .num_columns(8)
                .spacing(vec2(14.0, 3.0))
                .show(ui, |ui| {
                    // The numbers' headers over them, at the right.
                    for (h, right) in [
                        ("Node", false),
                        ("Cost", true),
                        ("Rows (est.)", true),
                        ("Rows (actual)", true),
                        ("Estimate", true),
                        ("Time", true),
                        ("Loops", true),
                        ("Exclusive", false),
                    ] {
                        let text = RichText::new(tr(h)).strong();
                        if right {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(text)
                            });
                        } else {
                            ui.label(text);
                        }
                    }
                    ui.end_row();
                    for &i in visible {
                        let Some(f) = flat.get(i) else { continue };
                        let selected = state.selected.as_ref() == Some(&f.path);
                        let row_resp = node_cell(
                            ui,
                            f,
                            selected,
                            hot.contains(&i),
                            state.collapsed.contains(&f.path),
                            &mut toggles,
                        );
                        if row_resp.clicked() {
                            clicked = Some(f.path.clone());
                        }
                        if row_resp.hovered() {
                            hovered = Some(f.path.clone());
                        }
                        num(ui, f.node.total_cost.map(|c| compact(c, 2)));
                        num(ui, f.node.plan_rows.map(|r| grouped(r.round())));
                        num(ui, f.node.actual_rows.map(|r| grouped(r.round())));
                        misestimate(ui, f);
                        num(ui, f.inclusive_ms.map(duration_ms));
                        num(ui, f.node.loops.map(grouped));
                        share_bar(ui, f, metric, hot.contains(&i));
                        ui.end_row();
                    }
                })
        });
    for path in toggles {
        if !state.collapsed.remove(&path) {
            state.collapsed.insert(path);
        }
    }
    if let Some(p) = clicked {
        state.selected = if state.selected.as_ref() == Some(&p) {
            None
        } else {
            Some(p)
        };
    }
    if hovered.is_some() {
        state.hovered = hovered.clone();
    }
    PlanResponse {
        selection_changed: state.selected != before,
        selected: state.selected.clone(),
        hovered,
        response: Some(out.inner.response),
    }
}

fn node_cell(
    ui: &mut Ui,
    f: &FlatPlanNode<'_>,
    selected: bool,
    hot: bool,
    collapsed: bool,
    toggles: &mut Vec<Vec<usize>>,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(f.depth as f32 * 16.0);
        if f.children.is_empty() {
            ui.add_space(18.0);
        } else {
            let caret = if collapsed {
                icon::CARET_RIGHT
            } else {
                icon::CARET_DOWN
            };
            if ui
                .add(egui::Button::new(caret).frame(false).small())
                .clicked()
            {
                toggles.push(f.path.clone());
            }
        }
        if hot {
            ui.label(RichText::new(icon::FIRE).color(colors::STATUS_CRITICAL))
                .on_hover_text(tr("One of the slowest nodes"));
        }
        let mut title = RichText::new(f.node.title());
        if hot {
            title = title.strong();
        }
        let resp = ui.add(egui::Button::selectable(selected, title).frame_when_inactive(false));
        if let Some(c) = &f.node.condition {
            ui.label(
                RichText::new(c.as_str())
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        }
        resp
    })
    .inner
}

fn num(ui: &mut Ui, v: Option<String>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| match v {
        Some(s) => ui.monospace(s),
        None => ui.weak("–"),
    });
}

/// How far the actual rows are from the estimate: "1.6K× more" / "8× fewer" (muted; in the
/// warning colour from 10 times on), "–" for a close estimate (half to twice).
fn misestimate(ui: &mut Ui, f: &FlatPlanNode<'_>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let p = ternarygrid_ui::pal(ui);
        match f.row_factor {
            Some(rf) if rf > 0.0 && (0.5..=2.0).contains(&rf) => ui
                .label(RichText::new("–").color(p.text_faint))
                .on_hover_text(tr("Close to the estimate")),
            Some(rf) if rf > 0.0 => {
                let more = rf >= 1.0;
                let k = if more { rf } else { 1.0 / rf };
                let n = compact(k, 1);
                let text = if more {
                    trf("{}× more", &[&n])
                } else {
                    trf("{}× fewer", &[&n])
                };
                let color = if k >= 10.0 { p.warn } else { p.text_muted };
                ui.label(RichText::new(text).monospace().color(color))
                    .on_hover_text(if more {
                        trf("{} times more rows than estimated", &[&n])
                    } else {
                        trf("{} times fewer rows than estimated", &[&n])
                    })
            }
            _ => ui.label(RichText::new("–").color(p.text_faint)),
        }
    });
}

fn share_bar(ui: &mut Ui, f: &FlatPlanNode<'_>, metric: PlanMetric, hot: bool) {
    let share = f.share(metric);
    let (rect, resp) = ui.allocate_exact_size(
        vec2(110.0, ui.spacing().interact_size.y * 0.8),
        Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let v = ui.visuals();
    painter.rect_filled(rect, 3.0, v.faint_bg_color);
    if let Some(s) = share {
        let w = (rect.width() * s as f32).max(if s > 0.0 { 2.0 } else { 0.0 });
        let fill = colors::heat((s * 2.0) as f32);
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, vec2(w, rect.height())),
            3.0,
            fill,
        );
        let text_color = if w > rect.width() * 0.55 {
            colors::text_on(fill)
        } else {
            v.text_color()
        };
        painter.text(
            rect.right_center() - vec2(4.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            format!("{:.1}%", s * 100.0),
            egui::FontId::monospace(11.0),
            text_color,
        );
        if hot {
            painter.rect_stroke(
                rect,
                3.0,
                Stroke::new(1.0, colors::STATUS_CRITICAL),
                egui::StrokeKind::Inside,
            );
        }
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "–",
            egui::FontId::monospace(11.0),
            v.weak_text_color(),
        );
    }
    let _ = resp.on_hover_text(match (f.exclusive_ms, f.exclusive_cost) {
        (Some(ms), _) => trf("Exclusive time {}", &[&duration_ms(ms)]),
        (None, Some(c)) => trf("Exclusive cost {}", &[&compact(c, 2)]),
        _ => tr("No measurements").to_string(),
    });
}

/// Shows the textual plan (as printed by the database, or rendered from the
/// node tree when the server gave no text form) in a scrollable monospace view.
pub fn show_plan_text(ui: &mut Ui, plan: &QueryPlan) {
    let text = if plan.text.trim().is_empty() {
        plan.to_text_tree()
    } else {
        plan.text.clone()
    };
    // Inset as the results' other views: Copy in a bar 16 / 8 from the edges, the text 16 in.
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 16,
            right: 16,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            use ternarygrid_ui::kit;
            if kit::button_full(
                ui,
                kit::Kind::Secondary,
                Some(icon::COPY),
                tr("Copy"),
                true,
                false,
            )
            .clicked()
            {
                ui.ctx().copy_text(text.clone());
            }
            ui.add_space(4.0);
            egui::ScrollArea::both()
                .auto_shrink(false)
                .id_salt("ternarygrid_plan_text")
                .show(ui, |ui| {
                    let mut t = text.as_str();
                    ui.add(
                        egui::TextEdit::multiline(&mut t)
                            .code_editor()
                            .desired_width(f32::INFINITY),
                    );
                });
        });
}
