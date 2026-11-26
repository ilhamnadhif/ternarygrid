//! Headless tests of the composite viewer, toolbar, views and panels.

use crate::grid::{GridAction, GridOptions, PanelKind, ResultViewKind};
use crate::model::ResultModel;
use crate::panels::{GroupingPanel, GroupingPanelState, ReferencesPanel, ReferencesPanelState, Relation, RelationDirection};
use crate::result_view::{ResultViewState, ResultViewer};
use crate::test_support::{Ui, editable_model, press, release};
use egui::{Event, Modifiers, PointerButton, Pos2};
use ternarygrid_core::{ColumnMeta, DataKind, ObjectRef, Value};

struct Viewer {
    ui: Ui,
    model: ResultModel,
    state: ResultViewState,
    options: GridOptions,
    actions: Vec<GridAction>,
    output: egui::PlatformOutput,
}

impl Viewer {
    fn new(model: ResultModel) -> Self {
        let state = ResultViewState::new(&model);
        let mut v = Viewer { ui: Ui::new(), model, state, options: GridOptions::default(), actions: Vec::new(), output: Default::default() };
        v.ui.size = egui::vec2(1200.0, 700.0);
        v.ui.ctx.enable_accesskit();
        v.frame(vec![]);
        v
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.frame_with(events, Modifiers::NONE);
    }

    /// A frame with `modifiers` held.
    fn frame_with(&mut self, events: Vec<Event>, modifiers: Modifiers) {
        let mut actions = Vec::new();
        let (model, state, options) = (&mut self.model, &mut self.state, &self.options);
        self.output = self.ui.run(events, modifiers, |ui| {
            actions = ResultViewer::new(model, state, options).show(ui).actions;
        });
        self.actions = actions;
    }

    /// Centre of the first accessibility node whose label contains `text`.
    fn find(&self, text: &str) -> Option<Pos2> {
        let update = self.output.accesskit_update.as_ref()?;
        update.nodes.iter().find_map(|(_, n)| {
            let label = n.label().or(n.value())?;
            if !label.contains(text) {
                return None;
            }
            let b = n.bounds()?;
            Some(Pos2::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32))
        })
    }

    /// Centres of the accessibility nodes labelled exactly `text`, top to bottom.
    fn find_all_exact(&self, text: &str) -> Vec<Pos2> {
        let Some(update) = self.output.accesskit_update.as_ref() else { return Vec::new() };
        let mut found: Vec<Pos2> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.label().is_some_and(|l| l == text))
            .filter_map(|(_, n)| n.bounds().map(|b| Pos2::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)))
            .collect();
        found.sort_by(|a, b| a.y.total_cmp(&b.y));
        found
    }

    /// The filter bar's Apply (the topmost: a panel can have an Apply of its own).
    fn filter_apply(&self) -> Option<Pos2> {
        self.find_all_exact("Apply").first().copied()
    }

    /// Centre of the first text input holding `text`.
    fn find_input(&self, text: &str) -> Option<Pos2> {
        let update = self.output.accesskit_update.as_ref()?;
        update.nodes.iter().find_map(|(_, n)| {
            if !matches!(n.role(), egui::accesskit::Role::TextInput | egui::accesskit::Role::MultilineTextInput) || !n.value().or(n.label()).is_some_and(|l| l.contains(text)) {
                return None;
            }
            let b = n.bounds()?;
            Some(Pos2::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32))
        })
    }

    fn click(&mut self, p: Pos2) {
        self.frame(vec![Event::PointerMoved(p)]);
        self.frame(vec![press(p, PointerButton::Primary, Modifiers::NONE)]);
        self.frame(vec![release(p, PointerButton::Primary, Modifiers::NONE)]);
    }

    fn wait_jobs(&mut self) {
        for _ in 0..500 {
            if !self.model.is_view_job_running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
            self.frame(vec![]);
        }
        self.frame(vec![]);
    }
}

#[test]
fn state_persists_and_types_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<ResultModel>();
    assert_send::<crate::grid::DataGridState>();
    assert_send::<crate::view::ClientFilter>();
    let mut v = Viewer::new(editable_model(5));
    v.state.grid.set_column_width(1, 333.0);
    v.state.grid.set_column_visible(2, false);
    v.state.toolbar.filter_text = "id > 1".into();
    v.state.toolbar.auto_refresh_secs = Some(30);
    v.state.open_panel(PanelKind::Calc);
    let json = serde_json::to_string(&v.state).unwrap();
    let back: ResultViewState = serde_json::from_str(&json).unwrap();
    assert_eq!(back.grid.column_width(1), 333.0);
    assert!(!back.grid.columns[2].visible);
    assert_eq!(back.toolbar.filter_text, "id > 1");
    assert_eq!(back.toolbar.auto_refresh_secs, Some(30));
    assert!(back.toolbar.is_panel_open(PanelKind::Calc));
}

#[test]
fn idle_frames_request_no_repaint() {
    let mut v = Viewer::new(editable_model(500));
    v.state.grid.select_cell(2, 1);
    v.state.open_panel(PanelKind::Calc);
    v.state.open_panel(PanelKind::Metadata);
    v.state.open_panel(PanelKind::Value);
    for kind in [ResultViewKind::Grid, ResultViewKind::Text, ResultViewKind::Record, ResultViewKind::Json] {
        v.state.toolbar.view = kind;
        // Let one-off animations (scroll bars fading in) finish.
        for _ in 0..40 {
            v.frame(vec![]);
        }
        assert_eq!(v.ui.repaint_delay, std::time::Duration::MAX, "{kind:?} keeps repainting while idle");
    }
    // A transient status message schedules exactly one timed repaint, not a continuous one.
    v.state.toolbar.view = ResultViewKind::Grid;
    v.state.grid.show_message("hello", false);
    v.frame(vec![]);
    assert!(v.ui.repaint_delay > std::time::Duration::from_millis(500));
}

#[test]
fn every_view_and_panel_renders() {
    let mut v = Viewer::new(editable_model(200));
    v.state.grid.select_cell(3, 1);
    for p in [PanelKind::Value, PanelKind::Metadata, PanelKind::Grouping, PanelKind::Calc, PanelKind::References] {
        v.state.open_panel(p);
        v.frame(vec![]);
        v.frame(vec![]);
    }
    for kind in [ResultViewKind::Text, ResultViewKind::Record, ResultViewKind::Json, ResultViewKind::Xml, ResultViewKind::Grid] {
        v.state.toolbar.view = kind;
        v.frame(vec![]);
        v.frame(vec![]);
    }
    // The text view caches its rendering.
    v.state.toolbar.view = ResultViewKind::Text;
    v.frame(vec![]);
    assert!(v.find("name 3").is_some(), "text view shows rows");
}

#[test]
fn client_filter_from_filter_bar() {
    let mut v = Viewer::new(editable_model(100));
    v.state.toolbar.filter_text = "id >= 90 and flag".into();
    v.state.toolbar.order_text = "id desc".into();
    // Press Enter in the filter field: simulate by clicking the filter bar's Apply.
    let apply = v.filter_apply().expect("apply button");
    v.click(apply);
    v.wait_jobs();
    assert_eq!(v.model.row_count(), 5);
    assert_eq!(v.model.cell(0, 0), Some(Value::Int(98)));
    assert_eq!(v.state.toolbar.filter_history.first().map(String::as_str), Some("id >= 90 and flag"));
    // A broken filter reports an error with its position and changes nothing.
    v.state.toolbar.filter_text = "id >> 3".into();
    v.click(apply);
    assert!(v.state.toolbar.filter_error().is_some_and(|e| e.contains("position")));
    assert_eq!(v.model.row_count(), 5);
}

#[test]
fn filter_text_undoes_a_word_at_a_time() {
    let mut v = Viewer::new(editable_model(10));
    v.state.toolbar.focus_filter();
    v.frame(vec![]);
    // Typed without a pause.
    for t in ["status", " ", "=", " ", "'open'"] {
        v.frame(vec![Event::Text(t.into())]);
    }
    assert_eq!(v.state.toolbar.filter_text, "status = 'open'");
    let undo = Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND };
    for expected in ["status =", "status", ""] {
        v.frame_with(vec![undo.clone()], Modifiers::COMMAND);
        assert_eq!(v.state.toolbar.filter_text, expected);
    }
    let redo = Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND | Modifiers::SHIFT };
    v.frame_with(vec![redo], Modifiers::COMMAND | Modifiers::SHIFT);
    assert_eq!(v.state.toolbar.filter_text, "status");
}

