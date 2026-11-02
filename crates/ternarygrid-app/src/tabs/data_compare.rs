//! Data compare (FR-M3): the rows of two tables (same or different
//! connections) matched by key columns, the differing rows with per-column
//! highlights, and the INSERT / UPDATE / DELETE script that makes the target
//! equal to the source, generated in the target dialect, opened in a SQL
//! editor or applied in one transaction.
//!
//! Both sides read through their own dedicated session: the merge join keeps
//! two cursors open at the same time, and a long comparison must not block
//! the navigator.
//!
//! Also home of the connection / schema / table pickers and the dedicated
//! session helper shared by the tool tabs.

use crate::backend::{Job, Loadable};
use crate::command::AppCommand;
use crate::conn::session::SessionHandle;
use crate::cx::AppContext;
use crate::sqllang::SqlLanguage;
use crate::tabs::TabView;
use crate::tabs::dbtools::look::{self, Cell};
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use parking_lot::Mutex;
use std::collections::BTreeSet;
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};
use ternarygrid_compare::{CompareStrategy, DataCompareJob, DataCompareOptions, DataCompareProgress, DataDiff, RowDiff, RowDiffKind, TableSpec};
use ternarygrid_core::ddl::TableDef;
use ternarygrid_core::meta::{ObjectKind, ObjectRef, SchemaRef};
use ternarygrid_core::{DbResult, ErrorKind, Session, Value, tr, trf};
use ternarygrid_ui::kit;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------- pickers

/// Selector over all configured connections; choosing a closed connection
/// connects it. `none` adds an entry for "no connection". Returns true when
/// the selection changed.
pub(crate) fn connection_combo(ui: &mut Ui, cx: &mut AppContext, id: impl Hash + std::fmt::Debug, conn: &mut Option<String>, none: Option<&str>) -> bool {
    connection_combo_ex(ui, cx, id, conn, none, 190.0, true)
}

/// [`connection_combo`] `width` wide; `glyph` puts the plug icon before the name.
pub(crate) fn connection_combo_ex(
    ui: &mut Ui,
    cx: &mut AppContext,
    id: impl Hash + std::fmt::Debug,
    conn: &mut Option<String>,
    none: Option<&str>,
    width: f32,
    glyph: bool,
) -> bool {
    let names: Vec<(String, String, bool)> = cx.conns.configs().iter().map(|c| (c.id.clone(), c.name.clone(), cx.conns.is_connected(&c.id))).collect();
    let current = conn
        .as_ref()
        .and_then(|c| names.iter().find(|(i, _, _)| i == c))
        .map(|(_, n, _)| n.clone())
        .or_else(|| if conn.is_none() { none.map(str::to_string) } else { None })
        .unwrap_or_else(|| tr("Choose a connection…").to_string());
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).icon(ternarygrid_ui::kit::combo_caret).selected_text(if glyph { format!("{} {current}", icon::PLUGS) } else { current }).width(width).show_ui(
        ui,
        |ui| {
            if let Some(n) = none
                && ui.selectable_label(conn.is_none(), n).clicked()
                && conn.is_some()
            {
                *conn = None;
                changed = true;
            }
            for (cid, name, connected) in &names {
                let st = if *connected { icon::PLUGS_CONNECTED } else { icon::PLUGS };
                if ui.selectable_label(conn.as_deref() == Some(cid), format!("{st} {name}")).clicked() && conn.as_deref() != Some(cid) {
                    *conn = Some(cid.clone());
                    changed = true;
                }
            }
        },
    );
    if changed
        && let Some(c) = conn.as_ref()
        && !cx.conns.is_connected(c)
    {
        cx.push(AppCommand::Connect(c.clone()));
    }
    changed
}

/// The schema a connection works in by default.
pub(crate) fn default_schema(cx: &AppContext, conn: &str) -> Option<SchemaRef> {
    let live = cx.conns.live(conn)?;
    let name = live.current_schema.clone().or_else(|| live.current_database.clone())?;
    Some(SchemaRef::new(None, Some(name)))
}

/// Schema selector of a connected connection (non-system schemas, or the
/// current schema when the database has none). Selects the current schema
/// when nothing is chosen. Returns true when the selection changed.
pub(crate) fn schema_combo(ui: &mut Ui, cx: &mut AppContext, id: impl Hash + std::fmt::Debug, conn: &str, schema: &mut Option<SchemaRef>) -> bool {
    schema_combo_ex(ui, cx, id, conn, schema, 150.0, true)
}

/// [`schema_combo`] `width` wide; `glyph` puts the schema icon before the name.
pub(crate) fn schema_combo_ex(ui: &mut Ui, cx: &mut AppContext, id: impl Hash + std::fmt::Debug, conn: &str, schema: &mut Option<SchemaRef>, width: f32, glyph: bool) -> bool {
    let backend = cx.backend;
    let fallback = default_schema(cx, conn);
    let Some(live) = cx.conns.live_mut(conn) else {
        ui.weak(tr("Not connected"));
        return false;
    };
    let main = live.main.clone();
    live.meta.request_schemas(backend, &main, None);
    let loading = live.meta.schemas_of(None).is_loading();
    let mut names: Vec<String> = live.meta.schemas_of(None).get().map(|v| v.iter().filter(|s| !s.is_system).map(|s| s.name.clone()).collect()).unwrap_or_default();
    if let Some(f) = fallback.as_ref().and_then(|f| f.schema.clone())
        && !names.contains(&f)
    {
        names.insert(0, f);
    }
    let mut changed = false;
    if schema.is_none()
        && let Some(f) = fallback.clone().or_else(|| names.first().map(|n| SchemaRef::new(None, Some(n.clone()))))
    {
        *schema = Some(f);
        changed = true;
    }
    let label = schema.as_ref().and_then(|s| s.schema.clone()).unwrap_or_else(|| tr("Schema…").to_string());
    egui::ComboBox::from_id_salt(id).icon(ternarygrid_ui::kit::combo_caret).selected_text(if glyph { format!("{} {label}", icon::STACK) } else { label }).width(width).show_ui(
        ui,
        |ui| {
            for n in &names {
                let on = schema.as_ref().and_then(|s| s.schema.as_deref()) == Some(n.as_str());
                if ui.selectable_label(on, n).clicked() && !on {
                    *schema = Some(SchemaRef::new(None, Some(n.clone())));
                    changed = true;
                }
            }
        },
    );
    if loading {
        ui.spinner();
    }
    changed
}

