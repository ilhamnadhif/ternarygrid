//! References panel: foreign key relations from / to the current table and
//! the related rows of the active row (FR-D5). The panel only renders; the
//! application loads related rows on [`GridAction::RequestReferences`] and
//! hands them over with [`ReferencesPanelState::set_related_rows`].

use crate::grid::{DataGrid, DataGridState, GridAction, GridOptions};
use crate::model::{ForeignKeyLink, ResultModel};
use egui::{RichText, Ui};
use ternarygrid_core::{ColumnMeta, ObjectRef, Row, Value, tr, trf};

/// Direction of a relation seen from the current result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationDirection {
    /// This table references `table` (a foreign key of ours).
    References,
    /// `table` references this table (their foreign key).
    ReferencedBy,
}

/// A relation between the current result and another table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    /// Constraint name.
    pub name: Option<String>,
    /// Direction.
    pub direction: RelationDirection,
    /// The other table.
    pub table: ObjectRef,
    /// Columns in the other table.
    pub columns: Vec<String>,
    /// Matching result columns of the current result (same order).
    pub key_columns: Vec<usize>,
}

struct Related {
    key: Vec<Value>,
    model: ResultModel,
    grid: DataGridState,
}

/// State of the references panel.
#[derive(Default)]
pub struct ReferencesPanelState {
    relations: Vec<Relation>,
    selected: Option<usize>,
    requested: Option<(usize, Vec<Value>)>,
    related: Option<(usize, Related)>,
    /// The key last read: ((content generation, row, relation), values), for
    /// frames where the row store is busy.
    key: Option<((u64, usize, usize), Vec<Value>)>,
    options: GridOptions,
}

impl std::fmt::Debug for ReferencesPanelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReferencesPanelState").field("relations", &self.relations).field("selected", &self.selected).finish_non_exhaustive()
    }
}

impl ReferencesPanelState {
    /// Relations of the current table (supplied by the application from metadata).
    pub fn set_relations(&mut self, relations: Vec<Relation>) {
        if self.relations != relations {
            self.relations = relations;
            self.selected = (!self.relations.is_empty()).then_some(0);
            self.related = None;
            self.requested = None;
        }
    }

    /// Relations shown.
    pub fn relations(&self) -> &[Relation] {
        &self.relations
    }

    /// Supply the rows related to `key` through relation `relation`.
    pub fn set_related_rows(&mut self, relation: usize, key: Vec<Value>, columns: Vec<ColumnMeta>, rows: Vec<Row>) {
        let model = ResultModel::from_rows(columns, rows);
        let grid = DataGridState::for_model(&model);
        self.related = Some((relation, Related { key, model, grid }));
    }

    /// Build relations from the result's foreign keys (outgoing only).
    pub fn relations_from_foreign_keys(fks: &[ForeignKeyLink]) -> Vec<Relation> {
        fks.iter()
            .map(|fk| Relation {
                name: fk.name.clone(),
                direction: RelationDirection::References,
                table: fk.ref_table.clone(),
                columns: fk.ref_columns.clone(),
                key_columns: fk.columns.clone(),
            })
            .collect()
    }
}

/// The references panel.
pub struct ReferencesPanel<'a> {
    model: &'a ResultModel,
    grid: &'a DataGridState,
    state: &'a mut ReferencesPanelState,
}

impl<'a> ReferencesPanel<'a> {
    /// New panel.
    pub fn new(model: &'a ResultModel, grid: &'a DataGridState, state: &'a mut ReferencesPanelState) -> Self {
        ReferencesPanel { model, grid, state }
    }

