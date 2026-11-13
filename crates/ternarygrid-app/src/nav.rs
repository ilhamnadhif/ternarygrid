//! Database navigator: connections → databases → schemas → object folders →
//! objects → child folders → children. Objects load lazily and are cached per
//! connection (FR-N1..N3). The visible tree is flattened every frame and drawn
//! with row virtualisation, so schemas with thousands of tables stay fast.

use crate::backend::Loadable;
use crate::command::{AppCommand, ObjectPage};
use crate::conn::{ConnStatus, LiveConnection};
use crate::cx::AppContext;
use egui::{Color32, Key, RichText, Sense, Ui};
use egui_phosphor::regular as icon;
use std::collections::HashSet;
use ternarygrid_core::catalog::{CatalogContext, CatalogFolder, CatalogGroup, CatalogIcon, CatalogScope};
use ternarygrid_core::config::{ConnectionConfig, NavigatorView, wildcard_match};
use ternarygrid_core::meta::{ObjectInfo, ObjectKind, ObjectRef, SchemaRef};
use ternarygrid_core::{DbKind, tr};

/// Identity of a tree node (used for expansion and selection state).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NavNode {
    Folder(String),
    Connection(String),
    Database {
        conn: String,
        db: String,
    },
    Schema {
        conn: String,
        schema: SchemaRef,
    },
    KindFolder {
        conn: String,
        schema: SchemaRef,
        kind: ObjectKind,
    },
    Object {
        conn: String,
        obj: ObjectRef,
    },
    ChildFolder {
        conn: String,
        parent: ObjectRef,
        kind: ObjectKind,
    },
    Child {
        conn: String,
        obj: ObjectRef,
    },
    Extensions {
        conn: String,
        db: Option<String>,
    },
    /// Group of server-level catalog folders (Security, Storage, Administer, System Info).
    CatalogGroup {
        conn: String,
        group: CatalogGroup,
    },
    /// A catalog folder (roles, settings …); `ctx` holds its database, table or parent object.
    CatalogFolder {
        conn: String,
        folder: &'static str,
        ctx: CatalogContext,
    },
    /// An object of a catalog folder (`ctx.name`).
    CatalogItem {
        conn: String,
        folder: &'static str,
        ctx: CatalogContext,
    },
}

impl NavNode {
    pub fn connection(&self) -> Option<&str> {
        match self {
            NavNode::Folder(_) => None,
            NavNode::Connection(c)
            | NavNode::Database { conn: c, .. }
            | NavNode::Schema { conn: c, .. }
            | NavNode::KindFolder { conn: c, .. }
            | NavNode::Object { conn: c, .. }
            | NavNode::ChildFolder { conn: c, .. }
            | NavNode::Child { conn: c, .. }
            | NavNode::Extensions { conn: c, .. }
            | NavNode::CatalogGroup { conn: c, .. }
            | NavNode::CatalogFolder { conn: c, .. }
            | NavNode::CatalogItem { conn: c, .. } => Some(c),
        }
    }
}

#[derive(Clone)]
struct Row {
    node: NavNode,
    depth: usize,
    label: String,
    detail: Option<String>,
    icon: &'static str,
    icon_color: Option<Color32>,
    /// A dot on the icon (connection status).
    badge: Option<Color32>,
    /// The database's logo instead of `icon` (connections).
    logo: Option<DbKind>,
    /// Background of the row: the connection type's colour (DBeaver-like).
    tint: Option<Color32>,
    /// A table's size right-aligned (as in DBeaver: "2.4M") and its hover text.
    size: Option<(String, String)>,
    expandable: bool,
    expanded: bool,
    /// Placeholder rows: "Loading…", errors, empty markers.
    kind: RowKind,
}

#[derive(Clone, PartialEq)]
enum RowKind {
    Node,
    Loading,
    Error(String),
    Empty,
    /// Why a connection failed, with Retry and Edit connection under it: a block of several
    /// rows (`part` of them), each drawing its slice of it.
    Failed {
        conn: String,
        message: String,
        part: usize,
    },
}

/// The block under a failed connection: its message (the code in the error colour, the text
/// muted, at most three lines) and the Retry / Edit connection links, laid out for `width`.
struct FailedBlock {
    message: std::sync::Arc<egui::Galley>,
    height: f32,
}

impl FailedBlock {
    const TOP: f32 = 5.0;
    const LINKS_H: f32 = 22.0;

    fn layout(ui: &Ui, message: &str, width: f32) -> FailedBlock {
        let p = ternarygrid_ui::pal(ui);
        let size = 12.5;
        // "[SQLITE_CANTOPEN] Cannot open …": the code apart from the message.
        let (code, text) = match message.strip_prefix('[').and_then(|m| m.split_once(']')) {
            Some((code, rest)) => (Some(code), rest.trim_start()),
            None => (None, message),
        };
        let mut job = egui::text::LayoutJob::default();
        if let Some(code) = code {
            job.append(code, 0.0, egui::TextFormat::simple(ternarygrid_ui::style::medium(ui.ctx(), size), p.err));
            job.append(" ", 0.0, egui::TextFormat::simple(egui::FontId::proportional(size), p.text_muted));
        }
        job.append(text.lines().collect::<Vec<_>>().join(" ").as_str(), 0.0, egui::TextFormat::simple(egui::FontId::proportional(size), p.text_muted));
        job.wrap = egui::text::TextWrapping { max_width: width.max(40.0), max_rows: 3, break_anywhere: false, overflow_character: Some('…') };
        let message = ui.painter().layout_job(job);
        let height = Self::TOP + message.size().y + 2.0 + Self::LINKS_H;
        FailedBlock { message, height }
    }
}

#[derive(Default)]
pub struct Navigator {
    expanded: HashSet<NavNode>,
    pub selected: Option<NavNode>,
    /// Object to select once its row exists (Link with editor), with a deadline.
    pending_reveal: Option<(String, ObjectRef, std::time::Instant)>,
    pub filter: String,
    rows: Vec<Row>,
    scroll_to_selected: bool,
    dark: bool,
    /// Navigator preferences (copied from the settings every frame).
    prefs: ternarygrid_config::settings::NavigatorSettings,
    /// The last click was in the navigator: Refresh (F5) is its own, not the active tab's.
    pub has_focus: bool,
}

/// Text next to a connection: `user@host:port/db`, or for a database file
/// its folder (home as `~`, long paths shortened to the last folders).
pub(crate) fn connection_detail(cfg: &ConnectionConfig) -> String {
    if !cfg.kind.is_file_based() {
        return cfg.summary();
    }
    let Some(path) = cfg.file_path.as_deref().filter(|p| !p.is_empty()) else { return String::new() };
    let path = std::path::Path::new(path);
    let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = path.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
    let home = std::env::var("HOME").ok().filter(|h| h.len() > 1);
    let dir = match home {
        // Whole path components: HOME /Users/al is no prefix of /Users/alice.
        Some(h) if std::path::Path::new(&dir).starts_with(&h) => format!("~{}", &dir[h.trim_end_matches('/').len()..]),
        _ => dir,
    };
    let dir = if dir.chars().count() > 32 {
        let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
        format!("…/{}", parts[parts.len().saturating_sub(2)..].join("/"))
    } else {
        dir
    };
    // The name is usually the file name: then the folder is enough.
    if cfg.name == file { dir } else { format!("{dir}/{file}") }
}

/// A size as DBeaver shows it in the navigator: `512`, `16K`, `2.4M`, `57M`, `1.2G`.
/// Size shown at the right of a table or partition (as in DBeaver), with the
/// size, estimated rows and its detail (bounds, engine …) on hover.
fn size_badge(o: &ObjectInfo) -> Option<(String, String)> {
    let b = o.size_bytes.filter(|b| *b >= 0)?;
    let mut tip = format!("{}: {}", tr("Size"), long_size(b));
    if let Some(r) = o.row_estimate.filter(|r| *r >= 0) {
        tip.push_str(&format!("\n{}: ~{r}", tr("Estimated rows")));
    }
    if let Some(d) = &o.detail {
        tip.push_str(&format!("\n{d}"));
    }
    Some((tree_size(b), tip))
}

/// A size as the design shows it at the right of a table: `976 KB`, `4.4 MB`, `31 MB`.
fn tree_size(bytes: i64) -> String {
    match long_size(bytes) {
        s if s.ends_with(" bytes") => s.replace(" bytes", " B"),
        s => s,
    }
}

fn short_size(bytes: i64) -> String {
    let mut v = bytes as f64;
    for unit in ["", "K", "M", "G", "T"] {
        if v < 1024.0 || unit == "T" {
            return if unit.is_empty() || v >= 10.0 { format!("{}{unit}", v.round()) } else { format!("{:.1}{unit}", v).replace(".0", "") };
        }
        v /= 1024.0;
    }
    unreachable!()
}

/// A size with its unit for hover texts (`2.4 MB`, `812 bytes`).
pub(crate) fn long_size(bytes: i64) -> String {
    match short_size(bytes) {
        s if s.ends_with(|c: char| c.is_ascii_digit()) => format!("{s} bytes"),
        s => format!("{} {}B", &s[..s.len() - 1], &s[s.len() - 1..]),
    }
}

pub fn kind_icon(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Database => icon::DATABASE,
        ObjectKind::Schema => icon::STACK,
        ObjectKind::Table | ObjectKind::Partition => icon::TABLE,
        ObjectKind::View => icon::EYE,
        ObjectKind::MaterializedView => icon::EYE_CLOSED,
        ObjectKind::ForeignTable => icon::GLOBE,
        ObjectKind::Column => icon::COLUMNS,
        ObjectKind::Index => icon::LIGHTNING,
        ObjectKind::PrimaryKey => icon::KEY,
        ObjectKind::UniqueKey => icon::FINGERPRINT,
        ObjectKind::CheckConstraint => icon::CHECK_SQUARE,
        ObjectKind::ForeignKey => icon::LINK,
        ObjectKind::Trigger => icon::LIGHTNING_A,
        ObjectKind::Sequence => icon::LIST_NUMBERS,
        ObjectKind::Function | ObjectKind::Aggregate => icon::FUNCTION,
        ObjectKind::Procedure => icon::GEAR_SIX,
        ObjectKind::Type | ObjectKind::Domain => icon::SHAPES,
        ObjectKind::Extension => icon::PUZZLE_PIECE,
        ObjectKind::Event => icon::CALENDAR,
        ObjectKind::Synonym => icon::ARROW_BEND_UP_RIGHT,
        ObjectKind::Role => icon::USER,
        ObjectKind::Tablespace => icon::HARD_DRIVES,
    }
}

pub fn db_icon(kind: DbKind) -> &'static str {
    match kind {
        DbKind::Sqlite => icon::FILE_SQL,
        DbKind::ClickHouse => icon::CHART_BAR,
        _ => icon::DATABASE,
    }
}

/// The connection filter that applies to objects of `kind`.
pub fn name_filter(f: &ternarygrid_core::config::ObjectFilters, kind: ObjectKind) -> &ternarygrid_core::config::NameFilter {
    match kind {
        ObjectKind::Database => &f.databases,
        ObjectKind::Schema => &f.schemas,
        ObjectKind::Table | ObjectKind::Partition | ObjectKind::ForeignTable => &f.tables,
        ObjectKind::View | ObjectKind::MaterializedView => &f.views,
        k if k.is_routine() => &f.routines,
        _ => &f.other,
    }
}

/// Mutable [`name_filter`].
pub fn name_filter_mut(f: &mut ternarygrid_core::config::ObjectFilters, kind: ObjectKind) -> &mut ternarygrid_core::config::NameFilter {
    match kind {
        ObjectKind::Database => &mut f.databases,
        ObjectKind::Schema => &mut f.schemas,
        ObjectKind::Table | ObjectKind::Partition | ObjectKind::ForeignTable => &mut f.tables,
        ObjectKind::View | ObjectKind::MaterializedView => &mut f.views,
        k if k.is_routine() => &mut f.routines,
        _ => &mut f.other,
    }
}

fn filter_ok(cfg: &ConnectionConfig, kind: ObjectKind, name: &str) -> bool {
    let f = &cfg.filters;
    match kind {
        ObjectKind::Database => f.databases.matches(name),
        ObjectKind::Schema => f.schemas.matches(name),
        ObjectKind::Table | ObjectKind::Partition | ObjectKind::ForeignTable => f.tables.matches(name),
        ObjectKind::View | ObjectKind::MaterializedView => f.views.matches(name),
        k if k.is_routine() => f.routines.matches(name),
        _ => f.other.matches(name),
    }
}

impl Navigator {
    pub fn select(&mut self, node: NavNode) {
        self.selected = Some(node);
        self.scroll_to_selected = true;
    }

    /// Expand the path to `obj` and select it (once loaded).
    pub fn reveal(&mut self, conn: &str, obj: &ObjectRef, conns: &crate::conn::ConnectionManager) {
        let c = conn.to_string();
        self.expanded.insert(NavNode::Connection(c.clone()));
        // The folders the connection sits in.
        if let Some(folder) = conns.config(conn).and_then(|cfg| cfg.folder.clone()) {
            let mut path = String::new();
            for part in folder.split('/').filter(|p| !p.is_empty()) {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(part);
                self.expanded.insert(NavNode::Folder(path.clone()));
            }
        }
        // An object without a database is in the connection's current one (PostgreSQL,
        // SQL Server list databases first).
        let current = conns.live(conn).and_then(|l| l.current_database.clone());
        if let Some(db) = obj.database.clone().or(current.clone()) {
            self.expanded.insert(NavNode::Database { conn: c.clone(), db });
        }
        for database in [obj.database.clone(), current, None] {
            let schema = SchemaRef { database, schema: obj.schema.clone() };
            self.expanded.insert(NavNode::Schema { conn: c.clone(), schema: schema.clone() });
            self.expanded.insert(NavNode::KindFolder { conn: c.clone(), schema, kind: obj.kind });
        }
        self.pending_reveal = Some((c, obj.clone(), std::time::Instant::now() + std::time::Duration::from_secs(5)));
    }

    /// Select the pending revealed object when its row is there.
    fn finish_reveal(&mut self) {
        let Some((conn, obj, until)) = self.pending_reveal.clone() else { return };
        let same = |r: &&Row, db_exact: bool| match &r.node {
            NavNode::Object { conn: c, obj: o } => {
                *c == conn
                    && o.kind == obj.kind
                    && o.name == obj.name
                    && o.schema == obj.schema
                    && (o.database == obj.database || (!db_exact && (o.database.is_none() || obj.database.is_none())))
            }
            _ => false,
        };
        // The same database first; a reference without one means the default database.
        let found = self.rows.iter().find(|r| same(r, true)).or_else(|| self.rows.iter().find(|r| same(r, false)));
        if let Some(r) = found {
            self.selected = Some(r.node.clone());
            self.scroll_to_selected = true;
            self.pending_reveal = None;
        } else if std::time::Instant::now() > until {
            self.pending_reveal = None;
        }
    }