#[test]
fn server_mode_filter_bar_emits_query_change() {
    let mut v = Viewer::new(editable_model(10));
    v.state.grid.mode = crate::grid::DataMode::Server;
    v.state.toolbar.filter_text = "status = 'open'".into();
    let apply = v.filter_apply().expect("apply button");
    v.frame(vec![Event::PointerMoved(apply)]);
    v.frame(vec![press(apply, PointerButton::Primary, Modifiers::NONE)]);
    v.frame(vec![release(apply, PointerButton::Primary, Modifiers::NONE)]);
    let changed = v.actions.iter().any(|a| matches!(a, GridAction::ServerQueryChanged { where_clause: Some(w), .. } if w == "status = 'open'"));
    assert!(changed, "{:?}", v.actions);
}

#[test]
fn value_panel_edits_active_cell() {
    let mut v = Viewer::new(editable_model(10));
    v.state.grid.select_cell(2, 1);
    v.state.open_panel(PanelKind::Value);
    v.frame(vec![]);
    v.frame(vec![]);
    // Focus the editor and type.
    let editor = v.find_input("name 2").expect("value editor");
    v.click(editor);
    v.frame(vec![crate::test_support::key(egui::Key::End, Modifiers::NONE)]);
    v.frame(vec![Event::Text("!".into())]);
    // The value panel's Apply: below the filter bar's.
    let apply = v.find_all_exact("Apply").last().copied().expect("apply button");
    v.click(apply);
    assert_eq!(v.model.cell(2, 1), Some(Value::text("name 2!")));
}

#[test]
fn auto_refresh_emits_refresh() {
    let mut v = Viewer::new(editable_model(5));
    v.state.toolbar.auto_refresh_secs = Some(5);
    v.frame(vec![]);
    assert!(!v.actions.contains(&GridAction::Refresh));
    v.ui.time += 6.0;
    v.frame(vec![]);
    assert!(v.actions.contains(&GridAction::Refresh));
    // Paused while there are unsaved changes.
    v.model.set_cell(0, 1, "x").unwrap();
    v.ui.time += 6.0;
    v.frame(vec![]);
    assert!(!v.actions.contains(&GridAction::Refresh));
}

#[test]
fn open_value_panel_action_opens_panel() {
    let mut v = Viewer::new(editable_model(5));
    v.model.set_cell(1, 1, "line one\nline two").unwrap();
    v.state.grid.select_cell(1, 1);
    v.frame(vec![]);
    // Enter on a multi-line value opens the value panel instead of the inline editor.
    let cell = v.find("Data grid").expect("grid");
    v.click(cell);
    v.state.grid.select_cell(1, 1);
    v.frame(vec![]);
    v.frame(vec![crate::test_support::key(egui::Key::Enter, Modifiers::NONE)]);
    v.frame(vec![]);
    assert!(v.state.toolbar.is_panel_open(PanelKind::Value));
    assert_eq!(v.state.active_panel, Some(PanelKind::Value));
}

#[test]
fn a_refused_edit_pops_up_once() {
    let mut m = editable_model(5);
    m.set_editability(Err(crate::model::ReadOnlyReason::NoKey));
    let mut v = Viewer::new(m);
    let cell = v.find("Data grid").expect("grid");
    v.click(cell);
    v.state.grid.select_cell(1, 1);
    v.frame(vec![]);
    v.frame(vec![crate::test_support::key(egui::Key::Enter, Modifiers::NONE)]);
    // The pop-up opens, then settles in the middle (sized on its first frames).
    for _ in 0..4 {
        v.frame(vec![]);
    }
    assert!(!v.state.grid.is_editing());
    assert!(v.state.grid.rt.message.is_none(), "no overlay on the grid");
    // Once, in full, in a pop-up with its remedy.
    let count = |v: &Viewer, text: &str| {
        v.output
            .accesskit_update
            .as_ref()
            .map_or(0, |u| u.nodes.iter().filter(|(_, n)| n.role() != egui::accesskit::Role::TextRun && n.label().or(n.value()).is_some_and(|l| l.contains(text))).count())
    };
    assert_eq!(count(&v, "Define a virtual key (columns whose values identify a row)"), 1);
    assert_eq!(v.state.toolbar.error_popup.as_deref().map(|m| m.contains("no primary or unique key")), Some(true));
    assert!(v.state.toolbar.notice.is_none(), "not in the status bar");
    let button = v.find("Define virtual key").expect("Define virtual key… in the pop-up");
    v.click(button);
    assert!(v.actions.iter().any(|a| matches!(a, GridAction::DefineVirtualKey { .. })));
    v.frame(vec![]);
    assert_eq!(count(&v, "Define a virtual key (columns whose values identify a row)"), 0, "taken care of");
}

#[test]
fn grouping_panel_groups_in_background() {
    let mut ui = Ui::new();
    let mut model = editable_model(300);
    let mut state = GroupingPanelState::default();
    state.spec.columns = vec![3];
    state.spec.measures = vec![0];
    let mut action = None;
    for _ in 0..400 {
        ui.run(vec![], Modifiers::NONE, |ui| {
            action = GroupingPanel::new(&mut model, &mut state).show(ui);
        });
        if !state.is_running() && state.result().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(action.is_none());
    let result = state.result().expect("grouping finished");
    assert_eq!(result.groups.len(), 3);
    assert!(result.groups.iter().all(|g| g.count == 100));
    assert!(result.groups.iter().all(|g| g.measures.first().and_then(|m| m.sum).is_some()));
}

#[test]
fn references_panel_requests_and_shows_related_rows() {
    let mut ui = Ui::new();
    let model = editable_model(5);
    let mut grid = crate::grid::DataGridState::for_model(&model);
    grid.select_cell(1, 0);
    let mut state = ReferencesPanelState::default();
    state.set_relations(vec![Relation {
        name: Some("fk".into()),
        direction: RelationDirection::References,
        table: ObjectRef::table(Some("app".into()), "categories"),
        columns: vec!["id".into()],
        key_columns: vec![3],
    }]);
    let mut actions = Vec::new();
    ui.run(vec![], Modifiers::NONE, |ui| actions = ReferencesPanel::new(&model, &grid, &mut state).show(ui));
    assert!(matches!(&actions[..], [GridAction::RequestReferences { relation: 0, values }] if values == &vec![Value::Int(2)]));
    // Not requested again for the same key.
    ui.run(vec![], Modifiers::NONE, |ui| actions = ReferencesPanel::new(&model, &grid, &mut state).show(ui));
    assert!(actions.is_empty());
    state.set_related_rows(
        0,
        vec![Value::Int(2)],
        vec![ColumnMeta::new("id", "int", DataKind::Int), ColumnMeta::new("name", "text", DataKind::Text)],
        vec![vec![Value::Int(2), Value::text("Music")]],
    );
    ui.run(vec![], Modifiers::NONE, |ui| actions = ReferencesPanel::new(&model, &grid, &mut state).show(ui));
    assert!(actions.is_empty());
}

#[test]
fn calc_values_copy() {
    let mut v = Viewer::new(editable_model(10));
    v.state.grid.select_cell(2, 0);
    v.state.open_panel(PanelKind::Calc);
    v.frame(vec![]);
    v.frame(vec![]);
    let copied = |v: &Viewer| {
        v.output.commands.iter().find_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        })
    };
    // A click selects the row, ⌘C copies its value.
    let count = v.find("Count: 1").expect("count row");
    v.click(count);
    v.frame(vec![Event::Copy]);
    assert_eq!(copied(&v).as_deref(), Some("1"));
    // The copy button at the end of a hovered row.
    let bounds = v.output.accesskit_update.as_ref().and_then(|u| u.nodes.iter().find_map(|(_, n)| n.label().or(n.value()).filter(|l| l.starts_with("Distinct")).and(n.bounds())));
    let button = bounds.map(|b| egui::pos2(b.x1 as f32 - 14.0, ((b.y0 + b.y1) / 2.0) as f32)).expect("distinct row");
    v.click(button);
    assert_eq!(copied(&v).as_deref(), Some("1"));
}

#[test]
fn record_view_picks_several_values_and_copies_them() {
    let mut v = Viewer::new(editable_model(5));
    v.state.grid.select_cell(1, 0);
    v.state.grid.selection_mut().extend_to(crate::grid::CellPos::new(2, 0));
    v.state.toolbar.view = ResultViewKind::Record;
    v.frame(vec![]);
    v.frame(vec![]);
    let lead = |v: &Viewer, name: &str| -> Pos2 {
        let update = v.output.accesskit_update.as_ref().expect("accesskit");
        update
            .nodes
            .iter()
            .find_map(|(_, n)| {
                let b = n.bounds()?;
                (n.label().or(n.value()) == Some(name)).then(|| Pos2::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32))
            })
            .unwrap_or_else(|| panic!("no column name {name}"))
    };
    // A column name picks it in both rows; ⌘-click on another adds it.
    let name = lead(&v, "name");
    v.click(name);
    let flag = lead(&v, "flag");
    v.frame(vec![Event::PointerMoved(flag)]);
    let cmd = Modifiers::COMMAND;
    v.frame_with(vec![press(flag, PointerButton::Primary, cmd)], cmd);
    v.frame_with(vec![release(flag, PointerButton::Primary, cmd)], cmd);
    v.frame(vec![Event::Copy]);
    let copied = v.output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    assert_eq!(copied.as_deref(), Some("name 1\tname 2\nfalse\ttrue"));
    // The rows shown side by side stay selected in the grid.
    assert_eq!(v.state.grid.selected_rows(5, 10), vec![1, 2]);
}

