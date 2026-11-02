//! Table migration between connections (FR-T3): source tables, target
//! connection and schema, table / column names and type overrides, table
//! preparation, indexes and foreign keys; runs with per-table progress and
//! can be saved as a task.

use super::wizard::{self, Clicked, W, col, line};
use super::{
    MessageLog, Running, TaskSaver, combo_w, connection_combo, connection_name, current_schema, error_text, next_uid, not_connected, on_error_options, progress_ui, schema_combo,
    schema_tables, section, table_def_of,
};
use crate::cx::AppContext;
use crate::tabs::TabView;
use crate::transferexec::{AppSessionFactory, database_of, migration_task_config};
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use std::collections::BTreeMap;
use std::sync::Arc;
use ternarygrid_core::meta::{ObjectRef, SchemaRef};
use ternarygrid_core::{DbError, DbKind, DbResult, tr, trf};
use ternarygrid_tasks::TaskKind;
use ternarygrid_transfer::{MessageLevel, MigrationJob, MigrationMode, MigrationSummary, NameCase, SessionFactory, run_migration};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::{self, Tone};

const PAGE_SOURCE: usize = 0;
const PAGE_TARGET: usize = 1;
const PAGE_MAPPING: usize = 2;
const PAGE_OPTIONS: usize = 3;
const PAGE_RUN: usize = 4;

/// One source table with its target names.
#[derive(Clone)]
struct TableEntry {
    enabled: bool,
    source: ObjectRef,
    /// Target table name (empty: the source name with the name case applied).
    target_name: String,
    /// Target column names by source column (empty values are ignored).
    column_names: BTreeMap<String, String>,
    /// Target types by source column (empty values are ignored).
    column_types: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableState {
    Pending,
    Running,
    Done,
    Failed,
}

/// Live state of one table during a run.
struct TableRun {
    name: String,
    state: TableState,
    rows: u64,
    base: u64,
}

/// The migration tab.
pub struct MigrationTab {
    /// Finished successfully: the pop-up closes.
    done: bool,
    uid: u64,
    conn: String,
    page: usize,
    tables: Vec<TableEntry>,
    picker_schema: Option<SchemaRef>,
    picker_filter: String,
    target_conn: Option<String>,
    target_schema: Option<SchemaRef>,
    /// Options (tables and names are built from `tables`).
    options: MigrationJob,
    run: Option<Running<MigrationSummary>>,
    table_runs: Vec<TableRun>,
    log: MessageLog,
    result: Option<Result<MigrationSummary, String>>,
    error: Option<String>,
    confirm: bool,
    saver: TaskSaver,
}

fn apply_case(case: NameCase, s: &str) -> String {
    match case {
        NameCase::Keep => s.to_string(),
        NameCase::Lower => s.to_lowercase(),
        NameCase::Upper => s.to_uppercase(),
    }
}

impl MigrationTab {
    /// Migrate `objects` (tables) of `conn` to another connection.
    pub fn new(conn: String, objects: Vec<ObjectRef>) -> Self {
        let mut picker_schema = None;
        let mut tables = Vec::new();
        for o in objects {
            if o.kind == ternarygrid_core::meta::ObjectKind::Schema {
                picker_schema = Some(SchemaRef::new(o.database.clone(), Some(o.name.clone())));
            } else if o.kind.is_table_like() {
                if picker_schema.is_none() {
                    picker_schema = Some(o.schema_ref()).filter(|s| s.schema.is_some());
                }
                tables.push(TableEntry { enabled: true, source: o, target_name: String::new(), column_names: BTreeMap::new(), column_types: BTreeMap::new() });
            }
        }
        MigrationTab {
            done: false,
            uid: next_uid(),
            conn,
            page: PAGE_SOURCE,
            tables,
            picker_schema,
            picker_filter: String::new(),
            target_conn: None,
            target_schema: None,
            options: MigrationJob::default(),
            run: None,
            table_runs: Vec::new(),
            log: MessageLog::default(),
            result: None,
            error: None,
            confirm: false,
            saver: TaskSaver::default(),
        }
    }

    // ------------------------------------------------------------ public API

    /// Choose the target connection and schema (`None`: its default schema).
    pub fn set_target(&mut self, conn: String, schema: Option<String>) {
        self.target_conn = Some(conn);
        self.target_schema = schema.map(|s| SchemaRef::new(None, Some(s)));
    }

    /// Rename the target table of `source` (by source table name).
    pub fn set_target_name(&mut self, source: &str, target: &str) {
        if let Some(t) = self.tables.iter_mut().find(|t| t.source.name == source) {
            t.target_name = target.to_string();
        }
    }

    /// Override the target type of a column (`""` removes the override).
    pub fn set_column_type(&mut self, table: &str, column: &str, data_type: &str) {
        if let Some(t) = self.tables.iter_mut().find(|t| t.source.name == table) {
            if data_type.trim().is_empty() {
                t.column_types.remove(column);
            } else {
                t.column_types.insert(column.to_string(), data_type.trim().to_string());
            }
        }
    }

