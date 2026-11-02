//! Schema compare (FR-M3): compare two schemas (same or different
//! connections), review the differences, generate the migration script in
//! the target dialect, apply it, export an HTML report or a Liquibase
//! changelog.

use crate::backend::Job;
use crate::command::AppCommand;
use crate::conn::session::SessionHandle;
use crate::cx::AppContext;
use crate::sqllang::SqlLanguage;
use crate::tabs::TabView;
use crate::tabs::dbtools::look::{self, Cell};
use crate::tabs::transfer::wizard::{W, choice};
use egui::{Align, Layout, Margin, RichText, Sense, Ui, UiBuilder, vec2};
use egui_phosphor::regular as icon;
use std::collections::BTreeSet;
use std::sync::Arc;
use ternarygrid_compare::{CompareOptions, DiffItem, DiffStatus, LiquibaseOptions, SchemaDiff, ScriptOptions};
use ternarygrid_core::ddl::{SchemaSnapshot, SnapshotOptions};
use ternarygrid_core::meta::SchemaRef;
use ternarygrid_core::{DbError, DbResult, tr, trf};
use ternarygrid_ui::{kit, parts, style};

#[derive(Clone, Default, PartialEq)]
struct Side {
    conn: Option<String>,
    schema: Option<SchemaRef>,
}

type Snapshots = (Arc<SchemaSnapshot>, Arc<SchemaSnapshot>);

pub struct SchemaCompareTab {
    uid: u64,
    source: Side,
    target: Side,
    job: Option<Job<DbResult<Snapshots>>>,
    snapshots: Option<Snapshots>,
    diff: Option<SchemaDiff>,
    /// Keys (`kind:qualified name`) of the objects excluded from the script.
    excluded: BTreeSet<String>,
    /// The pair `excluded` belongs to (kept when the same pair is compared again).
    compared: Option<(String, SchemaRef, String, SchemaRef)>,
    show_unchanged: bool,
    drop_removed: bool,
    cascade: bool,
    script: Option<Vec<String>>,
    /// Highlighting of the script (the target's dialect).
    script_lang: Option<SqlLanguage>,
    apply_job: Option<Job<DbResult<usize>>>,
    confirm_apply: bool,
    error: Option<String>,
    selected: Option<String>,
}

/// Run the migration script on the target connection's own session (the
/// navigator's); returns the number of statements run.
pub(crate) async fn apply_script(s: SessionHandle, target_schema: Option<String>, stmts: Vec<String>) -> DbResult<usize> {
    // The session is shared: other uncommitted work would be committed with the migration.
    if s.has_uncommitted_writes() {
        return Err(DbError::config(tr("The session has uncommitted changes (another tab, the data editor): commit or roll them back before applying the migration.")));
    }
    // Unqualified names in view bodies and foreign keys resolve in the active schema:
    // it is the target for the run, then the session's own again.
    let previous = match &target_schema {
        Some(t) => {
            let mut g = s.lock().await;
            let previous = match g.current_schema().await {
                Ok(Some(p)) => Some(p),
                _ => g.current_database().await.ok().flatten(),
            };
            if previous.as_deref() == Some(t.as_str()) {
                None
            } else {
                g.set_current_schema(t).await?;
                previous
            }
        }
        None => None,
    };
    let mut n = 0;
    let mut result = Ok(());
    for sql in &stmts {
        if let Err(e) = s.execute_update(sql, &[]).await {
            result = Err(e.with_context(sql.chars().take(200).collect::<String>()));
            break;
        }
        n += 1;
    }
    // In manual commit mode the migration would stay in an open transaction holding
    // its locks: committed, or undone when a statement failed.
    if s.tx_state().in_transaction {
        match &result {
            Ok(()) => result = s.commit().await,
            Err(_) => {
                let _ = s.rollback().await;
            }
        }
    }
    if let Some(p) = &previous {
        let _ = s.lock().await.set_current_schema(p).await;
    }
    result.map(|_| n)
}

fn key(i: &DiffItem) -> String {
    format!("{:?}:{}", i.kind, i.qualified_name())
}

fn status_color(s: DiffStatus, p: &style::Palette) -> egui::Color32 {
    match s {
        DiffStatus::Added => p.ok,
        DiffStatus::Removed => p.err,
        DiffStatus::Modified => p.warn,
        DiffStatus::Unchanged => p.text_faint,
    }
}