    pub fn expand(&mut self, node: NavNode) {
        self.expanded.insert(node);
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
    }

    fn filter_matches(&self, text: &str) -> bool {
        if self.filter.trim().is_empty() {
            return true;
        }
        let f = self.filter.trim();
        if f.contains('*') || f.contains('?') { wildcard_match(f, text) } else { text.to_lowercase().contains(&f.to_lowercase()) }
    }

    /// Rebuild the flattened list of visible rows.
    fn build(&mut self, cx: &mut AppContext) {
        let mut rows = Vec::new();
        // Folders: explicit folders plus folders used by connections.
        let mut folders: Vec<String> = cx.conns.folders().to_vec();
        for c in cx.conns.configs() {
            if let Some(f) = &c.folder {
                let mut acc = String::new();
                for part in f.split('/').filter(|p| !p.is_empty()) {
                    if !acc.is_empty() {
                        acc.push('/');
                    }
                    acc.push_str(part);
                    if !folders.contains(&acc) {
                        folders.push(acc.clone());
                    }
                }
            }
        }
        folders.sort_by_key(|f| f.to_lowercase());
        let mut configs: Vec<ConnectionConfig> = cx.conns.configs().to_vec();
        configs.sort_by_key(|c| (c.sort_index.unwrap_or(u32::MAX), c.name.to_lowercase()));

        // Top level folders, then connections without folder.
        self.build_folder_level(cx, &mut rows, &folders, &configs, None, 0);
        self.rows = rows;
    }

    fn build_folder_level(&self, cx: &mut AppContext, rows: &mut Vec<Row>, folders: &[String], configs: &[ConnectionConfig], parent: Option<&str>, depth: usize) {
        for f in folders {
            let is_child = match parent {
                None => !f.contains('/'),
                Some(p) => f.strip_prefix(p).and_then(|r| r.strip_prefix('/')).is_some_and(|r| !r.contains('/')),
            };
            if !is_child {
                continue;
            }
            let node = NavNode::Folder(f.clone());
            let expanded = self.expanded.contains(&node) || !self.filter.trim().is_empty();
            let label = f.rsplit('/').next().unwrap_or(f).to_string();
            rows.push(Row {
                node: node.clone(),
                depth,
                label,
                detail: None,
                icon: if expanded { icon::FOLDER_OPEN } else { icon::FOLDER },
                icon_color: None,
                badge: None,
                logo: None,
                tint: None,
                size: None,
                expandable: true,
                expanded,
                kind: RowKind::Node,
            });
            if expanded {
                self.build_folder_level(cx, rows, folders, configs, Some(f), depth + 1);
            }
        }
        for c in configs.iter().filter(|c| c.folder.as_deref().filter(|f| !f.is_empty()) == parent) {
            self.build_connection(cx, rows, c, depth);
        }
    }

    fn build_connection(&self, cx: &mut AppContext, rows: &mut Vec<Row>, cfg: &ConnectionConfig, depth: usize) {
        let node = NavNode::Connection(cfg.id.clone());
        let status = cx.conns.status(&cfg.id);
        let unfolded = self.expanded.contains(&node);
        let expanded = unfolded && matches!(status, ConnStatus::Connected | ConnStatus::Checking | ConnStatus::Lost);
        // Unfolded while connecting or after a failure: the state shows under the row.
        let pending = match &status {
            ConnStatus::Connecting if unfolded => Some(RowKind::Loading),
            ConnStatus::Failed(e) if unfolded => Some(RowKind::Failed { conn: cfg.id.clone(), message: e.clone(), part: 0 }),
            _ => None,
        };
        let filter_hit = self.filter_matches(&cfg.name);
        let dark = self.dark;
        let color = match &status {
            ConnStatus::Connected => Some(crate::theme::success_color(dark)),
            ConnStatus::Connecting | ConnStatus::Checking => Some(crate::theme::warning_color(dark)),
            ConnStatus::Failed(_) | ConnStatus::Lost => Some(crate::theme::error_color(dark)),
            ConnStatus::Disconnected => None,
        };
        let marker = cx.conns.color_of(&cfg.id).and_then(|c| crate::theme::marker(c, dark));
        let start = rows.len();
        rows.push(Row {
            node: node.clone(),
            depth,
            label: cfg.name.clone(),
            detail: self.prefs.show_connection_details.then(|| connection_detail(cfg)),
            icon: db_icon(cfg.kind),
            icon_color: Some(crate::theme::logo_color(cfg.kind, dark)),
            badge: color,
            logo: Some(cfg.kind),
            tint: marker,
            size: None,
            expandable: true,
            expanded: expanded || pending.is_some(),
            kind: RowKind::Node,
        });
        if !expanded {
            if !filter_hit {
                rows.truncate(start);
            } else if let Some(kind) = pending {
                rows.push(placeholder(depth + 1, kind));
            }
            return;
        }
        let backend = cx.backend;
        let Some(live) = cx.conns.live_mut(&cfg.id) else { return };
        let caps = live.driver.info().capabilities.clone();
        let show_system = live.config.init.show_system_objects || self.prefs.show_system_objects;
        let mut child_rows = Vec::new();
        if caps.multiple_databases {
            live.meta.request_databases(backend, &live.main.clone());
            match &live.meta.databases {
                Loadable::Loaded(dbs) => {
                    let current = live.current_database.clone();
                    let show_all = live.config.init.show_all_databases;
                    let mut dbs = dbs.clone();
                    dbs.retain(|d| (show_all || Some(&d.name) == current.as_ref() || d.is_current) && (show_system || !d.is_system));
                    dbs.retain(|d| filter_ok(&live.config, ObjectKind::Database, &d.name));
                    if dbs.is_empty()
                        && let Some(cur) = current.clone()
                    {
                        dbs.push(ternarygrid_core::meta::DatabaseInfo { name: cur, is_current: true, ..Default::default() });
                    }
                    for d in dbs {
                        self.build_database(backend, live, &mut child_rows, &cfg.id, &d.name, depth + 1, &caps, show_system);
                    }
                }
                Loadable::Failed(e) => child_rows.push(placeholder(depth + 1, RowKind::Error(e.to_string()))),
                _ => child_rows.push(placeholder(depth + 1, RowKind::Loading)),
            }
        } else {
            self.build_schemas(backend, live, &mut child_rows, &cfg.id, None, depth + 1, &caps, show_system);
        }
        self.push_server_catalog(live, &mut child_rows, &cfg.id, depth + 1, !caps.multiple_databases);
        if !filter_hit && !child_rows.iter().any(|r| r.kind == RowKind::Node) {
            rows.truncate(start);
            return;
        }
        rows.extend(child_rows);
    }

    #[allow(clippy::too_many_arguments)]
    fn build_database(
        &self,
        backend: &crate::backend::Backend,
        live: &mut LiveConnection,
        rows: &mut Vec<Row>,
        conn: &str,
        db: &str,
        depth: usize,
        caps: &ternarygrid_core::driver::Capabilities,
        show_system: bool,
    ) {
        let node = NavNode::Database { conn: conn.to_string(), db: db.to_string() };
        let expanded = self.expanded.contains(&node);
        let is_current = Some(db) == live.current_database.as_deref();
        rows.push(Row {
            node: node.clone(),
            depth,
            label: db.to_string(),
            detail: if is_current { Some(tr("current").to_string()) } else { None },
            icon: icon::DATABASE,
            icon_color: None,
            badge: None,
            logo: None,
            tint: None,
            size: None,
            expandable: true,
            expanded,
            kind: RowKind::Node,
        });
        if expanded {
            let db_opt = if is_current { None } else { Some(db) };
            self.build_schemas(backend, live, rows, conn, db_opt.map(|s| s.to_string()).as_deref(), depth + 1, caps, show_system);
            if live.kind().is_postgres_family() {
                let ext = NavNode::Extensions { conn: conn.to_string(), db: db_opt.map(str::to_string) };
                let ext_expanded = self.expanded.contains(&ext);
                rows.push(Row {
                    node: ext.clone(),
                    depth: depth + 1,
                    label: tr(ObjectKind::Extension.plural_label()).to_string(),
                    detail: None,
                    icon: icon::PUZZLE_PIECE,
                    icon_color: None,
                    badge: None,
                    logo: None,
                    tint: None,
                    size: None,
                    expandable: true,
                    expanded: ext_expanded,
                    kind: RowKind::Node,
                });
                if ext_expanded {
                    let schema = SchemaRef { database: db_opt.map(str::to_string), schema: None };
                    if let Some(session) = live.ensure_session(backend, db_opt) {
                        live.meta.request_objects(backend, &session, &schema, ObjectKind::Extension);
                    }
                    self.push_objects(live, rows, conn, &schema, ObjectKind::Extension, depth + 2, false);
                }
            }
            let folders: Vec<CatalogFolder> = live.catalog_folders.iter().filter(|f| f.scope == CatalogScope::Database).cloned().collect();
            let ctx = CatalogContext { database: db_opt.map(str::to_string), ..Default::default() };
            self.push_catalog_folders(live, rows, conn, folders, ctx, depth + 1);
        }
    }

    /// Server-level catalog groups (and the database-level folders of
    /// connections without a database level).
    fn push_server_catalog(&self, live: &mut LiveConnection, rows: &mut Vec<Row>, conn: &str, depth: usize, with_database_scope: bool) {
        let all = live.catalog_folders.clone();
        if with_database_scope {
            let folders: Vec<CatalogFolder> = all.iter().filter(|f| f.scope == CatalogScope::Database).cloned().collect();
            self.push_catalog_folders(live, rows, conn, folders, CatalogContext::default(), depth);
        }
        for group in [CatalogGroup::Security, CatalogGroup::Storage, CatalogGroup::Administer, CatalogGroup::SystemInfo] {
            let folders: Vec<CatalogFolder> = all.iter().filter(|f| f.scope == CatalogScope::Server && f.group == group).cloned().collect();
            if folders.is_empty() {
                continue;
            }
            let node = NavNode::CatalogGroup { conn: conn.to_string(), group };
            let expanded = self.expanded.contains(&node);
            let filtering = !self.filter.trim().is_empty();
            if filtering && !expanded {
                continue;
            }
            let start = rows.len();
            let group_icon = match group {
                CatalogGroup::Security => icon::SHIELD_CHECK,
                CatalogGroup::Storage => icon::HARD_DRIVES,
                CatalogGroup::Administer => icon::WRENCH,
                _ => icon::INFO,
            };
            rows.push(Row {
                node,
                depth,
                label: tr(group.label()).to_string(),
                detail: None,
                icon: group_icon,
                icon_color: None,
                badge: None,
                logo: None,
                tint: None,
                size: None,
                expandable: true,
                expanded,
                kind: RowKind::Node,
            });
            if expanded {
                self.push_catalog_folders(live, rows, conn, folders, CatalogContext::default(), depth + 1);
            }
            if filtering && rows.len() == start + 1 {
                rows.truncate(start);
            }
        }
    }

