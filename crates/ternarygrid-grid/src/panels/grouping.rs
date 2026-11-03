//! Grouping panel: group fetched rows by columns with counts and an optional
//! SUM / AVG of a measure; clicking a group filters the grid (FR-D5).

use crate::grouping::{GroupResult, GroupSpec, compute_groups, group_filter};
use crate::model::ResultModel;
use crate::task::{BackgroundTask, TaskPoll};
use egui::{ComboBox, RichText, ScrollArea, Ui};
use std::sync::Arc;
use ternarygrid_core::{tr, trf};

/// Maximum number of distinct groups kept.
pub const MAX_GROUPS: usize = 10_000;

/// State of the grouping panel.
#[derive(Debug)]
pub struct GroupingPanelState {
    /// Columns to group by.
    pub spec: GroupSpec,
    result: Option<GroupResult>,
    result_key: Option<(u64, GroupSpec)>,
    task: Option<BackgroundTask<GroupResult>>,
    pending_key: Option<(u64, GroupSpec)>,
    /// Auto recompute when the data changes.
    pub live: bool,
}

impl Default for GroupingPanelState {
    fn default() -> Self {
        GroupingPanelState { spec: GroupSpec::default(), result: None, result_key: None, task: None, pending_key: None, live: true }
    }
}

impl GroupingPanelState {
    /// The last computed grouping.
    pub fn result(&self) -> Option<&GroupResult> {
        self.result.as_ref()
    }

    /// True while grouping runs in the background.
    pub fn is_running(&self) -> bool {
        self.task.is_some()
    }
}

/// What the panel asks the caller to do.
#[derive(Debug, Clone, PartialEq)]
pub enum GroupingAction {
    /// A client filter was applied for the clicked group (its text).
    Filtered(String),
}

/// The grouping panel.
pub struct GroupingPanel<'a> {
    model: &'a mut ResultModel,
    state: &'a mut GroupingPanelState,
}

impl<'a> GroupingPanel<'a> {
    /// New panel.
    pub fn new(model: &'a mut ResultModel, state: &'a mut GroupingPanelState) -> Self {
        GroupingPanel { model, state }
    }

    fn start(model: &ResultModel, state: &mut GroupingPanelState, ctx: &egui::Context) {
        let store = model.store().clone();
        let view: Option<Arc<Vec<u32>>> = model.view().map(|v| Arc::new(v.to_vec()));
        let spec = state.spec.clone();
        let vctx = model.view_context();
        let repaint = ctx.clone();
        state.pending_key = Some((model.generation(), spec.clone()));
        state.task = Some(BackgroundTask::spawn(
            "ternarygrid-grid-group",
            move |cancel| compute_groups(&store, view.as_deref().map(|v| v.as_slice()), &spec, &vctx, MAX_GROUPS, cancel),
            Box::new(move || repaint.request_repaint()),
        ));
    }

