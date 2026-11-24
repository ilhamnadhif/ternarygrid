//! Grouping panel: group fetched rows by columns with counts and one or more
//! SUM / AVG measures, sortable columns (ASC / DESC), and click-to-filter (FR-D5).

use crate::grouping::{GroupResult, GroupSort, GroupSortCol, GroupSpec, compute_groups, group_filter, sort_groups};
use crate::model::ResultModel;
use crate::task::{BackgroundTask, TaskPoll};
use egui::{ComboBox, RichText, ScrollArea, Ui};
use egui_phosphor::regular as icon;
use std::sync::Arc;
use ternarygrid_core::{tr, trf};

/// Maximum number of distinct groups kept.
pub const MAX_GROUPS: usize = 10_000;

/// State of the grouping panel.
#[derive(Debug)]
pub struct GroupingPanelState {
    /// Columns to group by and numeric measures to aggregate.
    pub spec: GroupSpec,
    /// Current sort column and direction of the grouped table.
    pub sort: GroupSort,
    result: Option<GroupResult>,
    result_key: Option<(u64, GroupSpec)>,
    task: Option<BackgroundTask<GroupResult>>,
    pending_key: Option<(u64, GroupSpec)>,
    /// Auto recompute when the data changes.
    pub live: bool,
}

impl Default for GroupingPanelState {
    fn default() -> Self {
        GroupingPanelState { spec: GroupSpec::default(), sort: GroupSort::default(), result: None, result_key: None, task: None, pending_key: None, live: true }
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

    /// Poll any in-flight background grouping task. Returns `true` when it finishes.
    pub fn poll(&mut self) -> bool {
        let Some(task) = &self.task else { return false };
        match task.poll() {
            TaskPoll::Running => false,
            TaskPoll::Done(mut r) => {
                GroupingPanel::clamp_sort(self);
                sort_groups(&mut r.groups, self.sort);
                self.result = Some(r);
                self.result_key = self.pending_key.take();
                self.task = None;
                true
            }
            TaskPoll::Failed => {
                self.task = None;
                true
            }
        }
    }
}

/// What the panel asks the caller to do.
#[derive(Debug, Clone, PartialEq)]
pub enum GroupingAction {
    /// A client filter was applied for the clicked group (its text).
    Filtered(String),
}

struct TableCol {
    title: String,
    tooltip: String,
    sort_col: GroupSortCol,
    right: bool,
    share: f32,
    min_w: f32,
}

/// Draw a removable pill chip (`name ×`), returning true when clicked.
fn removable_chip(ui: &mut Ui, name: &str, tip: &str, pal: &ternarygrid_ui::style::Palette) -> bool {
    let label = format!("{name} ✕");
    let mut job = egui::text::LayoutJob::default();
    job.append(name, 0.0, egui::TextFormat::simple(ternarygrid_ui::style::medium(ui.ctx(), 12.5), pal.text));
    job.append(icon::X, 6.0, egui::TextFormat::simple(ternarygrid_ui::style::icon_font(12.0), pal.text_muted));
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 20.0, ternarygrid_ui::style::SMALL_H), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    ui.painter().rect_filled(rect, ternarygrid_ui::style::RADIUS, if resp.hovered() { pal.border_strong } else { pal.subtle });
    ui.painter().galley(rect.center() - g.size() / 2.0, g, pal.text);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tip).clicked()
}