#[test]
fn record_view_takes_now_in_a_timestamp_and_says_why_a_value_is_refused() {
    let src = |c: &str| ternarygrid_core::ColumnSource { database: None, schema: Some("app".into()), table: "services".into(), column: c.into() };
    let cols = vec![
        ColumnMeta::new("id", "int", DataKind::Int).with_source(src("id")),
        ColumnMeta::new("created_at", "timestamptz", DataKind::DateTimeTz).with_source(src("created_at")),
        ColumnMeta::new("country", "text", DataKind::Text).with_source(src("country")),
    ];
    let stamp = Value::parse_as("2025-11-24 13:48:03.461+07", DataKind::DateTimeTz).unwrap();
    let mut m = ResultModel::from_rows(cols, vec![vec![Value::Int(160), stamp, Value::text("de")]]);
    let e = crate::model::determine_editability(m.columns(), |_| Some(vec!["id".into()]), false);
    m.set_editability(e);
    let mut v = Viewer::new(m);
    v.state.grid.select_cell(0, 1);
    v.state.toolbar.view = ResultViewKind::Record;
    v.frame(vec![]);
    v.frame(vec![]);
    let value_of = |v: &Viewer, name: &str| -> Pos2 {
        let update = v.output.accesskit_update.as_ref().expect("accesskit");
        update
            .nodes
            .iter()
            .find_map(|(_, n)| {
                let b = n.bounds()?;
                (n.label().or(n.value()) == Some(name)).then(|| Pos2::new(b.x1 as f32 + 60.0, ((b.y0 + b.y1) / 2.0) as f32))
            })
            .unwrap_or_else(|| panic!("no column name {name}"))
    };
    let edit = |v: &mut Viewer, text: &str| {
        let at = value_of(v, "created_at");
        v.click(at);
        v.click(at);
        // The editor takes the focus.
        v.frame(vec![]);
        let all = Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND };
        v.frame_with(vec![all], Modifiers::COMMAND);
        v.frame(vec![Event::Text(text.into())]);
        // Another field: the edit ends.
        let country = value_of(v, "country");
        v.click(country);
    };
    edit(&mut v, "now()");
    assert!(v.model.is_cell_modified(0, 1), "now() is a change");
    assert_eq!(v.model.cell(0, 1), Some(Value::text("now()")));
    // A value that is no timestamp: why, at once.
    for _ in 0..30 {
        v.frame(vec![]);
    }
    edit(&mut v, "tomorrow");
    v.frame(vec![]);
    let reason = v.find("is not a timestamp").expect("the reason is shown");
    let fields = v.find("country").unwrap();
    assert!(reason.y < fields.y, "above the fields, in sight");
}

#[test]
fn record_view_copies_values_as_they_are_and_edits_by_typing() {
    let src = |c: &str| ternarygrid_core::ColumnSource { database: None, schema: Some("app".into()), table: "api_calls".into(), column: c.into() };
    let json = r#"{"ResultCode":"200","ResultDesc":"SUCCESS"}"#;
    let cols = vec![
        ColumnMeta::new("id", "int", DataKind::Int).with_source(src("id")),
        ColumnMeta::new("url", "text", DataKind::Text).with_source(src("url")),
        ColumnMeta::new("response_body", "text", DataKind::Text).with_source(src("response_body")),
        ColumnMeta::new("note", "text", DataKind::Text).with_source(src("note")),
    ];
    let mut m = ResultModel::from_rows(cols, vec![vec![Value::Int(12), Value::text("https://api.example/x"), Value::text(json), Value::text("a")]]);
    let e = crate::model::determine_editability(m.columns(), |_| Some(vec!["id".into()]), false);
    m.set_editability(e);
    let mut v = Viewer::new(m);
    v.state.grid.select_cell(0, 1);
    v.state.toolbar.view = ResultViewKind::Record;
    v.frame(vec![]);
    v.frame(vec![]);
    let value_of = |v: &Viewer, name: &str| -> Pos2 {
        let update = v.output.accesskit_update.as_ref().expect("accesskit");
        update
            .nodes
            .iter()
            .find_map(|(_, n)| {
                let b = n.bounds()?;
                (n.label().or(n.value()) == Some(name)).then(|| Pos2::new(b.x1 as f32 + 60.0, ((b.y0 + b.y1) / 2.0) as f32))
            })
            .unwrap_or_else(|| panic!("no column name {name}"))
    };
    // Two fields picked (⌘-click) and copied: the values as they are.
    v.click(value_of(&v, "url"));
    let body = value_of(&v, "response_body");
    let cmd = Modifiers::COMMAND;
    v.frame_with(vec![Event::PointerMoved(body)], cmd);
    v.frame_with(vec![press(body, PointerButton::Primary, cmd)], cmd);
    v.frame_with(vec![release(body, PointerButton::Primary, cmd)], cmd);
    v.frame(vec![Event::Copy]);
    let copied = v.output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    assert_eq!(copied.as_deref(), Some(format!("https://api.example/x\n{json}").as_str()));
    // Typing edits the field (no double click), Enter keeps it, the keyboard goes on.
    v.click(value_of(&v, "note"));
    v.frame(vec![Event::Text("b".into())]);
    v.frame(vec![]);
    v.frame(vec![Event::Text("c".into())]);
    let key = |k: egui::Key| Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    v.frame(vec![key(egui::Key::Enter)]);
    v.frame(vec![]);
    assert_eq!(v.model.cell(0, 3), Some(Value::text("bc")), "typed over the value");
    assert!(v.model.is_cell_modified(0, 3));
    // ↑: the field above; typing there, Escape: nothing changes.
    v.frame(vec![key(egui::Key::ArrowUp)]);
    v.frame(vec![Event::Text("x".into())]);
    v.frame(vec![]);
    v.frame(vec![key(egui::Key::Escape)]);
    v.frame(vec![]);
    v.frame(vec![]);
    assert_eq!(v.model.cell(0, 2), Some(Value::text(json)));
    assert!(!v.model.is_cell_modified(0, 2));
    // Enter on a field edits its value as it is; typing one character and Enter saves it.
    v.frame(vec![key(egui::Key::ArrowDown)]);
    v.frame(vec![Event::Text("z".into())]);
    v.frame(vec![]);
    v.frame(vec![key(egui::Key::Enter)]);
    v.frame(vec![]);
    assert_eq!(v.model.cell(0, 3), Some(Value::text("z")), "one typed character is a change");
    // Enter, as in the grid: the value's text selected, typing replaces it.
    v.frame(vec![key(egui::Key::ArrowUp)]);
    v.frame(vec![]);
    v.frame(vec![key(egui::Key::Enter)]);
    v.frame(vec![]);
    v.frame(vec![Event::Text("y".into())]);
    v.frame(vec![]);
    v.frame(vec![key(egui::Key::Enter)]);
    v.frame(vec![]);
    assert_eq!(v.model.cell(0, 2), Some(Value::text("y")), "Enter selects the text: typing replaces it");
}

#[test]
fn record_view_picks_enum_values_from_the_list_as_the_grid() {
    let src = |c: &str| ternarygrid_core::ColumnSource { database: None, schema: Some("app".into()), table: "users".into(), column: c.into() };
    let cols = vec![ColumnMeta::new("id", "int", DataKind::Int).with_source(src("id")), ColumnMeta::new("role", "text", DataKind::Text).with_source(src("role"))];
    let mut m = ResultModel::from_rows(cols, vec![vec![Value::Int(1), Value::text("admin")]]);
    let e = crate::model::determine_editability(m.columns(), |_| Some(vec!["id".into()]), false);
    m.set_editability(e);
    m.set_enum_values(1, vec!["admin".into(), "user".into(), "guest".into()]);
    let mut v = Viewer::new(m);
    v.state.grid.select_cell(0, 1);
    v.state.toolbar.view = ResultViewKind::Record;
    v.frame(vec![]);
    v.frame(vec![]);
    // The value: right of the field's name.
    let update = v.output.accesskit_update.as_ref().expect("accesskit");
    let role = update
        .nodes
        .iter()
        .find_map(|(_, n)| {
            let b = n.bounds()?;
            (n.label().or(n.value()) == Some("role")).then(|| Pos2::new(b.x1 as f32 + 60.0, ((b.y0 + b.y1) / 2.0) as f32))
        })
        .expect("the role field");
    v.click(role);
    let key = |k: egui::Key| Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    // Enter: the list of values (no text editor), its search takes the keyboard.
    v.frame(vec![key(egui::Key::Enter)]);
    v.frame(vec![]);
    v.frame(vec![]);
    assert!(v.find("guest").is_some(), "the list of values is open");
    v.frame(vec![Event::Text("gue".into())]);
    v.frame(vec![]);
    v.frame(vec![key(egui::Key::Enter)]);
    v.frame(vec![]);
    v.frame(vec![]);
    assert_eq!(v.model.cell(0, 1), Some(Value::text("guest")));
    assert!(v.find("user").is_none(), "the list closed");
    // The keyboard is the record's again: Enter opens the list, Escape closes it unchanged.
    v.frame(vec![key(egui::Key::Enter)]);
    v.frame(vec![]);
    v.frame(vec![]);
    assert!(v.find("user").is_some());
    v.frame(vec![key(egui::Key::Escape)]);
    v.frame(vec![]);
    v.frame(vec![]);
    assert!(v.find("user").is_none());
    assert_eq!(v.model.cell(0, 1), Some(Value::text("guest")));
}