    /// Catalog folders and, when expanded, their objects and sub-folders.
    fn push_catalog_folders(&self, live: &mut LiveConnection, rows: &mut Vec<Row>, conn: &str, folders: Vec<CatalogFolder>, base: CatalogContext, depth: usize) {
        let filtering = !self.filter.trim().is_empty();
        for f in folders {
            let node = NavNode::CatalogFolder { conn: conn.to_string(), folder: f.id, ctx: base.clone() };
            let expanded = !f.table_only && self.expanded.contains(&node);
            // Filtering: only the unfolded folders that have a match.
            if filtering && !expanded {
                continue;
            }
            let folder_row = rows.len();
            rows.push(Row {
                node,
                depth,
                label: tr(f.label).to_string(),
                detail: None,
                icon: if f.table_only { icon::TABLE } else { catalog_icon(f.icon) },
                icon_color: None,
                badge: None,
                logo: None,
                tint: None,
                size: None,
                expandable: !f.table_only,
                expanded,
                kind: RowKind::Node,
            });
            if !expanded {
                continue;
            }
            let items = match live.meta.catalog.get(&(f.id, base.clone())) {
                Some(Loadable::Loaded(v)) => v.clone(),
                Some(Loadable::Failed(e)) => {
                    rows.push(placeholder(depth + 1, RowKind::Error(e.to_string())));
                    continue;
                }
                _ => {
                    rows.push(placeholder(depth + 1, RowKind::Loading));
                    continue;
                }
            };
            let mut shown = 0;
            for it in items.iter().filter(|it| self.filter_matches(&it.name)) {
                shown += 1;
                let ctx = CatalogContext { name: Some(it.name.clone()), ..base.clone() };
                let item = NavNode::CatalogItem { conn: conn.to_string(), folder: f.id, ctx };
                let has_children = !f.children.is_empty();
                let item_expanded = has_children && self.expanded.contains(&item);
                rows.push(Row {
                    node: item,
                    depth: depth + 1,
                    label: it.name.clone(),
                    detail: it.detail.clone(),
                    icon: catalog_icon(f.icon),
                    icon_color: None,
                    badge: None,
                    logo: None,
                    tint: None,
                    size: None,
                    expandable: has_children,
                    expanded: item_expanded,
                    kind: RowKind::Node,
                });
                if item_expanded {
                    let children: Vec<CatalogFolder> = f.children.iter().filter_map(|id| live.catalog_folder(id).cloned()).collect();
                    let child_ctx = CatalogContext { parent: Some(it.name.clone()), name: None, ..base.clone() };
                    self.push_catalog_folders(live, rows, conn, children, child_ctx, depth + 2);
                }
            }
            if shown == 0 && !filtering {
                rows.push(placeholder(depth + 1, RowKind::Empty));
            } else if shown == 0 {
                rows.truncate(folder_row);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_schemas(
        &self,
        backend: &crate::backend::Backend,
        live: &mut LiveConnection,
        rows: &mut Vec<Row>,
        conn: &str,
        db: Option<&str>,
        depth: usize,
        caps: &ternarygrid_core::driver::Capabilities,
        show_system: bool,
    ) {
        let Some(session) = live.ensure_session(backend, db) else {
            let row = match live.session_error(db) {
                Some(e) => RowKind::Error(e),
                None => RowKind::Loading,
            };
            rows.push(placeholder(depth, row));
            return;
        };
        live.meta.request_schemas(backend, &session, db);
        let schemas = match live.meta.schemas_of(db) {
            Loadable::Loaded(s) => s.clone(),
            Loadable::Failed(e) => {
                rows.push(placeholder(depth, RowKind::Error(e.to_string())));
                return;
            }
            _ => {
                rows.push(placeholder(depth, RowKind::Loading));
                return;
            }
        };
        let mut schemas: Vec<_> = schemas.into_iter().filter(|s| (show_system || !s.is_system) && filter_ok(&live.config, ObjectKind::Schema, &s.name)).collect();
        schemas.sort_by(|a, b| b.is_current.cmp(&a.is_current).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        // SQLite with only "main": show folders directly.
        let flatten = matches!(live.kind(), DbKind::Sqlite | DbKind::Elasticsearch | DbKind::Neo4j) && schemas.len() == 1;
        for s in schemas {
            let schema_ref = SchemaRef { database: db.map(str::to_string), schema: Some(s.name.clone()) };
            if flatten {
                self.build_kind_folders(backend, live, rows, conn, &schema_ref, depth, caps);
                continue;
            }
            let node = NavNode::Schema { conn: conn.to_string(), schema: schema_ref.clone() };
            let expanded = self.expanded.contains(&node);
            let start = rows.len();
            rows.push(Row {
                node,
                depth,
                label: s.name.clone(),
                detail: if s.is_current { Some(tr("default").to_string()) } else { None },
                icon: if caps.schema_label == "Databases" { icon::DATABASE } else { icon::STACK },
                icon_color: None,
                badge: None,
                logo: None,
                tint: None,
                size: None,
                expandable: true,
                expanded,
                kind: RowKind::Node,
            });
            if expanded {
                self.build_kind_folders(backend, live, rows, conn, &schema_ref, depth + 1, caps);
            } else if !self.filter_matches(&s.name) {
                rows.truncate(start);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_kind_folders(
        &self,
        backend: &crate::backend::Backend,
        live: &mut LiveConnection,
        rows: &mut Vec<Row>,
        conn: &str,
        schema: &SchemaRef,
        depth: usize,
        caps: &ternarygrid_core::driver::Capabilities,
    ) {
        let simple = live.config.navigator_view == NavigatorView::Simple;
        for kind in caps.schema_object_kinds.iter().copied() {
            if simple && !matches!(kind, ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView) {
                continue;
            }
            let node = NavNode::KindFolder { conn: conn.to_string(), schema: schema.clone(), kind };
            let filtering = !self.filter.trim().is_empty();
            let expanded = self.expanded.contains(&node) || (filtering && matches!(kind, ObjectKind::Table | ObjectKind::View));
            let count = live.meta.objects.get(&(schema.clone(), kind)).and_then(|l| l.get()).map(|v| v.len());
            // Filtering: a folder whose objects are not loaded has nothing to show.
            if filtering && !expanded && count.is_none() {
                continue;
            }
            let folder_row = rows.len();
            rows.push(Row {
                node,
                depth,
                label: tr(live.driver.info().kind.folder_label(kind)).to_string(),
                detail: count.map(|c| c.to_string()),
                icon: icon::FOLDER_SIMPLE,
                icon_color: None,
                badge: None,
                logo: None,
                tint: None,
                size: None,
                expandable: true,
                expanded,
                kind: RowKind::Node,
            });
            if expanded {
                if let Some(session) = live.ensure_session(backend, schema.database.as_deref()) {
                    live.meta.request_objects(backend, &session, schema, kind);
                }
                // Redis keys have no columns or indexes to show.
                let leaf = live.config.kind == DbKind::Redis;
                let has_children = !leaf && (kind.is_table_like() || matches!(kind, ObjectKind::View | ObjectKind::MaterializedView));
                self.push_objects(live, rows, conn, schema, kind, depth + 1, has_children);
                if filtering && let Some(total) = count {
                    let shown = rows[folder_row + 1..].iter().filter(|r| r.depth == depth + 1 && r.kind == RowKind::Node).count();
                    // Filtering: the folders without a match are hidden.
                    if shown == 0 {
                        rows.truncate(folder_row);
                        continue;
                    }
                    rows[folder_row].detail = Some(ternarygrid_core::trf("{} of {}", &[&shown, &total]));
                }
                // expanded objects get their child folders
                let objs: Vec<ObjectInfo> = live.meta.objects.get(&(schema.clone(), kind)).and_then(|l| l.get()).cloned().unwrap_or_default();
                let _ = objs;
            } else if filtering {
                // Folded but loaded: shown with its matches only when it has some.
                let cfg = &live.config;
                let show_system = cfg.init.show_system_objects || self.prefs.show_system_objects;
                let list = live.meta.objects.get(&(schema.clone(), kind)).and_then(|l| l.get());
                let shown = list.map_or(0, |v| v.iter().filter(|o| (show_system || !o.is_system) && filter_ok(cfg, kind, &o.obj.name) && self.filter_matches(&o.obj.name)).count());
                if shown == 0 {
                    rows.truncate(folder_row);
                } else if let Some(total) = count {
                    rows[folder_row].detail = Some(ternarygrid_core::trf("{} of {}", &[&shown, &total]));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_objects(&self, live: &mut LiveConnection, rows: &mut Vec<Row>, conn: &str, schema: &SchemaRef, kind: ObjectKind, depth: usize, has_children: bool) {
        let show_system = live.config.init.show_system_objects || self.prefs.show_system_objects;
        let cfg = live.config.clone();
        let list = match live.meta.objects.get(&(schema.clone(), kind)) {
            Some(Loadable::Loaded(v)) => v.clone(),
            Some(Loadable::Failed(e)) => {
                rows.push(placeholder(depth, RowKind::Error(e.to_string())));
                return;
            }
            _ => {
                rows.push(placeholder(depth, RowKind::Loading));
                return;
            }
        };
        let mut shown = 0;
        let mut list = list;
        if self.prefs.sort == ternarygrid_config::settings::NavigatorSort::Alphabetical {
            list.sort_by_key(|o| o.obj.name.to_lowercase());
        }
        for o in list.iter().filter(|o| (show_system || !o.is_system) && filter_ok(&cfg, kind, &o.obj.name)) {
            if !self.filter_matches(&o.obj.name) {
                continue;
            }
            shown += 1;
            let node = NavNode::Object { conn: conn.to_string(), obj: o.obj.clone() };
            let expanded = has_children && self.expanded.contains(&node);
            let mut label = o.obj.name.clone();
            if let Some(sig) = &o.obj.signature
                && sig.starts_with('(')
            {
                label.push_str(sig);
            }
            rows.push(Row {
                node,
                depth,
                label,
                // Where the server tells a table's size, only the size shows (as in
                // DBeaver); "partitioned", the engine and the rows are on hover.
                detail: match o.size_bytes {
                    Some(_) => None,
                    None => o.detail.clone().or_else(|| o.row_estimate.filter(|r| *r >= 0).map(|r| format!("~{r}"))),
                },
                icon: kind_icon(kind),
                icon_color: None,
                badge: None,
                logo: None,
                tint: None,
                size: size_badge(o),
                expandable: has_children,
                expanded,
                kind: RowKind::Node,
            });
            if expanded {
                self.push_child_folders(live, rows, conn, &o.obj, depth + 1);
            }
        }
        if shown == 0 && self.filter.trim().is_empty() {
            rows.push(placeholder(depth, RowKind::Empty));
        }
    }

    fn push_child_folders(&self, live: &mut LiveConnection, rows: &mut Vec<Row>, conn: &str, obj: &ObjectRef, depth: usize) {
        let simple = live.config.navigator_view == NavigatorView::Simple;
        let caps = live.driver.info().capabilities.clone();
        let kinds: Vec<ObjectKind> = if obj.kind.is_table_like() { caps.table_child_kinds.clone() } else { vec![ObjectKind::Column] };
        for kind in kinds {
            if simple && kind != ObjectKind::Column {
                continue;
            }
            let node = NavNode::ChildFolder { conn: conn.to_string(), parent: obj.clone(), kind };
            let expanded = self.expanded.contains(&node);
            rows.push(Row {
                node,
                depth,
                label: tr(live.driver.info().kind.folder_label(kind)).to_string(),
                detail: None,
                icon: icon::FOLDER_SIMPLE,
                icon_color: None,
                badge: None,
                logo: None,
                tint: None,
                size: None,
                expandable: true,
                expanded,
                kind: RowKind::Node,
            });
            if expanded {
                match live.meta.children.get(&(obj.clone(), kind)) {
                    Some(Loadable::Loaded(v)) => {
                        if v.is_empty() {
                            rows.push(placeholder(depth + 1, RowKind::Empty));
                        }
                        for c in v {
                            rows.push(Row {
                                node: NavNode::Child { conn: conn.to_string(), obj: c.obj.clone() },
                                depth: depth + 1,
                                label: c.obj.name.clone(),
                                // A partition with its size shows only that, as a table (its bounds on hover).
                                detail: if c.size_bytes.is_some() { None } else { c.detail.clone() },
                                icon: kind_icon(c.obj.kind),
                                icon_color: None,
                                badge: None,
                                logo: None,
                                tint: None,
                                size: size_badge(c),
                                expandable: false,
                                expanded: false,
                                kind: RowKind::Node,
                            });
                        }
                    }
                    Some(Loadable::Failed(e)) => rows.push(placeholder(depth + 1, RowKind::Error(e.to_string()))),
                    _ => rows.push(placeholder(depth + 1, RowKind::Loading)),
                }
            }
        }
        if obj.kind.is_table_like() && !simple {
            let folders: Vec<CatalogFolder> = live.catalog_folders.iter().filter(|f| f.scope == CatalogScope::Table).cloned().collect();
            let ctx = CatalogContext { database: obj.database.clone(), schema: obj.schema.clone(), table: Some(obj.name.clone()), ..Default::default() };
            self.push_catalog_folders(live, rows, conn, folders, ctx, depth);
        }
    }

    /// Request loads for newly expanded child folders (needs a session).
    fn request_child_loads(&self, cx: &mut AppContext) {
        let backend = cx.backend;
        for node in &self.expanded {
            if let NavNode::ChildFolder { conn, parent, kind } = node
                && let Some(live) = cx.conns.live_mut(conn)
                && let Some(session) = live.ensure_session(backend, parent.database.as_deref())
            {
                live.meta.request_children(backend, &session, parent, *kind);
            }
            if let NavNode::CatalogFolder { conn, folder, ctx } = node
                && let Some(live) = cx.conns.live_mut(conn)
                && let Some(f) = live.catalog_folder(folder).cloned()
                && !f.table_only
                && let Some(session) = live.ensure_session(backend, ctx.database.as_deref())
            {
                live.meta.request_catalog(backend, &session, &f, ctx);
            }
        }
    }

    /// Properties of the selected node (Properties panel).
    pub fn selected_properties(&self, cx: &AppContext) -> Vec<(String, String)> {
        let Some(node) = &self.selected else { return Vec::new() };
        let mut out: Vec<(String, String)> = Vec::new();
        let mut add = |k: &str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                out.push((tr(k).to_string(), v));
            }
        };
        let yes_no = |b: bool| Some(if b { tr("Yes") } else { tr("No") }.to_string());
        match node {
            NavNode::Folder(f) => add("Folder", Some(f.clone())),
            NavNode::Connection(c) => {
                let Some(cfg) = cx.conns.config(c) else { return Vec::new() };
                add("Name", Some(cfg.name.clone()));
                add("Database type", Some(cfg.kind.display_name().to_string()));
                add("Host", Some(cfg.host.clone()));
                add("Port", cfg.port.map(|p| p.to_string()));
                add("File", cfg.file_path.clone());
                add("Database", cfg.database.clone());
                add("User", cfg.user.clone());
                add("Connection type", Some(cx.conns.type_of(c).name));
                add("Read-only", yes_no(cfg.read_only));
                add("Description", cfg.description.clone());
                match cx.conns.live(c) {
                    Some(live) => {
                        add("Server", Some(format!("{} {}", live.server.product, live.server.version)));
                        add("Session", live.server.session_id.clone());
                    }
                    None => add("Status", Some(tr("not connected").to_string())),
                }
            }
            NavNode::Database { conn, db } => {
                add("Database", Some(db.clone()));
                add("Connection", cx.conns.config(conn).map(|c| c.name.clone()));
            }
            NavNode::Schema { conn, schema } => {
                add("Schema", schema.schema.clone());
                add("Database", schema.database.clone());
                add("Connection", cx.conns.config(conn).map(|c| c.name.clone()));
            }
            NavNode::KindFolder { conn, schema, kind } => {
                add("Folder", Some(tr(kind.plural_label()).to_string()));
                add("Schema", schema.schema.clone());
                let n = cx.conns.live(conn).and_then(|l| l.meta.objects.get(&(schema.clone(), *kind))).and_then(|l| l.get()).map(|v| v.len());
                add("Objects", n.map(|n| n.to_string()));
            }
            NavNode::ChildFolder { conn, parent, kind } => {
                add("Folder", Some(tr(kind.plural_label()).to_string()));
                add("Table", Some(parent.name.clone()));
                let n = cx.conns.live(conn).and_then(|l| l.meta.children.get(&(parent.clone(), *kind))).and_then(|l| l.get()).map(|v| v.len());
                add("Objects", n.map(|n| n.to_string()));
            }
            NavNode::Object { conn, obj } | NavNode::Child { conn, obj } => {
                add("Name", Some(obj.name.clone()));
                add("Type", Some(tr(obj.kind.label()).to_string()));
                add("Table", obj.table.clone());
                add("Schema", obj.schema.clone());
                add("Database", obj.database.clone());
                add("Signature", obj.signature.clone());
                if let Some(live) = cx.conns.live(conn) {
                    let info = live.meta.objects.values().chain(live.meta.children.values()).filter_map(|l| l.get()).flatten().find(|i| i.obj == *obj);
                    if let Some(info) = info {
                        add("Details", info.detail.clone());
                        add("Comment", info.comment.clone());
                        add("Estimated rows", info.row_estimate.filter(|n| *n >= 0).map(|n| n.to_string()));
                        if info.is_system {
                            add("System object", yes_no(true));
                        }
                    }
                    if let Some(def) = live.meta.table_def(obj) {
                        add("Columns", Some(def.columns.len().to_string()));
                    }
                }
            }
            NavNode::Extensions { db, .. } => {
                add("Folder", Some(tr("Extensions").to_string()));
                add("Database", db.clone());
            }
            NavNode::CatalogGroup { group, .. } => add("Folder", Some(tr(group.label()).to_string())),
            NavNode::CatalogFolder { conn, folder, ctx } => {
                let label = cx.conns.live(conn).and_then(|l| l.catalog_folder(folder)).map(|f| tr(f.label).to_string());
                add("Folder", label);
                let n = cx.conns.live(conn).and_then(|l| l.meta.catalog.get(&(*folder, ctx.clone()))).and_then(|l| l.get()).map(|v| v.len());
                add("Objects", n.map(|n| n.to_string()));
            }
            NavNode::CatalogItem { conn, folder, ctx } => {
                add("Name", ctx.name.clone());
                let list_ctx = CatalogContext { name: None, ..ctx.clone() };
                let row = cx
                    .conns
                    .live(conn)
                    .and_then(|l| l.meta.catalog.get(&(*folder, list_ctx)))
                    .and_then(|l| l.get())
                    .and_then(|rows| rows.iter().find(|r| Some(&r.name) == ctx.name.as_ref()));
                if let Some(row) = row {
                    for (k, v) in row.values.iter().skip(1) {
                        if !v.is_null() && !k.eq_ignore_ascii_case("detail") {
                            out.push((k.replace('_', " "), v.to_text().into_owned()));
                        }
                    }
                }
            }
        }
        out
    }

    /// The properties of the selected object under the tree, as in the design: a 34-point
    /// header (an info icon and "Properties"), then name / value rows (12.5 points, the names
    /// muted in a 110-point column, every other row striped, long values wrapped).
    fn properties_ui(&self, ui: &mut Ui, cx: &AppContext) {
        let p = ternarygrid_ui::pal(ui);
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.horizontal(|ui| {
            ui.set_min_height(34.0);
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add_space(14.0);
            ui.label(RichText::new(icon::INFO).font(ternarygrid_ui::style::icon_font(14.0)).color(p.text_muted));
            ternarygrid_ui::kit::semibold_label(ui, tr("Properties"), 12.5, p.text);
        });
        // only a node of the tree (not one removed since it was selected)
        let in_tree = self.selected.as_ref().is_some_and(|sel| self.rows.iter().any(|r| &r.node == sel));
        let props = if in_tree { self.selected_properties(cx) } else { Vec::new() };
        if props.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(RichText::new(tr("Select an object in the navigator.")).size(12.5).color(p.text_muted));
            });
            return;
        }
        egui::ScrollArea::vertical().id_salt("nav_properties_scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, (k, v)) in props.iter().enumerate() {
                let fill = if i % 2 == 1 { p.stripe } else { egui::Color32::TRANSPARENT };
                egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(14, 5)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.allocate_ui_with_layout(egui::vec2(110.0, 16.0), egui::Layout::left_to_right(egui::Align::Min), |ui| {
                            ui.set_min_width(110.0);
                            ui.add(egui::Label::new(RichText::new(k).size(12.5).color(p.text_muted)).truncate());
                        });
                        let first = v.lines().next().unwrap_or("");
                        let resp = ui.add(egui::Label::new(RichText::new(first).size(12.5).color(p.text)).wrap());
                        resp.context_menu(|ui| {
                            ternarygrid_ui::kit::menu_look(ui, 140.0);
                            if ternarygrid_ui::kit::menu_item(ui, Some(icon::COPY), tr("Copy"), "").clicked() {
                                ui.ctx().copy_text(v.clone());
                                ui.close();
                            }
                        });
                    });
                });
            }
            ui.add_space(6.0);
        });
    }