/// Tables (and views when `views`) of a schema, from the metadata cache.
pub(crate) fn schema_tables(cx: &mut AppContext, conn: &str, schema: &SchemaRef, views: bool) -> (Vec<ObjectRef>, bool) {
    let backend = cx.backend;
    let Some(live) = cx.conns.live_mut(conn) else { return (Vec::new(), false) };
    let Some(session) = live.ensure_session(backend, schema.database.as_deref()) else { return (Vec::new(), true) };
    let kinds: &[ObjectKind] = if views { &[ObjectKind::Table, ObjectKind::View] } else { &[ObjectKind::Table] };
    let mut out = Vec::new();
    let mut loading = false;
    for k in kinds {
        live.meta.request_objects(backend, &session, schema, *k);
        match live.meta.objects.get(&(schema.clone(), *k)) {
            Some(Loadable::Loaded(v)) => out.extend(v.iter().map(|o| o.obj.clone())),
            Some(Loadable::Loading(_)) | Some(Loadable::NotLoaded) | None => loading = true,
            Some(Loadable::Failed(_)) => {}
        }
    }
    (out, loading)
}

/// Table / view selector of a schema, `width` wide (`glyph`: the table icon before the
/// name). Returns true when the selection changed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn table_combo_ex(
    ui: &mut Ui,
    cx: &mut AppContext,
    id: impl Hash + std::fmt::Debug,
    conn: &str,
    schema: &SchemaRef,
    table: &mut Option<ObjectRef>,
    width: f32,
    glyph: bool,
) -> bool {
    let (items, loading) = schema_tables(cx, conn, schema, true);
    let label = table.as_ref().map(|t| t.name.clone()).unwrap_or_else(|| tr("Table…").to_string());
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).icon(ternarygrid_ui::kit::combo_caret).selected_text(if glyph { format!("{} {label}", icon::TABLE) } else { label }).width(width).show_ui(
        ui,
        |ui| {
            if items.is_empty() {
                ui.weak(if loading { tr("Loading…") } else { tr("No tables") });
            }
            for o in &items {
                let on = table.as_ref().is_some_and(|t| t.name == o.name && t.schema == o.schema);
                if ui.selectable_label(on, format!("{} {}", crate::nav::kind_icon(o.kind), o.name)).clicked() && !on {
                    *table = Some(o.clone());
                    changed = true;
                }
            }
        },
    );
    if loading {
        ui.spinner();
    }
    changed
}

/// Definition of `table` from the metadata cache (requested when missing);
/// `Err` with the error text when it cannot be read.
pub(crate) fn cached_table_def(cx: &mut AppContext, conn: &str, table: &ObjectRef) -> Result<Option<Arc<TableDef>>, String> {
    let backend = cx.backend;
    let Some(live) = cx.conns.live_mut(conn) else { return Ok(None) };
    let Some(session) = live.ensure_session(backend, table.database.as_deref()) else { return Ok(None) };
    live.meta.request_table_def(backend, &session, table);
    match live.meta.table_defs.get(table) {
        Some(Loadable::Loaded(t)) => Ok(Some(t.clone())),
        Some(Loadable::Failed(e)) => Err(e.full_text()),
        _ => Ok(None),
    }
}

// ------------------------------------------------------ dedicated sessions

/// State of a [`DedicatedSession`].
pub(crate) enum SessionState {
    Ready(SessionHandle),
    Opening,
    Failed(String),
    NotConnected,
}

/// A session of its own for a tool tab, opened on demand for a connection
/// (and database); switching the connection closes the previous session.
#[derive(Default)]
pub(crate) struct DedicatedSession {
    key: Option<(String, Option<String>)>,
    slot: Loadable<SessionHandle>,
    runtime: Option<tokio::runtime::Handle>,
}

impl DedicatedSession {
    /// Poll (and open when needed) the session for `conn` / `database`.
    pub(crate) fn poll(&mut self, cx: &AppContext, conn: &str, database: Option<&str>, label: &str) -> SessionState {
        let key = (conn.to_string(), database.map(str::to_string));
        if self.key.as_ref() != Some(&key) {
            self.close();
            self.key = Some(key);
        }
        self.slot.poll();
        if let Loadable::Loaded(s) = &self.slot {
            if !s.is_closed() {
                return SessionState::Ready(s.clone());
            }
            self.slot = Loadable::NotLoaded;
        }
        let Some(live) = cx.conns.live(conn) else { return SessionState::NotConnected };
        match &self.slot {
            Loadable::NotLoaded => {
                self.runtime = Some(cx.backend.handle());
                self.slot = Loadable::Loading(live.open_session_job(cx.backend, format!("{} ({label})", live.config.name), database.map(str::to_string)));
                SessionState::Opening
            }
            Loadable::Failed(e) => SessionState::Failed(e.full_text()),
            _ => SessionState::Opening,
        }
    }

    /// The open session, if any (no polling).
    pub(crate) fn current(&self) -> Option<SessionHandle> {
        self.slot.get().cloned()
    }

    /// Forget a failed attempt so the next poll tries again.
    pub(crate) fn retry(&mut self) {
        if matches!(self.slot, Loadable::Failed(_)) {
            self.slot = Loadable::NotLoaded;
        }
    }

    pub(crate) fn close(&mut self) {
        if let (Loadable::Loaded(s), Some(rt)) = (std::mem::take(&mut self.slot), &self.runtime) {
            rt.spawn(async move { s.close().await });
        }
    }
}

impl Drop for DedicatedSession {
    fn drop(&mut self) {
        self.close();
    }
}

// ------------------------------------------------------------ data compare

#[derive(Clone, Default, PartialEq, Debug)]
struct Side {
    conn: Option<String>,
    schema: Option<SchemaRef>,
    table: Option<ObjectRef>,
    /// Optional filter (SQL expression without WHERE).
    filter: String,
}