    /// Add a source table.
    pub fn add_table(&mut self, table: ObjectRef) {
        if !self.tables.iter().any(|t| t.source == table) {
            self.tables.push(TableEntry { enabled: true, source: table, target_name: String::new(), column_names: BTreeMap::new(), column_types: BTreeMap::new() });
        }
    }

    /// Options (mode, batch size, indexes, foreign keys, transactions, errors).
    pub fn options_mut(&mut self) -> &mut MigrationJob {
        &mut self.options
    }

    /// The job as it would run now.
    pub fn job(&self) -> MigrationJob {
        let mut job = self.options.clone();
        let enabled: Vec<&TableEntry> = self.tables.iter().filter(|t| t.enabled).collect();
        job.tables = enabled.iter().map(|t| t.source.clone()).collect();
        job.target_schema = self.target_schema.as_ref().and_then(|s| s.schema.clone());
        // Keyed by the schema-qualified source name: tables of the same name in two
        // schemas keep their own names. A name only one table has is also given
        // alone (the key of saved tasks and of readers that know no schema).
        let keys = |t: &TableEntry| {
            let mut keys = vec![table_key(&t.source)];
            if keys[0] != t.source.name && enabled.iter().filter(|o| o.source.name == t.source.name).count() == 1 {
                keys.push(t.source.name.clone());
            }
            keys
        };
        job.table_names = enabled.iter().filter(|t| !t.target_name.trim().is_empty()).flat_map(|t| keys(t).into_iter().map(|k| (k, t.target_name.trim().to_string()))).collect();
        job.column_names = enabled
            .iter()
            .flat_map(|t| {
                let names: BTreeMap<String, String> = t.column_names.iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.clone(), v.trim().to_string())).collect();
                keys(t).into_iter().map(move |k| (k, names.clone()))
            })
            .filter(|(_, m)| !m.is_empty())
            .collect();
        job.column_types = enabled
            .iter()
            .flat_map(|t| {
                let types: Vec<(&String, &String)> = t.column_types.iter().filter(|(_, v)| !v.trim().is_empty()).collect();
                keys(t).into_iter().flat_map(move |k| types.clone().into_iter().map(move |(c, v)| (format!("{k}.{c}"), v.trim().to_string())))
            })
            .collect();
        job
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn set_page(&mut self, page: usize) {
        self.page = page.min(PAGE_RUN);
    }