    /// The tree's buttons, right-aligned in `ui`'s row: as many of ⋯, New connection, Refresh,
    /// Connect / Disconnect, Collapse all and New folder (in that order) as fit in `room`
    /// points; ⋯ has the others, importing and exporting connections, and the panel's options.
    pub fn toolbar_ui(&mut self, ui: &mut Ui, cx: &mut AppContext, room: f32) {
        use ternarygrid_ui::kit;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let sel_conn = self.selected.as_ref().and_then(|n| n.connection()).map(str::to_string);
            let connected = sel_conn.as_deref().is_some_and(|c| cx.conns.is_connected(c));
            let failed = sel_conn.as_deref().is_some_and(|c| matches!(cx.conns.status(c), ConnStatus::Failed(_)));
            let sel_name = sel_conn.as_deref().and_then(|c| cx.conns.config(c)).map(|c| c.name.clone()).unwrap_or_default();
            // No connections (the empty state): New connection and "⋯" only.
            let has_connections = !cx.conns.configs().is_empty();
            let on_bar = |priority: usize| has_connections && priority < toolbar_buttons(room);
            let (show_refresh, show_plug, show_collapse, show_folder) = (on_bar(2), on_bar(3), on_bar(4), on_bar(5));
            let (plug, plug_label) = if connected {
                (icon::PLUGS, ternarygrid_core::trf("Disconnect {}", &[&sel_name]))
            } else if sel_conn.is_some() {
                (icon::PLUGS_CONNECTED, ternarygrid_core::trf("Connect {}", &[&sel_name]))
            } else {
                (icon::PLUGS_CONNECTED, tr("Connect").to_string())
            };
            let plug_command = |c: &str| if connected { AppCommand::Disconnect(c.to_string()) } else { AppCommand::Connect(c.to_string()) };
            // Refresh; for a connection that failed, connect again.
            let refresh_tip = if failed { tr("Connect again") } else { tr("Refresh") };
            let can_refresh = connected || failed;
            let more = kit::icon_button_sized(ui, icon::DOTS_THREE, tr("More"), false, 26.0, 15.0);
            egui::Popup::menu(&more).gap(4.0).show(|ui| {
                kit::menu_look(ui, 200.0);
                if !show_folder && kit::menu_item(ui, Some(icon::FOLDER_PLUS), tr("New folder…"), "").clicked() {
                    self.new_folder(cx);
                    ui.close();
                }
                if !show_refresh && has_connections && ui.add_enabled_ui(can_refresh, |ui| kit::menu_item(ui, Some(icon::ARROW_CLOCKWISE), refresh_tip, "")).inner.clicked() {
                    self.refresh_or_reconnect(failed, sel_conn.as_deref(), cx);
                    ui.close();
                }
                if !show_plug
                    && let Some(c) = &sel_conn
                    && kit::menu_item(ui, Some(plug), &plug_label, "").clicked()
                {
                    cx.push(plug_command(c));
                    ui.close();
                }
                if !show_collapse && kit::menu_item(ui, Some(icon::ARROWS_IN_SIMPLE), tr("Collapse all"), "").clicked() {
                    self.collapse_all();
                    ui.close();
                }
                if !(show_folder && show_collapse) {
                    kit::menu_separator(ui);
                }
                if kit::menu_item(ui, Some(icon::DOWNLOAD_SIMPLE), tr("Import connections…"), "").clicked() {
                    cx.push(AppCommand::ImportConnections);
                    ui.close();
                }
                if ui.add_enabled_ui(!cx.conns.configs().is_empty(), |ui| kit::menu_item(ui, Some(icon::UPLOAD_SIMPLE), tr("Export connections…"), "")).inner.clicked() {
                    cx.push(AppCommand::ExportConnections(None));
                    ui.close();
                }
                kit::menu_separator(ui);
                let mut props = cx.settings.navigator.show_properties;
                if kit::menu_check(ui, &mut props, tr("Properties panel")).changed() {
                    cx.settings.navigator.show_properties = props;
                    cx.push(AppCommand::SettingsChanged);
                }
                let mut link = cx.settings.navigator.link_with_editor;
                if kit::menu_check(ui, &mut link, tr("Link with editor")).on_hover_text(tr("Link with editor: select the object of the active tab")).changed() {
                    cx.settings.navigator.link_with_editor = link;
                    cx.push(AppCommand::SettingsChanged);
                }
            });
            if show_collapse && kit::icon_button_sized(ui, icon::ARROWS_IN_SIMPLE, tr("Collapse all"), false, 26.0, 15.0).clicked() {
                self.collapse_all();
            }
            // In place without a connection selected: the buttons do not move with the selection.
            if show_plug
                && ui.add_enabled_ui(sel_conn.is_some(), |ui| kit::icon_button_sized(ui, plug, &plug_label, false, 26.0, 15.0)).inner.clicked()
                && let Some(c) = &sel_conn
            {
                cx.push(plug_command(c));
            }
            if show_refresh && ui.add_enabled_ui(can_refresh, |ui| kit::icon_button_sized(ui, icon::ARROW_CLOCKWISE, refresh_tip, false, 26.0, 15.0)).inner.clicked() {
                self.refresh_or_reconnect(failed, sel_conn.as_deref(), cx);
            }
            if show_folder && kit::icon_button_sized(ui, icon::FOLDER_PLUS, tr("New folder…"), false, 26.0, 15.0).clicked() {
                self.new_folder(cx);
            }
            if kit::icon_button_sized(ui, icon::PLUS, tr("New connection"), false, 26.0, 15.0).clicked() {
                cx.push(AppCommand::Action(crate::keys::Action::NewConnection));
            }
        });
    }

    /// A new folder inside the selected folder, else at the top level.
    fn new_folder(&self, cx: &mut AppContext) {
        let parent = match &self.selected {
            Some(NavNode::Folder(f)) => Some(f.clone()),
            _ => None,
        };
        cx.push(AppCommand::NewFolder { parent });
    }

    /// The toolbar's Refresh: the selected node, or connecting again where that failed.
    fn refresh_or_reconnect(&mut self, failed: bool, connection: Option<&str>, cx: &mut AppContext) {
        match connection {
            Some(c) if failed => cx.push(AppCommand::Connect(c.to_string())),
            _ => self.refresh_selected(cx),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        self.dark = ui.visuals().dark_mode;
        if self.prefs != cx.settings.navigator {
            self.prefs = cx.settings.navigator.clone();
        }
        // The filter: 10 points from the header and the sides, 6 above the tree.
        let search = egui::Frame::new()
            .inner_margin(egui::Margin { left: 10, right: 10, top: 10, bottom: 6 })
            .show(ui, |ui| ternarygrid_ui::kit::search_field(ui, egui::Id::new("nav_filter"), &mut self.filter, tr("Filter objects"), ui.available_width()))
            .inner;
        if search.changed() {
            // keep selection visible
            self.scroll_to_selected = true;
        }
        if cx.settings.navigator.show_properties && !cx.conns.configs().is_empty() {
            let frame = egui::Frame::new().fill(ternarygrid_ui::pal(ui).canvas);
            egui::Panel::bottom("nav_properties").resizable(true).default_size(230.0).size_range(60.0..=600.0).frame(frame).show(ui, |ui| self.properties_ui(ui, cx));
        }

        self.request_child_loads(cx);
        self.build(cx);
        self.finish_reveal();
        if self.pending_reveal.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }

        if cx.conns.configs().is_empty() {
            ternarygrid_ui::kit::empty_state_at(ui, 32.0, icon::DATABASE, tr("No connections yet"), tr("Connect to a database to browse it here."), |ui| {
                if ternarygrid_ui::kit::button_ex(ui, ternarygrid_ui::kit::Kind::Secondary, Some(icon::PLUS), tr("New connection"), true).clicked() {
                    cx.push(AppCommand::NewConnection { kind: None, folder: None });
                }
                if ternarygrid_ui::kit::button_ex(ui, ternarygrid_ui::kit::Kind::Ghost, Some(icon::DOWNLOAD_SIMPLE), tr("Import connections…"), true)
                    .on_hover_text(tr("From DBeaver, ~/.pgpass or a connection URL"))
                    .clicked()
                {
                    cx.push(AppCommand::ImportConnections);
                }
            });
            return;
        }

        // Rows of 26 points a point apart, inset by 6 points (their rounded fill).
        let (row_h, pitch) = (26.0, 27.0);
        let rows = std::mem::take(&mut self.rows);
        // A failed connection's message takes as many rows as its lines need.
        let text_left = |depth: usize| 6.0 + 8.0 + depth as f32 * 14.0 + 18.0 + 19.0;
        let panel_w = ui.available_width();
        let mut failed_blocks = std::collections::HashMap::new();
        let rows: Vec<Row> = rows
            .into_iter()
            .flat_map(|r| match &r.kind {
                RowKind::Failed { conn, message, .. } => {
                    let block = FailedBlock::layout(ui, message, panel_w - text_left(r.depth) - 14.0);
                    let parts = (block.height / pitch).ceil().max(1.0) as usize;
                    failed_blocks.insert(conn.clone(), block);
                    (0..parts)
                        .map(|part| {
                            let mut r = placeholder(r.depth, RowKind::Failed { conn: conn.clone(), message: message.clone(), part });
                            r.node = NavNode::Connection(conn.clone());
                            r
                        })
                        .collect::<Vec<_>>()
                }
                _ => vec![r],
            })
            .collect();
        let mut picked = RowPick::default();
        let mut scroll_target = None;
        if self.scroll_to_selected {
            scroll_target = rows.iter().position(|r| Some(&r.node) == self.selected.as_ref());
            self.scroll_to_selected = false;
        }
        // Vertical only: a row is as wide as the panel (long names end in "…"); a
        // horizontal bar was always shown for a few pixels of overflow.
        let mut sa = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("nav_scroll");
        if let Some(i) = scroll_target {
            sa = sa.vertical_scroll_offset((i as f32 * pitch - 80.0).max(0.0));
        }
        let p = ternarygrid_ui::pal(ui);
        let item_gap = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);
        // The rows reach the panel's edges (their own inset is the design's 6 points).
        let full = ui.available_rect_before_wrap();
        let mut area = ui.new_child(egui::UiBuilder::new().max_rect(full).layout(egui::Layout::top_down(egui::Align::Min)));
        area.spacing_mut().item_spacing.y = 0.0;
        let mut content_top = None;
        let shown = sa.show_rows(&mut area, pitch, rows.len(), |ui, range| {
            // Where the content starts, for the frame being drawn (the output's offset already
            // has this frame's wheel, which moves the rows only in the next one).
            content_top = Some(ui.max_rect().top() - range.start as f32 * pitch);
            for i in range {
                let r = &rows[i];
                let (slot, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(200.0), pitch), Sense::hover());
                let rect = egui::Rect::from_min_size(slot.min, egui::vec2(slot.width(), row_h));
                let row_id = ui.id().with(("nav_row", i));
                if r.kind == RowKind::Node {
                    self.node_row(ui, cx, r, i, rect, row_id, &mut picked);
                    continue;
                }
                let resp = ui.interact(rect, row_id, Sense::click());
                resp.widget_info(|| match &r.kind {
                    RowKind::Node => unreachable!("drawn by node_row"),
                    RowKind::Loading => egui::WidgetInfo::labeled(egui::WidgetType::Label, true, tr("Loading…")),
                    RowKind::Error(e) => egui::WidgetInfo::labeled(egui::WidgetType::Label, true, e),
                    RowKind::Empty => egui::WidgetInfo::labeled(egui::WidgetType::Label, true, tr("(empty)")),
                    RowKind::Failed { message, .. } => egui::WidgetInfo::labeled(egui::WidgetType::Label, true, message),
                });
                let inner = rect.shrink2(egui::vec2(6.0, 0.0));
                let x = inner.left() + 8.0 + r.depth as f32 * 14.0 + 18.0;
                let y = inner.center().y;
                let small = egui::FontId::proportional(12.5);
                match &r.kind {
                    RowKind::Loading => {
                        // A turning spinner (the history's clock meant something else).
                        let angle = ui.input(|i| i.time) as f32 * 4.0;
                        let c = egui::pos2(x + 6.5, y);
                        let shape = egui::epaint::PathShape::line(
                            (0..=16).map(|k| c + 5.0 * egui::Vec2::angled(angle + k as f32 / 16.0 * std::f32::consts::TAU * 0.75)).collect(),
                            egui::Stroke::new(1.5, p.text_faint),
                        );
                        ui.painter().add(shape);
                        ui.painter().text(egui::pos2(x + 19.0, y), egui::Align2::LEFT_CENTER, tr("Loading…"), small, p.text_faint);
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                        continue;
                    }
                    RowKind::Error(e) => {
                        let resp = resp.on_hover_text(e.clone());
                        ui.painter().text(egui::pos2(x + 6.5, y), egui::Align2::CENTER_CENTER, icon::WARNING, ternarygrid_ui::style::icon_font(13.0), p.err);
                        let g = egui::WidgetText::from(RichText::new(e.lines().next().unwrap_or("")).font(small).color(p.err)).into_galley(
                            ui,
                            Some(egui::TextWrapMode::Truncate),
                            (inner.right() - x - 25.0).max(30.0),
                            egui::TextStyle::Body,
                        );
                        ui.painter().galley(egui::pos2(x + 19.0, y - g.size().y / 2.0), g, p.err);
                        let _ = resp;
                        continue;
                    }
                    RowKind::Empty => {
                        ui.painter().text(egui::pos2(x, y), egui::Align2::LEFT_CENTER, tr("(empty)"), small, p.text_faint);
                        continue;
                    }
                    RowKind::Failed { conn, message, part } => {
                        let Some(block) = failed_blocks.get(conn) else { continue };
                        // This row's slice of the block, which starts `part` rows higher.
                        let top = slot.top() - *part as f32 * pitch;
                        let painter = ui.painter().with_clip_rect(slot.intersect(ui.clip_rect()));
                        painter.text(
                            egui::pos2(x + 6.5, top + FailedBlock::TOP + 8.0),
                            egui::Align2::CENTER_CENTER,
                            icon::WARNING_CIRCLE,
                            ternarygrid_ui::style::icon_font(13.0),
                            p.err,
                        );
                        let text_pos = egui::pos2(x + 19.0, top + FailedBlock::TOP);
                        let text_rect = egui::Rect::from_min_size(text_pos, block.message.size());
                        painter.galley(text_pos, block.message.clone(), p.text_muted);
                        if slot.intersects(text_rect) && block.message.elided {
                            ui.interact(text_rect.intersect(slot), ui.id().with(("nav_failed_text", i)), Sense::hover()).on_hover_text(message.clone());
                        }
                        // Retry · Edit connection…
                        let mut lx = text_pos.x;
                        let ly = text_rect.bottom() + 2.0 + FailedBlock::LINKS_H / 2.0;
                        for (k, (label, command)) in
                            [(tr("Retry"), AppCommand::Connect(conn.clone())), (tr("Edit connection…"), AppCommand::EditConnection(conn.clone()))].into_iter().enumerate()
                        {
                            if k > 0 {
                                let dot = painter.layout_no_wrap("·".into(), small.clone(), p.text_faint);
                                painter.galley(egui::pos2(lx + 6.0, ly - dot.size().y / 2.0), dot.clone(), p.text_faint);
                                lx += 12.0 + dot.size().x;
                            }
                            let g = painter.layout_no_wrap(label.to_string(), ternarygrid_ui::style::medium(ui.ctx(), 12.5), p.accent_text);
                            let r = egui::Rect::from_min_size(egui::pos2(lx, ly - g.size().y / 2.0), g.size());
                            lx = r.right();
                            if !slot.contains(r.center()) {
                                continue;
                            }
                            let link = ui.interact(r.expand2(egui::vec2(3.0, 3.0)), ui.id().with(("nav_failed_link", i, k)), Sense::click());
                            link.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
                            if link.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                painter.hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, p.accent_text));
                            }
                            painter.galley(r.min, g, p.accent_text);
                            if link.clicked() {
                                cx.push(command);
                            }
                        }
                        continue;
                    }
                    RowKind::Node => {}
                }
            }
        });
        // The connection whose objects are scrolled through stays at the top, until the next
        // connection pushes it out.
        if let Some(top) = content_top
            && let Some(pin) = pinned_row(&rows, shown.inner_rect.top() - top, pitch)
        {
            let view = shown.inner_rect;
            let band = egui::Rect::from_min_size(view.min + egui::vec2(0.0, pin.shift), egui::vec2(view.width(), pitch));
            let mut pinned = area.new_child(egui::UiBuilder::new().max_rect(band).layout(egui::Layout::top_down(egui::Align::Min)));
            pinned.set_clip_rect(view);
            pinned.painter().rect_filled(band, 0.0, p.canvas);
            let rect = egui::Rect::from_min_size(band.min, egui::vec2(band.width().max(200.0), row_h));
            self.node_row(&mut pinned, cx, &rows[pin.row], pin.row, rect, egui::Id::new("nav_pinned"), &mut picked);
            pinned.painter().hline(band.x_range(), band.bottom() - 0.5, egui::Stroke::new(1.0, p.border));
            // Folded from here, the rows under it shift: back to the connection.
            if picked.toggled == Some(pin.row) || picked.clicked == Some((pin.row, true)) {
                self.scroll_to_selected = true;
            }
        }
        ui.spacing_mut().item_spacing.y = item_gap;
        if let Some(i) = picked.toggled {
            self.toggle(&rows[i].node, cx);
            self.selected = Some(rows[i].node.clone());
        }
        if let Some((i, double)) = picked.clicked {
            let node = rows[i].node.clone();
            self.selected = Some(node.clone());
            if double {
                self.activate(&node, cx);
            }
        }
        self.rows = rows;
        self.keyboard(ui, cx);
    }

    /// A node's row (`i` in the rows), in the list or pinned at the top: its fill, the type's
    /// stripe, the caret, the logo or icon, the name, the state and the details, its clicks and
    /// its menu.
    #[allow(clippy::too_many_arguments)]
    fn node_row(&mut self, ui: &mut Ui, cx: &mut AppContext, r: &Row, i: usize, rect: egui::Rect, id: egui::Id, picked: &mut RowPick) {
        let p = ternarygrid_ui::pal(ui);
        let resp = ui.interact(rect, id, Sense::click());
        let selected = Some(&r.node) == self.selected.as_ref();
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &r.label));
        let inner = rect.shrink2(egui::vec2(6.0, 0.0));
        let fill = if selected {
            Some(p.accent_soft)
        } else if resp.hovered() {
            Some(p.hover)
        } else {
            None
        };
        if let Some(fill) = fill {
            ui.painter().rect_filled(inner, ternarygrid_ui::style::RADIUS, fill);
        }
        // A marked connection type (Production): a stripe on the row's left edge.
        if let Some(t) = r.tint {
            let stripe = egui::Rect::from_min_max(egui::pos2(inner.left(), inner.top() + 4.0), egui::pos2(inner.left() + 3.0, inner.bottom() - 4.0));
            ui.painter().rect_filled(stripe, 2.0, t);
        }
        let row_bg = fill.unwrap_or(p.canvas);
        let mut x = inner.left() + 8.0 + r.depth as f32 * 14.0;
        let y = inner.center().y;
        let text_color = if selected { p.accent_text } else { p.text };
        let size = ternarygrid_ui::style::body_size(ui.ctx());
        if r.expandable {
            let arrow = if r.expanded { icon::CARET_DOWN } else { icon::CARET_RIGHT };
            let arrow_rect = egui::Rect::from_center_size(egui::pos2(x + 6.0, y), egui::vec2(16.0, rect.height()));
            ui.painter().text(arrow_rect.center(), egui::Align2::CENTER_CENTER, arrow, ternarygrid_ui::style::icon_font(12.0), p.text_faint);
            if resp.clicked() && resp.interact_pointer_pos().is_some_and(|q| arrow_rect.contains(q)) {
                picked.toggled = Some(i);
            }
        }
        x += 18.0;
        let resp = match &r.node {
            NavNode::Connection(id) => match cx.conns.status(id) {
                ConnStatus::Lost => resp.on_hover_text(tr("The connection to the server was lost; it connects again by itself as soon as the server answers")),
                ConnStatus::Checking => resp.on_hover_text(tr("Checking that the connection still answers (the computer slept)")),
                _ => resp,
            },
            _ => resp,
        };
        let icol = if selected {
            p.accent_text
        } else {
            r.icon_color.or_else(|| crate::theme::object_icon_color(r.icon, row_bg)).or_else(|| ternarygrid_icons::color(r.icon)).unwrap_or(p.text_muted)
        };
        // A connection: its database's logo (the icon when there is none).
        let logo_rect = egui::Rect::from_center_size(egui::pos2(x + 7.5, y), egui::vec2(15.0, 15.0));
        let drawn = r.logo.is_some_and(|kind| crate::dblogo::paint(ui, kind, logo_rect, crate::theme::legible_on(icol, row_bg)));
        if !drawn {
            ui.painter().text(egui::pos2(x + 7.5, y), egui::Align2::CENTER_CENTER, r.icon, ternarygrid_ui::style::icon_font(15.0), icol);
        }
        x += 21.0;
        // Name and detail cut with "…" at the panel's edge (or the size).
        let mut right = inner.right() - 10.0;
        let fit = |ui: &Ui, text: egui::RichText, width: f32| egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), width, egui::TextStyle::Body);
        let is_conn = matches!(r.node, NavNode::Connection(_));
        // At the right: the size of a table, the count of a folder, an object's detail.
        let right_text = r.size.as_ref().map(|(short, _)| short.clone()).or_else(|| r.detail.clone().filter(|_| !is_conn));
        let resp = match right_text {
            Some(text) if right - x > 110.0 => {
                let g = fit(ui, RichText::new(text).size(11.5).color(p.text_faint), 90.0);
                let left = right - g.size().x;
                ui.painter().galley(egui::pos2(left, y - g.size().y / 2.0), g, p.text_faint);
                right = left - 8.0;
                match &r.size {
                    Some((_, tip)) => resp.on_hover_text(tip),
                    None => resp,
                }
            }
            _ => resp,
        };
        let font = if selected || r.depth == 0 { ternarygrid_ui::style::medium(ui.ctx(), size) } else { egui::FontId::proportional(size) };
        let room = (right - x - if r.badge.is_some() { 14.0 } else { 0.0 }).max(30.0);
        let galley = fit(ui, RichText::new(&r.label).font(font).color(text_color), room);
        let w = galley.size().x;
        ui.painter().galley(egui::pos2(x, y - galley.size().y / 2.0), galley, text_color);
        let mut after = x + w;
        // The connection's state: a dot after its name.
        if let Some(badge) = r.badge {
            ui.painter().circle_filled(egui::pos2(after + 9.5, y), 3.5, badge);
            after += 16.0;
        }
        // A connection's detail (the server, the file) only where enough of it shows:
        // 70 points, or the whole of a short one; else its tooltip has it.
        let detail_room = right - (after + 8.0);
        if is_conn
            && let Some(d) = &r.detail
            && (detail_room >= 70.0 || ui.painter().layout_no_wrap(d.clone(), egui::FontId::proportional(11.5), p.text_faint).size().x <= detail_room)
        {
            let g = fit(ui, RichText::new(d).size(11.5).color(p.text_faint), right - (after + 8.0));
            ui.painter().galley(egui::pos2(after + 8.0, y - g.size().y / 2.0), g, p.text_faint);
        }
        if resp.double_clicked() {
            picked.clicked = Some((i, true));
        } else if resp.clicked() && picked.toggled != Some(i) {
            picked.clicked = Some((i, false));
        }
        if resp.secondary_clicked() {
            self.selected = Some(r.node.clone());
        }
        let node = r.node.clone();
        resp.context_menu(|ui| self.context_menu(ui, cx, &node));
    }

    /// Follow where the user clicks: the navigator, or somewhere else.
    pub fn track_focus(&mut self, ui: &Ui) {
        if ui.input(|i| i.pointer.any_pressed()) {
            self.has_focus = ui.ui_contains_pointer();
        }
    }

    fn toggle(&mut self, node: &NavNode, cx: &mut AppContext) {
        // Folding never connects (a lost connection, red, stays as it is).
        if self.expanded.contains(node) {
            self.expanded.remove(node);
            return;
        }
        // Unfolding a connection connects it; a lost one (red) is connected again.
        if let NavNode::Connection(id) = node {
            if matches!(cx.conns.status(id), ConnStatus::Lost) {
                cx.push(AppCommand::Reconnect(id.clone()));
            } else if !cx.conns.is_connected(id) {
                cx.push(AppCommand::Connect(id.clone()));
            }
        }
        self.expanded.insert(node.clone());
    }

    /// Double click / Enter.
    fn activate(&mut self, node: &NavNode, cx: &mut AppContext) {
        match node {
            NavNode::Object { conn, obj } | NavNode::Child { conn, obj } => {
                let page = if obj.kind.has_data() {
                    ObjectPage::Data
                } else if obj.kind.is_routine() || obj.kind == ObjectKind::Trigger {
                    ObjectPage::Ddl
                } else {
                    ObjectPage::Properties
                };
                cx.push(AppCommand::OpenObject { connection: conn.clone(), object: obj.clone(), page });
            }
            NavNode::CatalogItem { conn, folder, ctx } => {
                cx.push(AppCommand::OpenCatalogObject { connection: conn.clone(), folder, ctx: ctx.clone() });
            }
            NavNode::CatalogFolder { conn, folder, ctx } if cx.conns.live(conn).and_then(|l| l.catalog_folder(folder)).is_some_and(|f| f.table_only) => {
                cx.push(AppCommand::OpenCatalogTable { connection: conn.clone(), folder, ctx: ctx.clone() });
            }
            other => self.toggle(&other.clone(), cx),
        }
    }

    pub fn selected_schema(&self) -> Option<SchemaRef> {
        match self.selected.as_ref()? {
            NavNode::Schema { schema, .. } | NavNode::KindFolder { schema, .. } => Some(schema.clone()),
            NavNode::Database { db, .. } => Some(SchemaRef { database: Some(db.clone()), schema: None }),
            NavNode::Object { obj, .. } | NavNode::Child { obj, .. } => Some(obj.schema_ref()),
            NavNode::ChildFolder { parent, .. } => Some(parent.schema_ref()),
            _ => None,
        }
    }

    pub fn refresh_selected(&mut self, cx: &mut AppContext) {
        let Some(node) = self.selected.clone() else { return };
        self.refresh_node(&node, cx.commands);
    }

    /// Ask to reload the metadata below `node`.
    pub fn refresh_node(&self, node: &NavNode, commands: &mut Vec<AppCommand>) {
        let Some(conn) = node.connection().map(str::to_string) else { return };
        let (object, schema) = match &node {
            NavNode::Object { obj, .. } | NavNode::Child { obj, .. } => (Some(obj.clone()), None),
            NavNode::ChildFolder { parent, .. } => (Some(parent.clone()), None),
            NavNode::Schema { schema, .. } | NavNode::KindFolder { schema, .. } => (None, Some(schema.clone())),
            // That database (its schema list too), not the whole connection.
            NavNode::Database { db, .. } => (None, Some(SchemaRef { database: Some(db.clone()), schema: None })),
            _ => (None, None),
        };
        commands.push(AppCommand::RefreshMetadata { connection: conn, object, schema });
    }

    fn keyboard(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        // Only while the user works in the navigator (clicked it last): keys typed in a
        // grid or another view, which may not take egui's focus, are theirs.
        if !self.has_focus {
            return;
        }
        let nodes: Vec<usize> = self.rows.iter().enumerate().filter(|(_, r)| r.kind == RowKind::Node).map(|(i, _)| i).collect();
        let cur = self.selected.as_ref().and_then(|s| self.rows.iter().position(|r| &r.node == s));
        // F5 (Refresh) comes through the keymap: `App::dispatch_action`.
        let (down, up, left, right, enter) =
            ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowLeft), i.key_pressed(Key::ArrowRight), i.key_pressed(Key::Enter)));
        // Copy the name / drop: only with the pointer over the navigator.
        let pointer_here = ui.ui_contains_pointer();
        let (copy, delete, rename) = ui.input(|i| {
            (
                pointer_here && i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                pointer_here && (i.key_pressed(Key::Delete) || (i.modifiers.command && i.key_pressed(Key::Backspace))),
                pointer_here && i.key_pressed(Key::F2),
            )
        });
        if ui.memory(|m| m.focused().is_some()) {
            return; // a text field (filter) has focus
        }
        if down || up {
            let pos = cur.and_then(|c| nodes.iter().position(|&n| n == c));
            let next = match (pos, down) {
                (None, _) => nodes.first().copied(),
                (Some(p), true) => nodes.get(p + 1).copied().or(Some(nodes[p])),
                (Some(p), false) => nodes.get(p.saturating_sub(1)).copied(),
            };
            if let Some(n) = next {
                self.select(self.rows[n].node.clone());
            }
        }
        if let Some(c) = cur {
            let node = self.rows[c].node.clone();
            if right && self.rows[c].expandable && !self.rows[c].expanded {
                self.toggle(&node, cx);
            }
            if left && self.rows[c].expanded {
                self.expanded.remove(&node);
            }
            if enter {
                self.activate(&node, cx);
            }
            if copy {
                let name = match &node {
                    NavNode::Object { conn, obj } | NavNode::Child { conn, obj } => cx.conns.live(conn).map(|l| l.dialect.qualified_ref(obj)).unwrap_or_else(|| obj.display_path()),
                    _ => self.rows[c].label.clone(),
                };
                ui.ctx().copy_text(name);
            }
            if delete && let NavNode::Object { conn, obj } | NavNode::Child { conn, obj } = &node {
                // Opens the drop dialog (preview and confirmation).
                cx.push(AppCommand::DropObjects { connection: conn.clone(), objects: vec![obj.clone()] });
            }
            if let NavNode::Database { conn, db } = &node {
                if delete {
                    let database = ObjectRef::new(ObjectKind::Database, None, db.clone()).with_database(Some(db.clone()));
                    cx.push(AppCommand::DropObjects { connection: conn.clone(), objects: vec![database] });
                }
                if rename && cx.conns.live(conn).is_some_and(|l| l.dialect.database_edits().name) {
                    cx.push(AppCommand::EditDatabase { connection: conn.clone(), database: db.clone(), rename: true });
                }
            }
        }
    }

    fn context_menu(&mut self, ui: &mut Ui, cx: &mut AppContext, node: &NavNode) {
        ternarygrid_ui::kit::menu_look(ui, 240.0);
        let conn = node.connection().map(str::to_string);
        match node {
            NavNode::Folder(f) => {
                if ternarygrid_ui::kit::menu_item(ui, Some(icon::PLUS), tr("New connection here"), "").clicked() {
                    cx.push(AppCommand::NewConnection { kind: None, folder: Some(f.clone()) });
                    ui.close();
                }
                if ternarygrid_ui::kit::menu_item(ui, Some(icon::FOLDER), tr("New folder…"), "").clicked() {
                    cx.push(AppCommand::NewFolder { parent: Some(f.clone()) });
                    ui.close();
                }
                let inside = |folder: &Option<String>| folder.as_deref().is_some_and(|x| x == f || x.starts_with(&format!("{f}/")));
                let ids: Vec<String> = cx.conns.configs().iter().filter(|c| inside(&c.folder)).map(|c| c.id.clone()).collect();
                if ui.add_enabled_ui(!ids.is_empty(), |ui| ternarygrid_ui::kit::menu_item(ui, Some(icon::UPLOAD_SIMPLE), tr("Export connections…"), "")).inner.clicked() {
                    cx.push(AppCommand::ExportConnections(Some(ids)));
                    ui.close();
                }
                ternarygrid_ui::kit::menu_separator(ui);
                if ternarygrid_ui::kit::menu_item(ui, Some(icon::PENCIL_SIMPLE), tr("Rename folder…"), "").clicked() {
                    cx.push(AppCommand::RenameFolder(f.clone()));
                    ui.close();
                }
                if ui
                    .scope(|ui| ternarygrid_ui::kit::menu_item_danger(ui, Some(icon::TRASH), tr("Delete folder…"), ""))
                    .inner
                    .on_hover_text(tr("Its connections move to the folder above; none is deleted"))
                    .clicked()
                {
                    cx.push(AppCommand::DeleteFolder(f.clone()));
                    ui.close();
                }
            }
            NavNode::Connection(id) => {
                let connected = cx.conns.is_connected(id);
                if connected {
                    if ternarygrid_ui::kit::menu_item(ui, Some(icon::PLUGS), tr("Disconnect"), "").clicked() {
                        cx.push(AppCommand::Disconnect(id.clone()));
                        ui.close();
                    }
                    if ternarygrid_ui::kit::menu_item(ui, Some(icon::ARROWS_CLOCKWISE), tr("Reconnect"), "").clicked() {
                        cx.push(AppCommand::Reconnect(id.clone()));
                        ui.close();
                    }
                } else if ternarygrid_ui::kit::menu_item(ui, Some(icon::PLUGS_CONNECTED), tr("Connect"), "").clicked() {
                    cx.push(AppCommand::Connect(id.clone()));
                    ui.close();
                }
                ternarygrid_ui::kit::menu_separator(ui);
                if item(ui, icon::FILE_CODE, tr("New SQL editor"), &crate::keys::label(cx.keymap, crate::keys::Action::NewSqlEditor)).clicked() {
                    cx.push(AppCommand::NewSqlEditor { connection: Some(id.clone()), schema: None, text: None });
                    ui.close();
                }
                if ternarygrid_ui::kit::menu_item(ui, Some(icon::TERMINAL_WINDOW), tr("New SQL console"), "").clicked() {
                    cx.push(AppCommand::NewSqlConsole { connection: id.clone() });
                    ui.close();
                }
                create_menu_items(ui, cx, id, None);
                ternarygrid_ui::kit::menu_separator(ui);
                if ternarygrid_ui::kit::menu_item(ui, Some(icon::PENCIL_SIMPLE), tr("Edit connection…"), &crate::keys::label(cx.keymap, crate::keys::Action::EditConnection))
                    .clicked()
                {
                    cx.push(AppCommand::EditConnection(id.clone()));
                    ui.close();
                }
                if ternarygrid_ui::kit::menu_item(ui, Some(icon::COPY), tr("Duplicate"), "").clicked() {
                    cx.push(AppCommand::DuplicateConnection(id.clone()));
                    ui.close();
                }
                if ternarygrid_ui::kit::menu_item(ui, Some(icon::UPLOAD_SIMPLE), tr("Export connection…"), "").clicked() {
                    cx.push(AppCommand::ExportConnections(Some(vec![id.clone()])));
                    ui.close();
                }
                let others: Vec<String> = cx.conns.configs().iter().map(|c| c.id.clone()).filter(|c| c != id && cx.conns.is_connected(c)).collect();
                if !others.is_empty() && ternarygrid_ui::kit::menu_item(ui, None, tr("Disconnect others"), "").clicked() {
                    for o in others {
                        cx.push(AppCommand::Disconnect(o));
                    }
                    ui.close();
                }
                ternarygrid_ui::kit::submenu(ui, Some(icon::ARROWS_DOWN_UP), tr("Reorder"), 170.0, &mut |ui| {
                    use crate::command::MovePos as M;
                    for (pos, label, i) in [
                        (M::Top, tr("To the top"), icon::ARROW_LINE_UP),
                        (M::Up, tr("Up"), icon::ARROW_UP),
                        (M::Down, tr("Down"), icon::ARROW_DOWN),
                        (M::Bottom, tr("To the bottom"), icon::ARROW_LINE_DOWN),
                    ] {
                        if ternarygrid_ui::kit::menu_item(ui, Some(i), label, "").clicked() {
                            cx.push(AppCommand::MoveConnection { id: id.clone(), to: pos });
                            ui.close();
                        }
                    }
                });
                let current = cx.conns.config(id).and_then(|c| c.folder.clone());
                let folders = cx.conns.folders().to_vec();
                ternarygrid_ui::kit::submenu(ui, Some(icon::FOLDER), tr("Move to folder"), 200.0, &mut |ui| {
                    if ui.add_enabled_ui(current.is_some(), |ui| ternarygrid_ui::kit::menu_item(ui, None, tr("(top level)"), "")).inner.clicked() {
                        cx.push(AppCommand::MoveToFolder { id: id.clone(), folder: None });
                        ui.close();
                    }
                    for f in &folders {
                        // Subfolders indented under their parent.
                        let depth = f.matches('/').count();
                        let name = f.rsplit('/').next().unwrap_or(f);
                        let label = format!("{}{name}", "    ".repeat(depth));
                        if ui.add_enabled_ui(current.as_deref() != Some(f.as_str()), |ui| ternarygrid_ui::kit::menu_item(ui, Some(icon::FOLDER), &label, "")).inner.clicked() {
                            cx.push(AppCommand::MoveToFolder { id: id.clone(), folder: Some(f.clone()) });
                            ui.close();
                        }
                    }
                    ternarygrid_ui::kit::menu_separator(ui);
                    if ternarygrid_ui::kit::menu_item(ui, Some(icon::FOLDER), tr("New folder…"), "").clicked() {
                        cx.push(AppCommand::NewFolder { parent: None });
                        ui.close();
                    }
                });
                if connected {
                    ternarygrid_ui::kit::menu_separator(ui);
                    ternarygrid_ui::kit::submenu(ui, Some(icon::WRENCH), tr("Tools"), 220.0, &mut |ui| {
                        tools_menu(ui, cx, id, None);
                    });
                }
                // Destructive, last and apart (it asks first).
                ternarygrid_ui::kit::menu_separator(ui);
                if ternarygrid_ui::kit::menu_item_danger(ui, Some(icon::TRASH), tr("Delete connection…"), "").clicked() {
                    cx.push(AppCommand::DeleteConnection(id.clone()));
                    ui.close();
                }
            }
            NavNode::Database { conn, db } => database_menu(ui, cx, conn, db),
            NavNode::Schema { conn, schema } | NavNode::KindFolder { conn, schema, .. } => {
                if item(ui, icon::FILE_CODE, tr("New SQL editor"), &crate::keys::label(cx.keymap, crate::keys::Action::NewSqlEditor)).clicked() {
                    cx.push(AppCommand::NewSqlEditor { connection: Some(conn.clone()), schema: Some(schema.clone()), text: None });
                    ui.close();
                }
                // Another schema in the same database.
                if matches!(node, NavNode::Schema { .. })
                    && let Some(live) = cx.conns.live(conn)
                    && live.dialect.create_schema_sql("x", None).is_some()
                {
                    let read_only = live.config.read_only;
                    let database = schema.database.clone().or_else(|| live.current_database.clone());
                    let b = ui
                        .add_enabled_ui(!read_only, |ui| ternarygrid_ui::kit::menu_item(ui, Some(icon::STACK), tr("New schema…"), ""))
                        .inner
                        .on_disabled_hover_text(tr("The connection is read-only."));
                    if b.clicked() {
                        cx.push(AppCommand::NewSchema { connection: conn.clone(), database });
                        ui.close();
                    }
                }
                // Table designer, diagrams, imports and compare write SQL.
                if is_sql(cx, conn) {
                    if ternarygrid_ui::kit::menu_item(ui, Some(icon::PLUS), tr("Create table"), "").clicked() {
                        cx.push(AppCommand::CreateTable { connection: conn.clone(), schema: schema.clone() });
                        ui.close();
                    }
                    if ternarygrid_ui::kit::menu_item(ui, Some(icon::GRAPH), tr("ER diagram"), "").clicked() {
                        cx.push(AppCommand::OpenErDiagram { connection: conn.clone(), schema: schema.clone(), tables: Vec::new() });
                        ui.close();
                    }
                    if ternarygrid_ui::kit::menu_item(ui, Some(icon::DOWNLOAD_SIMPLE), tr("Import data…"), "").clicked() {
                        cx.push(AppCommand::OpenImport { connection: conn.clone(), table: None, schema: Some(schema.clone()) });
                        ui.close();
                    }
                    if ternarygrid_ui::kit::menu_item(ui, Some(icon::GIT_DIFF), tr("Compare schema"), "").clicked() {
                        cx.push(AppCommand::OpenSchemaCompare { connection: Some(conn.clone()), schema: Some(schema.clone()) });
                        ui.close();
                    }
                }
                ternarygrid_ui::kit::submenu(ui, Some(icon::WRENCH), tr("Tools"), 220.0, &mut |ui| tools_menu(ui, cx, conn, Some(schema.clone())));
                if let NavNode::KindFolder { kind, .. } = node
                    && cx.conns.config(conn).is_some_and(|c| name_filter(&c.filters, *kind).is_active())
                    && ternarygrid_ui::kit::menu_item(ui, Some(icon::FUNNEL_X), tr("Clear navigator filter"), "")
                        .on_hover_text(tr("Show the hidden objects of this type again"))
                        .clicked()
                {
                    cx.push(AppCommand::ClearNavigatorFilter { connection: conn.clone(), kind: *kind });
                    ui.close();
                }
                ternarygrid_ui::kit::menu_separator(ui);
                if item(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), &crate::keys::label(cx.keymap, crate::keys::Action::Refresh)).clicked() {
                    cx.push(AppCommand::RefreshMetadata { connection: conn.clone(), object: None, schema: Some(schema.clone()) });
                    ui.close();
                }
            }
            NavNode::Object { conn, obj } | NavNode::Child { conn, obj } => {
                object_menu(ui, cx, conn, obj, true);
            }
            NavNode::ChildFolder { conn, parent, .. } => {
                if item(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), &crate::keys::label(cx.keymap, crate::keys::Action::Refresh)).clicked() {
                    cx.push(AppCommand::RefreshMetadata { connection: conn.clone(), object: Some(parent.clone()), schema: None });
                    ui.close();
                }
            }
            NavNode::Extensions { .. } => {
                if let Some(c) = conn
                    && item(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), &crate::keys::label(cx.keymap, crate::keys::Action::Refresh)).clicked()
                {
                    cx.push(AppCommand::RefreshMetadata { connection: c, object: None, schema: None });
                    ui.close();
                }
            }
            NavNode::CatalogGroup { conn, .. } => {
                if item(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), &crate::keys::label(cx.keymap, crate::keys::Action::Refresh)).clicked() {
                    cx.push(AppCommand::RefreshCatalog { connection: conn.clone(), folder: None });
                    ui.close();
                }
            }
            NavNode::CatalogFolder { conn, folder, ctx } => {
                if ternarygrid_ui::kit::menu_item(ui, Some(icon::TABLE), tr("View as table"), "").clicked() {
                    cx.push(AppCommand::OpenCatalogTable { connection: conn.clone(), folder, ctx: ctx.clone() });
                    ui.close();
                }
                if item(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), &crate::keys::label(cx.keymap, crate::keys::Action::Refresh)).clicked() {
                    cx.push(AppCommand::RefreshCatalog { connection: conn.clone(), folder: Some(folder) });
                    ui.close();
                }
            }
            NavNode::CatalogItem { conn, folder, ctx } => {
                if item(ui, icon::ARROW_SQUARE_OUT, tr("Open"), OPEN_KEYS).clicked() {
                    cx.push(AppCommand::OpenCatalogObject { connection: conn.clone(), folder, ctx: ctx.clone() });
                    ui.close();
                }
                if item(ui, icon::COPY, tr("Copy name"), COPY_KEYS).clicked() {
                    ui.ctx().copy_text(ctx.name.clone().unwrap_or_default());
                    ui.close();
                }
                if item(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), &crate::keys::label(cx.keymap, crate::keys::Action::Refresh)).clicked() {
                    cx.push(AppCommand::RefreshCatalog { connection: conn.clone(), folder: Some(folder) });
                    ui.close();
                }
            }
        }
    }
}