    /// Show the panel. Emits `RequestReferences` when related rows are needed
    /// and `NavigateForeignKey` to open the related table.
    pub fn show(self, ui: &mut Ui) -> Vec<GridAction> {
        let ReferencesPanel { model, grid, state } = self;
        let mut actions = Vec::new();
        if state.relations.is_empty() {
            ui.weak(tr("No foreign key relations are known for this result"));
            return actions;
        }
        let pal = ternarygrid_ui::pal(ui);
        ui.spacing_mut().item_spacing.y = 10.0;
        // The relations as pills on one line (→ references, ← referenced by); the chosen one in the accent.
        egui::ScrollArea::horizontal().id_salt("ternarygrid_references_chips").scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let font = ternarygrid_ui::style::semibold(ui.ctx(), 12.0);
                for (i, r) in state.relations.iter().enumerate() {
                    let arrow = match r.direction {
                        RelationDirection::References => "→",
                        RelationDirection::ReferencedBy => "←",
                    };
                    let label = format!("{arrow} {}", r.table.display_path());
                    let on = state.selected == Some(i);
                    let (fg, bg) = if on { (pal.accent_text, pal.accent_soft) } else { (pal.text_muted, pal.subtle) };
                    let g = ui.painter().layout_no_wrap(label.clone(), font.clone(), fg);
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 14.0, 20.0), egui::Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, &label));
                    ui.painter().rect_filled(rect, 10, if resp.hovered() && !on { pal.border_strong } else { bg });
                    ui.painter().galley(rect.center() - g.size() / 2.0, g, fg);
                    if resp.on_hover_text(r.name.clone().unwrap_or_default()).clicked() {
                        state.selected = Some(i);
                    }
                }
            });
        });
        let (line, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
        ui.painter().hline(line.x_range(), line.center().y, egui::Stroke::new(1.0, pal.border));
        let Some(sel) = state.selected.filter(|s| *s < state.relations.len()) else { return actions };
        let Some((row, _)) = grid.active_cell() else {
            ui.weak(tr("Select a row to see related rows"));
            return actions;
        };
        let rel = state.relations[sel].clone();
        // Read every frame: never waits long for the row store (the key read before
        // meanwhile; nothing until there is one).
        let cache = (model.content_generation(), row, sel);
        let mut key = vec![Value::Null; rel.key_columns.len()];
        let read = model.visit_rows_for(row..row + 1, crate::grid::STORE_WAIT, |_, cells| {
            for (k, &c) in key.iter_mut().zip(&rel.key_columns) {
                *k = cells.value_of(c).unwrap_or_default();
            }
        });
        if read {
            state.key = Some((cache, key.clone()));
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(10));
            match state.key.as_ref().filter(|(k, _)| *k == cache) {
                Some((_, k)) => key = k.clone(),
                None => return actions,
            }
        }
        ui.horizontal(|ui| {
            let desc =
                rel.columns.iter().zip(&key).map(|(c, v)| format!("{c} = {}", if v.is_null() { "NULL".into() } else { v.to_text().into_owned() })).collect::<Vec<_>>().join(", ");
            let title = trf("{} where {}", &[&rel.table.display_path(), &desc]);
            let open = ui
                .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let open = ternarygrid_ui::kit::button_full(ui, ternarygrid_ui::kit::Kind::Ghost, Some(egui_phosphor::regular::ARROW_SQUARE_OUT), tr("Open"), true, false);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(title).font(ternarygrid_ui::style::semibold(ui.ctx(), 13.0)).color(pal.text)).truncate());
                    });
                    open
                })
                .inner;
            if open.clicked() {
                let link = ForeignKeyLink { name: rel.name.clone(), columns: rel.key_columns.clone(), ref_table: rel.table.clone(), ref_columns: rel.columns.clone() };
                actions.push(GridAction::NavigateForeignKey { link, values: key.clone() });
            }
        });
        if key.iter().all(Value::is_null) {
            ui.weak(tr("The key is NULL; there are no related rows"));
            return actions;
        }
        let have = state.related.as_ref().is_some_and(|(r, d)| *r == sel && d.key == key);
        if !have && state.requested.as_ref() != Some(&(sel, key.clone())) {
            state.requested = Some((sel, key.clone()));
            actions.push(GridAction::RequestReferences { relation: sel, values: key.clone() });
        }
        match state.related.as_mut() {
            Some((r, d)) if *r == sel && d.key == key => {
                let mut options = state.options.clone();
                options.show_row_numbers = false;
                // In a bordered box as tall as its rows (two at least), not the whole panel.
                let rows = d.model.row_count().max(2) as f32;
                let height = (33.0 + rows * 29.0 + 19.0).min(ui.available_height());
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
                let resp = ui
                    .scope_builder(egui::UiBuilder::new().max_rect(rect.shrink(1.0)), |ui| {
                        ui.set_clip_rect(rect.shrink(1.0).intersect(ui.clip_rect()));
                        DataGrid::new(&mut d.model, &mut d.grid, &options).id_salt(("ternarygrid_references_grid", sel)).show(ui)
                    })
                    .inner;
                ui.painter().rect_stroke(rect, 8, egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
                for a in resp.actions {
                    if let GridAction::NavigateForeignKey { .. } = a {
                        actions.push(a);
                    }
                }
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak(tr("Loading related rows…"));
                });
            }
        }
        actions
    }
}
