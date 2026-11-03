//! Foreign data wrapper configurator (PostgreSQL): generate the script that
//! makes the tables of another TernaryGrid connection available as foreign tables
//! (CREATE EXTENSION, CREATE SERVER, CREATE USER MAPPING, IMPORT FOREIGN
//! SCHEMA). The password is never written into the script: it is the
//! `:password` parameter, asked when the script runs.

use crate::command::AppCommand;
use crate::cx::AppContext;
use crate::sqllang::SqlLanguage;
use crate::tabs::TabView;
use crate::tabs::dbtools::look;
use egui::Ui;
use egui_phosphor::regular as icon;
use ternarygrid_core::config::ConnectionConfig;
use ternarygrid_core::{DbKind, SchemaRef, tr, trf};
use ternarygrid_ui::kit;

/// The source of the foreign tables.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FdwSpec {
    pub kind: Option<DbKind>,
    /// Name of the foreign server to create.
    pub server: String,
    pub host: String,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub user: Option<String>,
    /// Database file (SQLite).
    pub file: Option<String>,
    /// Schema (MySQL: database) to import.
    pub remote_schema: String,
    /// Schema the foreign tables are created in.
    pub local_schema: String,
    pub create_local_schema: bool,
    /// Import only these tables (`LIMIT TO`); empty = all.
    pub tables: Vec<String>,
}

/// The extension that reads from a database kind.
pub fn wrapper_of(kind: DbKind) -> Option<&'static str> {
    match kind {
        DbKind::Postgres | DbKind::CockroachDb => Some("postgres_fdw"),
        DbKind::MySql | DbKind::MariaDb => Some("mysql_fdw"),
        DbKind::Sqlite => Some("sqlite_fdw"),
        DbKind::SqlServer => Some("tds_fdw"),
        DbKind::ClickHouse => Some("clickhouse_fdw"),
        DbKind::Oracle => Some("oracle_fdw"),
        DbKind::Cassandra | DbKind::Redis | DbKind::MongoDb | DbKind::Elasticsearch | DbKind::Neo4j | DbKind::Generic => None,
    }
}

fn ident(s: &str) -> String {
    let plain = s.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_') && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain { s.to_string() } else { format!("\"{}\"", s.replace('"', "\"\"")) }
}

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A lowercase identifier made from a connection name.
pub fn identifier_from(name: &str) -> String {
    let mut out: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    match out.chars().next() {
        None => "remote".into(),
        Some(c) if c.is_ascii_digit() => format!("db_{out}"),
        _ => out,
    }
}

/// Defaults for a source connection.
pub fn spec_for(source: &ConnectionConfig) -> FdwSpec {
    let name = identifier_from(&source.name);
    let remote_schema = match source.kind {
        DbKind::MySql | DbKind::MariaDb => source.database.clone().unwrap_or_default(),
        DbKind::ClickHouse => source.database.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| "default".into()),
        DbKind::SqlServer => "dbo".into(),
        DbKind::Sqlite => "main".into(),
        // Oracle schemas are users, stored in upper case.
        DbKind::Oracle => source.user.clone().unwrap_or_default().to_uppercase(),
        _ => "public".into(),
    };
    FdwSpec {
        kind: Some(source.kind),
        server: format!("{name}_server"),
        host: source.host.clone(),
        port: source.port.or(source.kind.default_port()),
        database: source.database.clone().filter(|d| !d.is_empty()),
        user: source.user.clone().filter(|u| !u.is_empty()),
        file: source.file_path.clone(),
        remote_schema,
        local_schema: name,
        create_local_schema: true,
        tables: Vec::new(),
    }
}

