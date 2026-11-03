//! Metadata and data search (FR-N5).
//!
//! * Metadata search: objects whose name (optionally comment) matches a
//!   pattern, across one or all connected connections
//!   ([`ternarygrid_core::Session::search_metadata`]); results open the object
//!   editor.
//! * Data search: a text in the text columns of chosen tables of a schema,
//!   counted up to a row limit per table on a dedicated, cancellable session;
//!   results open the table data filtered to the matching rows.

use crate::backend::Job;
use crate::command::{AppCommand, ObjectPage};
use crate::conn::session::SessionHandle;
use crate::cx::AppContext;
use crate::tabs::TabView;
use crate::tabs::data_compare::{DedicatedSession, SessionState, connection_combo_ex, schema_combo_ex};
use crate::tabs::tasks::look;
use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use parking_lot::Mutex;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use ternarygrid_core::ddl::SchemaSnapshot;
use ternarygrid_core::meta::{MetadataSearch, ObjectInfo, ObjectKind, ObjectRef, SchemaRef};
use ternarygrid_core::{DbError, DbKind, DbResult, Dialect, ErrorKind, Value, tr, trf, trn};
use ternarygrid_ui::kit::Kind;
use tokio_util::sync::CancellationToken;

/// Object kinds offered by the metadata search.
const KINDS: &[ObjectKind] = &[
    ObjectKind::Table,
    ObjectKind::View,
    ObjectKind::MaterializedView,
    ObjectKind::Column,
    ObjectKind::Index,
    ObjectKind::Sequence,
    ObjectKind::Function,
    ObjectKind::Procedure,
    ObjectKind::Trigger,
    ObjectKind::Type,
    ObjectKind::Schema,
];

/// Default row limit of the data search per table.
pub const DEFAULT_ROW_LIMIT: u64 = 1000;

// ------------------------------------------------------- data search SQL