#[test]
fn record_view_has_the_grids_menu_and_keys() {
    use crate::test_support::key;
    use egui::Key;
    let mut v = Viewer::new(editable_model(3));
    v.state.grid.select_cell(1, 1);
    v.state.toolbar.view = ResultViewKind::Record;
    v.frame(vec![]);
    v.frame(vec![]);
    // The value of a field: right of its name.
    let value_of = |v: &Viewer, name: &str| -> Pos2 {
        let update = v.output.accesskit_update.as_ref().expect("accesskit");
        update
            .nodes
            .iter()
            .find_map(|(_, n)| {
                let b = n.bounds()?;
                (n.label().or(n.value()) == Some(name)).then(|| Pos2::new(b.x1 as f32 + 60.0, ((b.y0 + b.y1) / 2.0) as f32))
            })
            .unwrap_or_else(|| panic!("no column name {name}"))
    };
    let value = value_of(&v, "name");
    v.frame(vec![Event::PointerMoved(value)]);
    v.frame(vec![press(value, PointerButton::Secondary, Modifiers::NONE)]);
    v.frame(vec![release(value, PointerButton::Secondary, Modifiers::NONE)]);
    v.frame(vec![]);
    for item in ["Copy with column names", "Set NULL", "Set DEFAULT", "Add row", "Duplicate row", "Delete row", "Filter by value", "Generate SQL"] {
        assert!(v.find(item).is_some(), "{item} in the menu");
    }
    assert!(v.find("Zoom").is_none(), "the grid's own");
    let set_null = v.find("Set NULL").unwrap();
    v.click(set_null);
    assert_eq!(v.model.cell(1, 1), Some(Value::Null));
    assert_eq!(v.state.grid.active_cell(), Some((1, 1)), "the record shown stays");
    // Its keys: Enter edits here (a list of values too), ⇧Delete sets NULL
    // on the picked value, Delete marks the row.
    let category = value_of(&v, "category");
    v.click(category);
    assert!(v.find_input("2").is_none());
    v.frame(vec![key(Key::Enter, Modifiers::NONE)]);
    v.frame(vec![]);
    assert!(v.find_input("2").is_some(), "an editor opened");
    v.frame(vec![key(Key::Escape, Modifiers::NONE)]);
    v.frame(vec![]);
    v.click(category);
    v.frame_with(vec![key(Key::Delete, Modifiers::SHIFT)], Modifiers::SHIFT);
    assert_eq!(v.model.cell(1, 3), Some(Value::Null));
    v.frame(vec![key(Key::Delete, Modifiers::NONE)]);
    assert_eq!(v.model.row_state(1), crate::model::RowState::Deleted);
}

#[test]
fn discard_drops_pending_changes_at_once() {
    let mut v = Viewer::new(editable_model(3));
    v.frame(vec![]);
    // Fixed-anchor Cancel/Save stay in place when clean (disabled).
    let clean_save = v.find_all_exact("Save")[0];
    v.model.set_value(1, 1, ternarygrid_core::Value::text("changed")).unwrap();
    v.frame(vec![]);
    assert!(v.model.has_changes());
    let dirty_save = v.find_all_exact("Save")[0];
    assert!((clean_save.x - dirty_save.x).abs() < 1.0, "fixed-anchor Save does not jump when becoming dirty: {clean_save:?} vs {dirty_save:?}");
    // No +1 / ~1 / -1 delta counter chips in the Cancel/Save cluster.
    assert!(v.find("~1").is_none() && v.find("+1").is_none() && v.find("-1").is_none(), "no delta counter chips in Cancel/Save");
    let discard = v.find("Discard").expect("Cancel/Discard button");
    v.click(discard);
    assert!(!v.model.has_changes(), "no Discard / Keep question first");
    assert!(v.actions.iter().any(|a| matches!(a, GridAction::ChangesDiscarded)));
    v.frame(vec![]);
    assert!(v.find("Keep").is_none());
}

#[test]
fn a_long_fetch_shows_a_spinner_then_cancel() {
    let mut v = Viewer::new(crate::test_support::editable_model(0));
    v.model.fetch.fetching = true;
    v.frame(vec![]);
    assert!(v.find("Fetching…").is_some(), "the spinner's text");
    // The Cancel over the grid's body (not a button of the footer).
    let cancel_in_body = |v: &Viewer| {
        let (body, ..) = v.state.grid.rt.last_geometry.unwrap();
        let update = v.output.accesskit_update.as_ref()?;
        update.nodes.iter().find_map(|(_, n)| {
            let b = n.bounds().filter(|_| n.label().is_some_and(|l| l.contains("Cancel")))?;
            let p = egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32);
            body.contains(p).then_some(p)
        })
    };
    assert!(cancel_in_body(&v).is_none(), "not at once");
    std::thread::sleep(crate::grid::CANCEL_AFTER);
    v.frame(vec![]);
    let cancel = cancel_in_body(&v).expect("Cancel once it takes long");
    v.click(cancel);
    assert!(v.actions.contains(&GridAction::CancelFetch));
}

#[test]
fn every_panel_has_a_tab() {
    let mut v = Viewer::new(crate::test_support::editable_model(3));
    v.state.open_panel(PanelKind::Value);
    v.frame(vec![]);
    v.frame(vec![]);
    assert!(!v.state.toolbar.is_panel_open(PanelKind::Calc));
    // The other panels are icons in the strip, named for screen readers and in the tooltip.
    let calc = v.find("Calc").expect("the Calc tab, before it was ever opened");
    v.click(calc);
    v.frame(vec![]);
    assert!(v.state.toolbar.is_panel_open(PanelKind::Calc), "a click opens it");
    assert_eq!(v.state.active_panel, Some(PanelKind::Calc), "and shows it");
}