/// The item with `key` in the tree.
fn find<'a>(items: &'a [DiffItem], k: &str) -> Option<&'a DiffItem> {
    items.iter().find_map(|i| if key(i) == k { Some(i) } else { find(&i.children, k) })
}

/// Width of the source and target connection pickers.
const CONN_W: f32 = 170.0;
const SCHEMA_W: f32 = 130.0;

impl SchemaCompareTab {
    pub fn new(conn: Option<String>, schema: Option<SchemaRef>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        SchemaCompareTab {
            uid: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            source: Side { conn: conn.clone(), schema: schema.clone() },
            target: Side { conn, schema: None },
            job: None,
            snapshots: None,
            diff: None,
            excluded: BTreeSet::new(),
            compared: None,
            show_unchanged: false,
            drop_removed: true,
            cascade: false,
            script: None,
            script_lang: None,
            apply_job: None,
            confirm_apply: false,
            error: None,
            selected: None,
        }
    }

    /// The logo, the connection and the schema of one side.
    fn side_ui(ui: &mut Ui, cx: &mut AppContext, id: &str, side: &mut Side) {
        ui.spacing_mut().item_spacing.x = 6.0;
        let names: Vec<(String, String)> = cx.conns.configs().iter().map(|c| (c.id.clone(), c.name.clone())).collect();
        let current = side.conn.as_ref().and_then(|c| names.iter().find(|(i, _)| i == c)).map(|(_, n)| n.clone()).unwrap_or_else(|| tr("Choose…").to_string());
        if let Some(kind) = side.conn.as_ref().and_then(|c| cx.conns.config(c)).map(|c| c.kind) {
            look::logo(ui, kind, 15.0);
            // 8 from the connection (the schema is 6 from it).
            ui.add_space(2.0);
        }
        egui::ComboBox::from_id_salt((id, "conn")).icon(kit::combo_caret).selected_text(current).width(CONN_W).show_ui(ui, |ui| {
            kit::menu_look(ui, CONN_W);
            for (cid, name) in &names {
                if choice(ui, side.conn.as_deref() == Some(cid), name) {
                    side.conn = Some(cid.clone());
                    side.schema = None;
                }
            }
        });
        let Some(conn) = side.conn.clone() else { return };
        if !cx.conns.is_connected(&conn) {
            if kit::button_ex(ui, kit::Kind::Secondary, Some(icon::PLUGS_CONNECTED), tr("Connect"), true).clicked() {
                cx.push(AppCommand::Connect(conn));
            }
            return;
        }
        let backend = cx.backend;
        let Some(live) = cx.conns.live_mut(&conn) else { return };
        let main = live.main.clone();
        live.meta.request_schemas(backend, &main, None);
        let schemas: Vec<String> = live.meta.schemas_of(None).get().map(|v| v.iter().map(|s| s.name.clone()).collect()).unwrap_or_default();
        let db = live.current_database.clone();
        let label = side.schema.as_ref().and_then(|s| s.schema.clone()).unwrap_or_else(|| tr("Schema…").to_string());
        egui::ComboBox::from_id_salt((id, "schema")).icon(kit::combo_caret).selected_text(label).width(SCHEMA_W).show_ui(ui, |ui| {
            kit::menu_look(ui, SCHEMA_W);
            if schemas.is_empty() {
                // Single-schema databases (SQLite, MySQL database = schema).
                let cur = live.current_schema.clone().or(db.clone());
                if choice(ui, false, &cur.clone().unwrap_or_else(|| tr("(default)").to_string())) {
                    side.schema = Some(SchemaRef::new(None, cur));
                }
            }
            for s in &schemas {
                if choice(ui, side.schema.as_ref().and_then(|x| x.schema.as_deref()) == Some(s.as_str()), s) {
                    side.schema = Some(SchemaRef::new(None, Some(s.clone())));
                }
            }
        });
    }