    /// Run the migration (the "Run" button without the confirmation).
    pub fn start(&mut self, cx: &mut AppContext) {
        if self.run.is_some() {
            return;
        }
        if let Err(e) = self.validate(cx) {
            self.error = Some(e);
            return;
        }
        let Some(target) = self.target_conn.clone() else { return };
        if let Some(cfg) = cx.conns.config(&target)
            && cfg.read_only
        {
            self.error = Some(trf("The connection \"{}\" is read-only.", &[&cfg.name]));
            return;
        }
        for c in [self.conn.clone(), target.clone()] {
            if cx.conns.live(&c).is_none() {
                self.error = Some(not_connected(cx, &c));
                return;
            }
        }
        let job = self.job();
        let (Some(src_live), Some(dst_live)) = (cx.conns.live(&self.conn), cx.conns.live(&target)) else { return };
        let src_db = database_of(job.tables.iter());
        let dst_db = self.target_schema.as_ref().and_then(|s| s.database.clone());
        let src: Arc<dyn SessionFactory> = Arc::new(AppSessionFactory::from_live(src_live, src_db.as_deref()));
        let dst: Arc<dyn SessionFactory> = Arc::new(AppSessionFactory::from_live(dst_live, dst_db.as_deref()));
        self.table_runs = job.tables.iter().map(|t| TableRun { name: t.display_path(), state: TableState::Pending, rows: 0, base: 0 }).collect();
        self.error = None;
        self.result = None;
        self.log.clear();
        self.page = PAGE_RUN;
        let title = trf("Migrate {} table(s) to {}", &[&job.tables.len(), &connection_name(cx, &target)]);
        self.run = Some(Running::spawn(cx, title, move |progress, cancel| async move { run_migration(job, src, dst, &progress, &cancel).await }));
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    pub fn cancel(&self) {
        if let Some(r) = &self.run {
            r.cancel();
        }
    }

    pub fn last_summary(&self) -> Option<&MigrationSummary> {
        self.result.as_ref().and_then(|r| r.as_ref().ok())
    }

    pub fn last_error(&self) -> Option<&str> {
        match &self.result {
            Some(Err(e)) => Some(e),
            _ => self.error.as_deref(),
        }
    }

    /// Save the current configuration as a task (in the background).
    pub fn save_task(&mut self, cx: &mut AppContext, name: &str, folder: Option<String>) {
        match self.task_kind(cx) {
            Ok(kind) => self.saver.save(cx, name.to_string(), folder, kind),
            Err(e) => self.saver.fail(e.full_text()),
        }
    }

    pub fn saved_task_id(&self) -> Option<&str> {
        self.saver.saved.as_deref()
    }

    // --------------------------------------------------------------- helpers

    fn task_kind(&self, cx: &AppContext) -> DbResult<TaskKind> {
        self.validate(cx).map_err(DbError::config)?;
        let target = self.target_conn.clone().ok_or_else(|| DbError::config(tr("Choose the target connection.")))?;
        Ok(TaskKind::Migration { source_connection_id: self.conn.clone(), target_connection_id: target, config: migration_task_config(&self.job())? })
    }

    fn tables_error(&self) -> Option<String> {
        if !self.tables.iter().any(|t| t.enabled) {
            return Some(tr("Choose at least one table to migrate.").to_string());
        }
        None
    }

    fn target_error(&self, cx: &AppContext) -> Option<String> {
        let Some(target) = &self.target_conn else { return Some(tr("Choose the target connection.").to_string()) };
        // Two sources into one target table: the second would overwrite (truncate, drop) the first.
        let mut names = std::collections::HashSet::new();
        for t in self.tables.iter().filter(|t| t.enabled) {
            let name = if t.target_name.trim().is_empty() { apply_case(self.options.name_case, &t.source.name) } else { t.target_name.trim().to_string() };
            if !names.insert(name.to_lowercase()) {
                return Some(trf("Several tables are migrated to {}; give them different target names.", &[&name]));
            }
        }
        if *target == self.conn {
            // Same connection: the target must not be the source table itself.
            let default = current_schema(cx, &self.conn).and_then(|s| s.schema);
            let target_schema = self.target_schema.as_ref().and_then(|s| s.schema.clone()).or(default.clone());
            for t in self.tables.iter().filter(|t| t.enabled) {
                let source_schema = t.source.schema.clone().or(default.clone());
                let name = if t.target_name.trim().is_empty() { apply_case(self.options.name_case, &t.source.name) } else { t.target_name.trim().to_string() };
                if source_schema == target_schema && name == t.source.name {
                    return Some(trf("The target of {} is the table itself; choose another schema or name.", &[&t.source.display_path()]));
                }
            }
        }
        None
    }

    fn validate(&self, cx: &AppContext) -> Result<(), String> {
        match self.tables_error().or_else(|| self.target_error(cx)) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn reachable(&self, cx: &AppContext) -> usize {
        if self.tables_error().is_some() {
            PAGE_SOURCE
        } else if self.target_error(cx).is_some() {
            PAGE_TARGET
        } else {
            PAGE_RUN
        }
    }

    fn destructive(&self) -> bool {
        matches!(self.options.mode, MigrationMode::Truncate | MigrationMode::DropAndCreate)
    }

    fn poll(&mut self, cx: &mut AppContext) {
        let mut finished = None;
        if let Some(run) = &mut self.run {
            let messages = run.progress.take_messages();
            let active = run.progress.active_items();
            let rows = run.progress.rows();
            for t in &mut self.table_runs {
                let is_active = active.contains(&t.name);
                match t.state {
                    TableState::Pending if is_active => {
                        t.state = TableState::Running;
                        t.base = rows;
                    }
                    TableState::Running if is_active => t.rows = rows.saturating_sub(t.base),
                    TableState::Running => {
                        t.state = TableState::Done;
                        t.rows = rows.saturating_sub(t.base);
                    }
                    _ => {}
                }
            }
            for m in &messages {
                if m.level == MessageLevel::Error
                    && let Some(item) = &m.item
                    && let Some(t) = self.table_runs.iter_mut().find(|t| &t.name == item)
                {
                    t.state = TableState::Failed;
                }
            }
            self.log.push(messages);
            if let Some(result) = run.poll(cx, describe) {
                self.log.drain(&run.progress);
                finished = Some(result);
            }
        }
        if let Some(result) = finished {
            self.run = None;
            match result {
                Ok(summary) => {
                    let failed = summary.tables.iter().filter(|t| t.error.is_some()).count();
                    if failed > 0 {
                        cx.toasts.warning(trf("Migration finished, {} table(s) failed", &[&failed]), None);
                    } else {
                        cx.toasts.success(describe(&summary));
                        self.done = true;
                    }
                    if let Some(target) = &self.target_conn {
                        let schema = self.target_schema.clone().or_else(|| current_schema(cx, target));
                        cx.push(crate::command::AppCommand::RefreshMetadata { connection: target.clone(), object: None, schema });
                    }
                    self.result = Some(Ok(summary));
                }
                Err(e) => {
                    if e.kind != ternarygrid_core::ErrorKind::Cancelled {
                        cx.toasts.error(tr("Migration failed"), Some(e.full_text()));
                    }
                    self.result = Some(Err(error_text(&e)));
                }
            }
        }
        self.saver.poll(cx);
    }

    // ------------------------------------------------------------------ pages

    fn source_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let p = ternarygrid_ui::pal(ui);
        section(ui, tr("Tables to migrate"));
        if self.tables.is_empty() {
            wizard::faint(ui, tr("No table selected yet: add tables below."));
        } else {
            let mut remove = None;
            let mut set_all = None;
            let states: Vec<bool> = self.tables.iter().map(|t| t.enabled).collect();
            let cols = [col("", W::Px(32.0)), col(tr("Table"), W::Fr(1.0)), col("", W::Px(44.0))];
            let tables = &mut self.tables;
            wizard::table(
                ui,
                ("mig_tables", self.uid),
                &cols,
                34.0,
                None,
                tables.len(),
                |ui, c| {
                    if c == 0
                        && let Some(on) = wizard::check_all(ui, &states)
                    {
                        set_all = Some(on);
                    }
                },
                |ui, i, c| {
                    let t = &mut tables[i];
                    match c {
                        0 => {
                            wizard::check(ui, &mut t.enabled, "");
                        }
                        1 => {
                            ui.label(RichText::new(icon::TABLE).font(ternarygrid_ui::style::icon_font(14.0)).color(p.text_muted));
                            ui.add(egui::Label::new(t.source.display_path()).truncate());
                        }
                        _ => {
                            if wizard::icon_button(ui, icon::TRASH, tr("Remove"), 24.0).clicked() {
                                remove = Some(i);
                            }
                        }
                    }
                },
            );
            if let Some(on) = set_all {
                self.tables.iter_mut().for_each(|t| t.enabled = on);
            }
            if let Some(i) = remove {
                self.tables.remove(i);
            }
        }
        section(ui, tr("Add tables"));
        let conn = self.conn.clone();
        if cx.conns.live(&conn).is_none() {
            ui.horizontal(|ui| {
                wizard::faint(ui, tr("Connect to list the tables."));
                if kit::secondary(ui, Some(icon::PLUGS_CONNECTED), tr("Connect")).clicked() {
                    cx.push(crate::command::AppCommand::Connect(conn.clone()));
                }
            });
            return;
        }
        let schema = self.picker_schema.clone().or_else(|| current_schema(cx, &conn));
        let list = schema.as_ref().map(|s| schema_tables(cx, &conn, s, false));
        let filter = self.picker_filter.to_lowercase();
        let shown: Option<Vec<ObjectRef>> = match &list {
            Some(Some(Ok(list))) => {
                Some(list.iter().filter(|o| !o.is_system && (filter.is_empty() || o.obj.name.to_lowercase().contains(&filter))).map(|o| o.obj.clone()).collect())
            }
            _ => None,
        };
        ui.horizontal(|ui| {
            wizard::muted(ui, tr("Schema"));
            schema_combo(ui, cx, &conn, ("mig_schema", self.uid), &mut self.picker_schema, 160.0);
            kit::search_field(ui, egui::Id::new(("mig_filter", self.uid)), &mut self.picker_filter, tr("Filter…"), 220.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(shown) = &shown {
                    if !shown.is_empty() && wizard::ghost(ui, None, tr("Add all shown")).clicked() {
                        for o in shown {
                            self.add_table(o.clone());
                        }
                    }
                    wizard::faint(ui, &trf("{} tables", &[&shown.len()]));
                }
            });
        });
        match list {
            None => {}
            Some(None) => {
                parts::spinner_label(ui, tr("Loading…"));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
            Some(Some(Err(e))) => crate::widgets::error_box(ui, &e.full_text()),
            Some(Some(Ok(_))) => {
                let shown = shown.unwrap_or_default();
                let mut add = None;
                let tables = &self.tables;
                super::export::picker_list(ui, ("mig_pick", self.uid), &shown, |o| tables.iter().any(|t| t.source == *o), |o| add = Some(o));
                if let Some(o) = add {
                    self.add_table(o);
                }
            }
        }
    }

    fn target_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        section(ui, tr("Target"));
        let id = self.uid;
        wizard::form(ui, ("mig_target", id), 1, 120.0, |f| {
            f.row(tr("Connection"), |ui| {
                let kind = self.target_conn.as_ref().and_then(|t| cx.conns.config(t)).map(|c| c.kind);
                if let Some(k) = kind {
                    crate::tabs::dbtools::look::logo(ui, k, 15.0);
                }
                if connection_combo(ui, cx, ("mig_conn", id), &mut self.target_conn, 260.0) {
                    self.target_schema = None;
                }
                if let Some(t) = self.target_conn.clone()
                    && !cx.conns.is_connected(&t)
                    && kit::secondary(ui, Some(icon::PLUGS_CONNECTED), tr("Connect")).clicked()
                {
                    cx.push(crate::command::AppCommand::Connect(t));
                }
            });
            if let Some(t) = self.target_conn.clone()
                && cx.conns.is_connected(&t)
            {
                f.row(tr("Schema"), |ui| schema_combo(ui, cx, &t, ("mig_tschema", id), &mut self.target_schema, 200.0));
            }
            f.row(tr("Names"), |ui| {
                parts::segmented(
                    ui,
                    &mut self.options.name_case,
                    &[(NameCase::Keep, tr("Keep them as they are")), (NameCase::Lower, tr("lower case")), (NameCase::Upper, tr("UPPER CASE"))],
                )
            });
        });
        if let (Some(t), Some(src)) = (&self.target_conn, cx.conns.config(&self.conn).map(|c| c.kind)) {
            let dst = cx.conns.config(t).map(|c| c.kind);
            if let Some(dst) = dst
                && dst != src
            {
                parts::callout(
                    ui,
                    Tone::Info,
                    None,
                    &trf("Column types are converted from {} to {}; check them on the next page.", &[&src.display_name(), &dst.display_name()]),
                    |_| {},
                );
            }
        }
        if let Some(e) = self.target_error(cx) {
            crate::widgets::error_box(ui, &e);
        }
    }