/// Columns of the compared rows: both queries list the key columns first,
/// then the compared columns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompareLayout {
    pub keys: Vec<String>,
    pub columns: Vec<String>,
    pub target_keys: Vec<String>,
    pub target_columns: Vec<String>,
    /// Selected source columns the target table does not have.
    pub missing_in_target: Vec<String>,
}

fn find_column<'a>(t: &'a TableDef, name: &str) -> Option<&'a str> {
    t.columns.iter().find(|c| c.name == name).or_else(|| t.columns.iter().find(|c| c.name.eq_ignore_ascii_case(name))).map(|c| c.name.as_str())
}

/// The comparison of `source` with `target`: rows matched by `keys` (source
/// column names, matched case-insensitively in the target), the other source
/// columns except `excluded` compared with the target columns of the same
/// name. Both specs list their columns explicitly so the rows of the result
/// follow [`CompareLayout`].
#[allow(clippy::too_many_arguments)]
pub fn build_compare_job(
    src_obj: &ObjectRef,
    src: &TableDef,
    dst_obj: &ObjectRef,
    dst: &TableDef,
    keys: &[String],
    excluded: &BTreeSet<String>,
    filters: (&str, &str),
    options: DataCompareOptions,
) -> Result<(DataCompareJob, CompareLayout), String> {
    // keys in table order
    let keys: Vec<String> = src.columns.iter().filter(|c| keys.contains(&c.name)).map(|c| c.name.clone()).collect();
    if keys.is_empty() {
        return Err(tr("Choose at least one key column.").to_string());
    }
    let mut target_keys = Vec::new();
    for k in &keys {
        match find_column(dst, k) {
            Some(t) => target_keys.push(t.to_string()),
            None => return Err(trf("The target table has no key column {}.", &[k])),
        }
    }
    let mut layout = CompareLayout { keys: keys.clone(), target_keys: target_keys.clone(), ..Default::default() };
    for c in &src.columns {
        if keys.contains(&c.name) || excluded.contains(&c.name) {
            continue;
        }
        match find_column(dst, &c.name) {
            Some(t) if !target_keys.iter().any(|k| k == t) => {
                layout.columns.push(c.name.clone());
                layout.target_columns.push(t.to_string());
            }
            _ => layout.missing_in_target.push(c.name.clone()),
        }
    }
    // Only keys: compare the first key with itself (rows present on one side only).
    let (src_cols, dst_cols) =
        if layout.columns.is_empty() { (vec![keys[0].clone()], vec![target_keys[0].clone()]) } else { (layout.columns.clone(), layout.target_columns.clone()) };
    let filter = |f: &str| Some(f.trim().to_string()).filter(|f| !f.is_empty());
    let job = DataCompareJob {
        source: TableSpec { obj: src_obj.clone(), key_columns: keys, columns: src_cols, where_clause: filter(filters.0) },
        target: TableSpec { obj: dst_obj.clone(), key_columns: target_keys, columns: dst_cols, where_clause: filter(filters.1) },
        options,
    };
    Ok((job, layout))
}

/// The sync statements as one script.
pub fn sync_script_text(statements: &[String]) -> String {
    let mut s = String::with_capacity(statements.iter().map(|x| x.len() + 2).sum());
    for st in statements {
        s.push_str(st);
        s.push_str(";\n");
    }
    s
}

struct Running {
    job: Job<DbResult<DataDiff>>,
    progress: Arc<Mutex<DataCompareProgress>>,
    cancel: CancellationToken,
    sessions: (SessionHandle, SessionHandle),
    layout: CompareLayout,
    started: Instant,
}

struct Outcome {
    diff: DataDiff,
    layout: CompareLayout,
    elapsed: Duration,
    target_conn: String,
    target_schema: Option<SchemaRef>,
    target_name: String,
    script: String,
    lang: SqlLanguage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultView {
    Rows,
    Script,
}

pub struct DataCompareTab {
    uid: u64,
    source: Side,
    target: Side,
    /// Source table the key / column choice was made for.
    chosen_for: Option<ObjectRef>,
    keys: Vec<String>,
    /// Source columns left out of the comparison.
    excluded: BTreeSet<String>,
    options: DataCompareOptions,
    src_session: DedicatedSession,
    dst_session: DedicatedSession,
    start_requested: bool,
    run: Option<Running>,
    result: Option<Outcome>,
    error: Option<String>,
    view: ResultView,
    filter: Option<RowDiffKind>,
    confirm_apply: bool,
    apply_job: Option<Job<DbResult<u64>>>,
    /// The "Keys and columns" card is open (`None`: open while no key is chosen).
    columns_open: Option<bool>,
    /// The "Options" card is open.
    options_open: bool,
}

fn kind_label(k: RowDiffKind) -> &'static str {
    match k {
        RowDiffKind::Different => tr("Different"),
        RowDiffKind::OnlyInSource => tr("Only in source"),
        RowDiffKind::OnlyInTarget => tr("Only in target"),
    }
}

fn kind_color(k: RowDiffKind, p: &ternarygrid_ui::style::Palette) -> egui::Color32 {
    match k {
        RowDiffKind::Different => p.warn,
        RowDiffKind::OnlyInSource => p.ok,
        RowDiffKind::OnlyInTarget => p.err,
    }
}

fn strategy_label(s: CompareStrategy) -> &'static str {
    match s {
        CompareStrategy::Auto => tr("Automatic"),
        CompareStrategy::MergeJoin => tr("Merge join (sorted keys)"),
        CompareStrategy::HashJoin => tr("Hash join (in memory)"),
    }
}

impl DataCompareTab {
    pub fn new(conn: Option<String>, table: Option<ObjectRef>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let source = Side { conn: conn.clone(), schema: table.as_ref().map(|t| t.schema_ref()), table: table.clone(), filter: String::new() };
        let target = Side { conn, ..Default::default() };
        DataCompareTab {
            uid: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            source,
            target,
            chosen_for: None,
            keys: Vec::new(),
            excluded: BTreeSet::new(),
            options: DataCompareOptions { max_samples: 1000, ..Default::default() },
            src_session: DedicatedSession::default(),
            dst_session: DedicatedSession::default(),
            start_requested: false,
            run: None,
            result: None,
            error: None,
            view: ResultView::Rows,
            filter: None,
            confirm_apply: false,
            apply_job: None,
            columns_open: None,
            options_open: false,
        }
    }