#[test]
fn filter_by_value_runs_the_filter_the_box_shows() {
    // Server mode: the action carries the whole WHERE, the one before included,
    // and the filter box shows exactly that (the application runs it as it is).
    let mut v = Viewer::new(editable_model(10));
    v.state.grid.mode = crate::grid::DataMode::Server;
    v.state.toolbar.filter_text = "id > 1".into();
    let by_value = |v: &mut Viewer, value: &str| {
        v.state.grid.select_cell(2, 1);
        v.state.grid.rt.header_popup = Some((1, egui::Rect::from_min_size(egui::pos2(300.0, 40.0), egui::vec2(16.0, 20.0)), true));
        v.frame(vec![]);
        v.frame(vec![]);
        let item = v.find(&format!("name = '{value}'")).expect("the column menu's filter by the cell value");
        v.click(item);
    };
    by_value(&mut v, "name 2");
    let sent: Vec<Option<String>> = v
        .actions
        .iter()
        .filter_map(|a| match a {
            GridAction::ServerQueryChanged { where_clause, .. } => Some(where_clause.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(sent.len(), 1, "{:?}", v.actions);
    let sent = sent[0].clone().unwrap();
    assert!(sent.starts_with("(id > 1) AND (") && sent.contains("name") && sent.ends_with("'name 2')"), "{sent}");
    assert_eq!(v.state.toolbar.filter_text, sent, "the box shows what runs");
    // Applied again from the box: the same condition, not doubled.
    v.frame(vec![]);
    assert_eq!(v.state.toolbar.filter_text, sent);
    // Client mode combines the same way: (filter before) AND (new condition).
    let mut v = Viewer::new(editable_model(10));
    v.state.toolbar.filter_text = "id > 1".into();
    let apply = v.filter_apply().expect("apply button");
    v.click(apply);
    v.wait_jobs();
    assert_eq!(v.model.row_count(), 8);
    by_value(&mut v, "name 4");
    v.wait_jobs();
    assert_eq!(v.state.toolbar.filter_text, "(id > 1) AND (name = 'name 4')");
    assert_eq!(v.model.row_count(), 1);
}

#[test]
fn nothing_to_copy_is_a_note_not_an_error_pop_up() {
    let mut v = Viewer::new(editable_model(5));
    v.state.grid.request_focus();
    v.frame(vec![]);
    v.state.grid.selection_mut().clear();
    v.frame(vec![Event::Copy]);
    v.frame(vec![]);
    assert_eq!(v.state.toolbar.notice, Some(("Nothing is selected".to_string(), false)));
    assert!(v.state.toolbar.error_popup.is_none(), "no modal for it");
    // Typing while editing is disabled (a save runs) says so, without a pop-up per key.
    v.options.allow_editing = false;
    v.state.grid.select_cell(1, 1);
    v.frame(vec![]);
    v.frame(vec![Event::Text("x".into())]);
    v.frame(vec![]);
    assert!(!v.state.grid.is_editing());
    assert_eq!(v.state.toolbar.notice, Some(("Editing is disabled".to_string(), false)));
    assert!(v.state.toolbar.error_popup.is_none());
    // A real error still pops up.
    v.options.allow_editing = true;
    v.model.set_editability(Err(crate::model::ReadOnlyReason::ConnectionReadOnly));
    v.frame(vec![Event::Text("x".into())]);
    v.frame(vec![]);
    assert!(v.state.toolbar.error_popup.is_some());
}

/// The footer's buttons on the bar now (their names, also those shown as icons).
fn on_footer(v: &Viewer) -> Vec<&'static str> {
    ["Refresh", "Add row", "Duplicate", "Delete", "Undo", "Export"].into_iter().filter(|name| !v.find_all_exact(name).is_empty()).collect()
}

/// A viewer `width` points wide, settled.
fn sized(v: &mut Viewer, width: f32) {
    v.ui.size = egui::vec2(width, 700.0);
    v.frame(vec![]);
    v.frame(vec![]);
}

#[test]
fn the_footer_moves_what_does_not_fit_into_its_menu() {
    let mut v = Viewer::new(editable_model(5));
    v.frame(vec![]);
    let all = vec!["Refresh", "Add row", "Duplicate", "Delete", "Undo", "Export"];
    assert_eq!(on_footer(&v), all, "wide");
    // Wide (Full): every primary group and split-button menu trigger is directly on the footer bar.
    for item in ["Auto refresh", "Fetch all rows", "Calculate total row count", "More row actions", "Discard", "Save", "Grid", "Record", "Text", "More views", "Panels"] {
        assert!(!v.find_all_exact(item).is_empty(), "{item} on the wide footer");
    }
    assert!(v.find_all_exact("More result actions").is_empty(), "nothing overflowed at full width");

    // Split-button menus expose secondary row actions, views/visuals, and panels cleanly.
    let row_drop = v.find_all_exact("More row actions")[0];
    v.click(row_drop);
    v.frame(vec![]);
    for item in ["Add row after", "Add row before", "Copy from row above", "Copy from row below"] {
        assert!(v.find(item).is_some(), "{item} in + Add ▾ menu");
    }
    v.frame(vec![crate::test_support::key(egui::Key::Escape, Modifiers::NONE)]);
    v.frame(vec![]);

    let views_drop = v.find_all_exact("More views")[0];
    v.click(views_drop);
    v.frame(vec![]);
    for item in ["JSON", "XML", "Chart", "Spatial view"] {
        assert!(v.find(item).is_some(), "{item} in More views menu");
    }
    v.frame(vec![crate::test_support::key(egui::Key::Escape, Modifiers::NONE)]);
    v.frame(vec![]);

    let panels_drop = v.find_all_exact("Panels")[0];
    v.click(panels_drop);
    v.frame(vec![]);
    for item in ["Value", "Metadata", "Grouping", "Calc", "References", "Show panels"] {
        assert!(v.find(item).is_some(), "{item} in Panels menu");
    }
    v.frame(vec![crate::test_support::key(egui::Key::Escape, Modifiers::NONE)]);
    v.frame(vec![]);

    // Medium (Core): core row buttons, fixed Cancel/Save, Export icon and views stay; Fetch all/Count move into ⋯.
    sized(&mut v, 840.0);
    assert_eq!(on_footer(&v), all, "medium (Core)");
    assert!(!v.find_all_exact("More result actions").is_empty(), "overflow menu appears at Core");

    // Narrow (Essential): Dup/Del/Undo & Export move into ⋯ while Refresh, Add row, compact views and Panels stay.
    sized(&mut v, 620.0);
    assert_eq!(on_footer(&v), vec!["Refresh", "Add row"], "narrow (Essential)");
    assert!(!v.find_all_exact("Grid").is_empty(), "compact views stay at Essential");
    assert!(v.find_all_exact("Save").is_empty(), "idle Save hides at Essential when clean");

    // Ultra-narrow (Minimal): views also move into ⋯; Refresh, Add row, Panels and ⋯ stay.
    sized(&mut v, 350.0);
    assert_eq!(on_footer(&v), vec!["Refresh", "Add row"], "minimal");
    assert!(v.find_all_exact("Grid").is_empty(), "views moved into ⋯ at Minimal");
    let more = v.find_all_exact("More result actions")[0];
    v.click(more);
    v.frame(vec![]);
    for item in ["Grid", "Text", "Record", "JSON", "XML", "Chart", "Spatial view", "Duplicate selected rows", "Delete selected rows", "Export…"] {
        assert!(v.find(item).is_some(), "{item} in ⋯ at Minimal");
    }
    assert!(v.find("Fetch next").is_none(), "nothing to fetch here");
}

#[test]
fn the_more_menu_has_only_what_the_bar_does_not() {
    let mut v = Viewer::new(editable_model(5));
    // At Core width, Dup/Del/Undo and Export are on the bar; only Count (and Fetch all when has_more) move into ⋯.
    sized(&mut v, 840.0);
    let more = v.find_all_exact("More result actions")[0];
    v.click(more);
    v.frame(vec![]);
    assert!(v.find("Calculate total row count").is_some(), "Count moved into ⋯ at Core");
    for item in ["Duplicate selected rows", "Delete selected rows", "Export…"] {
        assert!(v.find(item).is_none(), "{item} is already on the bar");
    }
    // Auto refresh and Panels remain exclusively on the bar (never duplicated in ⋯).
    assert_eq!(v.find_all_exact("Auto refresh").len(), 1, "Auto refresh stays only on Refresh ▾");
    assert_eq!(v.find_all_exact("Panels").len(), 1, "Panels stays only on Panels ▾");
}

#[test]
fn the_footers_row_buttons_act_like_the_keys() {
    let mut v = Viewer::new(editable_model(5));
    v.state.grid.select_cell(1, 1);
    v.frame(vec![]);
    // Delete says how many, Undo brings them back.
    let delete = v.find_all_exact("Delete")[0];
    v.click(delete);
    v.frame(vec![]);
    assert_eq!(v.state.toolbar.notice, Some(("1 row marked for deletion".to_string(), false)));
    assert!(v.model.has_changes());
    let undo = v.find_all_exact("Undo")[0];
    v.click(undo);
    v.frame(vec![]);
    assert!(!v.model.has_changes());
    // Duplicate: a new row, selected.
    let duplicate = v.find_all_exact("Duplicate")[0];
    v.click(duplicate);
    v.frame(vec![]);
    assert_eq!(v.model.row_count(), 6);
}

#[test]
fn a_new_row_from_the_footer_starts_in_the_first_cell_to_fill_in() {
    let mut v = Viewer::new(crate::test_support::auto_id_model(3));
    v.state.grid.select_cell(1, 3);
    v.frame(vec![]);
    let add = v.find_all_exact("Add row")[0];
    v.click(add);
    v.frame(vec![]);
    assert_eq!(v.model.row_count(), 4);
    assert_eq!(v.state.grid.active_cell().map(|(_, c)| c), Some(1), "the column after the automatic id");
}

#[test]
fn the_footers_edits_wait_while_editing_is_off() {
    let mut v = Viewer::new(editable_model(3));
    v.state.grid.select_cell(1, 1);
    v.options.allow_editing = false;
    v.frame(vec![]);
    v.frame(vec![]);
    for name in ["Add row", "Duplicate", "Delete"] {
        let p = v.find_all_exact(name)[0];
        v.click(p);
        v.frame(vec![]);
    }
    assert_eq!(v.model.row_count(), 3);
    assert!(!v.model.has_changes());
}

#[test]
fn pending_changes_keep_their_place_in_a_full_footer() {
    let mut v = Viewer::new(editable_model(5));
    v.model.set_cell(1, 1, "changed").unwrap();
    v.frame(vec![]);
    v.frame(vec![]);
    // Option A 3-zone order: Refresh (left) < Add row < Discard < Save (center) < Panels (right).
    let (refresh, add, discard, save, panels) =
        (v.find_all_exact("Refresh")[0], v.find_all_exact("Add row")[0], v.find_all_exact("Discard")[0], v.find_all_exact("Save")[0], v.find_all_exact("Panels")[0]);
    assert!(
        refresh.x < add.x && add.x < discard.x && discard.x < save.x && save.x < panels.x,
        "3-zone order preserved at full width: {refresh:?} {add:?} {discard:?} {save:?} {panels:?}"
    );
    // At narrow (Essential) width, Cancel/Save stay visible while dirty and nothing overlaps.
    sized(&mut v, 620.0);
    let (refresh, add, discard, save, panels) =
        (v.find_all_exact("Refresh")[0], v.find_all_exact("Add row")[0], v.find_all_exact("Discard")[0], v.find_all_exact("Save")[0], v.find_all_exact("Panels")[0]);
    assert!(refresh.x < add.x && add.x < discard.x && discard.x < save.x && save.x < panels.x, "Essential order: {refresh:?} {add:?} {discard:?} {save:?} {panels:?}");
    assert_eq!(on_footer(&v), vec!["Refresh", "Add row"]);

    // Ultra-narrow (Minimal): views move into ⋯, active Cancel/Save stay visible with zero overlap.
    sized(&mut v, 440.0);
    let (refresh, add, discard, save, more) =
        (v.find_all_exact("Refresh")[0], v.find_all_exact("Add row")[0], v.find_all_exact("Discard")[0], v.find_all_exact("Save")[0], v.find_all_exact("More result actions")[0]);
    assert!(refresh.x < add.x && add.x < discard.x && discard.x < save.x && save.x < more.x, "Minimal order: {refresh:?} {add:?} {discard:?} {save:?} {more:?}");
    assert!(v.find_all_exact("Grid").is_empty(), "the views are in ⋯ at Minimal");
    v.click(more);
    v.frame(vec![]);
    let text = v.find_all_exact("Text").first().copied().expect("the views in ⋯");
    v.click(text);
    v.frame(vec![]);
    assert_eq!(v.state.toolbar.view, ResultViewKind::Text);
}

#[test]
fn footer_handles_save_readonly_and_floating_notice_cleanly() {
    // 1) Mixed edits (add + edit + delete): Save emits SaveChanges, no +1/~1/-1 delta chips.
    let mut v = Viewer::new(editable_model(4));
    v.model.add_row(Some(0)).unwrap();
    v.model.set_cell(1, 1, "edited").unwrap();
    v.model.delete_rows(&[3]).unwrap();
    v.frame(vec![]);
    assert!(v.find("+1").is_none() && v.find("~1").is_none() && v.find("-1").is_none(), "no delta chips");
    let save = v.find_all_exact("Save")[0];
    v.click(save);
    assert!(v.actions.iter().any(|a| matches!(a, GridAction::SaveChanges)), "Save emits SaveChanges");

    // 2) Saving state: shows Saving… and disables row edits.
    v.model.set_saving(true);
    v.frame(vec![]);
    assert!(!v.find_all_exact("Saving…").is_empty(), "Saving… shown while save runs");
    let rows_before = v.model.row_count();
    let add = v.find_all_exact("Add row")[0];
    v.click(add);
    v.frame(vec![]);
    assert_eq!(v.model.row_count(), rows_before, "Add row disabled during save");

    // 3) Read-only table without unique key: shows clickable Virtual key… pill action.
    let mut ro = editable_model(2);
    ro.set_editability(Err(crate::model::ReadOnlyReason::NoKey));
    let mut v = Viewer::new(ro);
    v.frame(vec![]);
    assert!(v.find_all_exact("Add row").is_empty(), "no row edit buttons when read-only");
    let vkey = v.find_all_exact("Virtual key…").first().copied().expect("Virtual key… action on read-only pill");
    v.click(vkey);
    assert!(v.actions.iter().any(|a| matches!(a, GridAction::DefineVirtualKey { .. })), "clicking Virtual key… emits DefineVirtualKey");

    // 4) Non-error notice floats cleanly above the footer and dismisses via its × button.
    let mut v = Viewer::new(editable_model(4));
    v.state.grid.select_cell(1, 1);
    v.frame(vec![]);
    let delete = v.find_all_exact("Delete")[0];
    v.click(delete);
    v.frame(vec![]);
    assert!(v.find("1 row marked for deletion").is_some(), "floating notice visible");
    let dismiss = v.find_all_exact("Dismiss").first().copied().expect("floating notice Dismiss button");
    v.click(dismiss);
    v.frame(vec![]);
    assert!(v.state.toolbar.notice.is_none(), "Dismiss clears floating notice");
}

#[test]
fn record_view_keeps_its_values_while_the_store_is_busy() {
    let mut v = Viewer::new(editable_model(5));
    v.state.grid.select_cell(2, 1);
    v.state.toolbar.view = ResultViewKind::Record;
    v.frame(vec![]);
    v.frame(vec![]);
    // The check box of `flag` (true in row 3) says what it shows.
    let flag = |v: &Viewer| {
        let update = v.output.accesskit_update.as_ref()?;
        update.nodes.iter().filter(|(_, n)| n.role() == egui::accesskit::Role::CheckBox).find_map(|(_, n)| n.label().filter(|l| *l != "Hide empty").map(str::to_string))
    };
    assert_eq!(flag(&v).as_deref(), Some("true"));
    let store = v.model.store().clone();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let guard = store.lock();
        let _ = locked_tx.send(());
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
        drop(guard);
    });
    locked_rx.recv().unwrap();
    v.frame(vec![]);
    v.frame(vec![]);
    let busy = flag(&v);
    let _ = release_tx.send(());
    holder.join().unwrap();
    assert_eq!(busy.as_deref(), Some("true"), "the values read before, not NULL, while the fetch task holds the store");
}