    fn start(&mut self, cx: &mut AppContext) {
        let (Some(sc), Some(ss), Some(tc), Some(ts)) = (self.source.conn.clone(), self.source.schema.clone(), self.target.conn.clone(), self.target.schema.clone()) else {
            self.error = Some(tr("Choose the source and target schemas.").to_string());
            return;
        };
        let (Some(a), Some(b)) = (cx.conns.live(&sc).map(|l| l.main.clone()), cx.conns.live(&tc).map(|l| l.main.clone())) else { return };
        self.error = None;
        self.diff = None;
        self.script = None;
        let pair = (sc, ss.clone(), tc, ts.clone());
        if self.compared.as_ref() != Some(&pair) {
            self.excluded.clear();
            self.compared = Some(pair);
        }
        self.job = Some(cx.backend.spawn(async move {
            let opts = SnapshotOptions::full();
            let sa = a.lock().await.schema_snapshot(&ss, &opts).await?;
            let sb = b.lock().await.schema_snapshot(&ts, &opts).await?;
            Ok((Arc::new(sa), Arc::new(sb)))
        }));
    }

    fn selected_diff(&self) -> Option<SchemaDiff> {
        let mut d = self.diff.clone()?;
        d.retain(|i| !self.excluded.contains(&key(i)));
        Some(d)
    }

    fn script_options(&self) -> ScriptOptions {
        ScriptOptions { drop_removed: self.drop_removed, cascade: self.cascade, include_comments: true, ..Default::default() }
    }

    fn build_script(&mut self, cx: &AppContext) {
        let (Some(d), Some((a, b)), Some(tc)) = (self.selected_diff(), self.snapshots.clone(), self.target.conn.clone()) else { return };
        let Some(dialect) = cx.conns.live(&tc).map(|l| l.dialect.clone()) else { return };
        self.script = Some(ternarygrid_compare::migration_script(&d, &a, &b, dialect.as_ref(), &self.script_options()));
        self.script_lang = Some(SqlLanguage::new(dialect));
    }