/// `CREATE INDEX` / unique constraint DDL for one column (edit before running).
pub fn column_constraint_sql(d: &dyn ternarygrid_core::Dialect, column: &ObjectRef, unique: bool) -> String {
    let table = column.table.clone().unwrap_or_default();
    let qualified = d.qualified(column.schema.as_deref(), &table);
    let col = d.quote_ident(&column.name);
    let base = format!("{table}_{}", column.name).to_lowercase().replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "_");
    if unique && d.kind() != ternarygrid_core::DbKind::Sqlite {
        let name = d.quote_ident(&format!("uq_{base}"));
        format!("ALTER TABLE {qualified} ADD CONSTRAINT {name} UNIQUE ({col});")
    } else {
        let name = d.quote_ident(&format!("{}_{base}", if unique { "uq" } else { "idx" }));
        let schema_prefix = match (d.kind(), column.schema.as_deref()) {
            // SQLite and PostgreSQL put the index in the table's schema.
            (ternarygrid_core::DbKind::Sqlite, Some(s)) => format!("{}.", d.quote_ident(s)),
            _ => String::new(),
        };
        let table_ref = if d.kind() == ternarygrid_core::DbKind::Sqlite { d.quote_ident(&table) } else { qualified };
        format!("CREATE {}INDEX {schema_prefix}{name} ON {table_ref} ({col});", if unique { "UNIQUE " } else { "" })
    }
}