/// The configuration script.
pub fn fdw_script(spec: &FdwSpec) -> Result<String, String> {
    let kind = spec.kind.ok_or_else(|| tr("Choose the source connection").to_string())?;
    let wrapper = wrapper_of(kind).ok_or_else(|| tr("No foreign data wrapper reads this database").to_string())?;
    if spec.server.trim().is_empty() || spec.local_schema.trim().is_empty() {
        return Err(tr("Enter the server name and the local schema").to_string());
    }
    if spec.remote_schema.trim().is_empty() {
        return Err(tr("Enter the remote schema").to_string());
    }
    let server = ident(spec.server.trim());
    let local = ident(spec.local_schema.trim());
    let port = spec.port.map(|p| p.to_string()).unwrap_or_default();
    let db = spec.database.as_deref().unwrap_or("");
    let file = spec.file.as_deref().unwrap_or("");
    let mut options: Vec<(&str, String)> = match kind {
        DbKind::Postgres | DbKind::CockroachDb => vec![("host", spec.host.clone()), ("port", port), ("dbname", db.to_string())],
        DbKind::MySql | DbKind::MariaDb => vec![("host", spec.host.clone()), ("port", port)],
        DbKind::SqlServer => vec![("servername", spec.host.clone()), ("port", port), ("database", db.to_string())],
        DbKind::ClickHouse => vec![("host", spec.host.clone()), ("port", port), ("dbname", db.to_string()), ("driver", "http".into())],
        DbKind::Sqlite => vec![("database", file.to_string())],
        DbKind::Oracle => vec![("dbserver", format!("//{}:{port}/{db}", spec.host))],
        DbKind::Cassandra | DbKind::Redis | DbKind::MongoDb | DbKind::Elasticsearch | DbKind::Neo4j | DbKind::Generic => Vec::new(),
    };
    options.retain(|(_, v)| !v.is_empty());
    let options = options.iter().map(|(k, v)| format!("{k} {}", lit(v))).collect::<Vec<_>>().join(", ");
    let mut out = format!("-- {}\n", trf("Foreign tables of {} through {}", &[&kind.display_name(), &wrapper]));
    if !matches!(kind, DbKind::Postgres | DbKind::CockroachDb) {
        out.push_str(&format!("-- {}\n", trf("The {} extension must be installed on the server", &[&wrapper])));
    }
    out.push_str(&format!("-- {}\n\n", tr("The database server connects to the source itself: SSH tunnels and proxies of the connection are not used")));
    out.push_str(&format!("CREATE EXTENSION IF NOT EXISTS {wrapper};\n\n"));
    out.push_str(&format!("CREATE SERVER IF NOT EXISTS {server}\n    FOREIGN DATA WRAPPER {wrapper}"));
    if !options.is_empty() {
        out.push_str(&format!("\n    OPTIONS ({options})"));
    }
    out.push_str(";\n\n");
    if kind != DbKind::Sqlite {
        let user_key = match kind {
            DbKind::MySql | DbKind::MariaDb | DbKind::SqlServer => "username",
            _ => "user",
        };
        let user = spec.user.as_deref().unwrap_or("");
        out.push_str(&format!("-- {}\n", tr(":password is asked when the script runs; enter it as a string, e.g. 'secret'")));
        out.push_str(&format!("CREATE USER MAPPING IF NOT EXISTS FOR CURRENT_USER\n    SERVER {server}\n    OPTIONS ({user_key} {}, password :password);\n\n", lit(user)));
    }
    if spec.create_local_schema {
        out.push_str(&format!("CREATE SCHEMA IF NOT EXISTS {local};\n\n"));
    }
    let tables: Vec<String> = spec.tables.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).map(ident).collect();
    out.push_str(&format!("IMPORT FOREIGN SCHEMA {}", ident(spec.remote_schema.trim())));
    if !tables.is_empty() {
        out.push_str(&format!("\n    LIMIT TO ({})", tables.join(", ")));
    }
    out.push_str(&format!("\n    FROM SERVER {server}\n    INTO {local};\n"));
    Ok(out)
}