    /// One row of the differences tree: the check box (top level), the status, the kind's
    /// icon and the name; the row selects the object.
    fn item_row(&mut self, ui: &mut Ui, item: &DiffItem, depth: usize) {
        if !self.show_unchanged && !item.has_changes() {
            return;
        }
        let p = style::pal(ui);
        let k = key(item);
        let selected = self.selected.as_deref() == Some(k.as_str());
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, item.display_name()));
        if selected {
            ui.painter().rect_filled(rect, style::RADIUS, p.accent_soft);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, style::RADIUS, p.faint_row());
        }
        let mut row = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
        row.spacing_mut().item_spacing.x = 8.0;
        row.add_space(10.0 + depth as f32 * 16.0);
        if depth == 0 {
            let mut on = !self.excluded.contains(&k);
            if look::cell_check(&mut row, &mut on, tr("Include in the script")).on_hover_text(tr("Include in the script")).changed() {
                if on {
                    self.excluded.remove(&k);
                } else {
                    self.excluded.insert(k.clone());
                }
                self.script = None;
            }
        }
        row.allocate_ui_with_layout(vec2(62.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(62.0);
            ui.label(RichText::new(tr(item.status.label())).font(style::semibold(ui.ctx(), 11.0)).color(status_color(item.status, &p)));
        });
        row.label(RichText::new(crate::nav::kind_icon(item.kind)).font(style::icon_font(14.0)).color(p.text_muted));
        // Objects with their schema, their parts (columns, indexes) by name.
        let name = if depth == 0 { item.qualified_name() } else { item.display_name() };
        row.add(egui::Label::new(RichText::new(name).color(p.text)).truncate().selectable(false));
        if resp.clicked() {
            self.selected = Some(k.clone());
        }
        for c in &item.children {
            self.item_row(ui, c, depth + 1);
        }
    }

    /// The property changes of the selected object, with those of its parts ("status · type";
    /// an added or removed part as its kind on its side).
    fn changes_ui(&self, ui: &mut Ui, diff: &SchemaDiff) {
        let Some(item) = self.selected.as_deref().and_then(|k| find(&diff.items, k)) else { return };
        let p = style::pal(ui);
        let value = |v: &Option<String>| match v {
            Some(v) => Cell::new(v.clone()).mono(),
            None => Cell::new("—").mono().color(p.text_faint),
        };
        let mut rows: Vec<Vec<Cell>> = item.changes.iter().map(|c| vec![Cell::new(c.property.clone()), value(&c.source), value(&c.target)]).collect();
        for child in item.children.iter().filter(|c| c.has_changes()) {
            let kind = Some(child.kind.label().to_lowercase());
            match child.status {
                DiffStatus::Added => rows.push(vec![Cell::new(child.display_name()), value(&kind), value(&None)]),
                DiffStatus::Removed => rows.push(vec![Cell::new(child.display_name()), value(&None), value(&kind)]),
                _ => rows.extend(child.changes.iter().map(|c| vec![Cell::new(format!("{} · {}", child.display_name(), c.property)), value(&c.source), value(&c.target)])),
            }
        }
        if rows.is_empty() {
            return;
        }
        // As in the design: 12 below the tree and above the table (the rows' spacing is 8).
        ui.add_space(12.0);
        look::title(ui, &item.qualified_name());
        ui.add_space(4.0);
        let widths = [W::Fr(1.0), W::Fr(1.0), W::Fr(1.0)];
        look::table_w(ui, ("cmp_changes", self.uid), &look::headers(&[tr("Property"), tr("Source"), tr("Target")]), &rows, None, &widths);
    }

    /// Counts at the right of the toolbar.
    fn counts_ui(ui: &mut Ui, diff: &SchemaDiff) {
        let p = style::pal(ui);
        let t = diff.summary.total;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new(trf("{} unchanged", &[&t.unchanged])).size(12.5).color(p.text_faint));
            ui.label(RichText::new(trf("{} modified", &[&t.modified])).size(12.5).color(p.warn));
            ui.label(RichText::new(trf("{} removed", &[&t.removed])).size(12.5).color(p.err));
            ui.label(RichText::new(trf("{} added", &[&t.added])).size(12.5).color(p.ok));
        })
        .response
        .on_hover_text(trf("{} added, {} removed, {} modified, {} unchanged", &[&t.added, &t.removed, &t.modified, &t.unchanged]));
    }

    /// The script actions: SQL editor, apply, diagram, report, changelog.
    fn actions_ui(&mut self, ui: &mut Ui, cx: &mut AppContext, diff: &SchemaDiff, script: &[String]) {
        let target = self.target.conn.clone();
        if look::small(ui, kit::Kind::Secondary, icon::FILE_SQL, tr("Open script in SQL editor")).clicked() {
            let text = script.iter().map(|s| format!("{s};\n")).collect::<String>();
            cx.push(AppCommand::NewSqlEditor { connection: target.clone(), schema: self.target.schema.clone(), text: Some(text) });
        }
        let can_apply = !script.is_empty() && self.apply_job.is_none();
        if ui.add_enabled_ui(can_apply, |ui| look::small(ui, kit::Kind::Danger, icon::PLAY, tr("Apply to target…"))).inner.clicked() {
            self.confirm_apply = true;
        }
        if self.apply_job.is_some() {
            parts::spinner_label(ui, tr("Applying…"));
        }
        if look::small(ui, kit::Kind::Ghost, icon::GRAPH, tr("Diff diagram")).on_hover_text(tr("ER diagram of the target with the differences highlighted")).clicked()
            && let (Some((a, b)), Some(tc)) = (self.snapshots.clone(), self.target.conn.clone())
        {
            let old = ternarygrid_visual::er::ErModel::from_snapshot(&b, None);
            let new = ternarygrid_visual::er::ErModel::from_snapshot(&a, None);
            let model = ternarygrid_visual::er::ErModel::diff(&old, &new);
            let name = trf("Diff {}", &[&self.target.schema.as_ref().and_then(|s| s.schema.clone()).unwrap_or_default()]);
            let diagram = ternarygrid_visual::er::ErDiagram::new(name.clone(), model);
            let schema = self.target.schema.clone().unwrap_or_else(|| SchemaRef::new(None, None));
            cx.push(AppCommand::OpenTab(Box::new(crate::tabs::er::ErTab::from_saved(tc, schema, name, diagram))));
        }
        if look::small(ui, kit::Kind::Ghost, icon::EXPORT, tr("HTML report…")).clicked()
            && let Some(path) = rfd::FileDialog::new().set_file_name("schema-compare.html").save_file()
        {
            let src = self.source.schema.as_ref().and_then(|s| s.schema.clone()).unwrap_or_default();
            let dst = self.target.schema.as_ref().and_then(|s| s.schema.clone()).unwrap_or_default();
            let html = ternarygrid_compare::html_report(diff, &src, &dst);
            if let Err(e) = std::fs::write(&path, html) {
                cx.toasts.error(tr("Cannot write the file"), Some(e.to_string()));
            }
        }
        if look::small(ui, kit::Kind::Ghost, icon::FILE, tr("Liquibase changelog…")).clicked()
            && let (Some(d), Some((a, b))) = (self.selected_diff(), self.snapshots.clone())
            && let Some(path) = rfd::FileDialog::new().set_file_name("changelog.xml").save_file()
        {
            let opts = LiquibaseOptions { script: self.script_options(), dialect: target.as_ref().and_then(|t| cx.conns.live(t)).map(|l| l.dialect.clone()), ..Default::default() };
            let xml = ternarygrid_compare::liquibase_changelog(&d, &a, &b, &opts);
            if let Err(e) = std::fs::write(&path, xml) {
                cx.toasts.error(tr("Cannot write the file"), Some(e.to_string()));
            }
        }
    }

    /// "Apply the migration" confirmation.
    fn apply_modal(&mut self, ui: &mut Ui, cx: &mut AppContext, script: &[String]) {
        if !self.confirm_apply {
            return;
        }
        let target_name = self.target.conn.as_ref().and_then(|c| cx.conns.config(c)).map(|c| c.name.clone()).unwrap_or_default();
        let production = self.target.conn.as_ref().is_some_and(|c| cx.conns.type_of(c).is_production());
        let question = trf("Run {} statement(s) on \"{}\"?", &[&script.len(), &target_name]);
        let warning = trf("{} is a Production connection: the schema changes reach the live database.", &[&target_name]);
        let (mut run, mut cancel) = (false, false);
        let (resp, _) = parts::Dialog::new(tr("Apply the migration")).icon(icon::PLAY).width(520.0).body_padding(egui::Margin::symmetric(20, 18)).show_modal(
            ui.ctx(),
            egui::Id::new(("cmp_apply", self.uid)),
            |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.label(RichText::new(&question).size(13.5));
                if production {
                    look::note(ui, parts::Tone::Warning, None, &warning);
                }
            },
            |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if kit::button_ex(ui, kit::Kind::DangerFilled, Some(icon::PLAY), tr("Run"), false).clicked() {
                        run = true;
                    }
                    if kit::button_ex(ui, kit::Kind::Ghost, None, tr("Cancel"), false).clicked() {
                        cancel = true;
                    }
                });
            },
        );
        if run {
            let read_only = self.target.conn.as_ref().and_then(|c| cx.conns.config(c)).filter(|c| c.read_only).map(|c| c.name.clone());
            if let Some(name) = read_only {
                self.error = Some(trf("The connection \"{}\" is read-only.", &[&name]));
            } else if let Some(live) = self.target.conn.as_ref().and_then(|c| cx.conns.live(c)) {
                let s = live.main.clone();
                let target_schema = self.target.schema.as_ref().and_then(|t| t.schema.clone());
                let stmts = script.to_vec();
                self.error = None;
                self.apply_job = Some(cx.backend.spawn(async move { apply_script(s, target_schema, stmts).await }));
            }
        }
        if run || cancel || resp.close {
            self.confirm_apply = false;
        }
    }

    // ------------------------------------------------- headless test hooks

    /// Compare `source` with `target` (as choosing them and Compare).
    #[doc(hidden)]
    pub fn compare_now(&mut self, cx: &mut AppContext, source: (&str, SchemaRef), target: (&str, SchemaRef)) {
        self.source = Side { conn: Some(source.0.to_string()), schema: Some(source.1) };
        self.target = Side { conn: Some(target.0.to_string()), schema: Some(target.1) };
        self.start(cx);
    }

    /// Choose the source and the target (Compare is still to be clicked).
    #[doc(hidden)]
    pub fn set_sides(&mut self, source: (&str, SchemaRef), target: (&str, SchemaRef)) {
        self.source = Side { conn: Some(source.0.to_string()), schema: Some(source.1) };
        self.target = Side { conn: Some(target.0.to_string()), schema: Some(target.1) };
    }

    #[doc(hidden)]
    pub fn is_busy(&self) -> bool {
        self.job.is_some() || self.apply_job.is_some()
    }

    #[doc(hidden)]
    pub fn diff(&self) -> Option<&SchemaDiff> {
        self.diff.as_ref()
    }

    /// Select the first modified object (its property changes are shown).
    #[doc(hidden)]
    pub fn select_first_modified(&mut self) {
        self.selected = self.diff.as_ref().and_then(|d| d.items.iter().find(|i| i.status == DiffStatus::Modified)).map(key);
    }

    /// Ask to apply the script (the confirmation opens).
    #[doc(hidden)]
    pub fn ask_apply(&mut self) {
        self.confirm_apply = true;
    }
}