    /// Show the panel.
    pub fn show(self, ui: &mut Ui) -> Option<GroupingAction> {
        let GroupingPanel { model, state } = self;
        let mut action = None;
        // Poll the background task.
        if let Some(task) = &state.task {
            match task.poll() {
                TaskPoll::Running => {}
                TaskPoll::Done(r) => {
                    state.result = Some(r);
                    state.result_key = state.pending_key.take();
                    state.task = None;
                }
                TaskPoll::Failed => {
                    state.task = None;
                }
            }
        }
        let names = model.column_names();
        let mut changed = false;
        let pal = ternarygrid_ui::pal(ui);
        ui.spacing_mut().item_spacing.y = 8.0;
        // The combo boxes as tall as the Refresh button (the kit's small controls).
        ui.spacing_mut().interact_size.y = ternarygrid_ui::style::SMALL_H;
        let label_w =
            ["Group by", "Measure"].iter().map(|l| ui.painter().layout_no_wrap(tr(l).to_string(), egui::FontId::proportional(12.5), pal.text).size().x).fold(0.0, f32::max);
        let label = |ui: &mut Ui, text: &str| {
            ui.allocate_ui_with_layout(egui::vec2(label_w, ternarygrid_ui::style::SMALL_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_size(egui::vec2(label_w, ternarygrid_ui::style::SMALL_H));
                ui.label(RichText::new(text).size(12.5).color(pal.text_muted));
            });
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            label(ui, tr("Group by"));
            let mut remove = None;
            for (i, c) in state.spec.columns.iter().enumerate() {
                // A chip per grouping column ("status ✕"), in the controls' text size; a click removes it.
                let label = format!("{} ✕", names.get(*c).map_or("?", String::as_str));
                let mut job = egui::text::LayoutJob::default();
                job.append(names.get(*c).map_or("?", String::as_str), 0.0, egui::TextFormat::simple(ternarygrid_ui::style::medium(ui.ctx(), 12.5), pal.text));
                job.append(egui_phosphor::regular::X, 6.0, egui::TextFormat::simple(ternarygrid_ui::style::icon_font(12.0), pal.text_muted));
                let g = ui.fonts_mut(|f| f.layout_job(job));
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 20.0, ternarygrid_ui::style::SMALL_H), egui::Sense::click());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
                ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, if resp.hovered() { pal.border_strong } else { pal.subtle });
                ui.painter().galley(rect.center() - g.size() / 2.0, g, pal.text);
                if resp.on_hover_text(tr("Remove from the grouping")).clicked() {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                state.spec.columns.remove(i);
                changed = true;
            }
            ComboBox::from_id_salt("ternarygrid_group_add").width(140.0).icon(ternarygrid_ui::kit::combo_caret).selected_text(tr("Add column…")).show_ui(ui, |ui| {
                ternarygrid_ui::kit::menu_look(ui, 140.0);
                for (c, n) in names.iter().enumerate() {
                    if !state.spec.columns.contains(&c) && ternarygrid_ui::kit::menu_item(ui, None, n, "").clicked() {
                        state.spec.columns.push(c);
                        changed = true;
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            label(ui, tr("Measure"));
            let current = state.spec.measure.and_then(|m| names.get(m)).cloned().unwrap_or_else(|| tr("None").to_string());
            ComboBox::from_id_salt("ternarygrid_group_measure").width(120.0).icon(ternarygrid_ui::kit::combo_caret).selected_text(current).show_ui(ui, |ui| {
                ternarygrid_ui::kit::menu_look(ui, 120.0);
                if ternarygrid_ui::kit::menu_check(ui, &mut state.spec.measure.is_none(), tr("None")).clicked() {
                    state.spec.measure = None;
                    changed = true;
                }
                for (c, n) in names.iter().enumerate() {
                    let numeric = model.column_kind(c).is_numeric() || model.is_virtual_column(c);
                    if numeric && ternarygrid_ui::kit::menu_check(ui, &mut (state.spec.measure == Some(c)), n).clicked() {
                        state.spec.measure = Some(c);
                        changed = true;
                    }
                }
            });
            ternarygrid_ui::kit::checkbox(ui, &mut state.live, tr("Live")).on_hover_text(tr("Group again when the rows change"));
            // Live groups again by itself: Refresh only without it.
            let refresh = ui
                .add_enabled_ui(!state.live, |ui| ternarygrid_ui::kit::button_full(ui, ternarygrid_ui::kit::Kind::Secondary, None, tr("Refresh"), true, false))
                .inner
                .on_disabled_hover_text(tr("Live groups again when the rows change"));
            if refresh.clicked() {
                changed = true;
            }
        });
        let key = (model.generation(), state.spec.clone());
        let stale = state.result_key.as_ref() != Some(&key) && state.pending_key.as_ref() != Some(&key);
        if !state.spec.columns.is_empty() && (changed || (stale && (state.live || state.result.is_none()))) {
            Self::start(model, state, ui.ctx());
        }
        ui.add_space(2.0);
        if state.spec.columns.is_empty() {
            ui.label(RichText::new(tr("Choose one or more columns to group the fetched rows")).color(pal.text_muted));
            return None;
        }
        if state.task.is_some() {
            ternarygrid_ui::parts::spinner_label(ui, tr("Grouping…"));
        }
        let Some(result) = &state.result else { return None };
        let count = |n: usize, one: &str, many: &str| trf(if n == 1 { one } else { many }, &[&crate::format::display_count(n)]);
        let groups = count(result.groups.len(), tr("{} group"), tr("{} groups"));
        let rows = count(result.rows, tr("{} row"), tr("{} rows"));
        ui.label(RichText::new(trf("{} in {}", &[&groups, &rows])).size(12.0).color(pal.text_muted));
        ui.add_space(2.0);
        if result.truncated {
            ternarygrid_ui::parts::status(
                ui,
                ternarygrid_ui::parts::Tone::Warning,
                &trf("More than {} groups; the rest were dropped.", &[&crate::format::display_count(MAX_GROUPS)]),
            );
        }
        let spec = state.spec.clone();
        let has_measure = spec.measure.is_some();
        let mut clicked: Option<usize> = None;
        // A bordered table: the keys as links (a click filters the grid), the numbers at the right.
        let theme = crate::theme::GridTheme::from_visuals(ui.visuals());
        let (head_h, row_h) = (33.0, 31.0);
        let mut shares: Vec<f32> = spec.columns.iter().map(|_| 1.4).collect();
        shares.push(0.7);
        if has_measure {
            shares.extend([1.0, 1.0]);
        }
        let width = ui.available_width();
        let unit = width / shares.iter().sum::<f32>();
        let widths: Vec<f32> = shares.iter().map(|s| s * unit).collect();
        let nkeys = spec.columns.len();
        let head_font = ternarygrid_ui::style::semibold(ui.ctx(), 12.0);
        let link_font = egui::FontId::proportional(13.0);
        let mono = egui::FontId::monospace(12.0);
        let cell = |ui: &Ui, rect: egui::Rect, text: &str, font: &egui::FontId, color: egui::Color32, right: bool, underline: bool| {
            let g = ui.painter().layout_no_wrap(text.to_string(), font.clone(), color);
            let inner = rect.shrink2(egui::vec2(10.0, 0.0));
            let x = if right { inner.right() - g.size().x } else { inner.left() };
            let pos = egui::pos2(x, rect.center().y - g.size().y / 2.0);
            let w = g.size().x.min(inner.width());
            let painter = ui.painter().with_clip_rect(inner.intersect(ui.clip_rect()));
            painter.galley(pos, g, color);
            if underline {
                painter.hline(pos.x..=pos.x + w, rect.center().y + 8.0, egui::Stroke::new(1.0, color));
            }
        };
        ui.spacing_mut().item_spacing.y = 0.0;
        let (head, _) = ui.allocate_exact_size(egui::vec2(width, head_h), egui::Sense::hover());
        ui.painter().rect_filled(head, egui::CornerRadius { nw: 8, ne: 8, ..Default::default() }, pal.header);
        ui.painter().hline(head.x_range(), head.bottom() - 0.5, egui::Stroke::new(1.0, pal.border));
        let mut titles: Vec<&str> = spec.columns.iter().map(|&c| names.get(c).map_or("?", String::as_str)).collect();
        titles.push(tr("Count"));
        if has_measure {
            titles.extend([tr("Sum"), tr("Avg")]);
        }
        let mut x = head.left();
        for (i, t) in titles.iter().enumerate() {
            cell(ui, egui::Rect::from_min_size(egui::pos2(x, head.top()), egui::vec2(widths[i], head_h - 1.0)), t, &head_font, pal.text_muted, i >= nkeys, false);
            x += widths[i];
        }
        let top = head.top();
        let mut bottom = head.bottom();
        ScrollArea::vertical().auto_shrink([false, true]).show_rows(ui, row_h, result.groups.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let g = &result.groups[i];
                let (row, _) = ui.allocate_exact_size(egui::vec2(width, row_h), egui::Sense::hover());
                ui.painter().rect_filled(row, 0.0, if i % 2 == 1 { theme.stripe } else { pal.surface });
                ui.painter().hline(row.x_range(), row.bottom() - 0.5, egui::Stroke::new(1.0, theme.line));
                let mut x = row.left();
                for (j, k) in g.keys.iter().enumerate() {
                    let r = egui::Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(widths[j], row_h - 1.0));
                    let resp = ui.interact(r, ui.id().with(("group_key", i, j)), egui::Sense::click()).on_hover_text(tr("Filter the grid to this group"));
                    let text = if k.is_null() { "NULL".to_string() } else { k.to_text().chars().take(60).collect::<String>() };
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, true, &text));
                    // Links in the accent colour, underlined while hovered (as the app's other links).
                    if k.is_null() {
                        cell(ui, r, &text, &link_font, pal.text_faint, false, false);
                    } else {
                        cell(ui, r, &text, &link_font, pal.accent_text, false, resp.hovered());
                    }
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                    x += widths[j];
                }
                let mut numbers = vec![crate::format::display_count(g.count)];
                if has_measure {
                    numbers.push(g.sum.map(crate::format::display_number).unwrap_or_default());
                    numbers.push(g.avg.map(crate::format::display_number).unwrap_or_default());
                }
                for (j, n) in numbers.iter().enumerate() {
                    let w = widths[nkeys + j];
                    cell(ui, egui::Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(w, row_h - 1.0)), n, &mono, pal.text, true, false);
                    x += w;
                }
                bottom = bottom.max(row.bottom().min(ui.clip_rect().bottom()));
            }
        });
        let card = egui::Rect::from_min_max(egui::pos2(head.left(), top), egui::pos2(head.left() + width, bottom));
        ui.painter().rect_stroke(card, 8, egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
        if let Some(i) = clicked
            && let Some(f) = group_filter(&spec, &result.groups[i], &names)
        {
            // The groups were counted in the filtered rows: narrow that filter, do not replace it.
            let f = match model.requested_view_spec().filter {
                Some(current) => current.and(f),
                None => f,
            };
            let text = f.text().to_string();
            crate::grid::apply_client_filter(model, Some(f), ui.ctx());
            action = Some(GroupingAction::Filtered(text));
        }
        action
    }
}