#[test]
fn row_footer_actions_work_in_record_view_and_keyboard_nav_preserves_copy() {
    let mut v = Viewer::new(editable_model(5));
    v.state.grid.select_cell(1, 1);
    v.state.toolbar.view = ResultViewKind::Record;
    v.frame(vec![]);
    v.frame(vec![]);
    // Add row before works immediately from Record view via the + Add ▾ split menu.
    let more_rows = v.find_all_exact("More row actions")[0];
    v.click(more_rows);
    v.frame(vec![]);
    let before = v.find("Add row before").expect("Add row before in + Add ▾ menu");
    v.click(before);
    v.frame(vec![]);
    assert_eq!(v.model.row_count(), 6, "Add row before inserted immediately in Record view");
    // Copy from row below works immediately from Record view via the + Add ▾ split menu.
    v.state.grid.select_cell(2, 1);
    v.frame(vec![]);
    let more_rows = v.find_all_exact("More row actions")[0];
    v.click(more_rows);
    v.frame(vec![]);
    let copy_below = v.find("Copy from row below").expect("Copy from row below in + Add ▾ menu");
    v.click(copy_below);
    v.frame(vec![]);
    assert_eq!(v.model.cell(2, 1), v.model.cell(3, 1), "Copy from row below ran immediately in Record view");
    // Navigating to the next row via ArrowRight updates picked so Cmd+C copies the new row's value.
    v.state.record.request_focus();
    v.frame(vec![]);
    v.frame(vec![crate::test_support::key(egui::Key::ArrowRight, Modifiers::NONE)]);
    v.frame(vec![]);
    v.frame(vec![Event::Copy]);
    let copied = v.output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    assert_eq!(copied.as_deref(), Some("name 2"), "Cmd+C copies active field after ArrowRight navigation");
}

#[test]
fn typed_defaults_and_bool_checkbox_on_new_row() {
    use ternarygrid_core::ddl::{ColumnDef, TableDef};
    let mut model = editable_model(2);
    let mut def = TableDef::new(Some("app".into()), "t");
    def.columns = vec![
        ColumnDef::new("id", "int8").not_null().auto_increment().default_value("nextval('items_id_seq'::regclass)"),
        ColumnDef::new("name", "varchar(50)"),
        ColumnDef::new("flag", "bool").not_null().default_value("true"),
        {
            let mut c = ColumnDef::new("category", "int8").not_null().default_value("0");
            c.generated = Some("id * 2".into());
            c
        },
    ];
    model.apply_table_def(&def);
    assert_eq!(model.default_display(0), crate::model::DefaultDisplay::Expr("nextval('items_id_seq'::regclass)"));
    assert_eq!(model.default_display(1), crate::model::DefaultDisplay::Null);
    assert_eq!(model.default_bool(2), Some(true));
    assert!(model.can_edit_cell(0, 3).is_err(), "generated column must not be editable");

    // Insert a new row and inspect it in Record View.
    let new_row = model.add_row(Some(1)).unwrap();
    assert_eq!(model.effective_bool(new_row, 2), Some(true));

    let mut v = Viewer::new(model);
    v.state.grid.select_cell(new_row, 0);
    v.state.toolbar.view = ResultViewKind::Record;
    v.frame(vec![]);
    v.frame(vec![]);

    // Record view renders an interactive checkbox for the new row's boolean column (even while DEFAULT).
    let cb = v
        .output
        .accesskit_update
        .as_ref()
        .and_then(|u| {
            u.nodes.iter().find_map(|(_, n)| {
                if n.role() == egui::accesskit::Role::CheckBox && n.label().is_some_and(|l| l.contains("true")) {
                    let b = n.bounds()?;
                    Some(Pos2::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32))
                } else {
                    None
                }
            })
        })
        .expect("checkbox for default-true bool column on new row");
    // Single click on default-true checkbox flips it directly to false (not requiring 2 clicks).
    v.click(cb);
    v.frame(vec![]);
    assert_eq!(v.model.cell(new_row, 2), Some(Value::Bool(false)));
}