impl TabView for SchemaCompareTab {
    fn title(&self) -> String {
        tr("Schema compare").to_string()
    }
    fn icon(&self) -> &'static str {
        icon::ARROWS_DOWN_UP
    }
    fn connection_id(&self) -> Option<&str> {
        self.target.conn.as_deref()
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if let Some(j) = self.job.as_mut()
            && let Some(r) = j.poll()
        {
            self.job = None;
            match r {
                Ok((a, b)) => {
                    let diff = ternarygrid_compare::schema_diff(&a, &b, &CompareOptions::default());
                    self.snapshots = Some((a, b));
                    self.diff = Some(diff);
                }
                Err(e) => self.error = Some(e.full_text()),
            }
        }
        if let Some(j) = self.apply_job.as_mut()
            && let Some(r) = j.poll()
        {
            self.apply_job = None;
            match r {
                Ok(n) => {
                    cx.toasts.success(trf("{} statement(s) executed on the target", &[&n]));
                    if let (Some(tc), Some(ts)) = (&self.target.conn, &self.target.schema)
                        && let Some(l) = cx.conns.live_mut(tc)
                    {
                        l.meta.clear_schema(ts);
                    }
                    self.start(cx);
                }
                Err(e) => self.error = Some(e.full_text()),
            }
        }
        let p = style::pal(ui);
        ui.spacing_mut().item_spacing.y = 0.0;
        // Source and target (28 px pickers), straight above the toolbar.
        egui::Frame::new().inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
            ui.spacing_mut().interact_size.y = 28.0;
            egui::Grid::new(("cmp_sides", self.uid)).num_columns(2).spacing([12.0, 8.0]).min_col_width(70.0).show(ui, |ui| {
                look::muted(ui, tr("Source"));
                ui.horizontal(|ui| Self::side_ui(ui, cx, "src", &mut self.source));
                ui.end_row();
                look::muted(ui, tr("Target"));
                ui.horizontal(|ui| Self::side_ui(ui, cx, "dst", &mut self.target));
                ui.end_row();
            });
        });
        let diff = self.diff.clone();
        parts::toolbar(ui, |ui| {
            let busy = self.job.is_some();
            if ui.add_enabled_ui(!busy, |ui| look::small(ui, kit::Kind::Primary, icon::ARROWS_DOWN_UP, tr("Compare"))).inner.clicked() {
                self.start(cx);
            }
            if look::small(ui, kit::Kind::Ghost, icon::REPEAT, tr("Swap")).clicked() {
                std::mem::swap(&mut self.source, &mut self.target);
                self.diff = None;
                self.script = None;
            }
            if busy {
                parts::spinner_label(ui, tr("Reading both schemas…"));
            }
            kit::vdivider(ui, 18.0);
            look::checkbox(ui, &mut self.show_unchanged, tr("Show unchanged"));
            if look::checkbox(ui, &mut self.drop_removed, tr("Drop objects missing in the source")).changed() {
                self.script = None;
            }
            if look::checkbox(ui, &mut self.cascade, tr("Cascade")).changed() {
                self.script = None;
            }
            if let Some(d) = &diff {
                Self::counts_ui(ui, d);
            }
        });
        if let Some(e) = &self.error {
            egui::Frame::new().inner_margin(Margin::symmetric(16, 10)).show(ui, |ui| look::error(ui, e));
        }
        let Some(diff) = diff else {
            if self.job.is_none() && self.error.is_none() {
                kit::empty_state(ui, icon::ARROWS_DOWN_UP, tr("Schema compare"), tr("Choose the source and the target schema, then Compare."), |_| {});
            }
            return;
        };
        if self.script.is_none() {
            self.build_script(cx);
        }
        let script = self.script.clone().unwrap_or_default();
        parts::toolbar(ui, |ui| self.actions_ui(ui, cx, &diff, &script));
        // The script on the right, the differences on the left: 560 wide as in the design (the
        // panel's size includes its separator).
        egui::Panel::right(egui::Id::new(("cmp_script_panel", self.uid)))
            .resizable(true)
            .default_size(561.0)
            .size_range(240.0..=1200.0)
            .frame(egui::Frame::new().fill(p.surface))
            .show(ui, |ui| {
                let text = script.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
                match &self.script_lang {
                    Some(lang) => {
                        let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
                        let options = look::code_options(13.0, 21.0);
                        ternarygrid_editor::CodeView::new(&text, lang, &theme).options(&options).id_salt(("cmp_script", self.uid)).show(ui);
                    }
                    None => {
                        let mut t = text.as_str();
                        ui.add(egui::TextEdit::multiline(&mut t).code_editor().desired_width(f32::INFINITY));
                    }
                }
            });
        egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(Margin::symmetric(16, 12))).show(ui, |ui| {
            if !diff.has_differences() {
                parts::status(ui, parts::Tone::Success, tr("The schemas are identical."));
                ui.add_space(8.0);
            }
            let changes_h = if self.selected.is_some() { (ui.available_height() * 0.4).clamp(120.0, 260.0) } else { 0.0 };
            egui::ScrollArea::vertical().id_salt(("cmp_items", self.uid)).auto_shrink([false, true]).max_height((ui.available_height() - changes_h).max(80.0)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                for item in &diff.items {
                    self.item_row(ui, item, 0);
                }
            });
            ui.spacing_mut().item_spacing.y = 8.0;
            self.changes_ui(ui, &diff);
        });
        self.apply_modal(ui, cx, &script);
        if self.job.is_some() || self.apply_job.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    crate::impl_tab_any!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternarygrid_core::driver::{ConnectParams, Driver, TransactionMode};
    use ternarygrid_core::{ConnectionConfig, DbKind};

    async fn manual_session() -> SessionHandle {
        let mut cfg = ConnectionConfig::new(DbKind::Sqlite, "cmp");
        cfg.file_path = Some(format!(":memory:cmp-{}", uuid::Uuid::new_v4().simple()));
        let s = ternarygrid_driver_sqlite::SqliteDriver::new().connect(&ConnectParams::from_config(&cfg, None)).await.unwrap();
        let h = SessionHandle::new(s, "cmp", None);
        h.set_transaction_mode(TransactionMode::Manual).await.unwrap();
        h
    }

    async fn tables(s: &SessionHandle) -> Vec<String> {
        let (_, rows) = s.query_all("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name", &[]).await.unwrap();
        rows.iter().map(|r| r[0].to_string()).collect()
    }

    #[tokio::test]
    async fn applying_a_migration_in_manual_commit_mode() {
        // Committed: nothing is left open on the shared session.
        let s = manual_session().await;
        let n = apply_script(s.clone(), Some("main".into()), vec!["CREATE TABLE a (id INT)".into(), "CREATE TABLE b (id INT)".into()]).await.unwrap();
        assert_eq!(n, 2);
        assert!(!s.tx_state().in_transaction);
        assert_eq!(tables(&s).await, vec!["a", "b"]);
        s.rollback().await.unwrap();
        assert_eq!(tables(&s).await, vec!["a", "b"]);

        // A failing statement undoes the ones before it.
        let s = manual_session().await;
        let e = apply_script(s.clone(), None, vec!["CREATE TABLE c (id INT)".into(), "CREATE TABLE c (id INT)".into()]).await.unwrap_err();
        assert!(e.full_text().contains("CREATE TABLE c"), "{}", e.full_text());
        assert!(!s.tx_state().in_transaction);
        assert!(tables(&s).await.is_empty());

        // Someone else's uncommitted change is not committed with the migration.
        let s = manual_session().await;
        s.execute_update("CREATE TABLE mine (id INT)", &[]).await.unwrap();
        s.execute_update("INSERT INTO mine VALUES (1)", &[]).await.unwrap();
        assert!(s.has_uncommitted_writes());
        assert!(apply_script(s.clone(), None, vec!["CREATE TABLE d (id INT)".into()]).await.is_err());
        assert!(s.tx_state().in_transaction);
        assert_eq!(tables(&s).await, vec!["mine"]);
    }
}