/// Icon of a catalog folder and its objects.
pub fn catalog_icon(i: CatalogIcon) -> &'static str {
    match i {
        CatalogIcon::User => icon::USER,
        CatalogIcon::Users => icon::USERS,
        CatalogIcon::Storage => icon::HARD_DRIVES,
        CatalogIcon::Settings => icon::SLIDERS,
        CatalogIcon::Plug => icon::PLUG,
        CatalogIcon::Server => icon::HARD_DRIVE,
        CatalogIcon::Trigger => icon::LIGHTNING,
        CatalogIcon::Language => icon::CODE,
        CatalogIcon::Text => icon::TEXT_AA,
        CatalogIcon::Shield => icon::SHIELD_CHECK,
        CatalogIcon::Rule => icon::ARROW_BEND_DOWN_RIGHT,
        CatalogIcon::Gauge => icon::GAUGE,
        CatalogIcon::Engine => icon::ENGINE,
        CatalogIcon::Key => icon::KEY,
        CatalogIcon::Puzzle => icon::PUZZLE_PIECE,
    }
}

/// What the rows reported this frame.
#[derive(Default)]
struct RowPick {
    /// (row, double click)
    clicked: Option<(usize, bool)>,
    /// Its caret was clicked.
    toggled: Option<usize>,
}