/// `text` as a `LIKE` pattern matching values that contain it (`\` escapes).
fn like_contains(text: &str, kind: DbKind) -> String {
    let mut s = String::with_capacity(text.len() + 2);
    s.push('%');
    for c in text.chars() {
        let special = matches!(c, '%' | '_' | '\\') || (c == '[' && kind == DbKind::SqlServer);
        if special {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('%');
    s
}

/// Condition true when column `column` (unquoted name) contains `text`,
/// written for the dialect: `ILIKE` (PostgreSQL), `LOWER(..) LIKE` (MySQL,
/// SQL Server, Oracle, ANSI), `instr` (SQLite) or `position` (ClickHouse).
pub fn match_predicate(d: &dyn Dialect, column: &str, text: &str, case_sensitive: bool) -> String {
    let kind = d.kind();
    let c = d.quote_ident(column);
    let lit = |s: &str| d.string_literal(s);
    let esc = lit("\\");
    match kind {
        DbKind::Postgres | DbKind::CockroachDb => {
            format!("CAST({c} AS TEXT) {} {} ESCAPE {esc}", if case_sensitive { "LIKE" } else { "ILIKE" }, lit(&like_contains(text, kind)))
        }
        DbKind::MySql | DbKind::MariaDb => {
            let p = lit(&like_contains(text, kind));
            if case_sensitive { format!("CAST({c} AS BINARY) LIKE CAST({p} AS BINARY) ESCAPE {esc}") } else { format!("LOWER({c}) LIKE LOWER({p}) ESCAPE {esc}") }
        }
        DbKind::SqlServer => {
            let p = lit(&like_contains(text, kind));
            if case_sensitive {
                format!("CAST({c} AS NVARCHAR(MAX)) COLLATE Latin1_General_BIN LIKE {p} ESCAPE {esc}")
            } else {
                format!("LOWER(CAST({c} AS NVARCHAR(MAX))) LIKE LOWER({p}) ESCAPE {esc}")
            }
        }
        DbKind::Sqlite => {
            if case_sensitive {
                format!("instr({c}, {}) > 0", lit(text))
            } else {
                format!("instr(lower({c}), lower({})) > 0", lit(text))
            }
        }
        DbKind::ClickHouse => {
            if case_sensitive {
                format!("position(toString({c}), {}) > 0", lit(text))
            } else {
                format!("positionCaseInsensitiveUTF8(toString({c}), {}) > 0", lit(text))
            }
        }
        // Data search is offered for SQL connections only; the others get the
        // portable form.
        DbKind::Oracle | DbKind::Cassandra | DbKind::Redis | DbKind::MongoDb | DbKind::Elasticsearch | DbKind::Neo4j | DbKind::Generic => {
            let p = lit(&like_contains(text, kind));
            if case_sensitive { format!("{c} LIKE {p} ESCAPE {esc}") } else { format!("LOWER({c}) LIKE LOWER({p}) ESCAPE {esc}") }
        }
    }
}

/// `(a matches OR b matches ...)`, the filter of the matching rows.
pub fn data_where_clause(d: &dyn Dialect, columns: &[String], text: &str, case_sensitive: bool) -> String {
    let parts: Vec<String> = columns.iter().map(|c| match_predicate(d, c, text, case_sensitive)).collect();
    format!("({})", parts.join(" OR "))
}

/// Query counting the matching rows of `table`, at most `limit`.
pub fn data_count_sql(d: &dyn Dialect, table: &ObjectRef, columns: &[String], text: &str, case_sensitive: bool, limit: u64) -> String {
    let wc = data_where_clause(d, columns, text, case_sensitive);
    let first = columns.first().cloned().unwrap_or_default();
    let select = d.select_table(table, Some(std::slice::from_ref(&first)), Some(&wc), None, None);
    d.count_query(&d.limit_query(&select, limit.max(1), 0))
}

/// Text columns of a snapshot's tables and views.
fn text_columns(snap: &SchemaSnapshot) -> Vec<(ObjectRef, Vec<String>)> {
    let tables = snap
        .tables
        .iter()
        .map(|t| (ObjectRef::table(snap.schema.clone(), t.name.clone()), t.columns.iter().filter(|c| c.kind.is_textual()).map(|c| c.name.clone()).collect::<Vec<_>>()));
    let views = snap.views.iter().map(|v| {
        let kind = if v.materialized { ObjectKind::MaterializedView } else { ObjectKind::View };
        (ObjectRef::new(kind, snap.schema.clone(), v.name.clone()), v.columns.iter().filter(|c| c.kind.is_textual()).map(|c| c.name.clone()).collect::<Vec<_>>())
    });
    tables.chain(views).filter(|(_, c)| !c.is_empty()).collect()
}

fn count_of(v: &Value) -> u64 {
    match v {
        Value::Int(i) => (*i).max(0) as u64,
        Value::UInt(u) => *u,
        Value::Float(f) => f.max(0.0) as u64,
        other => other.to_text().trim().parse::<u64>().unwrap_or(0),
    }
}

// ------------------------------------------------------------------ state

/// One metadata search result.
#[derive(Debug, Clone)]
pub struct MetaHit {
    pub conn: String,
    pub conn_name: String,
    pub info: ObjectInfo,
}

/// Tables of the data search with matching rows (or an error).
#[derive(Debug, Clone)]
pub struct DataHit {
    pub table: ObjectRef,
    /// Matching rows, counted up to the row limit.
    pub count: u64,
    /// `count` reached the limit: there may be more.
    pub limited: bool,
    pub columns: Vec<String>,
    /// Filter selecting the matching rows (for the data editor).
    pub where_clause: String,
    pub error: Option<String>,
}

#[derive(Default)]
struct DataProgress {
    done: usize,
    total: usize,
    current: Option<String>,
    hits: Vec<DataHit>,
}

struct DataRun {
    job: Job<DbResult<()>>,
    state: Arc<Mutex<DataProgress>>,
    cancel: CancellationToken,
    session: SessionHandle,
    started: Instant,
    conn: String,
}

type MetaResults = Vec<(String, String, DbResult<Vec<ObjectInfo>>)>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Metadata,
    Data,
}

pub struct SearchTab {
    uid: u64,
    mode: Mode,
    /// Connection of the metadata search (`None` = all connected) and of the data search.
    conn: Option<String>,
    // metadata search
    pattern: String,
    kinds: BTreeSet<ObjectKind>,
    case_sensitive: bool,
    search_comments: bool,
    /// Schemas chosen for a single connection.
    schemas: BTreeSet<String>,
    /// Comma separated schemas (all connections).
    schema_text: String,
    limit: usize,
    meta_job: Option<Job<MetaResults>>,
    meta_hits: Vec<MetaHit>,
    meta_errors: Vec<String>,
    meta_selected: Option<usize>,
    meta_done: bool,
    // data search
    data_schema: Option<SchemaRef>,
    text: String,
    data_case_sensitive: bool,
    row_limit: u64,
    /// Chosen tables (names) of `data_schema`; `None` = all with text columns.
    chosen: Option<BTreeSet<String>>,
    table_filter: String,
    session: DedicatedSession,
    data_requested: bool,
    data_run: Option<DataRun>,
    data_hits: Vec<DataHit>,
    /// Connection the data hits were found on (the combo may show another one by now).
    hits_conn: Option<String>,
    data_done: Option<(usize, Duration, bool)>,
    notice: Option<String>,
}

/// A vertical hairline between groups of a form row, as the design's: 1 point wide, the row's
/// spacing on each side (the kit's divider brings its own room).
fn divider(ui: &mut Ui) {
    let p = ternarygrid_ui::pal(ui);
    let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 18.0), egui::Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), egui::Stroke::new(1.0, p.border));
}

