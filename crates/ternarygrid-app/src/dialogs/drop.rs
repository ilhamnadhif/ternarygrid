//! Drop objects: confirmation with the exact DDL, optional CASCADE, then
//! execution on the connection's session and a metadata refresh.

use crate::backend::Job;
use crate::command::AppCommand;
use crate::conn::session::SessionHandle;
use crate::cx::AppContext;
use egui::RichText;
use egui_phosphor::regular as icon;
use std::cell::Cell;
use ternarygrid_core::ddl::{CheckDef, ForeignKeyDef, IndexDef, KeyDef, RoutineDef, SequenceDef, TableDef, TriggerDef, ViewDef};
use ternarygrid_core::meta::{ObjectKind, ObjectRef};
use ternarygrid_core::{DbResult, Dialect, tr, trf};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::{self, Dialog};
use ternarygrid_ui::style;

pub struct DropDialog {
    conn: String,
    objects: Vec<ObjectRef>,
    cascade: bool,
    job: Option<Job<DbResult<usize>>>,
    error: Option<String>,
}

/// `DROP` statement for one object in the connection's dialect.
pub fn drop_sql(d: &dyn Dialect, o: &ObjectRef, cascade: bool) -> String {
    let ddl = d.ddl();
    let parent = || {
        let mut t = TableDef::new(o.schema.clone(), o.table.clone().unwrap_or_default());
        t.schema = o.schema.clone();
        t
    };
    let cascade_sfx = if cascade { " CASCADE" } else { "" };
    match o.kind {
        // A MySQL partition is no table: `DROP TABLE p0` would drop a table of that name.
        ObjectKind::Partition if matches!(d.kind(), ternarygrid_core::DbKind::MySql | ternarygrid_core::DbKind::MariaDb) => {
            let table = ObjectRef::table(o.schema.clone(), o.table.clone().unwrap_or_default()).with_database(o.database.clone());
            format!("ALTER TABLE {} DROP PARTITION {}", d.qualified_ref(&table), d.quote_ident(&o.name))
        }
        ObjectKind::Table | ObjectKind::ForeignTable | ObjectKind::Partition => ddl.drop_table(o.schema.as_deref(), &o.name, cascade),
        ObjectKind::View | ObjectKind::MaterializedView => {
            let v = ViewDef { schema: o.schema.clone(), name: o.name.clone(), materialized: o.kind == ObjectKind::MaterializedView, ..Default::default() };
            ddl.drop_view(&v) + cascade_sfx
        }
        ObjectKind::Sequence => ddl.drop_sequence(&SequenceDef { schema: o.schema.clone(), name: o.name.clone(), ..Default::default() }) + cascade_sfx,
        ObjectKind::Function | ObjectKind::Procedure | ObjectKind::Aggregate => {
            let r = RoutineDef {
                schema: o.schema.clone(),
                name: o.name.clone(),
                kind: Some(o.kind),
                arguments: o.signature.clone().map(|s| s.trim_start_matches('(').trim_end_matches(')').to_string()).unwrap_or_default(),
                ..Default::default()
            };
            ddl.drop_routine(&r)
        }
        ObjectKind::Index => ddl.drop_index(&parent(), &IndexDef { name: o.name.clone(), ..Default::default() }),
        ObjectKind::Trigger => ddl.drop_trigger(&parent(), &TriggerDef { name: o.name.clone(), ..Default::default() }),
        ObjectKind::ForeignKey => ddl.drop_foreign_key(&parent(), &ForeignKeyDef { name: Some(o.name.clone()), ..Default::default() }),
        ObjectKind::PrimaryKey => ddl.drop_primary_key(&parent(), &KeyDef { name: Some(o.name.clone()), ..Default::default() }),
        ObjectKind::UniqueKey => ddl.drop_unique_key(&parent(), &KeyDef { name: Some(o.name.clone()), ..Default::default() }),
        ObjectKind::CheckConstraint => ddl.drop_check(&parent(), &CheckDef { name: Some(o.name.clone()), ..Default::default() }),
        ObjectKind::Column => ddl.drop_column(&parent(), &o.name),
        ObjectKind::Schema => ddl.drop_schema(&o.name, cascade),
        ObjectKind::Database => format!("DROP DATABASE {}", d.quote_ident(&o.name)),
        other => format!("DROP {} {}{cascade_sfx}", other.label().to_uppercase(), d.qualified(o.schema.as_deref(), &o.name)),
    }
}

impl DropDialog {
    pub fn new(conn: String, objects: Vec<ObjectRef>) -> Self {
        DropDialog { conn, objects, cascade: false, job: None, error: None }
    }

