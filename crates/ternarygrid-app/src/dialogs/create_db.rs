//! New database / new schema: name and the options of the product, the
//! exact statement, then execution on a short-lived session in auto-commit
//! mode (PostgreSQL refuses `CREATE DATABASE` inside a transaction) and a
//! navigator refresh.

use super::{LABEL_W, add_field, opt_field, text_field};
use crate::backend::Job;
use crate::command::AppCommand;
use crate::cx::AppContext;
use egui_phosphor::regular as icon;
use std::cell::Cell;
use ternarygrid_core::driver::TransactionMode;
use ternarygrid_core::{DbKind, DbResult, NewDatabase, tr, trf};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::{self, Dialog};

pub struct CreateDatabaseDialog {
    conn: String,
    /// `Some(database)`: a schema in that database (`None` inside = the connection's).
    schema_in: Option<Option<String>>,
    db: NewDatabase,
    job: Option<Job<DbResult<()>>>,
    error: Option<String>,
    focus: bool,
    /// Turn on "Show all databases" so the new database appears in the
    /// navigator (offered while only the connection's database is shown).
    show_all: bool,
}

impl CreateDatabaseDialog {
    /// A new database on the server of `conn`.
    pub fn database(conn: String) -> Self {
        CreateDatabaseDialog { conn, schema_in: None, db: NewDatabase::default(), job: None, error: None, focus: true, show_all: true }
    }

    /// A new schema in `database` (`None` = the connection's database).
    pub fn schema(conn: String, database: Option<String>) -> Self {
        CreateDatabaseDialog { conn, schema_in: Some(database), db: NewDatabase::default(), job: None, error: None, focus: true, show_all: false }
    }

    /// The statement for the current input.
    pub fn sql(&self, dialect: &dyn ternarygrid_core::Dialect) -> Option<String> {
        match &self.schema_in {
            None => dialect.create_database_sql(&self.db),
            Some(_) => dialect.create_schema_sql(&self.db.name, self.db.owner.as_deref()),
        }
    }

    pub fn set_name(&mut self, name: &str) {
        self.db.name = name.to_string();
    }