    fn def_of(cx: &mut AppContext, side: &Side) -> Result<Option<Arc<TableDef>>, String> {
        match (&side.conn, &side.table) {
            (Some(c), Some(t)) => cached_table_def(cx, c, t),
            _ => Ok(None),
        }
    }

    /// Default keys (primary key, else the best unique key) and all columns.
    fn init_choice(&mut self, src: &TableDef) {
        if self.chosen_for.as_ref() == self.source.table.as_ref() {
            return;
        }
        self.chosen_for = self.source.table.clone();
        self.keys = src.row_key().map(|k| k.columns.clone()).unwrap_or_default();
        self.excluded.clear();
    }

    fn side_ui(&mut self, ui: &mut Ui, cx: &mut AppContext, target: bool) {
        let uid = self.uid;
        let side = if target { &mut self.target } else { &mut self.source };
        let before = side.clone();
        let tag = if target { "dst" } else { "src" };
        ui.spacing_mut().item_spacing.x = 6.0;
        if let Some(kind) = side.conn.as_ref().and_then(|c| cx.conns.config(c)).map(|c| c.kind) {
            look::logo(ui, kind, 15.0);
        }
        let mut conn = side.conn.clone();
        if connection_combo_ex(ui, cx, ("dc_conn", uid, tag), &mut conn, None, 170.0, false) {
            side.conn = conn;
            side.schema = None;
            side.table = None;
        }
        if let Some(c) = side.conn.clone() {
            if cx.conns.is_connected(&c) {
                let mut schema = side.schema.clone();
                if schema_combo_ex(ui, cx, ("dc_schema", uid, tag), &c, &mut schema, 130.0, false) {
                    side.schema = schema;
                    side.table = None;
                }
                if let Some(s) = side.schema.clone() {
                    let mut table = side.table.clone();
                    ui.add_space(6.0);
                    if table_combo_ex(ui, cx, ("dc_table", uid, tag), &c, &s, &mut table, 160.0, false) {
                        side.table = table;
                    }
                }
            } else if cx.conns.is_connecting() {
                ui.spinner();
            } else if kit::button_ex(ui, kit::Kind::Secondary, Some(icon::PLUGS_CONNECTED), tr("Connect"), true).clicked() {
                cx.push(AppCommand::Connect(c));
            }
        }
        ui.add_space(6.0);
        let p = ternarygrid_ui::pal(ui);
        let filter = egui::TextEdit::singleline(&mut side.filter)
            .hint_text(RichText::new(tr("WHERE filter (optional)")).color(p.text_faint))
            .font(look::mono(12.5))
            .margin(egui::Margin::symmetric(8, 5))
            .desired_width(ui.available_width().max(160.0))
            .id_salt(("dc_filter", uid, tag));
        ui.add(filter);
        if *side != before && before.table != side.table {
            self.result = None;
            self.error = None;
        }
    }

    /// Pick the source table's name in the target schema when nothing is chosen.
    fn auto_target_table(&mut self, cx: &mut AppContext) {
        let (Some(src), Some(conn), Some(schema), None) = (self.source.table.clone(), self.target.conn.clone(), self.target.schema.clone(), self.target.table.as_ref()) else {
            return;
        };
        if !cx.conns.is_connected(&conn) || (self.source.conn.as_deref() == Some(conn.as_str()) && Some(&schema) == self.source.schema.as_ref()) {
            return;
        }
        let (items, _) = schema_tables(cx, &conn, &schema, true);
        if let Some(t) = items.iter().find(|t| t.name == src.name).or_else(|| items.iter().find(|t| t.name.eq_ignore_ascii_case(&src.name))) {
            self.target.table = Some(t.clone());
        }
    }

    fn columns_ui(&mut self, ui: &mut Ui, src: &TableDef, dst: Option<&TableDef>) {
        let title = trf(
            "Keys and columns ({} key, {} compared)",
            &[&self.keys.len(), &src.columns.iter().filter(|c| !self.keys.contains(&c.name) && !self.excluded.contains(&c.name)).count()],
        );
        let mut open = self.columns_open.unwrap_or(self.keys.is_empty());
        let p = ternarygrid_ui::pal(ui);
        look::collapsible_card(ui, &mut open, &title, None, |ui| {
            ui.horizontal(|ui| {
                if kit::ghost(ui, Some(icon::KEY), tr("Primary key")).clicked() {
                    self.keys = src.row_key().map(|k| k.columns.clone()).unwrap_or_default();
                }
                if kit::ghost(ui, Some(icon::CHECK_SQUARE), tr("Compare all")).clicked() {
                    self.excluded.clear();
                }
                if kit::ghost(ui, Some(icon::SQUARE), tr("Compare none")).clicked() {
                    self.excluded = src.columns.iter().map(|c| c.name.clone()).collect();
                }
            });
            egui::ScrollArea::vertical().id_salt(("dc_cols_scroll", self.uid)).max_height(220.0).show(ui, |ui| {
                egui::Grid::new(("dc_cols_grid", self.uid)).num_columns(5).striped(true).spacing([18.0, 4.0]).show(ui, |ui| {
                    for h in [tr("Column"), tr("Type"), tr("Key"), tr("Compare"), tr("In target")] {
                        ui.label(RichText::new(h).font(ternarygrid_ui::style::semibold(ui.ctx(), 12.0)).color(p.text_muted));
                    }
                    ui.end_row();
                    for c in &src.columns {
                        ui.label(&c.name);
                        ui.label(RichText::new(&c.data_type).font(look::mono(11.5)).color(p.text_faint));
                        let mut key = self.keys.contains(&c.name);
                        if look::checkbox(ui, &mut key, "").on_hover_text(tr("Key")).changed() {
                            if key {
                                self.keys.push(c.name.clone());
                            } else {
                                self.keys.retain(|k| k != &c.name);
                            }
                        }
                        let mut cmp = !self.excluded.contains(&c.name);
                        ui.add_enabled_ui(!key, |ui| {
                            if look::checkbox(ui, &mut cmp, "").on_hover_text(tr("Compare")).changed() {
                                if cmp {
                                    self.excluded.remove(&c.name);
                                } else {
                                    self.excluded.insert(c.name.clone());
                                }
                            }
                        });
                        match dst.map(|d| find_column(d, &c.name)) {
                            Some(Some(t)) if t == c.name => {
                                ui.label(RichText::new(icon::CHECK).color(p.ok));
                            }
                            Some(Some(t)) => {
                                ui.label(RichText::new(t).color(p.text_muted));
                            }
                            Some(None) => {
                                ui.label(RichText::new(tr("missing")).color(p.warn));
                            }
                            None => {
                                ui.label(RichText::new("—").color(p.text_faint));
                            }
                        }
                        ui.end_row();
                    }
                });
            });
        });
        self.columns_open = Some(open);
    }