    /// Draw; returns false when closed.
    pub fn show(&mut self, ctx: &egui::Context, cx: &mut AppContext) -> bool {
        if let Some(j) = self.job.as_mut()
            && let Some(r) = j.poll()
        {
            self.job = None;
            match r {
                Ok(n) => {
                    cx.toasts.success(trf("{} object(s) dropped", &[&n]));
                    // A dropped database leaves the list of databases.
                    if self.objects.iter().any(|o| o.kind == ObjectKind::Database) {
                        cx.push(AppCommand::RefreshMetadata { connection: self.conn.clone(), object: None, schema: None });
                        return false;
                    }
                    let mut schemas: Vec<_> = self.objects.iter().map(|o| o.schema_ref()).collect();
                    schemas.sort();
                    schemas.dedup();
                    for s in schemas {
                        cx.push(AppCommand::RefreshMetadata { connection: self.conn.clone(), object: None, schema: Some(s) });
                    }
                    return false;
                }
                Err(e) => self.error = Some(e.full_text()),
            }
        }
        let backend = cx.backend;
        let Some(live) = cx.conns.live_mut(&self.conn) else { return false };
        let dialect = live.dialect.clone();
        // Each object is dropped in its own database (another database of the server has its own session).
        let sessions: Vec<Option<SessionHandle>> =
            self.objects.iter().map(|o| if o.kind == ObjectKind::Database { Some(live.main.clone()) } else { live.ensure_session(backend, o.database.as_deref()) }).collect();
        // A session that failed to open: its error instead of waiting for ever.
        let session_error = self.objects.iter().zip(&sessions).filter(|(_, s)| s.is_none()).find_map(|(o, _)| live.session_error(o.database.as_deref()));
        let opening = sessions.iter().any(Option::is_none);
        // TernaryGrid's own sessions on a database being dropped (the navigator's): the server
        // refuses the DROP while they are connected.
        let own: Vec<SessionHandle> =
            self.objects.iter().filter(|o| o.kind == ObjectKind::Database).filter_map(|o| live.db_sessions.get(&o.name).and_then(|l| l.get()).cloned()).collect();
        let read_only = live.config.read_only;
        // The subtitle: the connection, and the schema when every object is in the same one.
        let mut place = live.config.name.clone();
        if let Some(s) = self.objects.first().and_then(|o| o.schema.clone()).filter(|s| self.objects.iter().all(|o| o.schema.as_deref() == Some(s.as_str()))) {
            place = format!("{place} · {s}");
        }
        let statements: Vec<String> = self.objects.iter().map(|o| drop_sql(dialect.as_ref(), o, self.cascade)).collect();
        let script = statements.iter().map(|s| format!("{s};\n")).collect::<String>();
        let busy = self.job.is_some();
        let (drop, editor, cancel) = (Cell::new(false), Cell::new(false), Cell::new(false));
        let error = self.error.clone();
        let count = self.objects.len();
        let cascade = &mut self.cascade;
        let (resp, _) = Dialog::new(tr("Drop objects")).icon(icon::TRASH).subtitle(&place).width(580.0).body_padding(super::BODY).show_modal(
            ctx,
            egui::Id::new(("drop_objects", &self.conn)),
            |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                let question = if count == 1 { tr("Drop 1 object? This cannot be undone.").to_string() } else { trf("Drop {} objects? This cannot be undone.", &[&count]) };
                ui.add(egui::Label::new(RichText::new(question).font(style::medium(ui.ctx(), 13.5))).wrap());
                ternarygrid_ui::kit::checkbox(ui, cascade, tr("Cascade (also drop dependent objects)"));
                super::sql_preview(ui, "drop_sql", Some(dialect.clone()), script.trim_end(), 240.0, true);
                if read_only {
                    crate::dialogs::error_note(ui, tr("The connection is read-only."));
                }
                if let Some(e) = &session_error {
                    crate::dialogs::error_note(ui, e);
                } else if opening {
                    parts::spinner_label(ui, tr("Opening a session…"));
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                }
                if let Some(e) = &error {
                    crate::dialogs::error_note(ui, e);
                }
            },
            |ui| {
                if kit::button_ex(ui, Kind::Ghost, Some(icon::FILE_CODE), tr("Open in SQL editor"), false).clicked() {
                    editor.set(true);
                }
                super::right(ui, |ui| {
                    if ui.add_enabled_ui(!busy && !read_only && !opening, |ui| kit::button_ex(ui, Kind::DangerFilled, Some(icon::TRASH), tr("Drop"), false)).inner.clicked() {
                        drop.set(true);
                    }
                    if ui.add_enabled_ui(!busy, super::cancel_button).inner.clicked() {
                        cancel.set(true);
                    }
                    if busy {
                        ui.add(egui::Spinner::new().size(14.0));
                    }
                });
            },
        );
        if drop.get() && !busy && !read_only && !opening {
            let work: Vec<(String, SessionHandle)> = statements.iter().cloned().zip(sessions.iter().flatten().cloned()).collect();
            self.error = None;
            self.job = Some(backend.spawn(async move {
                // The sessions are shared: other uncommitted work would be committed with the drop.
                if work.iter().map(|(_, s)| s).chain(&own).any(|s| s.has_uncommitted_writes()) {
                    return Err(ternarygrid_core::DbError::config(tr(
                        "The session has uncommitted changes (another tab, the data editor): commit or roll them back before dropping objects.",
                    )));
                }
                for s in &own {
                    s.close().await;
                }
                let mut n = 0;
                for (sql, s) in &work {
                    s.execute_update(sql, &[]).await.map_err(|e| e.with_context(sql.clone()))?;
                    n += 1;
                }
                // In manual commit mode the drop would stay in an open transaction (holding its locks).
                let mut done: Vec<&SessionHandle> = Vec::new();
                for (_, s) in &work {
                    if !done.iter().any(|d| d.same(s)) {
                        done.push(s);
                        if s.tx_state().in_transaction {
                            s.commit().await?;
                        }
                    }
                }
                Ok(n)
            }));
        }
        if editor.get() {
            cx.push(AppCommand::NewSqlEditor { connection: Some(self.conn.clone()), schema: self.objects.first().map(|o| o.schema_ref()), text: Some(script) });
            return false;
        }
        !(cancel.get() || (resp.close && !busy))
    }
}