/// The configurator tab of a PostgreSQL connection.
pub struct FdwConfigTab {
    conn: String,
    source: Option<String>,
    spec: FdwSpec,
    tables: String,
    /// Highlighting of the script (the connection's dialect).
    lang: Option<SqlLanguage>,
}

impl FdwConfigTab {
    pub fn new(conn: String) -> Self {
        FdwConfigTab { conn, source: None, spec: FdwSpec::default(), tables: String::new(), lang: None }
    }

    /// Choose the source connection (fills the defaults).
    pub fn set_source(&mut self, source: &ConnectionConfig) {
        self.source = Some(source.id.clone());
        self.spec = spec_for(source);
        self.tables.clear();
    }

    pub fn script(&self) -> Result<String, String> {
        fdw_script(&FdwSpec { tables: self.tables.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(), ..self.spec.clone() })
    }
}

impl TabView for FdwConfigTab {
    fn popup_size(&self) -> Option<egui::Vec2> {
        // The design's window inside its border.
        Some(egui::vec2(860.0, 628.0))
    }

    fn title(&self) -> String {
        tr("Foreign data wrapper").to_string()
    }

    fn icon(&self) -> &'static str {
        icon::PLUGS_CONNECTED
    }

    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }

    fn dedupe_key(&self) -> Option<String> {
        Some(format!("fdw:{}", self.conn))
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let conn = self.conn.clone();
        let p = ternarygrid_ui::pal(ui);
        if self.lang.is_none() {
            let dialect =
                cx.conns.live(&self.conn).map(|l| l.dialect.clone()).or_else(|| cx.conns.config(&self.conn).and_then(|c| cx.conns.registry.get(c.kind)).map(|d| d.dialect()));
            self.lang = dialect.map(SqlLanguage::new);
        }
        egui::Frame::new().inner_margin(look::BODY).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            if !cx.popup {
                look::title(ui, tr("Foreign data wrapper"));
            }
            look::muted(ui, tr("Make the tables of another connection available in this database as foreign tables."));
            let sources: Vec<ConnectionConfig> = cx.conns.configs().iter().filter(|c| c.id != self.conn && wrapper_of(c.kind).is_some()).cloned().collect();
            let current = self.source.as_ref().and_then(|id| sources.iter().find(|c| &c.id == id)).cloned();
            let lw = 150.0;
            // The design's select box is 30 high, as the fields (the logo centred on it).
            ui.spacing_mut().interact_size.y = ternarygrid_ui::style::CONTROL_H;
            look::row(ui, tr("Source connection"), lw, |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if let Some(c) = &current {
                    look::logo(ui, c.kind, 15.0);
                }
                egui::ComboBox::from_id_salt(("fdw_source", &self.conn))
                    .icon(ternarygrid_ui::kit::combo_caret)
                    .popup_style(egui::containers::menu::menu_style.into())
                    .selected_text(current.as_ref().map(|c| format!("{} ({})", c.name, c.kind.display_name())).unwrap_or_else(|| tr("(choose)").to_string()))
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        ternarygrid_ui::kit::menu_look(ui, 260.0);
                        for c in &sources {
                            if crate::tabs::transfer::wizard::choice(ui, self.source.as_deref() == Some(&c.id), &format!("{} ({})", c.name, c.kind.display_name())) {
                                self.set_source(c);
                            }
                        }
                    });
            });
            look::row(ui, tr("Server name"), lw, |ui| look::field(ui, &mut self.spec.server, "", 260.0, false));
            look::row(ui, tr("Remote schema"), lw, |ui| look::field(ui, &mut self.spec.remote_schema, "", 200.0, false));
            look::row(ui, tr("Local schema"), lw, |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                look::field(ui, &mut self.spec.local_schema, "", 200.0, false);
                look::checkbox(ui, &mut self.spec.create_local_schema, tr("Create it"));
            });
            look::row(ui, tr("Tables"), lw, |ui| look::field(ui, &mut self.tables, tr("comma separated; empty = all"), f32::INFINITY, false));
            ui.add_space(4.0);
            // The buttons' row as high as they are (26).
            ui.spacing_mut().interact_size.y = ternarygrid_ui::style::SMALL_H;
            match self.script() {
                Ok(script) => {
                    ui.horizontal(|ui| {
                        if look::small(ui, kit::Kind::Secondary, icon::FILE_SQL, tr("Open in SQL editor")).clicked() {
                            let schema = cx.conns.config(&self.conn).and_then(|c| c.database.clone()).map(|d| SchemaRef { database: Some(d), schema: None });
                            cx.push(AppCommand::NewSqlEditor { connection: Some(self.conn.clone()), schema, text: Some(script.clone()) });
                        }
                        if look::small(ui, kit::Kind::Ghost, icon::COPY, tr("Copy")).clicked() {
                            ui.ctx().copy_text(script.clone());
                        }
                    });
                    // 250 high as in the design (more when the window is made taller).
                    let h = (ui.available_height() - 18.0).max(250.0);
                    egui::Frame::new().fill(p.surface).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(8).show(ui, |ui| {
                        ui.allocate_ui(egui::vec2(ui.available_width(), h - 2.0), |ui| match &self.lang {
                            Some(lang) => {
                                let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
                                let options = look::code_options(13.0, 21.0);
                                ternarygrid_editor::CodeView::new(&script, lang, &theme).options(&options).id_salt(("fdw_script", &conn)).show(ui);
                            }
                            None => {
                                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| crate::tabs::code(ui, cx, &conn, ("fdw_script", &conn), &script));
                            }
                        });
                    });
                }
                Err(e) => {
                    look::muted(ui, &e);
                }
            }
        });
    }

    crate::impl_tab_any!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts() {
        let mut src = ConnectionConfig::new(DbKind::Postgres, "Sales DB (prod)");
        src.host = "db.internal".into();
        src.database = Some("sales".into());
        src.user = Some("reader".into());
        let spec = spec_for(&src);
        assert_eq!((spec.server.as_str(), spec.local_schema.as_str(), spec.remote_schema.as_str(), spec.port), ("sales_db_prod_server", "sales_db_prod", "public", Some(5432)));
        let s = fdw_script(&FdwSpec { tables: vec!["orders".into(), "Line Items".into()], ..spec }).unwrap();
        assert!(s.contains("CREATE EXTENSION IF NOT EXISTS postgres_fdw;"), "{s}");
        assert!(s.contains("OPTIONS (host 'db.internal', port '5432', dbname 'sales')"), "{s}");
        assert!(s.contains("OPTIONS (user 'reader', password :password);"), "{s}");
        assert!(s.contains("IMPORT FOREIGN SCHEMA public\n    LIMIT TO (orders, \"Line Items\")\n    FROM SERVER sales_db_prod_server\n    INTO sales_db_prod;"), "{s}");

        let mut my = ConnectionConfig::new(DbKind::MySql, "shop");
        my.host = "10.0.0.5".into();
        my.database = Some("Shop".into());
        let s = fdw_script(&spec_for(&my)).unwrap();
        assert!(
            s.contains("mysql_fdw") && s.contains("OPTIONS (host '10.0.0.5', port '3306')") && s.contains("OPTIONS (username '") && s.contains("IMPORT FOREIGN SCHEMA \"Shop\""),
            "{s}"
        );

        let mut lite = ConnectionConfig::new(DbKind::Sqlite, "1 local");
        lite.file_path = Some("/data/o'k.db".into());
        let s = fdw_script(&spec_for(&lite)).unwrap();
        assert!(s.contains("OPTIONS (database '/data/o''k.db')") && !s.contains("USER MAPPING") && s.contains("INTO db_1_local;"), "{s}");

        assert!(fdw_script(&FdwSpec::default()).is_err());
        assert_eq!(identifier_from("--"), "remote");
    }
}