#[test]
fn grouping_panel_multi_measure_and_header_sort() {
    use crate::grouping::{GroupSort, GroupSortCol};
    let mut ui = Ui::new();
    ui.ctx.enable_accesskit();
    let mut model = editable_model(60);
    let mut state = GroupingPanelState::default();
    state.spec.columns = vec![3];
    state.spec.measures = vec![0, 3];
    for _ in 0..400 {
        ui.run(vec![], Modifiers::NONE, |ui| {
            let _ = GroupingPanel::new(&mut model, &mut state).show(ui);
        });
        if !state.is_running() && state.result().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let res = state.result().expect("multi-measure result");
    assert_eq!(res.groups.len(), 3);
    assert_eq!(res.groups[0].measures.len(), 2);

    // Click the "category" header to sort ascending, then click again to sort descending.
    let out = ui.run(vec![], Modifiers::NONE, |ui| {
        let _ = GroupingPanel::new(&mut model, &mut state).show(ui);
    });
    let cat_hdr = out
        .accesskit_update
        .as_ref()
        .and_then(|u| {
            u.nodes.iter().find_map(|(_, n)| {
                if n.role() == egui::accesskit::Role::Button && n.label() == Some("category") {
                    let b = n.bounds()?;
                    Some(Pos2::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32))
                } else {
                    None
                }
            })
        })
        .expect("sortable category header button");
    for ev in [Event::PointerMoved(cat_hdr), press(cat_hdr, PointerButton::Primary, Modifiers::NONE), release(cat_hdr, PointerButton::Primary, Modifiers::NONE)] {
        ui.run(vec![ev], Modifiers::NONE, |ui| {
            let _ = GroupingPanel::new(&mut model, &mut state).show(ui);
        });
    }
    assert_eq!(state.sort, GroupSort { col: GroupSortCol::Key(0), descending: false });
    let keys_asc: Vec<_> = state.result().unwrap().groups.iter().map(|g| g.keys[0].clone()).collect();
    assert_eq!(keys_asc, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    for ev in [Event::PointerMoved(cat_hdr), press(cat_hdr, PointerButton::Primary, Modifiers::NONE), release(cat_hdr, PointerButton::Primary, Modifiers::NONE)] {
        ui.run(vec![ev], Modifiers::NONE, |ui| {
            let _ = GroupingPanel::new(&mut model, &mut state).show(ui);
        });
    }
    assert_eq!(state.sort, GroupSort { col: GroupSortCol::Key(0), descending: true });
    let keys_desc: Vec<_> = state.result().unwrap().groups.iter().map(|g| g.keys[0].clone()).collect();
    assert_eq!(keys_desc, vec![Value::Int(3), Value::Int(2), Value::Int(1)]);
}

#[test]
fn references_panel_shows_error_and_retries() {
    let mut ui = Ui::new();
    ui.ctx.enable_accesskit();
    let model = editable_model(5);
    let mut grid = crate::grid::DataGridState::for_model(&model);
    grid.select_cell(1, 0);
    let mut state = ReferencesPanelState::default();
    state.set_relations(vec![Relation {
        name: Some("fk".into()),
        direction: RelationDirection::References,
        table: ObjectRef::table(Some("app".into()), "categories"),
        columns: vec!["id".into()],
        key_columns: vec![3],
    }]);
    let mut actions = Vec::new();
    ui.run(vec![], Modifiers::NONE, |ui| actions = ReferencesPanel::new(&model, &grid, &mut state).show(ui));
    assert_eq!(actions.len(), 1);
    state.set_related_error(0, vec![Value::Int(2)], "permission denied".into());
    let out = ui.run(vec![], Modifiers::NONE, |ui| actions = ReferencesPanel::new(&model, &grid, &mut state).show(ui));
    assert!(actions.is_empty());
    let retry = out
        .accesskit_update
        .as_ref()
        .and_then(|u| {
            u.nodes.iter().find_map(|(_, n)| {
                if n.role() == egui::accesskit::Role::Button && n.label() == Some("Retry") {
                    let b = n.bounds()?;
                    Some(Pos2::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32))
                } else {
                    None
                }
            })
        })
        .expect("Retry button shown on error");
    for ev in [Event::PointerMoved(retry), press(retry, PointerButton::Primary, Modifiers::NONE), release(retry, PointerButton::Primary, Modifiers::NONE)] {
        ui.run(vec![ev], Modifiers::NONE, |ui| actions = ReferencesPanel::new(&model, &grid, &mut state).show(ui));
    }
    ui.run(vec![], Modifiers::NONE, |ui| actions = ReferencesPanel::new(&model, &grid, &mut state).show(ui));
    assert!(matches!(&actions[..], [GridAction::RequestReferences { relation: 0, .. }]), "Retry re-requests related rows");
}

#[test]
fn inline_editor_discard_save_and_view_switch_races() {
    use crate::test_support::key;
    use egui::Key;

    // 1) Grid view: start editing a cell, type draft text, click footer Discard -> editor cancels and pending == 0.
    let mut v = Viewer::new(editable_model(4));
    v.state.grid.request_focus();
    v.state.grid.select_cell(1, 1);
    v.frame(vec![]);
    // Mark a pending change first so Discard is active even before the inline edit commits.
    v.model.set_cell(0, 1, "dirty_row0").unwrap();
    v.frame(vec![]);
    v.frame(vec![Event::Text("ghost_draft".into())]);
    v.frame(vec![]);
    assert!(v.state.grid.is_editing(), "grid inline editor opened");
    let discard = v.find_all_exact("Discard")[0];
    v.click(discard);
    v.frame(vec![]);
    assert!(!v.state.grid.is_editing(), "Discard cancels active grid editor");
    assert_eq!(v.model.pending_counts().total(), 0, "Discard leaves zero pending changes (no ghost re-commit)");
    assert_eq!(v.model.cell(1, 1), Some(Value::text("name 1")));

    // 2) Grid view: start editing, type draft text, click footer Save -> commits active edit then emits SaveChanges.
    v.model.set_cell(0, 1, "dirty_row0").unwrap();
    v.state.grid.request_focus();
    v.state.grid.select_cell(1, 1);
    v.frame(vec![]);
    v.frame(vec![Event::Text("flushed_on_save".into())]);
    v.frame(vec![]);
    assert!(v.state.grid.is_editing());
    let save = v.find_all_exact("Save")[0];
    v.click(save);
    assert!(!v.state.grid.is_editing(), "Save commits active grid editor");
    assert_eq!(v.model.cell(1, 1), Some(Value::text("flushed_on_save")), "in-flight draft saved before SaveChanges");
    assert!(v.actions.iter().any(|a| matches!(a, GridAction::SaveChanges)), "Save emits SaveChanges");

    // 3) Grid view -> Record view switch while editing: flushes active edit and closes grid editor.
    v.state.grid.request_focus();
    v.state.grid.select_cell(2, 1);
    v.frame(vec![]);
    v.frame(vec![Event::Text("switched_to_rec".into())]);
    v.frame(vec![]);
    assert!(v.state.grid.is_editing());
    let rec = v.find_all_exact("Record")[0];
    v.click(rec);
    v.frame(vec![]);
    assert!(!v.state.grid.is_editing(), "grid editor closed on view switch");
    assert_eq!(v.state.toolbar.view, ResultViewKind::Record);
    assert_eq!(v.model.cell(2, 1), Some(Value::text("switched_to_rec")));

    // 4) Record view: start editing, type draft, click Discard -> cancels record editor and clears pending.
    v.state.record.request_focus();
    v.frame(vec![]);
    v.frame(vec![key(Key::Enter, Modifiers::NONE)]);
    v.frame(vec![]);
    v.frame(vec![Event::Text("rec_ghost".into())]);
    v.frame(vec![]);
    assert!(v.state.record.is_editing(), "record editor active");
    let discard = v.find_all_exact("Discard")[0];
    v.click(discard);
    v.frame(vec![]);
    assert!(!v.state.record.is_editing(), "Discard cancels active record editor");
    assert_eq!(v.model.pending_counts().total(), 0, "no ghost edit after Record Discard");

    // 5) Record view: edit cell, click Save while editor still open -> commits record edit & emits SaveChanges.
    v.model.set_cell(0, 1, "dirty_row0").unwrap();
    v.state.grid.select_cell(1, 1);
    v.state.record.request_focus();
    v.frame(vec![]);
    v.frame(vec![key(Key::Enter, Modifiers::NONE)]);
    v.frame(vec![]);
    v.frame(vec![Event::Text("rec_saved".into())]);
    v.frame(vec![]);
    assert!(v.state.record.is_editing());
    let save = v.find_all_exact("Save")[0];
    v.click(save);
    assert!(!v.state.record.is_editing(), "Save commits active record editor");
    assert_eq!(v.model.cell(1, 1), Some(Value::text("rec_saved")));
    assert!(v.actions.iter().any(|a| matches!(a, GridAction::SaveChanges)));
}