    /// The options in one line (the closed card's summary).
    fn options_summary(&self) -> String {
        let o = &self.options;
        let mut parts: Vec<String> = Vec::new();
        if o.ignore_case {
            parts.push(tr("ignore case").to_string());
        }
        if o.trim_text {
            parts.push(tr("ignore trailing spaces").to_string());
        }
        if o.numeric_tolerance > 0.0 {
            parts.push(trf("tolerance {}", &[&o.numeric_tolerance]));
        }
        parts.push(
            match o.strategy {
                CompareStrategy::Auto => tr("automatic matching"),
                CompareStrategy::MergeJoin => tr("merge join"),
                CompareStrategy::HashJoin => tr("hash join"),
            }
            .to_string(),
        );
        if !o.generate_sync {
            parts.push(tr("no sync script").to_string());
        }
        parts.join(" · ")
    }

    fn options_ui(&mut self, ui: &mut Ui) {
        let summary = self.options_summary();
        let mut open = self.options_open;
        let uid = self.uid;
        let o = &mut self.options;
        look::collapsible_card(ui, &mut open, tr("Options"), Some(&summary), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                look::checkbox(ui, &mut o.ignore_case, tr("Ignore case"));
                look::checkbox(ui, &mut o.trim_text, tr("Ignore trailing spaces"));
                ui.horizontal(|ui| {
                    look::muted(ui, tr("Numeric tolerance"));
                    ui.add(egui::DragValue::new(&mut o.numeric_tolerance).range(0.0..=1e12).speed(0.001));
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                ui.horizontal(|ui| {
                    look::muted(ui, tr("Row matching"));
                    egui::ComboBox::from_id_salt(("dc_strategy", uid)).icon(kit::combo_caret).selected_text(strategy_label(o.strategy)).show_ui(ui, |ui| {
                        for s in [CompareStrategy::Auto, CompareStrategy::MergeJoin, CompareStrategy::HashJoin] {
                            ui.selectable_value(&mut o.strategy, s, strategy_label(s));
                        }
                    });
                });
                ui.horizontal(|ui| {
                    look::muted(ui, tr("Differing rows to show"));
                    ui.add(egui::DragValue::new(&mut o.max_samples).range(1..=1_000_000));
                });
                look::checkbox(ui, &mut o.generate_sync, tr("Generate the sync script"));
            });
        });
        self.options_open = open;
    }

    fn try_start(&mut self, cx: &mut AppContext, src: Option<&Arc<TableDef>>, dst: Option<&Arc<TableDef>>) {
        let (Some(sc), Some(st), Some(dc), Some(dt)) = (self.source.conn.clone(), self.source.table.clone(), self.target.conn.clone(), self.target.table.clone()) else {
            self.error = Some(tr("Choose the source and the target table.").to_string());
            self.start_requested = false;
            return;
        };
        if sc == dc && st == dt && self.source.filter.trim() == self.target.filter.trim() {
            self.error = Some(tr("The source and the target are the same table.").to_string());
            self.start_requested = false;
            return;
        }
        let (Some(src), Some(dst)) = (src, dst) else { return };
        let a = self.src_session.poll(cx, &sc, st.database.as_deref(), "data compare");
        let b = self.dst_session.poll(cx, &dc, dt.database.as_deref(), "data compare");
        let (a, b) = match (a, b) {
            (SessionState::Ready(a), SessionState::Ready(b)) => (a, b),
            (SessionState::Failed(e), _) | (_, SessionState::Failed(e)) => {
                self.error = Some(e);
                self.start_requested = false;
                self.src_session.retry();
                self.dst_session.retry();
                return;
            }
            (SessionState::NotConnected, _) | (_, SessionState::NotConnected) => {
                self.error = Some(tr("Connect both connections first.").to_string());
                self.start_requested = false;
                return;
            }
            _ => return,
        };
        self.start_requested = false;
        let (job, layout) = match build_compare_job(&st, src, &dt, dst, &self.keys, &self.excluded, (&self.source.filter, &self.target.filter), self.options.clone()) {
            Ok(j) => j,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        self.error = None;
        self.result = None;
        let progress = Arc::new(Mutex::new(DataCompareProgress::default()));
        let cancel = CancellationToken::new();
        let (p, token) = (progress.clone(), cancel.clone());
        let repaint = cx.backend.repainter();
        let (s1, s2) = (a.clone(), b.clone());
        let handle = cx.backend.spawn(async move {
            let (sd, dd) = (s1.dialect(), s2.dialect());
            let mut ga = s1.lock().await;
            let mut gb = s2.lock().await;
            let mut sp: &mut dyn Session = ga.as_mut();
            let mut tp: &mut dyn Session = gb.as_mut();
            let mut on_progress = move |pr: &DataCompareProgress| {
                *p.lock() = *pr;
                repaint.request();
            };
            ternarygrid_compare::run_data_compare(&job, &mut sp, &mut tp, sd.as_ref(), dd.as_ref(), &mut on_progress, &token).await
        });
        self.run = Some(Running { job: handle, progress, cancel, sessions: (a, b), layout, started: Instant::now() });
    }

    fn stop(&mut self, cx: &AppContext) {
        if let Some(r) = &self.run {
            r.cancel.cancel();
            let (a, b) = r.sessions.clone();
            cx.backend.spawn_detached(async move {
                let _ = a.cancel().await;
                let _ = b.cancel().await;
            });
        }
    }

    fn poll(&mut self, cx: &mut AppContext) {
        if let Some(r) = self.run.as_mut()
            && let Some(res) = r.job.poll()
        {
            let r = self.run.take().expect("running");
            match res {
                Ok(diff) => {
                    let dialect = r.sessions.1.dialect();
                    let script = sync_script_text(&diff.sync_statements);
                    self.result = Some(Outcome {
                        diff,
                        layout: r.layout,
                        elapsed: r.started.elapsed(),
                        target_conn: self.target.conn.clone().unwrap_or_default(),
                        target_schema: self.target.schema.clone(),
                        target_name: self.target.table.as_ref().map(|t| t.display_path()).unwrap_or_default(),
                        script,
                        lang: SqlLanguage::new(dialect),
                    });
                }
                Err(e) if e.kind == ErrorKind::Cancelled => self.error = Some(tr("The comparison was cancelled.").to_string()),
                Err(e) => self.error = Some(e.full_text()),
            }
        }
        if let Some(j) = self.apply_job.as_mut()
            && let Some(res) = j.poll()
        {
            self.apply_job = None;
            match res {
                Ok(n) => {
                    cx.toasts.success(trf("{} statement(s) applied to the target", &[&n]));
                    if let (Some(c), Some(t)) = (&self.target.conn, &self.target.table) {
                        cx.push(AppCommand::RefreshMetadata { connection: c.clone(), object: Some(t.clone()), schema: None });
                    }
                    // Compare again: the tables should now be equal.
                    self.start_requested = true;
                }
                Err(e) => {
                    self.error = Some(e.full_text());
                    cx.toasts.error(tr("The sync script failed; nothing was changed"), Some(e.full_text()));
                }
            }
        }
    }

    /// Run the sync script on the target in one transaction.
    fn apply(&mut self, cx: &mut AppContext) {
        let Some(o) = &self.result else { return };
        if let Some(cfg) = cx.conns.config(&o.target_conn)
            && cfg.read_only
        {
            self.error = Some(trf("The connection \"{}\" is read-only.", &[&cfg.name]));
            return;
        }
        let Some(session) = self.dst_session.current() else {
            self.error = Some(tr("The target session is closed; compare again first.").to_string());
            return;
        };
        let stmts: Vec<(String, Vec<Value>)> = o.diff.sync_statements.iter().map(|s| (s.clone(), Vec::new())).collect();
        let n = stmts.len() as u64;
        self.apply_job = Some(cx.backend.spawn(async move {
            let (_, committed) = session.apply_changes(&stmts).await?;
            if !committed {
                session.commit().await?;
            }
            Ok(n)
        }));
    }

    fn progress_ui(&mut self, ui: &mut Ui, cx: &AppContext) {
        let Some(r) = &self.run else { return };
        let p = *r.progress.lock();
        let elapsed = r.started.elapsed();
        let mut stop = false;
        let pal = ternarygrid_ui::pal(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ternarygrid_ui::parts::spinner_label(
                ui,
                &trf("{} source rows, {} target rows read; {} differences so far", &[&p.source_rows, &p.target_rows, &(p.different + p.only_in_source + p.only_in_target)]),
            );
            ui.label(RichText::new(crate::widgets::format_duration(elapsed)).color(pal.text_faint));
            if look::small(ui, kit::Kind::Danger, icon::STOP, tr("Stop")).clicked() {
                stop = true;
            }
        });
        if stop {
            self.stop(cx);
        }
    }

    /// Rows read, time and strategy of the last comparison (at the right of the toolbar).
    fn facts(o: &Outcome) -> String {
        let d = &o.diff;
        let strategy = match d.strategy {
            CompareStrategy::Auto => tr("automatic matching"),
            CompareStrategy::MergeJoin => tr("merge join"),
            CompareStrategy::HashJoin => tr("hash join"),
        };
        format!("{} · {strategy}", trf("{} source rows, {} target rows, {}", &[&d.source_rows, &d.target_rows, &crate::widgets::format_duration(o.elapsed)]))
    }

    fn summary_ui(ui: &mut Ui, o: &Outcome) {
        let d = &o.diff;
        let p = ternarygrid_ui::pal(ui);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if d.is_identical() {
                ternarygrid_ui::parts::status(ui, ternarygrid_ui::parts::Tone::Success, tr("The data is identical."));
            }
            ui.label(trf("{} equal", &[&d.identical]));
            look::tone(ui, &trf("{} different", &[&d.different]), p.warn, true);
            look::tone(ui, &trf("{} only in source", &[&d.only_in_source]), p.ok, true);
            look::tone(ui, &trf("{} only in target", &[&d.only_in_target]), p.err, true);
        });
        let mut notes: Vec<String> = d.warnings.clone();
        if d.skipped_null_keys > 0 {
            notes.push(trf("{} rows with a NULL key were skipped", &[&d.skipped_null_keys]));
        }
        if !o.layout.missing_in_target.is_empty() {
            notes.push(trf("Not compared (missing in the target): {}", &[&o.layout.missing_in_target.join(", ")]));
        }
        for n in &notes {
            look::note(ui, ternarygrid_ui::parts::Tone::Warning, None, n);
        }
        let shown = d.samples.len() as u64;
        let total = d.different + d.only_in_source + d.only_in_target;
        if shown < total {
            look::faint(ui, &trf("Showing the first {} of {} differing rows.", &[&shown, &total]));
        }
    }

    fn rows_ui(&mut self, ui: &mut Ui, cx: &AppContext) {
        let Some(o) = &self.result else { return };
        let fmt = cx.settings.results.format.clone();
        let p = ternarygrid_ui::pal(ui);
        let rows: Vec<&RowDiff> = o.diff.samples.iter().filter(|s| self.filter.is_none_or(|f| f == s.kind)).collect();
        if rows.is_empty() {
            look::muted(ui, tr("No differing rows."));
            return;
        }
        let nk = o.layout.keys.len();
        let text_of = |v: Option<&Value>| -> String {
            match v {
                Some(v) => v.display(&fmt).chars().take(200).collect(),
                None => String::new(),
            }
        };
        let is_null = |v: Option<&Value>| v.is_some_and(|v| v.is_null());
        let mut headers = vec![Cell::new(tr("Difference"))];
        headers.extend(o.layout.keys.iter().map(|k| Cell::new(k.clone()).glyph(icon::KEY, p.warn)));
        headers.extend(o.layout.columns.iter().zip(&o.layout.target_columns).map(|(c, t)| Cell::new(if c == t { c.clone() } else { format!("{c} → {t}") })));
        let cells: Vec<Vec<Cell>> = rows
            .iter()
            .map(|s| {
                let mut row = vec![Cell::new(kind_label(s.kind)).color(kind_color(s.kind, &p))];
                row.extend((0..nk).map(|i| Cell::new(text_of(s.key.get(i)))));
                for (j, name) in o.layout.columns.iter().enumerate() {
                    let src = s.source.as_ref().and_then(|r| r.get(nk + j));
                    let tgt = s.target.as_ref().and_then(|r| r.get(nk + j));
                    let value = |v: Option<&Value>| if is_null(v) { Cell::new(text_of(v)).color(p.text_faint) } else { Cell::new(text_of(v)) };
                    row.push(match s.kind {
                        RowDiffKind::Different if s.columns.iter().any(|c| c == name) => {
                            let (a, b) = (text_of(src), text_of(tgt));
                            let hover = format!("{}: {a}\n{}: {b}", tr("Source"), tr("Target"));
                            Cell::new(format!("{a} → {b}")).job(look::change_job(ui, &a, &b)).hover(hover)
                        }
                        RowDiffKind::OnlyInTarget => value(tgt),
                        _ => value(src),
                    });
                }
                row
            })
            .collect();
        look::table(ui, ("dc_rows", self.uid), &headers, &cells, None);
    }

    fn script_ui(&mut self, ui: &mut Ui) {
        let Some(o) = &self.result else { return };
        let p = ternarygrid_ui::pal(ui);
        let n = o.diff.sync_statements.len();
        if o.diff.sync_truncated {
            look::note(ui, ternarygrid_ui::parts::Tone::Warning, None, &trf("The script was cut off after {} statements; compare again after applying it.", &[&n]));
        }
        if !self.options.generate_sync {
            ui.label(RichText::new(tr("Generating the sync script is turned off in the options.")).color(p.text_muted));
        } else if n == 0 {
            ui.label(RichText::new(tr("Nothing to synchronize.")).color(p.text_muted));
            return;
        }
        let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
        egui::Frame::new().fill(p.surface).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(8).show(ui, |ui| {
            ternarygrid_editor::CodeView::new(&o.script, &o.lang, &theme).id_salt(("dc_script", self.uid)).show(ui);
        });
    }

    /// The script's actions, beside the view switch.
    fn script_actions(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let Some(o) = &self.result else { return };
        let n = o.diff.sync_statements.len();
        ui.add_enabled_ui(n > 0, |ui| {
            if look::small(ui, kit::Kind::Secondary, icon::FILE_SQL, tr("Open in SQL editor")).clicked() {
                cx.push(AppCommand::NewSqlEditor { connection: Some(o.target_conn.clone()), schema: o.target_schema.clone(), text: Some(o.script.clone()) });
            }
        });
        let can_apply = n > 0 && self.apply_job.is_none() && self.run.is_none();
        if ui.add_enabled_ui(can_apply, |ui| look::small(ui, kit::Kind::Danger, icon::PLAY, tr("Apply to target…"))).inner.clicked() {
            self.confirm_apply = true;
        }
        if ui.add_enabled_ui(n > 0, |ui| look::small(ui, kit::Kind::Ghost, icon::COPY, tr("Copy"))).inner.clicked() {
            ui.ctx().copy_text(o.script.clone());
        }
        if self.apply_job.is_some() {
            ternarygrid_ui::parts::spinner_label(ui, tr("Applying…"));
        }
        let p = ternarygrid_ui::pal(ui);
        ui.label(RichText::new(trf("{} statement(s)", &[&n])).size(12.0).color(p.text_faint));
    }

    fn apply_modal(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if !self.confirm_apply {
            return;
        }
        let Some(o) = &self.result else {
            self.confirm_apply = false;
            return;
        };
        let target_conn = cx.conns.config(&o.target_conn).map(|c| c.name.clone()).unwrap_or_default();
        let production = cx.conns.type_of(&o.target_conn).is_production();
        let (n, name) = (o.diff.sync_statements.len(), o.target_name.clone());
        let question = trf("Run {} statement(s) on {} (\"{}\") in one transaction?", &[&n, &name, &target_conn]);
        let warning = trf("{} is a Production connection: the rows of the live database are changed.", &[&target_conn]);
        let (mut run, mut cancel) = (false, false);
        let (resp, _) = ternarygrid_ui::parts::Dialog::new(tr("Apply the sync script")).icon(icon::PLAY).width(520.0).show_modal(
            ui.ctx(),
            egui::Id::new(("dc_apply", self.uid)),
            |ui| {
                let p = ternarygrid_ui::pal(ui);
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.label(RichText::new(&question).size(13.5));
                ui.label(RichText::new(tr("Rows of the target are inserted, updated and deleted to match the source.")).color(p.text_muted));
                if production {
                    look::note(ui, ternarygrid_ui::parts::Tone::Warning, None, &warning);
                }
            },
            |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if look::danger_solid(ui, Some(icon::PLAY), tr("Apply")).clicked() {
                        run = true;
                    }
                    if kit::button_ex(ui, kit::Kind::Ghost, None, tr("Cancel"), false).clicked() {
                        cancel = true;
                    }
                });
            },
        );
        if run {
            self.apply(cx);
        }
        if run || cancel || resp.close {
            self.confirm_apply = false;
        }
    }

    // ------------------------------------------------- headless test hooks

    #[doc(hidden)]
    pub fn set_source(&mut self, conn: &str, table: ObjectRef) {
        self.source = Side { conn: Some(conn.to_string()), schema: Some(table.schema_ref()), table: Some(table), filter: String::new() };
    }

    #[doc(hidden)]
    pub fn set_target(&mut self, conn: &str, table: ObjectRef) {
        self.target = Side { conn: Some(conn.to_string()), schema: Some(table.schema_ref()), table: Some(table), filter: String::new() };
    }

    /// Start comparing (as the Compare button).
    #[doc(hidden)]
    pub fn request_compare(&mut self) {
        self.start_requested = true;
    }

    #[doc(hidden)]
    pub fn is_busy(&self) -> bool {
        self.start_requested || self.run.is_some() || self.apply_job.is_some()
    }

    #[doc(hidden)]
    pub fn key_columns(&self) -> &[String] {
        &self.keys
    }

    #[doc(hidden)]
    pub fn diff(&self) -> Option<&DataDiff> {
        self.result.as_ref().map(|o| &o.diff)
    }

    #[doc(hidden)]
    pub fn layout(&self) -> Option<&CompareLayout> {
        self.result.as_ref().map(|o| &o.layout)
    }

    #[doc(hidden)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Show the sync script instead of the differing rows.
    #[doc(hidden)]
    pub fn show_script(&mut self, on: bool) {
        self.view = if on { ResultView::Script } else { ResultView::Rows };
    }

    /// Ask to apply the sync script (the confirmation opens).
    #[doc(hidden)]
    pub fn ask_apply(&mut self) {
        self.confirm_apply = true;
    }

    /// Apply the sync script without the confirmation dialog.
    #[doc(hidden)]
    pub fn apply_now(&mut self, cx: &mut AppContext) {
        self.apply(cx);
    }
}