    fn mapping_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        section(ui, tr("Tables and columns"));
        let id = self.uid;
        let src_kind = cx.conns.config(&self.conn).map(|c| c.kind).unwrap_or(DbKind::Generic);
        let dst_kind = self.target_conn.as_ref().and_then(|t| cx.conns.config(t)).map(|c| c.kind).unwrap_or(DbKind::Generic);
        let dst = cx.conns.registry.dialect(dst_kind);
        let same = src_kind == dst.kind() && src_kind != DbKind::Generic;
        let case = self.options.name_case;
        let conn = self.conn.clone();
        let p = ternarygrid_ui::pal(ui);
        wizard::faint(ui, tr("Leave a field empty to keep the default shown in grey."));
        let mut first = true;
        for (i, t) in self.tables.iter_mut().enumerate().filter(|(_, t)| t.enabled) {
            let default_name = apply_case(case, &t.source.name);
            let title = if t.target_name.trim().is_empty() {
                format!("{} → {default_name}", t.source.display_path())
            } else {
                format!("{} → {}", t.source.display_path(), t.target_name.trim())
            };
            let key = egui::Id::new(("mig_map", id, table_key(&t.source)));
            let mut open = wizard::remembered_open(ui, key, first);
            first = false;
            let toggled = wizard::fold_card(ui, key, &mut open, &title, None, true, |ui| {
                wizard::form(ui, ("mig_map_name", id, i), 1, 110.0, |f| {
                    f.row(tr("Target table"), |ui| ui.add(line(&mut t.target_name).hint_text(default_name.clone()).desired_width(260.0)));
                });
                match table_def_of(cx, &conn, &t.source) {
                    None => {
                        parts::spinner_label(ui, tr("Loading…"));
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    Some(Err(e)) => crate::widgets::error_box(ui, &e.full_text()),
                    Some(Ok(def)) => {
                        let cols = [col(tr("Column"), W::Fr(1.0)), col(tr("Source type"), W::Fr(1.0)), col(tr("Target column"), W::Fr(1.0)), col(tr("Target type"), W::Fr(1.0))];
                        let (names, types) = (&mut t.column_names, &mut t.column_types);
                        wizard::table(
                            ui,
                            ("mig_cols", id, i),
                            &cols,
                            36.0,
                            None,
                            def.columns.len(),
                            |_, _| {},
                            |ui, r, k| {
                                let c = &def.columns[r];
                                match k {
                                    0 => {
                                        wizard::mono(ui, &c.name, p.text, 12.0);
                                    }
                                    1 => {
                                        wizard::mono(ui, &c.data_type, p.text_muted, 12.0);
                                    }
                                    2 => {
                                        let name = names.entry(c.name.clone()).or_default();
                                        ui.add(line(name).hint_text(apply_case(case, &c.name)).desired_width(f32::INFINITY));
                                    }
                                    _ => {
                                        let mapped = if same { c.data_type.clone() } else { dst.ddl().map_type(src_kind, &c.data_type, c.kind) };
                                        let ty = types.entry(c.name.clone()).or_default();
                                        ui.add(wizard::mono_line(ty, f32::INFINITY).hint_text(RichText::new(mapped).monospace()));
                                    }
                                }
                            },
                        );
                    }
                }
            });
            if toggled {
                wizard::remember_open(ui, key, open);
            }
        }
        if !same && src_kind != DbKind::Generic && dst_kind != DbKind::Generic {
            parts::callout(
                ui,
                Tone::Info,
                None,
                &trf("Column types are converted from {} to {}; check them on this page.", &[&src_kind.display_name(), &dst_kind.display_name()]),
                |_| {},
            );
        }
    }