/// Paint single-line ellipsized text in `rect`; returns true when elided.
fn paint_cell(ui: &Ui, rect: egui::Rect, text: &str, font: &egui::FontId, color: egui::Color32, right: bool, underline: bool) -> bool {
    let inner = rect.shrink2(egui::vec2(10.0, 0.0));
    if inner.width() <= 2.0 {
        return !text.is_empty();
    }
    let mut job = egui::text::LayoutJob::single_section(text.to_string(), egui::TextFormat::simple(font.clone(), color));
    job.wrap = egui::text::TextWrapping { max_width: inner.width(), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = ui.painter().layout_job(job);
    let elided = g.elided;
    let w = g.size().x.min(inner.width());
    let x = if right { (inner.right() - w).max(inner.left()) } else { inner.left() };
    let pos = egui::pos2(x, rect.center().y - g.size().y / 2.0);
    let painter = ui.painter().with_clip_rect(inner.expand2(egui::vec2(1.0, 0.0)).intersect(ui.clip_rect()));
    painter.galley(pos, g, color);
    if underline {
        painter.hline(pos.x..=pos.x + w, rect.center().y + 8.0, egui::Stroke::new(1.0, color));
    }
    elided
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

    /// Ensure `state.sort` still references a valid column after columns/measures change.
    fn clamp_sort(state: &mut GroupingPanelState) {
        let valid = match state.sort.col {
            GroupSortCol::Key(i) => i < state.spec.columns.len(),
            GroupSortCol::Count => true,
            GroupSortCol::Sum(i) | GroupSortCol::Avg(i) => i < state.spec.measures.len(),
        };
        if !valid {
            state.sort = GroupSort::default();
        }
    }

    /// Show the panel.
    pub fn show(self, ui: &mut Ui) -> Option<GroupingAction> {
        let GroupingPanel { model, state } = self;
        let mut action = None;
        // Poll the background task.
        state.poll();
        let names = model.column_names();
        let col_count = names.len();
        state.spec.columns.retain(|&c| c < col_count);
        state.spec.measures.retain(|&c| c < col_count);

        let mut changed = false;
        let pal = ternarygrid_ui::pal(ui);
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.spacing_mut().interact_size.y = ternarygrid_ui::style::SMALL_H;
        let label_w =
            ["Group by", "Measures"].iter().map(|l| ui.painter().layout_no_wrap(tr(l).to_string(), egui::FontId::proportional(12.5), pal.text).size().x).fold(0.0, f32::max);
        let label = |ui: &mut Ui, text: &str| {
            ui.allocate_ui_with_layout(egui::vec2(label_w, ternarygrid_ui::style::SMALL_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_size(egui::vec2(label_w, ternarygrid_ui::style::SMALL_H));
                ui.label(RichText::new(text).size(12.5).color(pal.text_muted));
            });
        };

        // Row 1: Group by chips + Add column…
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            label(ui, tr("Group by"));
            let mut remove = None;
            for (i, &c) in state.spec.columns.iter().enumerate() {
                let name = names.get(c).map_or("?", String::as_str);
                if removable_chip(ui, name, tr("Remove from the grouping"), &pal) {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                state.spec.columns.remove(i);
                if let GroupSortCol::Key(k) = state.sort.col {
                    if k == i {
                        state.sort = GroupSort::default();
                    } else if k > i {
                        state.sort.col = GroupSortCol::Key(k - 1);
                    }
                }
                changed = true;
            }
            let remaining_cols: Vec<(usize, &String)> = names.iter().enumerate().filter(|(c, _)| !state.spec.columns.contains(c)).collect();
            ui.add_enabled_ui(!remaining_cols.is_empty(), |ui| {
                ComboBox::from_id_salt("ternarygrid_group_add").width(136.0).icon(ternarygrid_ui::kit::combo_caret).selected_text(tr("Add column…")).show_ui(ui, |ui| {
                    ternarygrid_ui::kit::menu_look(ui, 160.0);
                    for (c, n) in &remaining_cols {
                        if ternarygrid_ui::kit::menu_item(ui, None, n, "").clicked() {
                            state.spec.columns.push(*c);
                            changed = true;
                        }
                    }
                });
            });
        });

        // Row 2: Measures multi-column chips + Add measure… + Live + Refresh
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            label(ui, tr("Measures"));
            let mut remove_m = None;
            for (i, &c) in state.spec.measures.iter().enumerate() {
                let name = names.get(c).map_or("?", String::as_str);
                if removable_chip(ui, name, tr("Remove measure"), &pal) {
                    remove_m = Some(i);
                }
            }
            if let Some(i) = remove_m {
                state.spec.measures.remove(i);
                match state.sort.col {
                    GroupSortCol::Sum(m) if m == i => state.sort = GroupSort::default(),
                    GroupSortCol::Sum(m) if m > i => state.sort.col = GroupSortCol::Sum(m - 1),
                    GroupSortCol::Avg(m) if m == i => state.sort = GroupSort::default(),
                    GroupSortCol::Avg(m) if m > i => state.sort.col = GroupSortCol::Avg(m - 1),
                    _ => {}
                }
                changed = true;
            }
            let numeric_cols: Vec<(usize, &String)> =
                names.iter().enumerate().filter(|(c, _)| (model.column_kind(*c).is_numeric() || model.is_virtual_column(*c)) && !state.spec.measures.contains(c)).collect();
            ui.add_enabled_ui(!numeric_cols.is_empty() || !state.spec.measures.is_empty(), |ui| {
                ComboBox::from_id_salt("ternarygrid_group_measure").width(136.0).icon(ternarygrid_ui::kit::combo_caret).selected_text(tr("Add measure…")).show_ui(ui, |ui| {
                    ternarygrid_ui::kit::menu_look(ui, 168.0);
                    for (c, n) in &numeric_cols {
                        if ternarygrid_ui::kit::menu_item(ui, None, n, "").clicked() {
                            state.spec.measures.push(*c);
                            changed = true;
                        }
                    }
                    if !state.spec.measures.is_empty() {
                        if !numeric_cols.is_empty() {
                            ternarygrid_ui::kit::menu_separator(ui);
                        }
                        if ternarygrid_ui::kit::menu_item(ui, Some(icon::X), tr("Clear measures"), "").clicked() {
                            state.spec.measures.clear();
                            Self::clamp_sort(state);
                            changed = true;
                        }
                    }
                });
            });
            ternarygrid_ui::kit::checkbox(ui, &mut state.live, tr("Live")).on_hover_text(tr("Group again when the rows change"));
            let refresh = ui
                .add_enabled_ui(!state.live, |ui| ternarygrid_ui::kit::button_full(ui, ternarygrid_ui::kit::Kind::Secondary, None, tr("Refresh"), true, false))
                .inner
                .on_disabled_hover_text(tr("Live groups again when the rows change"));
            if refresh.clicked() {
                changed = true;
            }
        });

        Self::clamp_sort(state);
        if state.spec.columns.is_empty() {
            state.task = None;
            state.result = None;
            state.result_key = None;
            state.pending_key = None;
            ui.add_space(2.0);
            ui.label(RichText::new(tr("Choose one or more columns to group the fetched rows")).color(pal.text_muted));
            return None;
        }
        let key = (model.generation(), state.spec.clone());
        let stale = state.result_key.as_ref() != Some(&key) && state.pending_key.as_ref() != Some(&key);
        if changed || (stale && (state.live || state.result.is_none())) {
            Self::start(model, state, ui.ctx());
        }
        ui.add_space(2.0);
        if state.task.is_some() {
            ternarygrid_ui::parts::spinner_label(ui, tr("Grouping…"));
        }
        let result = state.result.as_mut()?;
        let count = |n: usize, one: &str, many: &str| trf(if n == 1 { one } else { many }, &[&crate::format::display_count(n)]);
        let groups_text = count(result.groups.len(), tr("{} group"), tr("{} groups"));
        let rows_text = count(result.rows, tr("{} row"), tr("{} rows"));
        ui.label(RichText::new(trf("{} in {}", &[&groups_text, &rows_text])).size(12.0).color(pal.text_muted));
        ui.add_space(2.0);
        if result.truncated {
            ternarygrid_ui::parts::status(
                ui,
                ternarygrid_ui::parts::Tone::Warning,
                &trf("More than {} groups; the rest were dropped.", &[&crate::format::display_count(MAX_GROUPS)]),
            );
        }

        let spec = state.result_key.as_ref().map_or_else(|| state.spec.clone(), |(_, s)| s.clone());
        let multi_measure = spec.measures.len() > 1;
        let head_font = ternarygrid_ui::style::semibold(ui.ctx(), 12.0);
        let link_font = egui::FontId::proportional(13.0);
        let mono = egui::FontId::monospace(12.0);
        let theme = crate::theme::GridTheme::from_visuals(ui.visuals());
        let (head_h, row_h) = (33.0, 31.0);

        let measure_w = |text: &str| ui.painter().layout_no_wrap(text.to_string(), head_font.clone(), pal.text).size().x;
        let mut cols: Vec<TableCol> = Vec::with_capacity(spec.columns.len() + 1 + spec.measures.len() * 2);
        for (i, &c) in spec.columns.iter().enumerate() {
            let title = names.get(c).map_or("?", String::as_str).to_string();
            let min_w = (measure_w(&title) + 38.0).clamp(88.0, 220.0);
            let tooltip = trf("Sort by {}", &[&title]);
            cols.push(TableCol { title, tooltip, sort_col: GroupSortCol::Key(i), right: false, share: 1.4, min_w });
        }
        {
            let title = tr("Count").to_string();
            let min_w = (measure_w(&title) + 38.0).max(74.0);
            cols.push(TableCol { title, tooltip: tr("Sort by Count").to_string(), sort_col: GroupSortCol::Count, right: true, share: 0.8, min_w });
        }
        for (i, &c) in spec.measures.iter().enumerate() {
            let mname = names.get(c).map_or("?", String::as_str);
            let sum_title = if multi_measure { format!("{} ({mname})", tr("Sum")) } else { tr("Sum").to_string() };
            let avg_title = if multi_measure { format!("{} ({mname})", tr("Avg")) } else { tr("Avg").to_string() };
            let sum_min = (measure_w(&sum_title) + 38.0).clamp(88.0, 220.0);
            let avg_min = (measure_w(&avg_title) + 38.0).clamp(88.0, 220.0);
            cols.push(TableCol { title: sum_title, tooltip: trf("Sort by Sum of {}", &[&mname]), sort_col: GroupSortCol::Sum(i), right: true, share: 1.0, min_w: sum_min });
            cols.push(TableCol { title: avg_title, tooltip: trf("Sort by Avg of {}", &[&mname]), sort_col: GroupSortCol::Avg(i), right: true, share: 1.0, min_w: avg_min });
        }

        // Compute widths: honor every column's minimum width (no overlapping/clipping),
        // expanding proportionally when extra panel width is available, and scrolling
        // horizontally when the columns exceed the panel width.
        let avail_w = ui.available_width().max(120.0);
        let min_total: f32 = cols.iter().map(|c| c.min_w).sum();
        let share_total: f32 = cols.iter().map(|c| c.share).sum();
        let widths: Vec<f32> = if min_total < avail_w && share_total > 0.0 {
            let extra = avail_w - min_total;
            cols.iter().map(|c| c.min_w + extra * (c.share / share_total)).collect()
        } else {
            cols.iter().map(|c| c.min_w).collect()
        };
        let table_w: f32 = widths.iter().sum();

        let mut clicked: Option<usize> = None;
        let mut clicked_sort: Option<GroupSortCol> = None;
        let active_sort = state.sort;

        ScrollArea::horizontal().id_salt("ternarygrid_group_hscroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let (head, _) = ui.allocate_exact_size(egui::vec2(table_w, head_h), egui::Sense::hover());
            ui.painter().rect_filled(head, egui::CornerRadius { nw: 8, ne: 8, ..Default::default() }, pal.header);
            ui.painter().hline(head.x_range(), head.bottom() - 0.5, egui::Stroke::new(1.0, pal.border));

            let mut x = head.left();
            for (idx, col) in cols.iter().enumerate() {
                let w = widths[idx];
                let rect = egui::Rect::from_min_size(egui::pos2(x, head.top()), egui::vec2(w, head_h - 1.0));
                let is_sorted = active_sort.col == col.sort_col;
                let resp = ui.interact(rect, ui.id().with(("group_head", idx)), egui::Sense::click());
                let dir_label = if is_sorted { if active_sort.descending { tr("DESC") } else { tr("ASC") } } else { "" };
                let a11y = if dir_label.is_empty() { col.title.clone() } else { format!("{} ({dir_label})", col.title) };
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &a11y));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    let corner = if idx == 0 {
                        egui::CornerRadius { nw: 8, ..Default::default() }
                    } else if idx + 1 == cols.len() {
                        egui::CornerRadius { ne: 8, ..Default::default() }
                    } else {
                        egui::CornerRadius::ZERO
                    };
                    ui.painter().rect_filled(rect, corner, pal.hover);
                }
                let text_color = if is_sorted {
                    pal.accent_text
                } else if resp.hovered() {
                    pal.text
                } else {
                    pal.text_muted
                };
                let arrow = if is_sorted {
                    Some((if active_sort.descending { icon::ARROW_DOWN } else { icon::ARROW_UP }, pal.accent_text))
                } else if resp.hovered() {
                    Some((icon::ARROWS_DOWN_UP, pal.text_faint))
                } else {
                    None
                };
                let inner = rect.shrink2(egui::vec2(10.0, 0.0));
                let arrow_w = if arrow.is_some() { 15.0 } else { 0.0 };
                let text_max = (inner.width() - arrow_w).max(4.0);
                let mut job = egui::text::LayoutJob::single_section(col.title.clone(), egui::TextFormat::simple(head_font.clone(), text_color));
                job.wrap = egui::text::TextWrapping { max_width: text_max, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                let g = ui.painter().layout_job(job);
                let gw = g.size().x.min(text_max);
                let painter = ui.painter().with_clip_rect(inner.expand2(egui::vec2(1.0, 0.0)).intersect(ui.clip_rect()));
                if col.right {
                    let tx = (inner.right() - arrow_w - gw).max(inner.left());
                    painter.galley(egui::pos2(tx, rect.center().y - g.size().y / 2.0), g, text_color);
                    if let Some((glyph, acolor)) = arrow {
                        painter.text(egui::pos2(inner.right() - 5.5, rect.center().y), egui::Align2::CENTER_CENTER, glyph, ternarygrid_ui::style::icon_font(11.5), acolor);
                    }
                } else {
                    painter.galley(egui::pos2(inner.left(), rect.center().y - g.size().y / 2.0), g, text_color);
                    if let Some((glyph, acolor)) = arrow {
                        let ax = (inner.left() + gw + 8.5).min(inner.right() - 5.5);
                        painter.text(egui::pos2(ax, rect.center().y), egui::Align2::CENTER_CENTER, glyph, ternarygrid_ui::style::icon_font(11.5), acolor);
                    }
                }
                let tip = if is_sorted {
                    let next_dir = if active_sort.descending { tr("ascending") } else { tr("descending") };
                    format!("{} ({})\n{}", col.tooltip, dir_label, trf("Click to sort {}", &[&next_dir]))
                } else {
                    col.tooltip.clone()
                };
                if resp.on_hover_text(tip).clicked() {
                    clicked_sort = Some(col.sort_col);
                }
                x += w;
            }

            let top = head.top();
            let mut bottom = head.bottom();
            let nkeys = spec.columns.len();
            ScrollArea::vertical().id_salt("ternarygrid_group_vscroll").auto_shrink([false, true]).show_rows(ui, row_h, result.groups.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for i in range {
                    let g = &result.groups[i];
                    let (row, _) = ui.allocate_exact_size(egui::vec2(table_w, row_h), egui::Sense::hover());
                    ui.painter().rect_filled(row, 0.0, if i % 2 == 1 { theme.stripe } else { pal.surface });
                    ui.painter().hline(row.x_range(), row.bottom() - 0.5, egui::Stroke::new(1.0, theme.line));
                    let mut x = row.left();
                    for (j, k) in g.keys.iter().take(nkeys).enumerate() {
                        let w = widths[j];
                        let r = egui::Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(w, row_h - 1.0));
                        let resp = ui.interact(r, ui.id().with(("group_key", i, j)), egui::Sense::click());
                        let full = if k.is_null() { "NULL".to_string() } else { k.to_text().into_owned() };
                        let shown = if full.chars().count() > 60 { format!("{}…", full.chars().take(60).collect::<String>()) } else { full.clone() };
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, true, &shown));
                        let elided = if k.is_null() {
                            paint_cell(ui, r, &shown, &link_font, pal.text_faint, false, false)
                        } else {
                            paint_cell(ui, r, &shown, &link_font, pal.accent_text, false, resp.hovered())
                        };
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        let tip = if elided || full.len() != shown.len() {
                            format!("{full}\n{}", tr("Filter the grid to this group"))
                        } else {
                            tr("Filter the grid to this group").to_string()
                        };
                        if resp.on_hover_text(tip).clicked() {
                            clicked = Some(i);
                        }
                        x += w;
                    }
                    // Count
                    let count_str = crate::format::display_count(g.count);
                    paint_cell(ui, egui::Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(widths[nkeys], row_h - 1.0)), &count_str, &mono, pal.text, true, false);
                    x += widths[nkeys];
                    // Measures (Sum, Avg for each measure)
                    for (m_idx, stats) in g.measures.iter().take(spec.measures.len()).enumerate() {
                        let base = nkeys + 1 + m_idx * 2;
                        if base + 1 >= widths.len() {
                            break;
                        }
                        let sum_s = stats.sum.map(crate::format::display_number).unwrap_or_default();
                        let avg_s = stats.avg.map(crate::format::display_number).unwrap_or_default();
                        let w_sum = widths[base];
                        let w_avg = widths[base + 1];
                        let r_sum = egui::Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(w_sum, row_h - 1.0));
                        let r_avg = egui::Rect::from_min_size(egui::pos2(x + w_sum, row.top()), egui::vec2(w_avg, row_h - 1.0));
                        if paint_cell(ui, r_sum, &sum_s, &mono, pal.text, true, false) {
                            ui.interact(r_sum, ui.id().with(("group_sum", i, m_idx)), egui::Sense::hover()).on_hover_text(&sum_s);
                        }
                        if paint_cell(ui, r_avg, &avg_s, &mono, pal.text, true, false) {
                            ui.interact(r_avg, ui.id().with(("group_avg", i, m_idx)), egui::Sense::hover()).on_hover_text(&avg_s);
                        }
                        x += w_sum + w_avg;
                    }
                    bottom = bottom.max(row.bottom().min(ui.clip_rect().bottom()));
                }
            });
            let card = egui::Rect::from_min_max(egui::pos2(head.left(), top), egui::pos2(head.left() + table_w, bottom));
            ui.painter().rect_stroke(card, 8, egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
        });

        if let Some(col) = clicked_sort {
            if state.sort.col == col {
                state.sort.descending = !state.sort.descending;
            } else {
                state.sort = GroupSort { col, descending: !matches!(col, GroupSortCol::Key(_)) };
            }
            sort_groups(&mut result.groups, state.sort);
        }

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