/// The connection row kept at the top of the tree.
#[derive(Debug, PartialEq)]
struct Pinned {
    row: usize,
    /// How far above the top it is drawn: the next connection pushes it out.
    shift: f32,
}

/// The expanded connection whose rows are at the top of the tree scrolled by `offset`, while
/// its own row is above the top.
fn pinned_row(rows: &[Row], offset: f32, pitch: f32) -> Option<Pinned> {
    if offset <= 0.0 {
        return None;
    }
    let top = ((offset / pitch).floor() as usize).min(rows.len().checked_sub(1)?);
    // Failed connections' message rows carry the connection too: only the real row.
    let row = (0..=top).rev().find(|&i| rows[i].kind == RowKind::Node && matches!(rows[i].node, NavNode::Connection(_)))?;
    let depth = rows[row].depth;
    if !rows[row].expanded {
        return None;
    }
    // Rows of the connection end at the next one at its level (a connection, a folder).
    let end = rows[row + 1..].iter().position(|r| r.depth <= depth).map_or(rows.len(), |k| row + 1 + k);
    if end <= top {
        return None;
    }
    Some(Pinned { row, shift: (end as f32 * pitch - offset - pitch).min(0.0) })
}

/// How many of the navigator's toolbar buttons (26 points, 2 apart) fit in `room`: always ⋯ and
/// New connection, at most the six there are.
fn toolbar_buttons(room: f32) -> usize {
    (((room + 2.0) / 28.0).floor() as usize).clamp(2, 6)
}

fn placeholder(depth: usize, kind: RowKind) -> Row {
    Row {
        node: NavNode::Folder(String::new()),
        depth,
        label: String::new(),
        detail: None,
        icon: "",
        icon_color: None,
        badge: None,
        logo: None,
        tint: None,
        size: None,
        expandable: false,
        expanded: false,
        kind,
    }
}

/// Menu of a database node: editors, its properties and rename, new
/// databases / schemas, maintenance and tools, then drop.
fn database_menu(ui: &mut Ui, cx: &mut AppContext, conn: &str, db: &str) {
    let schema = SchemaRef { database: Some(db.to_string()), schema: None };
    let database = ObjectRef::new(ObjectKind::Database, None, db.to_string()).with_database(Some(db.to_string()));
    let Some(live) = cx.conns.live(conn) else { return };
    let edits = live.dialect.database_edits();
    let editable = edits != ternarygrid_core::DatabaseEdits::default() || live.dialect.database_properties_sql(db).is_some();
    let maintenance = live.dialect.maintenance_ops().iter().any(|o| o.applies_to.contains(&ObjectKind::Database));
    let read_only = live.config.read_only;
    if item(ui, icon::FILE_CODE, tr("New SQL editor"), &crate::keys::label(cx.keymap, crate::keys::Action::NewSqlEditor)).clicked() {
        cx.push(AppCommand::NewSqlEditor { connection: Some(conn.to_string()), schema: Some(schema.clone()), text: None });
        ui.close();
    }
    ternarygrid_ui::kit::menu_separator(ui);
    if editable
        && ternarygrid_ui::kit::menu_item(ui, Some(icon::PENCIL), tr("Edit database…"), "")
            .on_hover_text(tr("Its owner, comment, limits … and the statements that change them"))
            .clicked()
    {
        cx.push(AppCommand::EditDatabase { connection: conn.to_string(), database: db.to_string(), rename: false });
        ui.close();
    }
    if edits.name && ui.add_enabled_ui(!read_only, |ui| ternarygrid_ui::kit::menu_item(ui, Some(icon::PENCIL_SIMPLE), tr("Rename…"), "F2")).inner.clicked() {
        cx.push(AppCommand::EditDatabase { connection: conn.to_string(), database: db.to_string(), rename: true });
        ui.close();
    }
    create_menu_items(ui, cx, conn, Some(db.to_string()));
    if maintenance && ternarygrid_ui::kit::menu_item(ui, Some(icon::WRENCH), tr("Maintenance…"), "").on_hover_text(tr("Vacuum, analyze, reindex the whole database")).clicked() {
        cx.push(AppCommand::OpenMaintenance { connection: conn.to_string(), objects: vec![database.clone()], op: None });
        ui.close();
    }
    ternarygrid_ui::kit::submenu(ui, Some(icon::WRENCH), tr("Tools"), 220.0, &mut |ui| tools_menu(ui, cx, conn, Some(schema.clone())));
    ternarygrid_ui::kit::menu_separator(ui);
    if item(ui, icon::COPY, tr("Copy name"), COPY_KEYS).clicked() {
        ui.ctx().copy_text(db.to_string());
        ui.close();
    }
    if item(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), &crate::keys::label(cx.keymap, crate::keys::Action::Refresh)).clicked() {
        cx.push(AppCommand::RefreshMetadata { connection: conn.to_string(), object: None, schema: Some(schema) });
        ui.close();
    }
    ternarygrid_ui::kit::menu_separator(ui);
    if ternarygrid_ui::kit::menu_item_danger(ui, Some(icon::TRASH), tr("Drop…"), DROP_KEYS).clicked() {
        cx.push(AppCommand::DropObjects { connection: conn.to_string(), objects: vec![database] });
        ui.close();
    }
}

/// "New database…" / "New schema…" of a connected server (what its
/// database type supports; `database`: schemas go into that database).
/// `database`: the database node the menu belongs to, `None` on the
/// connection node.
pub fn create_menu_items(ui: &mut Ui, cx: &mut AppContext, conn: &str, database: Option<String>) {
    let Some(live) = cx.conns.live(conn) else { return };
    let read_only = live.config.read_only;
    let probe = ternarygrid_core::NewDatabase { name: "x".into(), ..Default::default() };
    let databases = live.dialect.create_database_sql(&probe).is_some();
    let schemas = live.dialect.create_schema_sql("x", None).is_some();
    let multiple_databases = live.driver.info().capabilities.multiple_databases;
    let why = tr("The connection is read-only.");
    if databases {
        let b = ui.add_enabled_ui(!read_only, |ui| ternarygrid_ui::kit::menu_item(ui, Some(icon::DATABASE), tr("New database…"), "")).inner.on_disabled_hover_text(why);
        if b.clicked() {
            cx.push(AppCommand::NewDatabase { connection: conn.to_string() });
            ui.close();
        }
    }
    // A schema belongs to a database: on servers with several databases it
    // is created from a database (or schema) node, not the connection.
    if schemas && (database.is_some() || !multiple_databases) {
        let b = ui.add_enabled_ui(!read_only, |ui| ternarygrid_ui::kit::menu_item(ui, Some(icon::STACK), tr("New schema…"), "")).inner.on_disabled_hover_text(why);
        if b.clicked() {
            cx.push(AppCommand::NewSchema { connection: conn.to_string(), database });
            ui.close();
        }
    }
}

/// Administration tools for a connection.
pub fn tools_menu(ui: &mut Ui, cx: &mut AppContext, conn: &str, schema: Option<SchemaRef>) {
    let caps = cx.conns.live(conn).map(|l| l.driver.info().capabilities.clone()).unwrap_or_default();
    if caps.session_manager && ternarygrid_ui::kit::menu_item(ui, Some(icon::USERS), tr("Session manager"), "").clicked() {
        cx.push(AppCommand::OpenSessionManager { connection: conn.to_string() });
        ui.close();
    }
    if caps.lock_manager && ternarygrid_ui::kit::menu_item(ui, Some(icon::LOCK), tr("Lock manager"), "").clicked() {
        cx.push(AppCommand::OpenLockManager { connection: conn.to_string() });
        ui.close();
    }
    if caps.server_metrics && ternarygrid_ui::kit::menu_item(ui, Some(icon::CHART_BAR), tr("Server dashboard"), "").clicked() {
        cx.push(AppCommand::OpenDashboard { connection: conn.to_string() });
        ui.close();
    }
    if caps.backup && ternarygrid_ui::kit::menu_item(ui, Some(icon::PACKAGE), tr("Backup"), "").clicked() {
        cx.push(AppCommand::OpenBackup { connection: conn.to_string(), schema: schema.clone(), restore: false });
        ui.close();
    }
    if caps.restore && ternarygrid_ui::kit::menu_item(ui, Some(icon::REPEAT), tr("Restore"), "").clicked() {
        cx.push(AppCommand::OpenBackup { connection: conn.to_string(), schema: schema.clone(), restore: true });
        ui.close();
    }
    let sql = is_sql(cx, conn);
    if sql && ternarygrid_ui::kit::menu_item(ui, Some(icon::BLUEPRINT), tr("Query builder"), "").clicked() {
        cx.push(AppCommand::OpenQueryBuilder { connection: conn.to_string(), schema: schema.clone() });
        ui.close();
    }
    if ternarygrid_ui::kit::menu_item(ui, Some(icon::MAGNIFYING_GLASS), tr("Search metadata"), "").clicked() {
        cx.push(AppCommand::OpenSearch { connection: Some(conn.to_string()), data: false });
        ui.close();
    }
    if sql && ternarygrid_ui::kit::menu_item(ui, Some(icon::BINOCULARS), tr("Search data"), "").clicked() {
        cx.push(AppCommand::OpenSearch { connection: Some(conn.to_string()), data: true });
        ui.close();
    }
    if cx.conns.live(conn).is_some_and(|l| l.config.kind == DbKind::Postgres) && ternarygrid_ui::kit::menu_item(ui, Some(icon::PLUG), tr("Foreign data wrapper…"), "").clicked() {
        cx.push(AppCommand::OpenFdwConfigurator { connection: conn.to_string() });
        ui.close();
    }
    let can_change_password = cx.conns.live(conn).is_some_and(|l| l.dialect.change_password_sql(None, "x").is_some());
    if can_change_password && ternarygrid_ui::kit::menu_item(ui, Some(icon::KEY), tr("Change password"), "").clicked() {
        cx.push(AppCommand::ChangePassword { connection: conn.to_string() });
        ui.close();
    }
}