    fn options_page(&mut self, ui: &mut Ui, transactions: bool) {
        section(ui, tr("Options"));
        let id = self.uid;
        let o = &mut self.options;
        wizard::form(ui, ("mig_opts", id), 1, 150.0, |f| {
            f.check_row(tr("Target tables"), |ui| wizard::check(ui, &mut o.create_tables, tr("Create the tables that do not exist")));
            f.row(tr("Existing rows"), |ui| {
                combo_w(
                    ui,
                    ("mig_mode", id),
                    &mut o.mode,
                    &[
                        (MigrationMode::Append, tr("Keep them and add the new rows").to_string()),
                        (MigrationMode::Truncate, tr("Delete them first").to_string()),
                        (MigrationMode::Replace, tr("Insert or replace rows by primary key").to_string()),
                        (MigrationMode::DropAndCreate, tr("Drop and create the tables again").to_string()),
                    ],
                    320.0,
                )
            });
            if o.mode == MigrationMode::Replace && !transactions {
                super::replace_without_transactions_ui(f);
            }
            f.row(tr("Rows per batch"), |ui| wizard::number(ui, &mut o.batch_size, 1..=100_000, 120.0));
            f.check_row(tr("Copy"), |ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                wizard::check(ui, &mut o.copy_indexes, tr("Indexes"));
                wizard::check(ui, &mut o.copy_foreign_keys, tr("Foreign keys"));
            });
            f.check_row(tr("Transactions"), |ui| wizard::check(ui, &mut o.transaction_per_table, tr("Load every table in one transaction")));
            f.row(tr("On error"), |ui| combo_w(ui, ("mig_err", id), &mut o.on_error, &on_error_options(), 260.0));
            f.check_row(tr("Failed tables"), |ui| wizard::check(ui, &mut o.continue_on_table_error, tr("Continue with the next table")));
        });
    }

    fn run_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let p = ternarygrid_ui::pal(ui);
        if let Some(run) = &self.run {
            wizard::title(ui, tr("Migrating…"));
            if progress_ui(ui, &run.progress, run.is_cancelling()) {
                run.cancel();
            }
            let cols = [col(tr("Table"), W::Fr(1.4)), col(tr("Status"), W::Fr(1.0)), col(tr("Rows"), W::Fr(0.6)).right()];
            let runs = &self.table_runs;
            wizard::table(
                ui,
                ("mig_live", self.uid),
                &cols,
                30.0,
                Some(260.0),
                runs.len(),
                |_, _| {},
                |ui, r, c| {
                    let t = &runs[r];
                    match c {
                        0 => {
                            ui.add(egui::Label::new(&t.name).truncate());
                        }
                        1 => match t.state {
                            TableState::Pending => {
                                wizard::faint(ui, tr("waiting"));
                            }
                            TableState::Running => {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                ui.label(RichText::new(icon::CLOCK_COUNTER_CLOCKWISE).font(ternarygrid_ui::style::icon_font(13.0)).color(p.accent_text));
                                ui.label(RichText::new(tr("copying")).size(12.5).color(p.accent_text));
                            }
                            TableState::Done => {
                                wizard::tone_text(ui, Tone::Success, tr("done"));
                            }
                            TableState::Failed => {
                                wizard::tone_text(ui, Tone::Error, tr("failed"));
                            }
                        },
                        _ => {
                            ui.label(if t.state == TableState::Pending { "—".to_string() } else { super::group(t.rows) });
                        }
                    }
                },
            );
            self.log.ui(ui, ("mig_log", self.uid), 220.0);
            return;
        }
        match &self.result {
            Some(Ok(summary)) => {
                let summary = summary.clone();
                self.result_ui(ui, &summary);
            }
            Some(Err(e)) => {
                wizard::outcome(ui, Tone::Error, tr("The migration failed"), None);
                crate::widgets::error_box(ui, e);
                if kit::button_ex(ui, Kind::Ghost, Some(icon::PENCIL_SIMPLE), tr("Change settings"), false).clicked() {
                    self.result = None;
                    self.page = PAGE_SOURCE;
                }
            }
            None => self.summary_ui(ui, cx),
        }
        if self.result.is_some() {
            self.log.ui(ui, ("mig_log", self.uid), 220.0);
        }
    }

    fn summary_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        section(ui, tr("Summary"));
        let job = self.job();
        let target = self.target_conn.as_ref().map(|t| connection_name(cx, t)).unwrap_or_else(|| "—".into());
        let schema = job.target_schema.clone().unwrap_or_else(|| tr("(default)").to_string());
        wizard::facts(
            ui,
            ("mig_sum", self.uid),
            120.0,
            8.0,
            &[
                (tr("Source"), connection_name(cx, &self.conn), None),
                (tr("Target"), format!("{target} / {schema}"), None),
                (tr("Tables"), job.tables.iter().map(|t| t.display_path()).collect::<Vec<_>>().join(", "), None),
            ],
        );
        if let Err(e) = self.validate(cx) {
            crate::widgets::error_box(ui, &e);
        }
    }

    fn result_ui(&mut self, ui: &mut Ui, s: &MigrationSummary) {
        let p = ternarygrid_ui::pal(ui);
        let failed = s.tables.iter().any(|t| t.error.is_some());
        wizard::outcome(ui, if failed { Tone::Warning } else { Tone::Success }, &describe(s), None);
        let cols = [
            col(tr("Table"), W::Fr(1.2)),
            col(tr("Target"), W::Fr(1.3)),
            col(tr("Rows"), W::Fr(0.8)).right(),
            col(tr("Failed"), W::Fr(0.6)).right(),
            col(tr("Time"), W::Fr(0.6)).right(),
            col(tr("Status"), W::Fr(1.0)),
        ];
        wizard::table(
            ui,
            ("mig_res", self.uid),
            &cols,
            30.0,
            Some(320.0),
            s.tables.len(),
            |_, _| {},
            |ui, r, c| {
                let t = &s.tables[r];
                match c {
                    0 => {
                        ui.add(egui::Label::new(t.source.as_ref().map(ObjectRef::display_path).unwrap_or_default()).truncate());
                    }
                    1 => {
                        let mut target = t.target.as_ref().map(|o| o.name.clone()).unwrap_or_default();
                        if t.created {
                            target = trf("{} (created)", &[&target]);
                        }
                        ui.add(egui::Label::new(target).truncate());
                    }
                    2 => {
                        ui.label(super::group(t.rows_written));
                    }
                    3 => {
                        ui.label(super::group(t.rows_failed));
                    }
                    4 => {
                        ui.label(crate::widgets::format_duration(std::time::Duration::from_millis(t.duration_ms)));
                    }
                    _ => match &t.error {
                        Some(e) => {
                            wizard::tone_text(ui, Tone::Error, e).on_hover_text(e);
                        }
                        None => {
                            let text = if t.indexes_created > 0 { trf("OK, {} index(es)", &[&t.indexes_created]) } else { tr("OK").to_string() };
                            wizard::tone_text(ui, Tone::Success, &text);
                        }
                    },
                }
            },
        );
        if s.foreign_keys_created > 0 {
            wizard::muted(ui, &trf("{} foreign key(s) created", &[&s.foreign_keys_created]));
        }
        for w in &s.warnings {
            ui.label(RichText::new(format!("{} {w}", icon::WARNING)).color(p.warn));
        }
        let failed_rows: Vec<String> = s
            .tables
            .iter()
            .flat_map(|t| {
                let name = t.source.as_ref().map(ObjectRef::display_path).unwrap_or_default();
                t.errors.iter().take(20).map(move |e| trf("{}, row {}: {}", &[&name, &e.row, &e.message]))
            })
            .collect();
        if !failed_rows.is_empty() {
            section(ui, tr("Failed rows"));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for f in failed_rows {
                    ui.add(egui::Label::new(RichText::new(f).monospace().size(11.5).color(p.err)).wrap());
                }
            });
        }
        if kit::button_ex(ui, Kind::Ghost, Some(icon::PENCIL_SIMPLE), tr("Change settings"), false).clicked() {
            self.result = None;
            self.page = PAGE_SOURCE;
        }
    }

    fn footer(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let running = self.run.is_some();
        let reachable = self.reachable(cx);
        let ok = reachable == PAGE_RUN && !running;
        let footer = wizard::Footer {
            back: (self.page > PAGE_SOURCE).then_some(!running),
            next: (self.page < PAGE_RUN).then_some(self.page < reachable && !running),
            run: (tr("Run migration"), ok),
            save: Some((ok && !self.saver.is_saving(), None)),
            error: self.error.as_deref(),
        };
        match wizard::footer(ui, &footer) {
            Some(Clicked::Back) => self.page -= 1,
            Some(Clicked::Next) => self.page += 1,
            Some(Clicked::Run) => {
                if self.destructive() {
                    self.confirm = true;
                } else {
                    self.start(cx);
                }
            }
            Some(Clicked::Save) => {
                let target = self.target_conn.as_ref().map(|t| connection_name(cx, t)).unwrap_or_default();
                self.saver.open(trf("Migrate to {}", &[&target]));
            }
            None => {}
        }
    }

    fn confirm_ui(&mut self, ctx: &egui::Context, cx: &mut AppContext) {
        if !self.confirm {
            return;
        }
        let target = self.target_conn.as_ref().map(|t| connection_name(cx, t)).unwrap_or_default();
        let text = if self.options.mode == MigrationMode::DropAndCreate {
            trf("Existing target tables on \"{}\" are dropped and created again.", &[&target])
        } else {
            trf("All rows of the existing target tables on \"{}\" are deleted first.", &[&target])
        };
        let (mut run, mut cancel) = (false, false);
        let (resp, _) = parts::Dialog::new(tr("Replace the target data?")).icon(icon::WARNING).width(480.0).show_modal(
            ctx,
            egui::Id::new(("mig_confirm", self.uid)),
            |ui| {
                ui.add(egui::Label::new(RichText::new(&text).size(13.5)).wrap());
            },
            |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if wizard::danger_solid(ui, icon::PLAY, tr("Run")).clicked() {
                        run = true;
                    }
                    if kit::button_ex(ui, Kind::Ghost, None, tr("Cancel"), false).clicked() {
                        cancel = true;
                    }
                });
            },
        );
        if run {
            self.start(cx);
        }
        if run || cancel || resp.close {
            self.confirm = false;
        }
    }
}