    /// Draw; returns false when closed.
    pub fn show(&mut self, ctx: &egui::Context, cx: &mut AppContext) -> bool {
        let schema = self.schema_in.is_some();
        if let Some(j) = self.job.as_mut()
            && let Some(r) = j.poll()
        {
            self.job = None;
            match r {
                Ok(()) => {
                    let what = if schema { tr("Schema {} created") } else { tr("Database {} created") };
                    cx.toasts.success(trf(what, &[&self.db.name.trim()]));
                    if !schema && self.show_all && cx.conns.live(&self.conn).is_some_and(|l| !l.config.init.show_all_databases && l.driver.info().capabilities.multiple_databases) {
                        if let Some(mut cfg) = cx.conns.config(&self.conn).cloned() {
                            cfg.init.show_all_databases = true;
                            cx.conns.upsert(cfg);
                        }
                        if let Some(live) = cx.conns.live_mut(&self.conn) {
                            live.config.init.show_all_databases = true;
                        }
                    }
                    cx.push(AppCommand::RefreshMetadata { connection: self.conn.clone(), object: None, schema: None });
                    return false;
                }
                Err(e) => self.error = Some(e.full_text()),
            }
        }
        let Some(live) = cx.conns.live(&self.conn) else { return false };
        let (kind, read_only, conn_name) = (live.config.kind, live.config.read_only, live.config.name.clone());
        // Only the connection's database is listed: offer to list all of them.
        let offer_show_all = !schema && !live.config.init.show_all_databases && live.driver.info().capabilities.multiple_databases;
        let (driver, params, cfg) = (live.driver.clone(), live.params().clone(), live.config.clone());
        let sql = self.sql(live.dialect.as_ref());
        let dialect = live.dialect.clone();
        let busy = self.job.is_some();
        let enter = Cell::new(false);
        let (create, editor, cancel) = (Cell::new(false), Cell::new(false), Cell::new(false));
        let title = if schema { tr("New schema") } else { tr("New database") };
        let subtitle = trf("On {}", &[&conn_name]);
        let error = self.error.clone();
        let (resp, _) = Dialog::new(title).icon(icon::DATABASE).subtitle(&subtitle).width(560.0).show_modal(
            ctx,
            egui::Id::new(("create_database", &self.conn)),
            |ui| {
                super::form_spacing(ui, 12.0);
                let r = parts::form_row(ui, tr("Name"), LABEL_W, |ui| add_field(ui, text_field(&mut self.db.name, f32::INFINITY)));
                if std::mem::take(&mut self.focus) {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    enter.set(true);
                }
                let field = |ui: &mut egui::Ui, label: &str, v: &mut Option<String>, hint: &str| {
                    parts::form_row(ui, label, LABEL_W, |ui| opt_field(ui, v, hint, f32::INFINITY, false));
                };
                match (schema, kind) {
                    (true, DbKind::Postgres | DbKind::CockroachDb | DbKind::SqlServer) => field(ui, tr("Owner"), &mut self.db.owner, tr("(current user)")),
                    (false, DbKind::Postgres) => {
                        field(ui, tr("Owner"), &mut self.db.owner, tr("(current user)"));
                        field(ui, tr("Encoding"), &mut self.db.encoding, "UTF8");
                        field(ui, tr("Template"), &mut self.db.template, "template1");
                    }
                    (false, DbKind::MySql | DbKind::MariaDb) => {
                        field(ui, tr("Character set"), &mut self.db.charset, "utf8mb4");
                        field(ui, tr("Collation"), &mut self.db.collation, tr("(server default)"));
                    }
                    (false, DbKind::SqlServer) => field(ui, tr("Collation"), &mut self.db.collation, tr("(server default)")),
                    _ => {}
                }
                if offer_show_all {
                    parts::form_row(ui, "", LABEL_W, |ui| {
                        ternarygrid_ui::kit::checkbox(ui, &mut self.show_all, tr("Show all databases in the navigator"))
                            .on_hover_text(tr("The navigator lists only the connection's database; the new one appears when all databases are shown."))
                    });
                }
                match &sql {
                    // A bounded preview: a view that fills the available space
                    // would make the dialog grow every frame (and move its buttons).
                    Some(s) => super::sql_preview(ui, ("create_db_sql", &self.conn), Some(dialect.clone()), &format!("{s};"), 120.0),
                    None if self.db.name.trim().is_empty() => {
                        super::muted(ui, tr("Enter a name."));
                    }
                    None => {
                        super::muted(ui, tr("This database type cannot create it with SQL."));
                    }
                }
                if read_only {
                    crate::dialogs::error_note(ui, tr("The connection is read-only."));
                }
                if let Some(e) = &error {
                    crate::dialogs::error_note(ui, e);
                }
            },
            |ui| {
                if ui.add_enabled_ui(sql.is_some(), |ui| kit::button_ex(ui, Kind::Ghost, Some(icon::FILE_SQL), tr("Open in SQL editor"), false)).inner.clicked() {
                    editor.set(true);
                }
                super::right(ui, |ui| {
                    if ui.add_enabled_ui(sql.is_some() && !busy && !read_only, |ui| kit::primary_icon(ui, icon::PLUS, tr("Create"))).inner.clicked() {
                        create.set(true);
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
        let mut keep = !(cancel.get() || (resp.close && !busy));
        if editor.get()
            && let Some(s) = &sql
        {
            cx.push(AppCommand::NewSqlEditor { connection: Some(self.conn.clone()), schema: None, text: Some(format!("{s};\n")) });
            keep = false;
        }
        if (create.get() || enter.get())
            && !busy
            && !read_only
            && let Some(stmt) = sql
        {
            let mut params = params;
            if let Some(Some(db)) = &self.schema_in {
                params.database = Some(db.clone());
            }
            self.error = None;
            self.job = Some(cx.backend.spawn(async move {
                let mut s = driver.connect(&params).await?;
                let r = async {
                    crate::conn::init_session(s.as_mut(), &cfg, &params).await?;
                    // Outside any transaction: CREATE DATABASE cannot run in one.
                    s.set_transaction_mode(TransactionMode::Auto).await?;
                    ternarygrid_core::driver::execute_update(s.as_mut(), &stmt, &[]).await.map_err(|e| e.with_context(stmt.clone()))?;
                    Ok(())
                }
                .await;
                let _ = s.close().await;
                r
            }));
        }
        keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements() {
        let pg = ternarygrid_driver_postgres::PgDialect::new();
        let mut d = CreateDatabaseDialog::database("c".into());
        assert_eq!(d.sql(&pg), None, "no name");
        d.set_name("Sales 2026");
        d.db.owner = Some("app".into());
        d.db.encoding = Some("UTF8".into());
        d.db.template = Some("template0".into());
        assert_eq!(d.sql(&pg).unwrap(), "CREATE DATABASE \"Sales 2026\" OWNER app ENCODING 'UTF8' TEMPLATE template0");
        d.db.encoding = Some("UTF8'; DROP".into());
        assert!(!d.sql(&pg).unwrap().contains("DROP"), "odd option values are left out");
        let mut s = CreateDatabaseDialog::schema("c".into(), None);
        s.set_name("reporting");
        assert_eq!(s.sql(&pg).unwrap(), "CREATE SCHEMA reporting");
        let my = ternarygrid_driver_mysql::MySqlDialect::new(ternarygrid_driver_mysql::Flavor::MySql);
        let mut m = CreateDatabaseDialog::database("c".into());
        m.set_name("shop");
        m.db.charset = Some("utf8mb4".into());
        m.db.collation = Some("utf8mb4_unicode_ci".into());
        assert_eq!(m.sql(&my).unwrap(), "CREATE DATABASE shop CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci");
        let mut ms = CreateDatabaseDialog::schema("c".into(), None);
        ms.set_name("x");
        assert_eq!(ms.sql(&my), None, "MySQL schemas are databases");
        let lite = ternarygrid_driver_sqlite::SqliteDialect::new();
        assert_eq!(m.sql(&lite), None);
    }
}