/// A menu item with its shortcut on the right.
fn item(ui: &mut Ui, glyph: &str, text: &str, keys: &str) -> egui::Response {
    ternarygrid_ui::kit::menu_item(ui, Some(glyph), text, keys)
}

/// The navigator's own keys, as shown in its menus.
const OPEN_KEYS: &str = if crate::keys::IS_MAC { "↩" } else { "Enter" };
const COPY_KEYS: &str = if crate::keys::IS_MAC { "⌘C" } else { "Ctrl+C" };
const DROP_KEYS: &str = if crate::keys::IS_MAC { "⌘⌫" } else { "Delete" };

/// Context menu for a database object (navigator and object editors).
/// `in_navigator`: show the navigator's keys next to the items.
///
/// Outside the navigator (the object editor's ⋯ menu) the items its pages and
/// Refresh button already offer are left out: Open, View data, View DDL, ER diagram, Refresh.
/// The connection's language is SQL (or CQL): the SQL tools apply.
fn is_sql(cx: &AppContext, conn: &str) -> bool {
    cx.conns.config(conn).is_none_or(|c| c.kind.language().is_sql())
}

pub fn object_menu(ui: &mut Ui, cx: &mut AppContext, conn: &str, obj: &ObjectRef, in_navigator: bool) {
    ternarygrid_ui::kit::menu_look(ui, 260.0);
    let c = conn.to_string();
    let sql = is_sql(cx, conn);
    let keys = |k: &str| if in_navigator { k.to_string() } else { String::new() };
    // Enter (a double click) opens the page `activate` opens: its key beside that item.
    let enter = if obj.kind.has_data() {
        ObjectPage::Data
    } else if obj.kind.is_routine() || obj.kind == ObjectKind::Trigger {
        ObjectPage::Ddl
    } else {
        ObjectPage::Properties
    };
    let enter_keys = |page: ObjectPage| if page == enter { keys(OPEN_KEYS) } else { String::new() };
    if in_navigator && obj.kind.has_data() && item(ui, icon::TABLE, tr("View data"), &enter_keys(ObjectPage::Data)).clicked() {
        cx.push(AppCommand::OpenObject { connection: c.clone(), object: obj.clone(), page: ObjectPage::Data });
        ui.close();
    }
    if in_navigator && item(ui, icon::ARROW_SQUARE_OUT, tr("Properties"), &enter_keys(ObjectPage::Properties)).clicked() {
        cx.push(AppCommand::OpenObject { connection: c.clone(), object: obj.clone(), page: ObjectPage::Properties });
        ui.close();
    }
    if in_navigator && item(ui, icon::FILE_CODE, tr("View DDL"), &enter_keys(ObjectPage::Ddl)).clicked() {
        cx.push(AppCommand::OpenObject { connection: c.clone(), object: obj.clone(), page: ObjectPage::Ddl });
        ui.close();
    }
    if sql && obj.kind.is_table_like() {
        if ternarygrid_ui::kit::menu_item(ui, Some(icon::PENCIL), tr("Edit table"), "").clicked() {
            cx.push(AppCommand::EditTable { connection: c.clone(), table: obj.clone() });
            ui.close();
        }
        if in_navigator && ternarygrid_ui::kit::menu_item(ui, Some(icon::GRAPH), tr("ER diagram"), "").clicked() {
            cx.push(AppCommand::OpenErDiagram { connection: c.clone(), schema: obj.schema_ref(), tables: vec![obj.name.clone()] });
            ui.close();
        }
    }
    if sql {
        ternarygrid_ui::kit::submenu(ui, Some(icon::FILE_CODE), tr("Generate SQL"), 150.0, &mut |ui| {
            for (label, kind) in [("SELECT", "select"), ("INSERT", "insert"), ("UPDATE", "update"), ("DELETE", "delete"), ("UPSERT", "upsert"), ("DDL", "ddl")] {
                if (obj.kind.is_table_like() || kind == "ddl" || kind == "select") && ternarygrid_ui::kit::menu_item(ui, None, label, "").clicked() {
                    cx.push(AppCommand::GenerateSql { connection: c.clone(), object: obj.clone(), kind: kind.to_string() });
                    ui.close();
                }
            }
        });
    }
    if obj.kind.has_data() {
        ternarygrid_ui::kit::menu_separator(ui);
        if ternarygrid_ui::kit::menu_item(ui, Some(icon::EXPORT), tr("Export data…"), "").clicked() {
            cx.push(AppCommand::OpenExport { connection: c.clone(), objects: vec![obj.clone()], sql: None });
            ui.close();
        }
        if sql && obj.kind.is_table_like() {
            if ternarygrid_ui::kit::menu_item(ui, Some(icon::DOWNLOAD_SIMPLE), tr("Import data…"), "").clicked() {
                cx.push(AppCommand::OpenImport { connection: c.clone(), table: Some(obj.clone()), schema: Some(obj.schema_ref()) });
                ui.close();
            }
            if ternarygrid_ui::kit::menu_item(ui, Some(icon::MAGIC_WAND), tr("Generate mock data…"), "").clicked() {
                cx.push(AppCommand::OpenMockData { connection: c.clone(), table: obj.clone() });
                ui.close();
            }
            if ternarygrid_ui::kit::menu_item(ui, Some(icon::ARROW_RIGHT), tr("Migrate to another database…"), "").clicked() {
                cx.push(AppCommand::OpenMigration { connection: c.clone(), objects: vec![obj.clone()] });
                ui.close();
            }
            if ternarygrid_ui::kit::menu_item(ui, Some(icon::ARROWS_LEFT_RIGHT), tr("Compare data…"), "").clicked() {
                cx.push(AppCommand::OpenDataCompare { connection: Some(c.clone()), table: Some(obj.clone()) });
                ui.close();
            }
            if ternarygrid_ui::kit::menu_item(ui, Some(icon::WRENCH), tr("Maintenance…"), "").clicked() {
                cx.push(AppCommand::OpenMaintenance { connection: c.clone(), objects: vec![obj.clone()], op: None });
                ui.close();
            }
        }
    }
    if obj.kind.is_routine() && ternarygrid_ui::kit::menu_item(ui, Some(icon::PLAY), tr("Execute…"), "").on_hover_text(tr("Open a script that calls it with parameters")).clicked()
    {
        cx.push(AppCommand::ExecuteRoutine { connection: c.clone(), routine: obj.clone() });
        ui.close();
    }
    if obj.kind.is_routine() && ternarygrid_ui::kit::menu_item(ui, Some(icon::BUG), tr("Debug"), "").clicked() {
        cx.push(AppCommand::OpenDebugger { connection: c.clone(), routine: obj.clone() });
        ui.close();
    }
    ternarygrid_ui::kit::menu_separator(ui);
    if sql && obj.kind == ObjectKind::Column && obj.table.is_some() {
        for (label, unique) in [(tr("Create index…"), false), (tr("Add unique constraint…"), true)] {
            if ternarygrid_ui::kit::menu_item(ui, Some(if unique { icon::KEY } else { icon::LIST_NUMBERS }), label, "").clicked() {
                if let Some(live) = cx.conns.live(conn) {
                    let text = column_constraint_sql(live.dialect.as_ref(), obj, unique);
                    cx.push(AppCommand::NewSqlEditor { connection: Some(conn.to_string()), schema: Some(obj.schema_ref()), text: Some(text) });
                }
                ui.close();
            }
        }
    }
    if obj.kind == ObjectKind::Trigger
        && cx.conns.live(conn).is_some_and(|l| l.dialect.maintenance_ops().iter().any(|o| o.applies_to.contains(&ObjectKind::Trigger)))
        && ternarygrid_ui::kit::menu_item(ui, Some(icon::WRENCH), tr("Maintenance…"), "").on_hover_text(tr("Enable or disable the trigger")).clicked()
    {
        cx.push(AppCommand::OpenMaintenance { connection: conn.to_string(), objects: vec![obj.clone()], op: None });
        ui.close();
    }
    if obj.table.is_none()
        && ternarygrid_ui::kit::menu_item(ui, Some(icon::EYE_SLASH), tr("Hide in navigator"), "").on_hover_text(tr("Adds the name to the connection's navigator filter")).clicked()
    {
        cx.push(AppCommand::HideInNavigator { connection: conn.to_string(), object: obj.clone() });
        ui.close();
    }
    if ternarygrid_ui::kit::menu_item(ui, Some(icon::BOOKMARK_SIMPLE), tr("Add bookmark"), "").clicked() {
        cx.push(AppCommand::AddBookmark { connection: c.clone(), object: obj.clone() });
        ui.close();
    }
    if item(ui, icon::COPY, tr("Copy name"), &keys(COPY_KEYS)).clicked() {
        let name = cx.conns.live(conn).map(|l| l.dialect.qualified_ref(obj)).unwrap_or_else(|| obj.display_path());
        ui.ctx().copy_text(name);
        ui.close();
    }
    if in_navigator && item(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), &keys(&crate::keys::label(cx.keymap, crate::keys::Action::Refresh))).clicked() {
        cx.push(AppCommand::RefreshMetadata { connection: c.clone(), object: Some(obj.clone()), schema: None });
        ui.close();
    }
    ternarygrid_ui::kit::menu_separator(ui);
    // Truncate (Tools › Truncate in DBeaver): its options and SQL in the maintenance dialog.
    if sql
        && matches!(obj.kind, ObjectKind::Table | ObjectKind::Partition)
        && ternarygrid_ui::kit::menu_item_danger(ui, Some(icon::ERASER), tr("Truncate…"), "").on_hover_text(tr("Delete every row of the table")).clicked()
    {
        cx.push(AppCommand::OpenMaintenance { connection: c.clone(), objects: vec![obj.clone()], op: Some(ternarygrid_core::admin::MaintenanceOp::TRUNCATE.to_string()) });
        ui.close();
    }
    if ternarygrid_ui::kit::menu_item_danger(ui, Some(icon::TRASH), tr("Drop…"), &keys(DROP_KEYS)).clicked() {
        cx.push(AppCommand::DropObjects { connection: c, objects: vec![obj.clone()] });
        ui.close();
    }
}

#[cfg(test)]
mod size_tests {
    use super::{long_size, short_size};

    #[test]
    fn sizes_as_dbeaver_shows_them() {
        assert_eq!(short_size(512), "512");
        assert_eq!(short_size(16 * 1024), "16K");
        assert_eq!(short_size(2_516_582), "2.4M");
        assert_eq!(short_size(57 * 1024 * 1024), "57M");
        assert_eq!(short_size(3 * 1024 * 1024), "3M");
        assert_eq!(short_size(1_288_490_189), "1.2G");
        assert_eq!(long_size(2_516_582), "2.4 MB");
        assert_eq!(long_size(812), "812 bytes");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PITCH: f32 = 27.0;

    fn node(depth: usize, node: NavNode, expanded: bool) -> Row {
        let mut r = placeholder(depth, RowKind::Node);
        r.node = node;
        r.expanded = expanded;
        r
    }

    fn conn(depth: usize, id: &str, expanded: bool) -> Row {
        node(depth, NavNode::Connection(id.to_string()), expanded)
    }

    fn child(depth: usize) -> Row {
        node(depth, NavNode::Folder("x".to_string()), false)
    }

    /// Two connections, the first with `n` rows.
    fn two_connections(n: usize) -> Vec<Row> {
        let mut rows = vec![conn(0, "a", true)];
        rows.extend((0..n).map(|_| child(1)));
        rows.push(conn(0, "b", true));
        rows.push(child(1));
        rows
    }

    #[test]
    fn a_connection_stays_at_the_top_while_its_rows_scroll() {
        let rows = two_connections(10);
        assert_eq!(pinned_row(&rows, 0.0, PITCH), None, "nothing to pin at the top");
        assert_eq!(pinned_row(&rows, 3.0 * PITCH, PITCH), Some(Pinned { row: 0, shift: 0.0 }));
    }

    #[test]
    fn the_next_connection_pushes_it_out() {
        let rows = two_connections(10);
        // b's row is at 11: at 10 rows down a is still in place, then b pushes it up.
        assert_eq!(pinned_row(&rows, 10.0 * PITCH, PITCH), Some(Pinned { row: 0, shift: 0.0 }));
        assert_eq!(pinned_row(&rows, 10.5 * PITCH, PITCH), Some(Pinned { row: 0, shift: -0.5 * PITCH }));
        // Past b's row: b is the one pinned.
        assert_eq!(pinned_row(&rows, 11.5 * PITCH, PITCH), Some(Pinned { row: 11, shift: 0.0 }));
    }

    #[test]
    fn only_expanded_connections_are_pinned() {
        let rows = vec![conn(0, "a", false), conn(0, "b", false), conn(0, "c", false)];
        assert_eq!(pinned_row(&rows, 1.5 * PITCH, PITCH), None);
    }

    #[test]
    fn a_connection_in_a_folder_ends_at_the_next_folder() {
        let rows = vec![node(0, NavNode::Folder("f".to_string()), true), conn(1, "a", true), child(2), child(2), node(0, NavNode::Folder("g".to_string()), true), child(1)];
        assert_eq!(pinned_row(&rows, 2.5 * PITCH, PITCH).map(|p| p.row), Some(1));
        // The second folder at the top: it belongs to no connection.
        assert_eq!(pinned_row(&rows, 4.5 * PITCH, PITCH), None);
    }

    #[test]
    fn a_failed_connections_message_rows_are_not_the_connection() {
        let mut message = placeholder(1, RowKind::Failed { conn: "b".to_string(), message: "refused".to_string(), part: 0 });
        message.node = NavNode::Connection("b".to_string());
        let rows = vec![conn(0, "a", true), child(1), conn(0, "b", false), message.clone(), message];
        assert_eq!(pinned_row(&rows, 3.5 * PITCH, PITCH), None, "b is not expanded: its message scrolls away");
    }

    #[test]
    fn the_toolbar_gives_way_in_order() {
        assert_eq!(toolbar_buttons(0.0), 2, "⋯ and New connection are always there");
        assert_eq!(toolbar_buttons(-40.0), 2);
        assert_eq!(toolbar_buttons(111.0), 4, "Refresh and Disconnect at the default width");
        assert_eq!(toolbar_buttons(138.0), 5);
        assert_eq!(toolbar_buttons(400.0), 6);
    }
}