impl SearchTab {
    /// Metadata search, or the data search when `data`.
    pub fn new(conn: Option<String>, data: bool) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        SearchTab {
            uid: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            mode: if data { Mode::Data } else { Mode::Metadata },
            conn,
            pattern: String::new(),
            kinds: MetadataSearch::default().kinds.into_iter().collect(),
            case_sensitive: false,
            search_comments: false,
            schemas: BTreeSet::new(),
            schema_text: String::new(),
            limit: 500,
            meta_job: None,
            meta_hits: Vec::new(),
            meta_errors: Vec::new(),
            meta_selected: None,
            meta_done: false,
            data_schema: None,
            text: String::new(),
            data_case_sensitive: false,
            row_limit: DEFAULT_ROW_LIMIT,
            chosen: None,
            table_filter: String::new(),
            session: DedicatedSession::default(),
            data_requested: false,
            data_run: None,
            data_hits: Vec::new(),
            hits_conn: None,
            data_done: None,
            notice: None,
        }
    }

    // --------------------------------------------------- metadata search

    fn start_metadata(&mut self, cx: &mut AppContext) {
        self.notice = None;
        let pattern = self.pattern.trim().to_string();
        if pattern.is_empty() {
            self.notice = Some(tr("Enter a name or a pattern (* and ? are wildcards).").to_string());
            return;
        }
        if self.kinds.is_empty() {
            self.notice = Some(tr("Choose at least one object kind.").to_string());
            return;
        }
        let ids: Vec<String> = match &self.conn {
            Some(c) if cx.conns.is_connected(c) => vec![c.clone()],
            Some(_) => {
                self.notice = Some(tr("The connection is closed.").to_string());
                return;
            }
            None => {
                let mut v = cx.conns.live_ids();
                v.sort();
                v
            }
        };
        let targets: Vec<(String, String, SessionHandle)> = ids.iter().filter_map(|id| cx.conns.live(id).map(|l| (id.clone(), l.config.name.clone(), l.main.clone()))).collect();
        if targets.is_empty() {
            self.notice = Some(tr("No connection is open.").to_string());
            return;
        }
        let schemas: Vec<String> = if self.conn.is_some() {
            self.schemas.iter().cloned().collect()
        } else {
            self.schema_text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
        };
        let search = MetadataSearch {
            pattern,
            kinds: self.kinds.iter().copied().collect(),
            schemas,
            case_sensitive: self.case_sensitive,
            search_comments: self.search_comments,
            limit: self.limit.max(1),
        };
        self.meta_hits.clear();
        self.meta_errors.clear();
        self.meta_selected = None;
        self.meta_done = false;
        self.meta_job = Some(cx.backend.spawn(async move {
            let futs = targets.into_iter().map(|(id, name, s)| {
                let search = search.clone();
                async move {
                    let r = s.lock().await.search_metadata(&search).await;
                    (id, name, r)
                }
            });
            futures::future::join_all(futs).await
        }));
    }

    fn poll_metadata(&mut self) {
        let Some(j) = self.meta_job.as_mut() else { return };
        let Some(results) = j.poll() else { return };
        self.meta_job = None;
        self.meta_done = true;
        for (conn, conn_name, r) in results {
            match r {
                Ok(list) => self.meta_hits.extend(list.into_iter().map(|info| MetaHit { conn: conn.clone(), conn_name: conn_name.clone(), info })),
                Err(e) => self.meta_errors.push(format!("{conn_name}: {}", e.full_text())),
            }
        }
    }

    /// Open a result: table children open their table.
    fn open_hit(cx: &mut AppContext, hit: &MetaHit) {
        let obj = &hit.info.obj;
        let target = match obj.parent_table() {
            Some(mut parent) if obj.kind.is_table_child() => {
                let is_view = cx.conns.live(&hit.conn).is_some_and(|l| {
                    [ObjectKind::View, ObjectKind::MaterializedView]
                        .iter()
                        .any(|k| l.meta.objects.get(&(parent.schema_ref(), *k)).and_then(|x| x.get()).is_some_and(|v| v.iter().any(|o| o.obj.name == parent.name)))
                });
                if is_view {
                    parent.kind = ObjectKind::View;
                }
                parent
            }
            _ => obj.clone(),
        };
        cx.push(AppCommand::OpenObject { connection: hit.conn.clone(), object: target, page: ObjectPage::Properties });
    }

    /// Metadata / Data, at the start of the first row of both searches.
    fn mode_switch(&mut self, ui: &mut Ui) {
        let before = self.mode;
        ternarygrid_ui::parts::segmented(ui, &mut self.mode, &[(Mode::Metadata, tr("Metadata")), (Mode::Data, tr("Data"))]);
        if before != self.mode {
            self.notice = None;
        }
    }

    /// The search form: padding and a hairline under it.
    fn form(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
        egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 12.0);
            add(ui);
        });
        ternarygrid_ui::parts::hline(ui);
    }

    fn metadata_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let pal = ternarygrid_ui::pal(ui);
        let mut go = false;
        Self::form(ui, |ui| {
            ui.horizontal(|ui| {
                // The select at the height of the design's fields, its text 11 points in.
                ui.spacing_mut().interact_size.y = look::FIELD_H;
                ui.spacing_mut().button_padding = egui::vec2(11.0, 5.0);
                self.mode_switch(ui);
                let r = ternarygrid_ui::kit::search_field(ui, egui::Id::new(("ms_pattern", self.uid)), &mut self.pattern, tr("Name or pattern, e.g. cust* or *_id"), 340.0);
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
                ui.label(RichText::new(tr("in")).color(pal.text_muted));
                let mut conn = self.conn.clone();
                if connection_combo_ex(ui, cx, ("ms_conn", self.uid), &mut conn, Some(tr("All connected connections")), 230.0, false) {
                    self.conn = conn;
                    self.schemas.clear();
                }
                let busy = self.meta_job.is_some();
                if ui.add_enabled_ui(!busy, |ui| ternarygrid_ui::kit::button_ex(ui, Kind::Primary, Some(icon::MAGNIFYING_GLASS), tr("Search"), false)).inner.clicked() {
                    go = true;
                }
                if busy {
                    ui.add(egui::Spinner::new().size(14.0).color(pal.accent_text));
                }
            });
            // A line as tall as its 26 point fields (its first row starts at that height, so
            // the check boxes are centred on them).
            ui.spacing_mut().interact_size.y = 26.0;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                look::checkbox(ui, &mut self.case_sensitive, tr("Case sensitive"));
                look::checkbox(ui, &mut self.search_comments, tr("Search comments"));
                ui.label(RichText::new(tr("Limit")).size(12.5).color(pal.text_muted));
                look::number_h(ui, &mut self.limit, 1..=100_000, 70.0, 26.0);
                divider(ui);
                ui.horizontal(|ui| self.schema_filter_ui(ui, cx));
            });
            // The kinds flow on as many lines as they need (a label is never squeezed): a check
            // box that does not fit on the line starts the next one, under the first kind.
            let font = egui::FontId::proportional(13.0);
            let label_w = ui.painter().layout_no_wrap(tr("Kinds").to_string(), egui::FontId::proportional(12.5), egui::Color32::PLACEHOLDER).size().x;
            let kinds = &mut self.kinds;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                // Lines as tall as a check box (a wrapping row takes its first line's height
                // from here).
                ui.spacing_mut().interact_size.y = 17.0;
                ui.add_sized(egui::vec2(label_w, 17.0), egui::Label::new(RichText::new(tr("Kinds")).size(12.5).color(pal.text_muted)));
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(14.0, 10.0);
                    for k in KINDS {
                        let text = tr(k.plural_label());
                        let w = ui.painter().layout_no_wrap(text.to_string(), font.clone(), egui::Color32::PLACEHOLDER).size().x + 26.0;
                        if ui.available_width() < w {
                            ui.end_row();
                        }
                        let mut on = kinds.contains(k);
                        if look::checkbox(ui, &mut on, text).changed() {
                            if on {
                                kinds.insert(*k);
                            } else {
                                kinds.remove(k);
                            }
                        }
                    }
                });
            });
        });
        if go {
            self.start_metadata(cx);
        }
        egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            if let Some(n) = &self.notice {
                look::note(ui, ternarygrid_ui::parts::Tone::Warning, n);
            }
            for e in &self.meta_errors {
                look::error(ui, e);
            }
            if self.meta_done {
                if self.meta_hits.is_empty() {
                    ui.label(RichText::new(tr("No objects found.")).color(pal.text_muted));
                    return;
                }
                let n = self.meta_hits.len();
                ui.horizontal(|ui| {
                    // The row as tall as its button, the texts centred on it.
                    ui.set_min_height(ternarygrid_ui::style::SMALL_H);
                    let mut count = trn(n, tr("1 object"), tr("{} objects"));
                    if n >= self.limit {
                        count = trf("{} · the limit is reached", &[&count]);
                    }
                    ui.label(RichText::new(count).size(12.0).color(pal.text_muted));
                    let sel = self.meta_selected.and_then(|i| self.meta_hits.get(i).cloned());
                    if ui.add_enabled_ui(sel.is_some(), |ui| ternarygrid_ui::kit::button_ex(ui, Kind::Secondary, None, tr("Open"), true)).inner.clicked()
                        && let Some(h) = sel
                    {
                        Self::open_hit(cx, &h);
                    }
                    ui.label(RichText::new(tr("Double-click a result to open it.")).size(12.0).color(pal.text_muted));
                });
            }
            if self.meta_hits.is_empty() {
                return;
            }
            // Connection and kind as wide as their content; object, details and comment share
            // the rest 1.6 : 1 : 1.
            let hits = &self.meta_hits;
            let conn_w = look::fit(ui, hits.iter().map(|h| h.conn_name.as_str()), 0.0, 100.0, 220.0);
            let kind_w = look::fit(ui, hits.iter().map(|h| tr(h.info.obj.kind.label())), 20.0, 90.0, 180.0);
            let fr = ((ui.available_width() - 2.0 - conn_w - kind_w) / 3.6).max(60.0);
            let cols = [
                look::Col::new(tr("Connection"), look::Width::Fit(conn_w)),
                look::Col::new(tr("Kind"), look::Width::Fit(kind_w)),
                look::Col::new(tr("Object"), look::Width::Initial((fr * 1.6).floor())),
                look::Col::new(tr("Details"), look::Width::Initial(fr.floor())),
                look::Col::new(tr("Comment"), look::Width::Rest(60.0)),
            ];
            let (mut clicked, mut open) = (None, None);
            let selected = self.meta_selected;
            let hits = &self.meta_hits;
            look::table(ui, ("ms_results", self.uid), &cols, hits.len(), |row, i| {
                let h = &hits[i];
                row.set_selected(selected == Some(i));
                look::cell(row, false, |ui| {
                    look::cell_text(ui, RichText::new(&h.conn_name).color(pal.text_muted));
                });
                look::cell(row, false, |ui| {
                    ui.label(RichText::new(crate::nav::kind_icon(h.info.obj.kind)).font(ternarygrid_ui::style::icon_font(14.0)).color(pal.text_muted));
                    look::cell_text(ui, tr(h.info.obj.kind.label()));
                });
                look::cell(row, false, |ui| {
                    look::cell_text(ui, look::mono(ui, &h.info.obj.display_path(), pal.text));
                });
                look::cell(row, false, |ui| {
                    look::cell_text(ui, RichText::new(h.info.detail.clone().unwrap_or_default()).color(pal.text_muted));
                });
                look::cell(row, false, |ui| {
                    look::cell_text(ui, RichText::new(h.info.comment.clone().unwrap_or_default()).color(pal.text_muted));
                });
                let resp = row.response();
                if resp.clicked() {
                    clicked = Some(i);
                }
                if resp.double_clicked() {
                    open = Some(i);
                }
            });
            if let Some(i) = clicked {
                self.meta_selected = Some(i);
            }
            if let Some(i) = open
                && let Some(h) = self.meta_hits.get(i).cloned()
            {
                Self::open_hit(cx, &h);
            }
        });
    }

    fn schema_filter_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let pal = ternarygrid_ui::pal(ui);
        ui.label(RichText::new(tr("Schemas")).size(12.5).color(pal.text_muted));
        let Some(conn) = self.conn.clone().filter(|c| cx.conns.is_connected(c)) else {
            look::text_field_h(ui, &mut self.schema_text, 200.0, 26.0, tr("all (or a, b, c)"));
            return;
        };
        let backend = cx.backend;
        let Some(live) = cx.conns.live_mut(&conn) else { return };
        let main = live.main.clone();
        live.meta.request_schemas(backend, &main, None);
        let names: Vec<String> = live.meta.schemas_of(None).get().map(|v| v.iter().filter(|s| !s.is_system).map(|s| s.name.clone()).collect()).unwrap_or_default();
        let label = if self.schemas.is_empty() { tr("All").to_string() } else { self.schemas.iter().cloned().collect::<Vec<_>>().join(", ") };
        let b = ternarygrid_ui::kit::button_ex(ui, Kind::Secondary, Some(icon::STACK), &label, true);
        egui::Popup::menu(&b).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ternarygrid_ui::kit::menu_look(ui, 220.0);
            let mut all = self.schemas.is_empty();
            if ternarygrid_ui::kit::menu_check(ui, &mut all, tr("All")).clicked() {
                self.schemas.clear();
            }
            ternarygrid_ui::kit::menu_separator(ui);
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                for n in &names {
                    let mut on = self.schemas.contains(n);
                    if ternarygrid_ui::kit::menu_check(ui, &mut on, n).changed() {
                        if on {
                            self.schemas.insert(n.clone());
                        } else {
                            self.schemas.remove(n);
                        }
                    }
                }
            });
        });
    }

    // ------------------------------------------------------- data search

    fn snapshot(&self, cx: &mut AppContext) -> (Option<Arc<SchemaSnapshot>>, Option<String>) {
        let (Some(conn), Some(schema)) = (self.conn.clone(), self.data_schema.clone()) else { return (None, None) };
        let backend = cx.backend;
        let Some(live) = cx.conns.live_mut(&conn) else { return (None, None) };
        let main = live.main.clone();
        live.meta.request_snapshot(backend, &main, &schema);
        let err = live.meta.snapshots.get(&schema).and_then(|l| l.error()).map(|e| e.full_text());
        (live.meta.snapshot(&schema), err)
    }

    fn start_data(&mut self, cx: &mut AppContext) {
        self.notice = None;
        let text = self.text.clone();
        if text.trim().is_empty() {
            self.notice = Some(tr("Enter the text to find.").to_string());
            self.data_requested = false;
            return;
        }
        let Some(conn) = self.conn.clone() else {
            self.notice = Some(tr("Choose a connection.").to_string());
            self.data_requested = false;
            return;
        };
        let (snap, err) = self.snapshot(cx);
        if let Some(e) = err {
            self.notice = Some(e);
            self.data_requested = false;
            return;
        }
        let Some(snap) = snap else { return };
        let db = self.data_schema.as_ref().and_then(|s| s.database.clone());
        let session = match self.session.poll(cx, &conn, db.as_deref(), "search") {
            SessionState::Ready(s) => s,
            SessionState::Opening => return,
            SessionState::Failed(e) => {
                self.notice = Some(e);
                self.session.retry();
                self.data_requested = false;
                return;
            }
            SessionState::NotConnected => {
                self.notice = Some(tr("The connection is closed.").to_string());
                self.data_requested = false;
                return;
            }
        };
        self.data_requested = false;
        let tables: Vec<(ObjectRef, Vec<String>)> =
            text_columns(&snap).into_iter().filter(|(t, _)| self.chosen.as_ref().is_none_or(|c| c.contains(&t.name))).map(|(t, c)| (t.with_database(db.clone()), c)).collect();
        if tables.is_empty() {
            self.notice = Some(tr("Choose at least one table with text columns.").to_string());
            return;
        }
        let state = Arc::new(Mutex::new(DataProgress { total: tables.len(), ..Default::default() }));
        let cancel = CancellationToken::new();
        let (st, token, s) = (state.clone(), cancel.clone(), session.clone());
        let repaint = cx.backend.repainter();
        let cs = self.data_case_sensitive;
        let limit = self.row_limit.max(1);
        let job = cx.backend.spawn(async move {
            let d = s.dialect();
            let total = tables.len();
            for (i, (table, cols)) in tables.into_iter().enumerate() {
                if token.is_cancelled() {
                    return Err(DbError::cancelled());
                }
                {
                    let mut p = st.lock();
                    p.done = i;
                    p.current = Some(table.display_path());
                }
                repaint.request();
                let where_clause = data_where_clause(d.as_ref(), &cols, &text, cs);
                let sql = data_count_sql(d.as_ref(), &table, &cols, &text, cs, limit);
                let hit = match s.query_all(&sql, &[]).await {
                    Ok((_, rows)) => {
                        let n = rows.first().and_then(|r| r.first()).map(count_of).unwrap_or(0);
                        DataHit { table, count: n, limited: n >= limit, columns: cols, where_clause, error: None }
                    }
                    Err(_) if token.is_cancelled() => return Err(DbError::cancelled()),
                    Err(e) => DataHit { table, count: 0, limited: false, columns: cols, where_clause, error: Some(e.full_text()) },
                };
                if hit.count > 0 || hit.error.is_some() {
                    st.lock().hits.push(hit);
                }
            }
            let mut p = st.lock();
            p.done = total;
            p.current = None;
            Ok(())
        });
        self.data_hits.clear();
        self.data_done = None;
        self.data_run = Some(DataRun { job, state, cancel, session, started: Instant::now(), conn });
    }

    fn stop_data(&mut self, cx: &AppContext) {
        if let Some(r) = &self.data_run {
            r.cancel.cancel();
            let s = r.session.clone();
            cx.backend.spawn_detached(async move {
                let _ = s.cancel().await;
            });
        }
    }

    fn poll_data(&mut self) {
        let Some(r) = self.data_run.as_mut() else { return };
        self.data_hits = r.state.lock().hits.clone();
        self.hits_conn = Some(r.conn.clone());
        if let Some(res) = r.job.poll() {
            let p = r.state.lock();
            let cancelled = matches!(&res, Err(e) if e.kind == ErrorKind::Cancelled);
            if let Err(e) = &res
                && !cancelled
            {
                self.notice = Some(e.full_text());
            }
            self.data_done = Some((p.done, r.started.elapsed(), cancelled));
            drop(p);
            self.data_run = None;
        }
    }

    fn data_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let pal = ternarygrid_ui::pal(ui);
        let busy = self.data_run.is_some() || self.data_requested;
        let mut go = false;
        let mut stop = false;
        Self::form(ui, |ui| {
            // The data search's rows are 10 apart (the metadata search's 12).
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().interact_size.y = look::FIELD_H;
                ui.spacing_mut().button_padding = egui::vec2(11.0, 5.0);
                self.mode_switch(ui);
                // The database's logo, the connection (180) and the schema (140).
                if let Some(kind) = self.conn.as_deref().and_then(|c| cx.conns.config(c)).map(|c| c.kind)
                    && let Some(img) = crate::dblogo::image(ui.ctx(), kind, 15.0, crate::theme::logo_color(kind, ui.visuals().dark_mode))
                {
                    ui.add(img);
                }
                let mut conn = self.conn.clone();
                if connection_combo_ex(ui, cx, ("ds_conn", self.uid), &mut conn, None, 180.0, false) {
                    self.conn = conn;
                    self.data_schema = None;
                    self.chosen = None;
                }
                if let Some(c) = self.conn.clone().filter(|c| cx.conns.is_connected(c)) {
                    let mut schema = self.data_schema.clone();
                    if schema_combo_ex(ui, cx, ("ds_schema", self.uid), &c, &mut schema, 140.0, false) {
                        self.data_schema = schema;
                        self.chosen = None;
                    }
                }
            });
            // One line of 30 point controls, the labels and the check box centred on it (a
            // wrapping row takes its line's height from its parent).
            ui.spacing_mut().interact_size.y = look::FIELD_H;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new(tr("Find")).size(12.5).color(pal.text_muted));
                let r = look::text_field(ui, &mut self.text, 320.0, tr("Text to find in text columns"), false);
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
                look::checkbox(ui, &mut self.data_case_sensitive, tr("Case sensitive"));
                divider(ui);
                ui.label(RichText::new(tr("Rows per table")).size(12.5).color(pal.text_muted));
                look::number_h(ui, &mut self.row_limit, 1..=10_000_000, 80.0, look::FIELD_H);
                if busy {
                    stop = ternarygrid_ui::kit::button_ex(ui, Kind::Danger, Some(icon::STOP_CIRCLE), tr("Stop"), false).clicked();
                } else if ternarygrid_ui::kit::button_ex(ui, Kind::Primary, Some(icon::MAGNIFYING_GLASS), tr("Search"), false).clicked() {
                    go = true;
                }
            });
        });
        if stop {
            self.data_requested = false;
            self.stop_data(cx);
        }
        if go && !busy {
            self.data_requested = true;
        }
        if self.data_requested {
            self.start_data(cx);
        }
        let (snap, err) = self.snapshot(cx);
        let left = egui::Frame::new().fill(pal.canvas).inner_margin(egui::Margin { left: 0, right: 0, top: 12, bottom: 0 });
        egui::Panel::left(egui::Id::new(("ds_tables_panel", self.uid))).resizable(true).default_size(300.0).frame(left).show(ui, |ui| {
            self.tables_ui(ui, snap.as_deref());
        });
        let right = egui::Frame::new().fill(pal.canvas).inner_margin(egui::Margin::symmetric(16, 14));
        egui::CentralPanel::default().frame(right).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            if let Some(n) = &self.notice {
                look::note(ui, ternarygrid_ui::parts::Tone::Warning, n);
            }
            if let Some(e) = err {
                look::error(ui, &e);
            }
            self.data_results_ui(ui, cx);
        });
    }

    fn tables_ui(&mut self, ui: &mut Ui, snap: Option<&SchemaSnapshot>) {
        let pal = ternarygrid_ui::pal(ui);
        let Some(snap) = snap else {
            if self.data_schema.is_some() {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ternarygrid_ui::parts::spinner_label(ui, tr("Reading the schema…"));
                });
            } else {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new(tr("Choose a schema.")).color(pal.text_muted));
                });
            }
            return;
        };
        let tables = text_columns(snap);
        let chosen_count = tables.iter().filter(|(t, _)| self.chosen.as_ref().is_none_or(|c| c.contains(&t.name))).count();
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.scope(|ui| {
            // A line as tall as its text.
            ui.spacing_mut().interact_size.y = 0.0;
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.spacing_mut().item_spacing.x = 8.0;
                ternarygrid_ui::kit::semibold_label(ui, tr("Tables"), 12.5, pal.text);
                let with_text = trn(tables.len(), tr("1 with text columns"), tr("{} with text columns"));
                ui.label(RichText::new(trf("{} of {}", &[&chosen_count, &with_text])).size(12.0).color(pal.text_muted));
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let w =
                (ui.available_width() - 12.0 - ternarygrid_ui::kit::button_width(ui, None, tr("All"), true) - ternarygrid_ui::kit::button_width(ui, None, tr("None"), true) - 16.0)
                    .max(80.0);
            look::text_field_h(ui, &mut self.table_filter, w, 26.0, tr("Filter…"));
            if ternarygrid_ui::kit::ghost(ui, None, tr("All")).clicked() {
                self.chosen = None;
            }
            if ternarygrid_ui::kit::ghost(ui, None, tr("None")).clicked() {
                self.chosen = Some(BTreeSet::new());
            }
        });
        let filter = self.table_filter.to_lowercase();
        egui::ScrollArea::vertical().id_salt(("ds_tables", self.uid)).auto_shrink([false, false]).show(ui, |ui| {
            // Rows of 28, 6 apart.
            ui.spacing_mut().item_spacing.y = 6.0;
            for (t, cols) in &tables {
                if !filter.is_empty() && !t.name.to_lowercase().contains(&filter) {
                    continue;
                }
                let mut on = self.chosen.as_ref().is_none_or(|c| c.contains(&t.name));
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
                resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, &t.name));
                if resp.hovered() {
                    ui.painter().rect_filled(rect.shrink2(egui::vec2(6.0, 1.0)), ternarygrid_ui::style::RADIUS, pal.hover);
                }
                let cy = rect.center().y;
                look::paint_box(ui, egui::Rect::from_center_size(egui::pos2(rect.left() + 20.0, cy), egui::vec2(15.0, 15.0)), on, resp.hovered());
                ui.painter().text(
                    egui::pos2(rect.left() + 42.0, cy),
                    egui::Align2::CENTER_CENTER,
                    crate::nav::kind_icon(t.kind),
                    ternarygrid_ui::style::icon_font(14.0),
                    pal.text_muted,
                );
                ui.painter().text(
                    egui::pos2(rect.left() + 57.0, cy),
                    egui::Align2::LEFT_CENTER,
                    &t.name,
                    egui::FontId::proportional(ternarygrid_ui::style::body_size(ui.ctx())),
                    pal.text,
                );
                if resp.on_hover_text(cols.join(", ")).clicked() {
                    on = !on;
                    let set = self.chosen.get_or_insert_with(|| tables.iter().map(|(x, _)| x.name.clone()).collect());
                    if on {
                        set.insert(t.name.clone());
                    } else {
                        set.remove(&t.name);
                    }
                }
            }
        });
    }

    fn data_results_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let pal = ternarygrid_ui::pal(ui);
        if let Some(r) = &self.data_run {
            let p = r.state.lock();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let size = ternarygrid_ui::style::body_size(ui.ctx());
                ui.label(RichText::new(trf("Table {} of {}", &[&(p.done + 1).min(p.total), &p.total])).font(ternarygrid_ui::style::medium(ui.ctx(), size)).color(pal.text));
                if let Some(c) = &p.current {
                    ui.label(RichText::new(c).color(pal.text_faint));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let frac = if p.total == 0 { 0.0 } else { p.done as f32 / p.total as f32 };
                    ternarygrid_ui::parts::progress(ui, frac, (ui.available_width() - 8.0).clamp(80.0, 360.0));
                });
            });
        } else if self.data_requested {
            ternarygrid_ui::parts::spinner_label(ui, tr("Opening a session…"));
        }
        if let Some((n, d, cancelled)) = self.data_done {
            let found = self.data_hits.iter().filter(|h| h.error.is_none()).count();
            let searched = trn(n, tr("1 table"), tr("{} tables"));
            let matches = match found {
                0 => tr("no matches").to_string(),
                1 => tr("1 with matches").to_string(),
                f => trf("{} with matches", &[&f]),
            };
            let text = if cancelled {
                trf("Stopped after {} · {}", &[&searched, &matches])
            } else {
                trf("{} searched in {} · {}", &[&searched, &crate::widgets::format_duration(d), &matches])
            };
            ui.label(RichText::new(text).size(12.0).color(pal.text_muted));
        }
        if self.data_hits.is_empty() {
            return;
        }
        let conn = self.data_run.as_ref().map(|r| r.conn.clone()).or_else(|| self.hits_conn.clone()).unwrap_or_default();
        let cols = [
            look::Col::new(tr("Table"), look::Width::Initial((ui.available_width() - 262.0).max(160.0))),
            look::Col::new(tr("Matching rows"), look::Width::Initial(140.0)).right(),
            look::Col::new("", look::Width::Rest(120.0)),
        ];
        let hits = &self.data_hits;
        let mut open = None;
        // Rows of 40 (their buttons inside).
        look::table_h(ui, ("ds_hits", self.uid), &cols, hits.len(), 41.0, |row, i| {
            let h = &hits[i];
            look::cell(row, false, |ui| {
                ui.label(RichText::new(crate::nav::kind_icon(h.table.kind)).font(ternarygrid_ui::style::icon_font(14.0)).color(pal.text_muted));
                look::cell_text(ui, h.table.display_path()).on_hover_text(trf("Searched columns: {}", &[&h.columns.join(", ")]));
            });
            match &h.error {
                Some(e) => {
                    look::cell(row, true, |ui| {
                        look::cell_text(ui, RichText::new(format!("{} {}", icon::WARNING, tr("Error"))).color(pal.err)).on_hover_text(e);
                    });
                    look::cell(row, false, |_| {});
                }
                None => {
                    look::cell(row, true, |ui| {
                        look::cell_text(ui, if h.limited { trf("{} or more", &[&h.count]) } else { h.count.to_string() });
                    });
                    look::cell(row, false, |ui| {
                        if ternarygrid_ui::kit::button_ex(ui, Kind::Secondary, None, tr("Show rows"), true).clicked() {
                            open = Some(i);
                        }
                    });
                }
            }
        });
        if let Some(i) = open
            && let Some(h) = self.data_hits.get(i)
        {
            let tab = crate::results::data_tab::TableDataTab::new(conn, h.table.clone(), Some(h.where_clause.clone()));
            cx.push(AppCommand::OpenTab(Box::new(tab)));
        }
    }

    // ------------------------------------------------- headless test hooks

    #[doc(hidden)]
    pub fn set_pattern(&mut self, pattern: &str) {
        self.pattern = pattern.to_string();
    }

    #[doc(hidden)]
    pub fn search_metadata_now(&mut self, cx: &mut AppContext) {
        self.start_metadata(cx);
    }

    #[doc(hidden)]
    pub fn metadata_hits(&self) -> &[MetaHit] {
        &self.meta_hits
    }

    #[doc(hidden)]
    pub fn notice_text(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Prepare a data search in `schema` for `text` (all tables when `tables` is empty).
    #[doc(hidden)]
    pub fn set_data_search(&mut self, schema: SchemaRef, text: &str, tables: &[&str]) {
        self.mode = Mode::Data;
        self.data_schema = Some(schema);
        self.text = text.to_string();
        self.chosen = if tables.is_empty() { None } else { Some(tables.iter().map(|t| t.to_string()).collect()) };
    }

    #[doc(hidden)]
    pub fn request_data_search(&mut self) {
        self.data_requested = true;
    }

    #[doc(hidden)]
    pub fn data_hits(&self) -> &[DataHit] {
        &self.data_hits
    }

    #[doc(hidden)]
    pub fn data_finished(&self) -> bool {
        self.data_done.is_some() && self.data_run.is_none()
    }

    #[doc(hidden)]
    pub fn is_busy(&self) -> bool {
        self.meta_job.is_some() || self.data_run.is_some() || self.data_requested
    }
}

impl TabView for SearchTab {
    fn title(&self) -> String {
        match self.mode {
            Mode::Metadata => tr("Search metadata").to_string(),
            Mode::Data => tr("Search data").to_string(),
        }
    }

    fn icon(&self) -> &'static str {
        match self.mode {
            Mode::Metadata => icon::MAGNIFYING_GLASS,
            Mode::Data => icon::BINOCULARS,
        }
    }

    fn connection_id(&self) -> Option<&str> {
        self.conn.as_deref()
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        self.poll_metadata();
        self.poll_data();
        ui.spacing_mut().item_spacing.y = 0.0;
        match self.mode {
            Mode::Metadata => self.metadata_ui(ui, cx),
            Mode::Data => self.data_ui(ui, cx),
        }
        if self.is_busy() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    fn on_close(&mut self, cx: &mut AppContext) -> bool {
        self.stop_data(cx);
        true
    }

    crate::impl_tab_any!();
}