impl TabView for DataCompareTab {
    fn title(&self) -> String {
        match &self.source.table {
            Some(t) => format!("{} {}", tr("Data compare"), t.name),
            None => tr("Data compare").to_string(),
        }
    }

    fn icon(&self) -> &'static str {
        icon::GIT_DIFF
    }

    fn tooltip(&self) -> Option<String> {
        let path = |s: &Side| s.table.as_ref().map(|t| t.display_path()).unwrap_or_else(|| "—".into());
        Some(format!("{} ⇄ {}", path(&self.source), path(&self.target)))
    }

    fn connection_id(&self) -> Option<&str> {
        self.target.conn.as_deref().or(self.source.conn.as_deref())
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        self.poll(cx);
        let p = ternarygrid_ui::pal(ui);
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
            egui::Grid::new(("dc_sides", self.uid)).num_columns(2).spacing([12.0, 8.0]).min_col_width(58.0).show(ui, |ui| {
                look::muted(ui, tr("Source"));
                ui.horizontal(|ui| self.side_ui(ui, cx, false));
                ui.end_row();
                look::muted(ui, tr("Target"));
                ui.horizontal(|ui| self.side_ui(ui, cx, true));
                ui.end_row();
            });
        });
        self.auto_target_table(cx);
        let src = Self::def_of(cx, &self.source.clone());
        let dst = Self::def_of(cx, &self.target.clone());
        let errors: Vec<String> = [src.as_ref().err(), dst.as_ref().err()].into_iter().flatten().cloned().collect();
        let src = src.ok().flatten();
        let dst = dst.ok().flatten();
        // Keys and columns, options.
        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 0, bottom: 10 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            for e in &errors {
                look::error(ui, e);
            }
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if let Some(s) = &src {
                    self.init_choice(s);
                    self.columns_ui(ui, s, dst.as_deref());
                } else if self.source.table.is_some() {
                    ternarygrid_ui::parts::spinner_label(ui, tr("Reading the table definition…"));
                }
                self.options_ui(ui);
            });
        });
        ternarygrid_ui::parts::hline(ui);
        let busy = self.run.is_some() || self.apply_job.is_some();
        ternarygrid_ui::parts::toolbar(ui, |ui| {
            let ready = src.is_some() && dst.is_some() && !self.keys.is_empty();
            if ui.add_enabled_ui(ready && !busy && !self.start_requested, |ui| look::small(ui, kit::Kind::Primary, icon::GIT_DIFF, tr("Compare"))).inner.clicked() {
                self.start_requested = true;
            }
            if ui.add_enabled_ui(!busy, |ui| look::small(ui, kit::Kind::Ghost, icon::ARROWS_LEFT_RIGHT, tr("Swap"))).inner.clicked() {
                std::mem::swap(&mut self.source, &mut self.target);
                self.result = None;
                self.chosen_for = None;
            }
            if self.keys.is_empty() && src.is_some() {
                look::tone(ui, tr("The source table has no primary key: choose the key columns."), p.warn, false);
            }
            if self.start_requested {
                ternarygrid_ui::parts::spinner_label(ui, tr("Opening the sessions…"));
            }
            if let Some(o) = &self.result {
                let facts = Self::facts(o);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(facts).size(12.0).color(p.text_faint));
                });
            }
        });
        if self.start_requested {
            self.try_start(cx, src.as_ref(), dst.as_ref());
        }
        egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            self.progress_ui(ui, cx);
            if let Some(e) = &self.error {
                look::error(ui, e);
            }
            let Some(o) = &self.result else { return };
            Self::summary_ui(ui, o);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ternarygrid_ui::parts::segmented(ui, &mut self.view, &[(ResultView::Rows, tr("Differences")), (ResultView::Script, tr("Sync script"))]);
                match self.view {
                    ResultView::Rows => {
                        ui.add_space(2.0);
                        look::muted(ui, tr("Show"));
                        let items = [
                            (None, tr("All")),
                            (Some(RowDiffKind::Different), kind_label(RowDiffKind::Different)),
                            (Some(RowDiffKind::OnlyInSource), kind_label(RowDiffKind::OnlyInSource)),
                            (Some(RowDiffKind::OnlyInTarget), kind_label(RowDiffKind::OnlyInTarget)),
                        ];
                        ternarygrid_ui::parts::segmented_small(ui, &mut self.filter, &items);
                    }
                    ResultView::Script => self.script_actions(ui, cx),
                }
            });
            match self.view {
                ResultView::Rows => self.rows_ui(ui, cx),
                ResultView::Script => self.script_ui(ui),
            }
        });
        self.apply_modal(ui, cx);
        if busy || self.start_requested {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    fn on_close(&mut self, cx: &mut AppContext) -> bool {
        self.stop(cx);
        true
    }

    crate::impl_tab_any!();
}