fn describe(s: &MigrationSummary) -> String {
    let failed = s.tables.iter().filter(|t| t.error.is_some()).count();
    if failed > 0 {
        trf("{} rows copied, {} of {} table(s) failed", &[&super::group(s.rows), &failed, &s.tables.len()])
    } else {
        trf("{} rows copied into {} table(s)", &[&super::group(s.rows), &s.tables.len()])
    }
}

impl TabView for MigrationTab {
    fn take_done(&mut self) -> bool {
        std::mem::take(&mut self.done)
    }

    fn popup_size(&self) -> Option<egui::Vec2> {
        Some(wizard::WINDOW)
    }

    fn title(&self) -> String {
        let n = self.tables.iter().filter(|t| t.enabled).count();
        match self.tables.iter().filter(|t| t.enabled).collect::<Vec<_>>().as_slice() {
            [one] => trf("Migrate {}", &[&one.source.name]),
            _ if n == 0 => tr("Migrate tables").to_string(),
            _ => trf("Migrate {} tables", &[&n]),
        }
    }

    fn icon(&self) -> &'static str {
        icon::ARROWS_DOWN_UP
    }

    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        self.poll(cx);
        let uid = self.uid;
        wizard::top_panel(ui, egui::Id::new(("mig_top", uid)), |ui| {
            // In a pop-up its title bar names the wizard and the connection.
            if !cx.popup {
                ui.horizontal(|ui| {
                    crate::widgets::icon_heading(ui, icon::ARROWS_DOWN_UP, tr("Migrate tables"));
                    ui.weak(connection_name(cx, &self.conn));
                });
            }
            let steps = [tr("Source"), tr("Target"), tr("Mapping"), tr("Options"), tr("Run")];
            let reachable = if self.run.is_some() { self.page } else { self.reachable(cx) };
            let finished = self.page == PAGE_RUN && matches!(self.result, Some(Ok(_)));
            if let Some(p) = wizard::steps(ui, &steps, self.page, reachable.max(self.page), finished)
                && self.run.is_none()
            {
                self.page = p;
            }
        });
        wizard::bottom_panel(ui, egui::Id::new(("mig_bottom", uid)), |ui| self.footer(ui, cx));
        wizard::body(ui, egui::Id::new(("mig_body", uid)), |ui| match self.page {
            PAGE_SOURCE => self.source_page(ui, cx),
            PAGE_TARGET => self.target_page(ui, cx),
            PAGE_MAPPING => self.mapping_page(ui, cx),
            PAGE_OPTIONS => self.options_page(ui, self.target_conn.as_deref().is_none_or(|c| super::has_transactions(cx, c))),
            _ => self.run_page(ui, cx),
        });
        let ctx = ui.ctx().clone();
        self.confirm_ui(&ctx, cx);
        if let Some((name, folder)) = self.saver.ui(&ctx, egui::Id::new(("mig_save", uid))) {
            self.save_task(cx, &name, folder);
        }
    }

    fn on_close(&mut self, cx: &mut AppContext) -> bool {
        if let Some(run) = &mut self.run {
            run.abandon(cx);
        }
        true
    }

    crate::impl_tab_any!();
}