#[test]
fn option_a_toolbar_narrow_widths_readonly_and_clamps() {
    // 1) Sweep narrow-to-wide widths with dirty edits + active auto-refresh + dismissed fetch error chip.
    let mut v = Viewer::new(editable_model(5));
    v.model.set_cell(1, 1, "dirty").unwrap();
    v.state.toolbar.auto_refresh_secs = Some(10);
    v.model.fetch.error = Some("fetch error".into());
    v.state.toolbar.dismissed_fetch_error = Some("fetch error".into());
    for width in [300.0, 350.0, 440.0, 620.0, 840.0, 1200.0] {
        sized(&mut v, width);
        let refresh = v.find_all_exact("Refresh")[0];
        let panels = v.find_all_exact("Panels")[0];
        assert!(refresh.x < panels.x, "Refresh stays left of Panels at width {width}");
        assert!(panels.x <= width + 2.0, "Panels stays inside pane at width {width}: {panels:?}");
    }

    // 2) Read-only NoKey at Minimal width: Virtual key moves into ⋯ overflow menu and works.
    let mut ro = editable_model(2);
    ro.set_editability(Err(crate::model::ReadOnlyReason::NoKey));
    let mut v = Viewer::new(ro);
    sized(&mut v, 350.0);
    assert!(v.find_all_exact("Virtual key…").is_empty(), "inline Virtual key hides at Minimal");
    let more = v.find_all_exact("More result actions")[0];
    v.click(more);
    v.frame(vec![]);
    let vkey = v.find("Define virtual key").expect("Define virtual key in ⋯ at Minimal");
    v.click(vkey);
    assert!(v.actions.iter().any(|a| matches!(a, GridAction::DefineVirtualKey { .. })));

    // 3) can_edit = false disables Save and Discard even when model has pending edits.
    let mut v = Viewer::new(editable_model(3));
    v.model.set_cell(0, 1, "edited").unwrap();
    v.options.allow_editing = false;
    v.frame(vec![]);
    v.frame(vec![]);
    let save = v.find_all_exact("Save")[0];
    v.click(save);
    assert!(!v.actions.iter().any(|a| matches!(a, GridAction::SaveChanges)), "Save disabled when allow_editing = false");
    let discard = v.find_all_exact("Discard")[0];
    v.click(discard);
    assert!(v.model.has_changes(), "Discard disabled when allow_editing = false");

    // 4) auto_refresh_secs = Some(0) clamps to >= 1s (does not emit Refresh immediately every frame).
    let mut v = Viewer::new(editable_model(2));
    v.state.toolbar.auto_refresh_secs = Some(0);
    v.frame(vec![]);
    assert!(!v.actions.iter().any(|a| matches!(a, GridAction::Refresh)), "first tick initializes timer");
    v.frame(vec![]);
    assert!(!v.actions.iter().any(|a| matches!(a, GridAction::Refresh)), "0s interval clamped to 1s");

    // 5) Floating notice toast on ultra-narrow pane (140 px) renders safely without panicking.
    let mut v = Viewer::new(editable_model(3));
    v.state.toolbar.notice = Some(("Very long notification message that must clamp cleanly".into(), false));
    sized(&mut v, 140.0);
    assert!(v.find("Dismiss").is_some(), "floating notice stays dismissible at 140px");
}

#[test]
fn filter_bar_client_order_clear_mode_switch_and_null_to_empty() {
    use crate::test_support::key;
    use egui::Key;

    // 1) Client mode ORDER BY clear + Client -> Server switch clearing client view.
    let mut v = Viewer::new(editable_model(5));
    v.state.toolbar.order_text = "id DESC".into();
    let apply = v.filter_apply().expect("apply");
    v.click(apply);
    v.wait_jobs();
    assert_eq!(v.model.cell(0, 0), Some(Value::Int(4)), "sorted descending");
    // Open Sort / ORDER BY popup and clear it.
    let order_btn = v.find("Sort").expect("Sort chip");
    v.click(order_btn);
    v.frame(vec![]);
    v.state.toolbar.order_text.clear();
    let apply_pop = v.find_all_exact("Apply").last().copied().expect("popup Apply");
    v.click(apply_pop);
    v.wait_jobs();
    assert_eq!(v.model.cell(0, 0), Some(Value::Int(0)), "clearing ORDER BY restores original order");

    // Apply a client filter, then switch Client -> Server mode: client view must be cleared.
    v.state.toolbar.filter_text = "id > 2".into();
    let apply = v.filter_apply().expect("apply");
    v.click(apply);
    v.wait_jobs();
    assert_eq!(v.model.row_count(), 2);
    assert!(v.model.has_client_view());
    let mode_btn = v.find("Filter options").expect("Filter options menu");
    v.click(mode_btn);
    v.frame(vec![]);
    let server = v.find("Server: in the database").expect("Server option in mode menu");
    v.click(server);
    v.frame(vec![]);
    assert_eq!(v.state.grid.mode, crate::grid::DataMode::Server);
    assert!(!v.model.has_client_view(), "Client->Server clears stale client filter view");
    assert_eq!(v.model.row_count(), 5);

    // 2) NULL text cell -> commit empty string "" in Grid view and Record view.
    let mut v = Viewer::new(editable_model(3));
    v.model.set_value(0, 1, Value::Null).unwrap();
    v.model.set_value(1, 1, Value::Null).unwrap();
    v.state.grid.request_focus();
    v.state.grid.select_cell(0, 1);
    v.frame(vec![]);
    v.frame(vec![key(Key::Enter, Modifiers::NONE)]);
    v.frame(vec![]);
    assert!(v.state.grid.is_editing());
    v.frame(vec![key(Key::Enter, Modifiers::NONE)]);
    v.frame(vec![]);
    assert_eq!(v.model.cell(0, 1), Some(Value::text("")), "NULL text cell committed as empty string in Grid view");

    v.state.grid.select_cell(1, 1);
    v.state.toolbar.view = ResultViewKind::Record;
    v.state.record.request_focus();
    v.frame(vec![]);
    v.frame(vec![]);
    v.frame(vec![key(Key::Enter, Modifiers::NONE)]);
    v.frame(vec![]);
    assert!(v.state.record.is_editing());
    v.frame(vec![key(Key::Enter, Modifiers::NONE)]);
    v.frame(vec![]);
    assert_eq!(v.model.cell(1, 1), Some(Value::text("")), "NULL text cell committed as empty string in Record view");

    // 3) set_cells duplicate (row, col) in one batch preserves true original value; duplicate_rows leaves generated Default.
    use ternarygrid_core::ddl::{ColumnDef, TableDef};
    let mut model = editable_model(2);
    let mut def = TableDef::new(Some("app".into()), "t");
    def.columns = vec![ColumnDef::new("id", "int8").not_null().auto_increment(), ColumnDef::new("name", "varchar(50)"), ColumnDef::new("flag", "bool"), {
        let mut c = ColumnDef::new("category", "int8");
        c.generated = Some("id * 2".into());
        c
    }];
    model.apply_table_def(&def);
    model.set_cells(vec![(0, 1, crate::model::CellValue::Value(Value::text("first"))), (0, 1, crate::model::CellValue::Value(Value::text("second")))]).unwrap();
    assert_eq!(model.cell(0, 1), Some(Value::text("second")));
    model.undo();
    assert_eq!(model.cell(0, 1), Some(Value::text("name 0")), "undo restores true pre-batch original");
    let dup = model.duplicate_rows(&[0]).unwrap();
    assert_eq!(dup.len(), 1);
    assert!(model.cell_is_default(dup[0], 3), "duplicate_rows leaves generated column as Default");
}