/// Key of a source table in the job's name maps: `schema.table` (`table` without a schema).
pub fn table_key(o: &ObjectRef) -> String {
    match o.schema.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => format!("{s}.{}", o.name),
        None => o.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_from_entries() {
        let mut tab = MigrationTab::new("src".into(), vec![ObjectRef::table(Some("public".into()), "users"), ObjectRef::table(Some("public".into()), "orders")]);
        tab.set_target("dst".into(), Some("archive".into()));
        tab.set_target_name("users", "app_users");
        tab.set_column_type("users", "id", "BIGINT");
        tab.set_column_type("orders", "total", " ");
        let job = tab.job();
        assert_eq!(job.tables.len(), 2);
        assert_eq!(job.target_schema.as_deref(), Some("archive"));
        assert_eq!(job.table_names.get("users").map(String::as_str), Some("app_users"));
        assert!(!job.table_names.contains_key("orders"));
        assert_eq!(job.table_names.get("public.users").map(String::as_str), Some("app_users"));
        assert!(!job.table_names.contains_key("orders"));
        assert_eq!(job.column_types.get("users.id").map(String::as_str), Some("BIGINT"));
        assert_eq!(job.column_types.get("public.users.id").map(String::as_str), Some("BIGINT"));
        assert_eq!(job.column_types.len(), 2);
        assert_eq!(apply_case(NameCase::Upper, "ab"), "AB");
    }

    #[test]
    fn same_table_names_in_two_schemas_keep_their_own_mapping() {
        let mut tab = MigrationTab::new("src".into(), vec![ObjectRef::table(Some("sales".into()), "orders"), ObjectRef::table(Some("archive".into()), "orders")]);
        tab.tables[1].target_name = "old_orders".into();
        tab.tables[1].column_names.insert("id".into(), "order_id".into());
        tab.tables[1].column_types.insert("id".into(), "BIGINT".into());
        tab.tables[0].column_types.insert("id".into(), "INT".into());
        let job = tab.job();
        // Only the archive table is renamed; no unqualified key could tell them apart.
        assert_eq!(job.table_names, BTreeMap::from([("archive.orders".to_string(), "old_orders".to_string())]));
        assert_eq!(job.column_names.keys().collect::<Vec<_>>(), vec!["archive.orders"]);
        assert_eq!(job.column_types.get("archive.orders.id").map(String::as_str), Some("BIGINT"));
        assert_eq!(job.column_types.get("sales.orders.id").map(String::as_str), Some("INT"));
        assert!(!job.column_types.contains_key("orders.id"));
    }
}
