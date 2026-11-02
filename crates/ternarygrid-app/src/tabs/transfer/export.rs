//! Export wizard (FR-T1): tables (with optional filters) or a query, every
//! export format with its options, output folder and file name pattern,
//! parallel workers; runs with progress and can be saved as a task.

use super::wizard::{self, Clicked, Form, W, col, line};
use super::{
    MessageLog, Running, TaskSaver, char_edit, combo, connection_name, current_schema, default_output_folder, delimiter_choice, error_text, next_uid, not_connected, opt_char_edit,
    opt_number, opt_text, progress_ui, schema_combo, schema_tables, section,
};
use crate::cx::AppContext;
use crate::tabs::TabView;
use crate::transferexec::{AppSessionFactory, ExportTaskExtras, export_database, export_task_config};
use egui::{Margin, RichText, Ui, vec2};
use egui_phosphor::regular as icon;
use std::path::PathBuf;
use std::sync::Arc;
use ternarygrid_core::meta::{ObjectKind, ObjectRef, SchemaRef};
use ternarygrid_core::{ColumnMeta, DbError, DbKind, DbResult, Dialect, SharedRowStore, tr, trf};
use ternarygrid_tasks::TaskKind;
use ternarygrid_transfer::{
    BinaryEncoding, CancellationToken, CsvOptions, DbUnitOptions, EscapeStyle, ExportFormat, ExportItem, ExportItemSummary, ExportJob, ExportJobSummary, ExportOptions,
    ExportTarget, HeaderStyle, HtmlOptions, IdentifierQuoting, JsonLayout, JsonOptions, LineEnding, MarkdownOptions, NameContext, ParquetCompression, ParquetOptions, PhpOptions,
    Progress, QuoteMode, RowStoreSource, SessionFactory, SqlInsertMode, SqlOptions, TxtOptions, ValueFormat, XlsxOptions, XmlNullMode, XmlOptions, create_sink, expand_file_name,
    run_export_batched, run_export_job_with,
};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::{self, Tone};

const PAGE_SOURCE: usize = 0;
const PAGE_FORMAT: usize = 1;
const PAGE_OUTPUT: usize = 2;
const PAGE_RUN: usize = 3;

/// Character encodings offered for text output (WHATWG labels).
const ENCODINGS: [&str; 14] =
    ["UTF-8", "UTF-16LE", "UTF-16BE", "windows-1252", "ISO-8859-1", "ISO-8859-15", "windows-1250", "windows-1251", "KOI8-R", "Shift_JIS", "EUC-JP", "GBK", "Big5", "EUC-KR"];

#[derive(Clone)]
struct SourceItem {
    enabled: bool,
    item: ExportItem,
}

/// Rows already fetched into a result grid ([`ExportTab::from_result`]).
struct Fetched {
    title: String,
    /// The table (or partition) the rows come from, for the file name.
    table: Option<ObjectRef>,
    store: SharedRowStore,
    columns: Vec<ColumnMeta>,
    /// Selected rows (store indices).
    rows: Option<Vec<usize>>,
    /// Exported columns (indices into `columns`, in output order; empty = all).
    cols: Vec<usize>,
    sql: Option<String>,
}

/// What an export of a result grid writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultSource {
    /// Every row fetched so far.
    Fetched,
    /// The rows selected in the grid.
    Selected,
    /// The query run again for all rows.
    Query,
}

/// Store indices of result grid display rows, for [`ExportTab::from_result`].
/// Display rows differ from store rows while a client-side sort or filter
/// is active; pending inserted rows are not in the store and are left out.
pub fn grid_store_rows(model: &ternarygrid_grid::ResultModel, display_rows: &[usize]) -> Vec<usize> {
    display_rows
        .iter()
        .filter_map(|&r| match model.row_key(r) {
            Some(ternarygrid_grid::model::RowKey::Stored(s)) => Some(s as usize),
            _ => None,
        })
        .collect()
}

/// Database kind and current database of a connection, for file names
/// (SQLite: the database file name without its extension).
pub fn name_context(cx: &AppContext, conn: &str) -> NameContext {
    let Some(cfg) = cx.conns.config(conn) else { return NameContext::default() };
    let database = match cfg.kind {
        DbKind::Sqlite => cfg.file_path.as_deref().and_then(|p| std::path::Path::new(p).file_stem()).map(|s| s.to_string_lossy().into_owned()),
        _ => cx.conns.live(conn).and_then(|l| l.current_database.clone()).or_else(|| cfg.database.clone()),
    };
    NameContext { kind: Some(cfg.kind), database: database.filter(|d| !d.is_empty()) }
}

/// The export wizard tab.
pub struct ExportTab {
    /// Finished successfully: the pop-up closes.
    done: bool,
    uid: u64,
    conn: String,
    page: usize,
    items: Vec<SourceItem>,
    /// Query item whose SQL editor is open.
    expanded: Option<usize>,
    picker_schema: Option<SchemaRef>,
    picker_filter: String,
    picker_views: bool,
    /// Format, options and output settings (the items live in `items`).
    settings: ExportJob,
    /// Product the SQL format writes for; `None` = the source's.
    sql_dialect: Option<DbKind>,
    run: Option<Running<ExportJobSummary>>,
    log: MessageLog,
    result: Option<Result<ExportJobSummary, String>>,
    error: Option<String>,
    saver: TaskSaver,
    /// Rows of a result grid (export without database access).
    fetched: Option<Fetched>,
    result_source: ResultSource,
    /// The "Value formatting" and "File and reading" cards are open.
    values_open: bool,
    common_open: bool,
}

impl ExportTab {
    /// Export `objects` (tables / views) of `conn`, or the result of `sql`
    /// (named after the single object when one is given).
    pub fn new(conn: String, objects: Vec<ObjectRef>, sql: Option<String>) -> Self {
        let mut items = Vec::new();
        let mut picker_schema = None;
        match sql.filter(|s| !s.trim().is_empty()) {
            Some(sql) => {
                let name = match objects.as_slice() {
                    [one] => Some(one.name.clone()),
                    _ => None,
                };
                items.push(SourceItem { enabled: true, item: ExportItem::Query { sql, name } });
            }
            None => {
                for o in objects {
                    if o.kind == ObjectKind::Schema {
                        picker_schema = Some(SchemaRef::new(o.database.clone(), Some(o.name.clone())));
                    } else if o.kind.has_data() {
                        if picker_schema.is_none() {
                            picker_schema = Some(o.schema_ref()).filter(|s| s.schema.is_some());
                        }
                        items.push(SourceItem { enabled: true, item: ExportItem::Table { table: o, where_clause: None } });
                    }
                }
            }
        }
        let settings = ExportJob { output_folder: default_output_folder(), ..Default::default() };
        ExportTab {
            done: false,
            uid: next_uid(),
            conn,
            page: PAGE_SOURCE,
            items,
            expanded: None,
            picker_schema,
            picker_filter: String::new(),
            picker_views: true,
            settings,
            sql_dialect: None,
            run: None,
            log: MessageLog::default(),
            result: None,
            error: None,
            saver: TaskSaver::default(),
            fetched: None,
            result_source: ResultSource::Fetched,
            values_open: false,
            common_open: false,
        }
    }

    /// Export the result shown in a grid (FR-T2): the rows already fetched
    /// into `store` (`columns` describes its rows), or only the `rows`
    /// selected in the grid (store indices, see [`grid_store_rows`]), limited
    /// to the columns `cols` (indices into `columns`, in output order; empty
    /// = all). Both run without database access. With `sql` the query can
    /// also be run again for all rows (then the export can be saved as a task).
    #[allow(clippy::too_many_arguments)]
    pub fn from_result(
        conn: String,
        title: String,
        table: Option<ObjectRef>,
        store: SharedRowStore,
        columns: Vec<ColumnMeta>,
        rows: Option<Vec<usize>>,
        cols: Vec<usize>,
        sql: Option<String>,
    ) -> Self {
        let sql = sql.filter(|s| !s.trim().is_empty());
        let mut tab = Self::new(conn, Vec::new(), sql.clone());
        let name = Some(title.trim().to_string()).filter(|t| !t.is_empty());
        if let Some(SourceItem { item: ExportItem::Query { name: n, .. }, .. }) = tab.items.first_mut() {
            *n = name;
        }
        tab.result_source = if rows.is_some() { ResultSource::Selected } else { ResultSource::Fetched };
        tab.fetched = Some(Fetched { title, table, store, columns, rows, cols, sql });
        tab
    }

    /// What a result grid export writes (see [`ExportTab::from_result`]).
    pub fn set_result_source(&mut self, source: ResultSource) {
        self.result_source = source;
    }

    pub fn result_source(&self) -> ResultSource {
        self.result_source
    }

    // ------------------------------------------------------------ public API

    /// Format, options and output settings (everything but the objects).
    pub fn settings_mut(&mut self) -> &mut ExportJob {
        &mut self.settings
    }

    /// Apply the options of an `@export {…}` command (DBeaver's names, nested keys
    /// joined with `.`, the section prefix optional) to the settings they map to.
    /// Returns the keys that map to nothing here.
    pub fn apply_client_options(&mut self, opts: &std::collections::BTreeMap<String, String>) -> Vec<String> {
        let flag = |v: &str| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on");
        let mut ignored = Vec::new();
        for (key, value) in opts {
            let name = key.rsplit('.').next().unwrap_or(key).to_ascii_lowercase();
            // `type`, `format` and `extension` name the format at any level (`markdown.table`).
            let format = || {
                let v = value.trim().to_ascii_lowercase();
                let v = v.split('.').next().unwrap_or("").to_string();
                match v.as_str() {
                    "dbunit" => Some(ExportFormat::DbUnit),
                    "excel" => Some(ExportFormat::Xlsx),
                    "jsonlines" => Some(ExportFormat::JsonLines),
                    _ => ExportFormat::from_extension(&v),
                }
            };
            let s = &mut self.settings;
            match name.as_str() {
                "type" | "format" => match format() {
                    Some(f) => s.format = f,
                    None => ignored.push(key.clone()),
                },
                "outputfolder" | "folder" | "directory" => s.output_folder = PathBuf::from(value.trim()),
                "outputfilepattern" | "filepattern" | "filename" => s.file_name_pattern = value.trim().to_string(),
                "outputencoding" | "encoding" => s.options.encoding = value.trim().to_string(),
                "outputencodingbom" | "insertbom" | "bom" => s.options.insert_bom = flag(value),
                "delimiter" => s.options.csv.delimiter = value.replace("\\t", "\t"),
                "quotechar" => match value.chars().next() {
                    Some(c) => s.options.csv.quote_char = c,
                    None => ignored.push(key.clone()),
                },
                "header" => s.options.csv.header = !matches!(value.trim().to_ascii_lowercase().as_str(), "none" | "false" | "0" | "no" | "off"),
                "nullstring" => s.options.csv.null_string = value.clone(),
                "fetchsize" => match value.trim().parse::<usize>() {
                    Ok(n) if n > 0 => s.options.fetch_size = n,
                    _ => ignored.push(key.clone()),
                },
                // The format's own extension is used.
                "extension" => {}
                _ => ignored.push(key.clone()),
            }
        }
        ignored
    }

    /// Database product of SQL output (`None`: the source's).
    pub fn set_sql_dialect(&mut self, kind: Option<DbKind>) {
        self.sql_dialect = kind;
    }

    /// The job as it would run now.
    pub fn job(&self) -> ExportJob {
        let mut job = self.settings.clone();
        job.items = self
            .items
            .iter()
            .filter(|i| i.enabled)
            .map(|i| match &i.item {
                ExportItem::Query { sql, name } => {
                    ExportItem::Query { sql: sql.trim().trim_end_matches(';').trim_end().to_string(), name: name.as_ref().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) }
                }
                ExportItem::Table { table, where_clause } => {
                    ExportItem::Table { table: table.clone(), where_clause: where_clause.as_ref().map(|w| w.trim().to_string()).filter(|w| !w.is_empty()) }
                }
            })
            .collect();
        job
    }

    /// Add a table or view.
    pub fn add_table(&mut self, table: ObjectRef) {
        let exists = self.items.iter().any(|i| matches!(&i.item, ExportItem::Table { table: t, .. } if *t == table));
        if !exists {
            self.items.push(SourceItem { enabled: true, item: ExportItem::Table { table, where_clause: None } });
        }
    }

    /// Wizard page (0 source, 1 format, 2 output, 3 run).
    pub fn page(&self) -> usize {
        self.page
    }

    pub fn set_page(&mut self, page: usize) {
        self.page = page.min(PAGE_RUN);
    }

    /// Run the export (the "Run" button).
    pub fn start(&mut self, cx: &mut AppContext) {
        self.settings.source = name_context(cx, &self.conn);
        if self.run.is_some() {
            return;
        }
        if let Err(e) = self.validate() {
            self.error = Some(e);
            return;
        }
        if self.fetched_mode() {
            self.start_fetched(cx);
            return;
        }
        if cx.conns.live(&self.conn).is_none() {
            self.error = Some(not_connected(cx, &self.conn));
            return;
        }
        let job = self.job();
        let database = export_database(&job);
        let Some(live) = cx.conns.live(&self.conn) else { return };
        let factory: Arc<dyn SessionFactory> = Arc::new(AppSessionFactory::from_live(live, database.as_deref()));
        let sql_dialect = if job.format == ExportFormat::Sql { self.sql_dialect.map(|k| cx.conns.registry.dialect(k)) } else { None };
        self.error = None;
        self.result = None;
        self.log.clear();
        self.page = PAGE_RUN;
        let title = self.title();
        self.run = Some(Running::spawn(cx, title, move |progress, cancel| run_export_job_with(job, factory, sql_dialect, progress, cancel)));
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// Cancel a running export.
    pub fn cancel(&self) {
        if let Some(r) = &self.run {
            r.cancel();
        }
    }

    /// Summary of the last finished run.
    pub fn last_summary(&self) -> Option<&ExportJobSummary> {
        self.result.as_ref().and_then(|r| r.as_ref().ok())
    }

    /// Error of the last run (or of the last attempt to start one).
    pub fn last_error(&self) -> Option<&str> {
        match &self.result {
            Some(Err(e)) => Some(e),
            _ => self.error.as_deref(),
        }
    }

    /// Save the current configuration as a task (in the background).
    pub fn save_task(&mut self, cx: &mut AppContext, name: &str, folder: Option<String>) {
        match self.task_kind() {
            Ok(kind) => self.saver.save(cx, name.to_string(), folder, kind),
            Err(e) => self.saver.fail(e.full_text()),
        }
    }

    /// Id of the task saved last.
    pub fn saved_task_id(&self) -> Option<&str> {
        self.saver.saved.as_deref()
    }

    // --------------------------------------------------------------- helpers

    /// The rows of a result grid are exported (no database access).
    fn fetched_mode(&self) -> bool {
        self.fetched.is_some() && self.result_source != ResultSource::Query
    }

    /// Item naming the output file of a result grid export: the table the
    /// rows come from, else the result's title.
    fn fetched_item(&self) -> ExportItem {
        if let Some(table) = self.fetched.as_ref().and_then(|f| f.table.clone()) {
            return ExportItem::Table { table, where_clause: None };
        }
        let name = self.fetched.as_ref().map(|f| f.title.trim().to_string()).filter(|t| !t.is_empty());
        ExportItem::Query { sql: String::new(), name }
    }

    fn fetched_rows(&self) -> usize {
        let Some(f) = &self.fetched else { return 0 };
        match (self.result_source, &f.rows) {
            (ResultSource::Selected, Some(rows)) => rows.len(),
            _ => f.store.lock().len(),
        }
    }

    fn start_fetched(&mut self, cx: &mut AppContext) {
        let Some(f) = &self.fetched else { return };
        let mut source = RowStoreSource::new(f.store.clone(), f.columns.clone());
        if self.result_source == ResultSource::Selected
            && let Some(rows) = &f.rows
        {
            source = source.with_rows(rows.clone());
        }
        if !f.cols.is_empty() {
            source = source.with_columns(f.cols.clone());
        }
        let kind = cx.conns.config(&self.conn).map(|c| c.kind).unwrap_or(DbKind::Generic);
        let mut options = self.settings.options.clone();
        if options.sql.source_kind.is_none() {
            options.sql.source_kind = Some(kind);
        }
        let format = self.settings.format;
        let dialect = cx.conns.registry.dialect(self.sql_dialect.unwrap_or(kind));
        let item = self.fetched_item();
        let file = expand_file_name(&self.settings.file_name_pattern, &item, &self.settings.source, 1, format, chrono::Local::now().naive_local());
        let path = self.settings.output_folder.join(file);
        let name = item.name();
        self.error = None;
        self.result = None;
        self.log.clear();
        self.page = PAGE_RUN;
        let title = self.title();
        self.run = Some(Running::spawn(cx, title, move |progress, cancel| export_fetched(source, name, format, options, path, dialect, progress, cancel)));
    }

    fn task_kind(&self) -> DbResult<TaskKind> {
        if self.fetched_mode() {
            return Err(DbError::config(tr("Rows fetched into the grid cannot be saved as a task; choose to run the query again instead.")));
        }
        self.validate().map_err(DbError::config)?;
        let job = self.job();
        let extras = ExportTaskExtras { sql_dialect: if job.format == ExportFormat::Sql { self.sql_dialect } else { None } };
        Ok(TaskKind::Export { connection_id: self.conn.clone(), config: export_task_config(&job, &extras)? })
    }

    fn items_error(&self) -> Option<String> {
        if self.fetched_mode() {
            return None;
        }
        let enabled: Vec<&SourceItem> = self.items.iter().filter(|i| i.enabled).collect();
        if enabled.is_empty() {
            return Some(tr("Choose at least one table, view or query to export.").to_string());
        }
        if enabled.iter().any(|i| matches!(&i.item, ExportItem::Query { sql, .. } if sql.trim().is_empty())) {
            return Some(tr("A query to export is empty.").to_string());
        }
        None
    }

    fn output_error(&self) -> Option<String> {
        if self.settings.output_folder.as_os_str().is_empty() {
            return Some(tr("Choose the output folder.").to_string());
        }
        if self.settings.format == ExportFormat::Csv && self.settings.options.csv.delimiter.is_empty() {
            return Some(tr("The CSV delimiter must not be empty.").to_string());
        }
        None
    }

    fn validate(&self) -> Result<(), String> {
        match self.items_error().or_else(|| self.output_error()) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn reachable(&self) -> usize {
        if self.items_error().is_some() {
            PAGE_SOURCE
        } else if self.output_error().is_some() {
            PAGE_OUTPUT
        } else {
            PAGE_RUN
        }
    }

    fn subject(&self) -> String {
        if let Some(f) = &self.fetched {
            return f.title.clone();
        }
        let enabled: Vec<&SourceItem> = self.items.iter().filter(|i| i.enabled).collect();
        match enabled.as_slice() {
            [] => String::new(),
            [one] => one.item.name(),
            many => trf("{} objects", &[&many.len()]),
        }
    }

    fn poll(&mut self, cx: &mut AppContext) {
        let mut finished = None;
        if let Some(run) = &mut self.run {
            self.log.drain(&run.progress);
            if let Some(result) = run.poll(cx, describe) {
                self.log.drain(&run.progress);
                finished = Some(result);
            }
        }
        if let Some(result) = finished {
            self.run = None;
            match result {
                Ok(summary) => {
                    if summary.failed() > 0 {
                        cx.toasts.warning(trf("Export finished, {} object(s) failed", &[&summary.failed()]), summary.first_error().map(str::to_string));
                    } else {
                        // One file: its name and "Show in Finder" (the pop-up closes).
                        let files: Vec<&std::path::PathBuf> = summary.items.iter().filter_map(|i| i.file.as_ref()).collect();
                        match files.as_slice() {
                            [one] => cx.toasts.success_file(describe(&summary), (*one).clone()),
                            _ => cx.toasts.success(describe(&summary)),
                        }
                        self.done = true;
                    }
                    self.result = Some(Ok(summary));
                }
                Err(e) => {
                    if e.kind != ternarygrid_core::ErrorKind::Cancelled {
                        cx.toasts.error(tr("Export failed"), Some(e.full_text()));
                    }
                    self.result = Some(Err(error_text(&e)));
                }
            }
        }
        self.saver.poll(cx);
    }

    // ------------------------------------------------------------------ pages

    fn result_source_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let Some(f) = &self.fetched else { return };
        section(ui, tr("Rows to export"));
        let fetched = f.store.lock().len();
        let selected = f.rows.as_ref().map(Vec::len);
        let has_sql = f.sql.is_some();
        let columns = if f.cols.is_empty() { f.columns.len() } else { f.cols.len() };
        let sql = f.sql.clone().unwrap_or_default();
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            wizard::radio_value(ui, &mut self.result_source, ResultSource::Fetched, &trf("Fetched rows ({})", &[&fetched]));
            if let Some(n) = selected {
                wizard::radio_value(ui, &mut self.result_source, ResultSource::Selected, &trf("Selected rows ({})", &[&n]));
            }
            if has_sql {
                wizard::radio_value(ui, &mut self.result_source, ResultSource::Query, tr("Run the query again (all rows)"));
            }
        });
        match self.result_source {
            ResultSource::Query => {
                wizard::faint(ui, tr("The query runs on a new connection and every row is exported, with all its columns."));
                if let Some(SourceItem { item: ExportItem::Query { name, .. }, .. }) = self.items.first_mut() {
                    wizard::form(ui, ("exp_rows_name", self.uid), 1, 64.0, |f| {
                        f.row(tr("Name"), |ui| opt_text(ui, name, tr("name used for ${table}"), f32::INFINITY));
                    });
                }
                // The query, highlighted in the connection's dialect (read-only, selectable).
                use ternarygrid_editor::EditorLanguage;
                let tokens = cx.conns.live(&self.conn).map(|l| crate::sqllang::SqlLanguage::new(l.dialect.clone()).highlight(&sql)).unwrap_or_default();
                let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
                let job = crate::sqllang::highlight_job(&sql, &tokens, &theme, egui::FontId::monospace(12.5));
                wizard::boxed(ui, Margin::symmetric(12, 10), |ui| {
                    ui.add(egui::Label::new(job).wrap().selectable(true));
                });
            }
            _ => {
                wizard::faint(ui, &trf("{} column(s); the rows are written from memory, without running the query again.", &[&columns]));
            }
        }
    }

    fn source_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if self.fetched.is_some() {
            self.result_source_page(ui, cx);
            return;
        }
        let p = ternarygrid_ui::pal(ui);
        section(ui, tr("Objects to export"));
        if self.items.is_empty() {
            wizard::faint(ui, tr("Nothing selected yet: add tables or views below, or a query."));
        } else {
            let mut remove = None;
            let mut toggle = None;
            let mut set_all = None;
            let states: Vec<bool> = self.items.iter().map(|i| i.enabled).collect();
            let expanded = self.expanded;
            let cols = [col("", W::Px(32.0)), col(tr("Object"), W::Px(240.0)), col(tr("Filter"), W::Fr(1.0)), col("", W::Px(44.0))];
            let items = &mut self.items;
            wizard::table(
                ui,
                ("exp_items", self.uid),
                &cols,
                38.0,
                None,
                items.len(),
                |ui, c| {
                    if c == 0
                        && let Some(on) = wizard::check_all(ui, &states)
                    {
                        set_all = Some(on);
                    }
                },
                |ui, i, c| {
                    let it = &mut items[i];
                    match c {
                        0 => {
                            wizard::check(ui, &mut it.enabled, "");
                        }
                        1 => {
                            let (glyph, name) = match &it.item {
                                ExportItem::Table { table, .. } => (if table.kind == ObjectKind::View { icon::EYE } else { icon::TABLE }, table.display_path()),
                                ExportItem::Query { .. } => (icon::FILE_SQL, it.item.name()),
                            };
                            ui.label(RichText::new(glyph).font(ternarygrid_ui::style::icon_font(14.0)).color(p.text_muted));
                            ui.add(egui::Label::new(name).truncate());
                        }
                        2 => match &mut it.item {
                            ExportItem::Table { where_clause, .. } => {
                                wizard::prefixed_opt_text(ui, where_clause, "WHERE", tr("optional filter, e.g. id > 100"), f32::INFINITY);
                            }
                            ExportItem::Query { name: qname, .. } => {
                                opt_text(ui, qname, tr("name used for ${table}"), 200.0);
                                let label = if expanded == Some(i) { tr("Hide SQL") } else { tr("Edit SQL") };
                                if wizard::ghost(ui, None, label).clicked() {
                                    toggle = Some(i);
                                }
                            }
                        },
                        _ => {
                            if wizard::icon_button(ui, icon::TRASH, tr("Remove"), 24.0).clicked() {
                                remove = Some(i);
                            }
                        }
                    }
                },
            );
            if let Some(on) = set_all {
                self.items.iter_mut().for_each(|i| i.enabled = on);
            }
            if let Some(i) = toggle {
                self.expanded = if self.expanded == Some(i) { None } else { Some(i) };
            }
            if let Some(i) = remove {
                self.items.remove(i);
                self.expanded = None;
            }
        }
        if let Some(i) = self.expanded
            && let Some(SourceItem { item: ExportItem::Query { sql, .. }, .. }) = self.items.get_mut(i)
        {
            ui.add(
                egui::TextEdit::multiline(sql)
                    .code_editor()
                    .desired_rows(6)
                    .desired_width(f32::INFINITY)
                    .margin(Margin::symmetric(10, 8))
                    .hint_text(tr("SELECT … (one statement returning rows)")),
            );
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if wizard::ghost(ui, Some(icon::PLUS), tr("Add query")).clicked() {
                self.items.push(SourceItem { enabled: true, item: ExportItem::Query { sql: String::new(), name: None } });
                self.expanded = Some(self.items.len() - 1);
            }
            if !self.items.is_empty() {
                if wizard::ghost(ui, None, tr("Select all")).clicked() {
                    self.items.iter_mut().for_each(|i| i.enabled = true);
                }
                if wizard::ghost(ui, None, tr("Select none")).clicked() {
                    self.items.iter_mut().for_each(|i| i.enabled = false);
                }
            }
        });

        section(ui, tr("Add tables and views"));
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
        let list = schema.as_ref().map(|s| schema_tables(cx, &conn, s, self.picker_views));
        let filter = self.picker_filter.to_lowercase();
        let shown: Option<Vec<ObjectRef>> = match &list {
            Some(Some(Ok(list))) => {
                Some(list.iter().filter(|o| !o.is_system && (filter.is_empty() || o.obj.name.to_lowercase().contains(&filter))).map(|o| o.obj.clone()).collect())
            }
            _ => None,
        };
        ui.horizontal(|ui| {
            wizard::muted(ui, tr("Schema"));
            schema_combo(ui, cx, &conn, ("exp_schema", self.uid), &mut self.picker_schema);
            kit::search_field(ui, egui::Id::new(("exp_filter", self.uid)), &mut self.picker_filter, tr("Filter…"), 220.0);
            wizard::check(ui, &mut self.picker_views, tr("Views"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(shown) = &shown {
                    if !shown.is_empty() && wizard::ghost(ui, None, tr("Add all shown")).clicked() {
                        for o in shown {
                            self.add_table(o.clone());
                        }
                    }
                    wizard::faint(ui, &trf("{} objects", &[&shown.len()]));
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
                let items = &self.items;
                picker_list(ui, ("exp_pick", self.uid), &shown, |o| items.iter().any(|i| matches!(&i.item, ExportItem::Table { table, .. } if table == o)), |o| add = Some(o));
                if let Some(o) = add {
                    self.add_table(o);
                }
            }
        }
    }

    fn format_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        section(ui, tr("Format"));
        let formats: Vec<(ExportFormat, &str)> = ExportFormat::ALL.iter().map(|f| (*f, tr(f.label()))).collect();
        parts::chips(ui, &mut self.settings.format, &formats);
        parts::hline(ui);
        let format = self.settings.format;
        let id = self.uid;
        let o = &mut self.settings.options;
        match format {
            ExportFormat::Csv | ExportFormat::Tsv => csv_ui(ui, id, &mut o.csv, format == ExportFormat::Tsv),
            ExportFormat::Json | ExportFormat::JsonLines => json_ui(ui, id, &mut o.json, format == ExportFormat::Json),
            ExportFormat::Sql => {
                let source = cx.conns.config(&self.conn).map(|c| c.kind);
                let kinds = cx.conns.registry.kinds();
                sql_ui(ui, id, &mut o.sql, &mut self.sql_dialect, source, &kinds);
            }
            ExportFormat::Xlsx => xlsx_ui(ui, id, &mut o.xlsx),
            ExportFormat::Markdown => markdown_ui(ui, id, &mut o.markdown),
            ExportFormat::Html => html_ui(ui, id, &mut o.html),
            ExportFormat::Xml => xml_ui(ui, id, &mut o.xml),
            ExportFormat::Txt => txt_ui(ui, id, &mut o.txt),
            ExportFormat::DbUnit => dbunit_ui(ui, id, &mut o.dbunit),
            ExportFormat::Php => php_ui(ui, id, &mut o.php),
            ExportFormat::Parquet => parquet_ui(ui, id, &mut o.parquet),
        }
        // The value formatting and the file options fold away under two cards.
        let values = uses_value_format(format);
        let reading = if format.is_binary() { trf("{} rows per batch", &[&o.fetch_size]) } else { format!("{} · {}", o.encoding, trf("{} rows per batch", &[&o.fetch_size])) };
        let (mut values_open, mut common_open) = (self.values_open && values, self.common_open);
        let fill = values_open || common_open;
        let mut cards = |ui: &mut Ui| {
            if values {
                wizard::fold_card(ui, egui::Id::new(("exp_values", id)), &mut values_open, tr("Value formatting"), Some(tr("dates, numbers, NULL, booleans")), fill, |ui| {
                    values_ui(ui, id, &mut o.values, format)
                });
            }
            wizard::fold_card(ui, egui::Id::new(("exp_common", id)), &mut common_open, tr("File and reading"), Some(&reading), fill, |ui| common_ui(ui, id, o, format));
        };
        ui.add_space(4.0);
        if fill {
            cards(ui);
        } else {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                cards(ui)
            });
        }
        if values {
            self.values_open = values_open;
        }
        self.common_open = common_open;
    }

    fn output_page(&mut self, ui: &mut Ui) {
        section(ui, tr("Output"));
        let id = self.uid;
        let open_new = self.settings.open_new_connections;
        wizard::form(ui, ("exp_out", id), 1, 130.0, |f| {
            f.row(tr("Folder"), |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::secondary(ui, Some(icon::FOLDER_OPEN), tr("Browse…")).clicked() {
                        let mut dialog = rfd::FileDialog::new().set_title(tr("Output folder"));
                        if self.settings.output_folder.is_dir() {
                            dialog = dialog.set_directory(&self.settings.output_folder);
                        }
                        if let Some(p) = dialog.pick_folder() {
                            self.settings.output_folder = p;
                        }
                    }
                    let mut folder = self.settings.output_folder.display().to_string();
                    if ui.add(line(&mut folder).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)).changed() {
                        self.settings.output_folder = folder.into();
                    }
                });
            });
            f.row(tr("File name"), |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.add(line(&mut self.settings.file_name_pattern).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
                    wizard::muted(ui, tr("Variables: ${object} ${database} ${schema} ${table} ${date} ${time} ${timestamp} ${index} ${extension}"));
                });
            });
            f.row(tr("Workers"), |ui| {
                ui.add_enabled(open_new, egui::DragValue::new(&mut self.settings.threads).range(1..=16));
                wizard::faint(ui, tr("objects exported in parallel"));
            });
        });
        wizard::check(ui, &mut self.settings.open_new_connections, tr("Open a separate connection for every worker"));
        wizard::check(ui, &mut self.settings.continue_on_error, tr("Continue with the other objects when one fails"));
        wizard::check(ui, &mut self.settings.count_rows, tr("Count the rows first (exact progress, one extra query per object)"));

        section(ui, tr("Files"));
        let p = ternarygrid_ui::pal(ui);
        let now = chrono::Local::now().naive_local();
        let files: Vec<String> = if self.fetched_mode() {
            let name = expand_file_name(&self.settings.file_name_pattern, &self.fetched_item(), &self.settings.source, 1, self.settings.format, now);
            vec![self.settings.output_folder.join(name).display().to_string()]
        } else {
            let job = self.job();
            job.items
                .iter()
                .enumerate()
                .take(50)
                .map(|(i, item)| job.output_folder.join(expand_file_name(&job.file_name_pattern, item, &job.source, i + 1, job.format, now)).display().to_string())
                .collect()
        };
        let more = if self.fetched_mode() { 0 } else { self.items.iter().filter(|i| i.enabled).count().saturating_sub(50) };
        wizard::card(ui, 12, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            if files.is_empty() {
                wizard::faint(ui, tr("No object selected."));
            }
            for f in &files {
                wizard::mono(ui, &home_relative(f), p.text, 12.0).on_hover_text(f);
            }
            if more > 0 {
                wizard::faint(ui, &trf("… and {} more", &[&more]));
            }
        });
    }

    fn run_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if let Some(run) = &self.run {
            wizard::title(ui, tr("Exporting…"));
            if progress_ui(ui, &run.progress, run.is_cancelling()) {
                run.cancel();
            }
            self.log.ui(ui, ("exp_log", self.uid), 260.0);
            return;
        }
        match &self.result {
            Some(Ok(summary)) => {
                let summary = summary.clone();
                self.result_ui(ui, &summary);
            }
            Some(Err(e)) => {
                wizard::outcome(ui, Tone::Error, tr("The export failed"), None);
                crate::widgets::error_box(ui, e);
            }
            None => self.summary_ui(ui, cx),
        }
        if self.result.is_some() {
            self.log.ui(ui, ("exp_log", self.uid), 220.0);
        }
    }

    fn summary_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        section(ui, tr("Summary"));
        let job = self.job();
        let objects = if self.fetched_mode() {
            trf("{} rows of {}", &[&self.fetched_rows(), &self.subject()])
        } else {
            format!("{} ({})", job.items.len(), job.items.iter().map(ExportItem::name).collect::<Vec<_>>().join(", "))
        };
        let workers = if job.open_new_connections { job.threads.to_string() } else { "1".to_string() };
        wizard::facts(
            ui,
            ("exp_sum", self.uid),
            120.0,
            &[
                (tr("Connection"), connection_name(cx, &self.conn), None),
                (tr("Objects"), objects, None),
                (tr("Format"), tr(job.format.label()).to_string(), None),
                (tr("Folder"), job.output_folder.display().to_string(), None),
                (tr("Workers"), workers, None),
            ],
        );
        if let Some(e) = self.items_error().or_else(|| self.output_error()) {
            crate::widgets::error_box(ui, &e);
        }
    }

    fn result_ui(&mut self, ui: &mut Ui, summary: &ExportJobSummary) {
        let tone = if summary.failed() > 0 { Tone::Warning } else { Tone::Success };
        wizard::outcome(ui, tone, &describe(summary), None);
        let cols = [
            col(tr("Object"), W::Fr(1.1)),
            col(tr("File"), W::Fr(2.0)),
            col(tr("Rows"), W::Fr(0.8)).right(),
            col(tr("Size"), W::Fr(0.7)).right(),
            col(tr("Time"), W::Fr(0.6)).right(),
            col(tr("Status"), W::Fr(0.9)),
        ];
        let mut open_error = None;
        wizard::table(
            ui,
            ("exp_res", self.uid),
            &cols,
            30.0,
            Some(320.0),
            summary.items.len(),
            |_, _| {},
            |ui, i, c| {
                let item = &summary.items[i];
                match c {
                    0 => {
                        ui.add(egui::Label::new(&item.name).truncate());
                    }
                    1 => match &item.file {
                        Some(f) => {
                            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let link = egui::Label::new(RichText::new(name).underline().color(ternarygrid_ui::pal(ui).accent_text)).truncate().sense(egui::Sense::click());
                            let r = ui.add(link).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(f.display().to_string());
                            if r.clicked()
                                && let Err(e) = open::that_detached(f)
                            {
                                open_error = Some(e.to_string());
                            }
                        }
                        None => {
                            wizard::faint(ui, "—");
                        }
                    },
                    2 => {
                        ui.label(item.rows.to_string());
                    }
                    3 => {
                        ui.label(crate::widgets::format_bytes(item.bytes.min(i64::MAX as u64) as i64));
                    }
                    4 => {
                        ui.label(crate::widgets::format_duration(std::time::Duration::from_millis(item.duration_ms)));
                    }
                    _ => match &item.error {
                        Some(e) => {
                            wizard::tone_text(ui, Tone::Error, e).on_hover_text(e);
                        }
                        None if !item.warnings.is_empty() => {
                            wizard::tone_text(ui, Tone::Warning, &trf("{} warning(s)", &[&item.warnings.len()])).on_hover_text(item.warnings.join("\n"));
                        }
                        None => {
                            wizard::tone_text(ui, Tone::Success, tr("OK"));
                        }
                    },
                }
            },
        );
        if open_error.is_some() {
            self.error = open_error;
        }
        ui.horizontal(|ui| {
            if kit::secondary(ui, Some(icon::FOLDER_OPEN), tr("Open folder")).clicked()
                && let Err(e) = open::that_detached(&self.settings.output_folder)
            {
                self.error = Some(e.to_string());
            }
            if kit::button_ex(ui, Kind::Ghost, Some(icon::PENCIL_SIMPLE), tr("Change settings"), false).clicked() {
                self.result = None;
                self.page = PAGE_SOURCE;
            }
        });
        if let Some(e) = &self.error {
            crate::widgets::error_box(ui, e);
        }
    }

    fn footer(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let running = self.run.is_some();
        let reachable = self.reachable();
        let ok = self.validate().is_ok() && !running;
        let fetched = self.fetched_mode();
        let savable = ok && !self.saver.is_saving() && !fetched;
        let footer = wizard::Footer {
            back: (self.page > PAGE_SOURCE).then_some(!running),
            next: (self.page < PAGE_RUN).then_some(self.page < reachable),
            run: (tr("Run export"), ok),
            save: Some((savable, fetched.then_some(tr("Exports of fetched rows cannot be saved as tasks.")))),
            error: self.error.as_deref(),
        };
        match wizard::footer(ui, &footer) {
            Some(Clicked::Back) => self.page -= 1,
            Some(Clicked::Next) => self.page += 1,
            Some(Clicked::Run) => self.start(cx),
            Some(Clicked::Save) => self.saver.open(trf("Export {}", &[&self.subject()])),
            None => {}
        }
    }
}

/// A path with the home folder shown as `~` (file lists only; fields keep the real path).
fn home_relative(path: &str) -> String {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    match path.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() && (rest.starts_with('/') || rest.starts_with('\\')) => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// The list of tables and views to add: a + button, the icon and the name (faint once added).
pub(crate) fn picker_list(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, shown: &[ObjectRef], added: impl Fn(&ObjectRef) -> bool, mut add: impl FnMut(ObjectRef)) {
    let p = ternarygrid_ui::pal(ui);
    wizard::boxed(ui, Margin::ZERO, |ui| {
        egui::ScrollArea::vertical().id_salt(egui::Id::new(id)).max_height(240.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, o) in shown.iter().enumerate() {
                let already = added(o);
                let r = ui.horizontal(|ui| {
                    ui.set_min_height(28.0);
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.add_space(6.0);
                    if ui.add_enabled_ui(!already, |ui| wizard::icon_button(ui, icon::PLUS, tr("Add"), 22.0)).inner.clicked() {
                        add(o.clone());
                    }
                    let glyph = if o.kind == ObjectKind::View { icon::EYE } else { icon::TABLE };
                    ui.label(RichText::new(glyph).font(ternarygrid_ui::style::icon_font(13.0)).color(p.text_muted));
                    ui.label(RichText::new(&o.name).color(if already { p.text_faint } else { p.text }));
                    ui.allocate_space(vec2(ui.available_width(), 1.0));
                });
                if i + 1 < shown.len() {
                    let rect = r.response.rect;
                    ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, p.border));
                }
            }
        });
    });
}

/// Export rows already fetched into a grid (no database access).
#[allow(clippy::too_many_arguments)]
async fn export_fetched(
    source: RowStoreSource,
    name: String,
    format: ExportFormat,
    options: ExportOptions,
    path: PathBuf,
    dialect: Arc<dyn Dialect>,
    progress: Arc<Progress>,
    cancel: CancellationToken,
) -> DbResult<ExportJobSummary> {
    let started = std::time::Instant::now();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|e| DbError::io(trf("Cannot create the folder {}: {}", &[&parent.display(), &e])))?;
    }
    let mut source = source;
    progress.set_items_total(1);
    progress.item_started(&name);
    let result = async {
        let mut sink = create_sink(format, &options, ExportTarget::path(&path), Some(dialect))?;
        run_export_batched(&mut source, sink.as_mut(), options.fetch_size, &progress, &cancel).await
    }
    .await;
    progress.item_finished(&name);
    let s = result?;
    for w in &s.warnings {
        progress.warn(Some(&name), w.clone());
    }
    progress.info(Some(&name), trf("{} rows exported to {}", &[&s.rows, &path.display()]));
    let item = ExportItemSummary { name, file: Some(path), rows: s.rows, bytes: s.bytes, warnings: s.warnings, error: None, duration_ms: s.duration_ms };
    Ok(ExportJobSummary { rows: item.rows, bytes: item.bytes, items: vec![item], duration_ms: started.elapsed().as_millis() as u64 })
}

fn describe(s: &ExportJobSummary) -> String {
    let files = s.items.iter().filter(|i| i.error.is_none() && i.file.is_some()).count();
    match s.failed() {
        0 => trf("{} rows exported to {} file(s)", &[&s.rows, &files]),
        n => trf("{} rows exported to {} file(s), {} failed", &[&s.rows, &files, &n]),
    }
}

fn uses_value_format(f: ExportFormat) -> bool {
    !matches!(f, ExportFormat::Sql | ExportFormat::Php | ExportFormat::Parquet | ExportFormat::Xlsx)
}

// ------------------------------------------------------------ option editors

/// Width of the label column of the format options.
const LABEL_W: f32 = 170.0;

fn opts<R>(ui: &mut Ui, id: u64, name: &str, add: impl FnOnce(&mut Form<'_>) -> R) -> R {
    wizard::form(ui, ("exp_opts", id, name.to_string()), 1, LABEL_W, add)
}

fn csv_ui(ui: &mut Ui, id: u64, o: &mut CsvOptions, tsv: bool) {
    opts(ui, id, "csv", |f| {
        if !tsv {
            f.row(tr("Delimiter"), |ui| {
                ui.add(line(&mut o.delimiter).font(egui::TextStyle::Monospace).desired_width(60.0));
                delimiter_choice(ui, &mut o.delimiter);
            });
        }
        f.row(tr("Quote character"), |ui| char_edit(ui, &mut o.quote_char));
        f.row(tr("Quote values"), |ui| {
            parts::segmented(
                ui,
                &mut o.quote_mode,
                &[(QuoteMode::Needed, tr("When needed")), (QuoteMode::Strings, tr("Text values")), (QuoteMode::Always, tr("Always")), (QuoteMode::Never, tr("Never"))],
            )
        });
        f.row(tr("Quotes inside values"), |ui| {
            parts::segmented(ui, &mut o.escape, &[(EscapeStyle::Doubled, tr("Doubled (\"\")")), (EscapeStyle::Backslash, tr("Backslash (\\\")"))])
        });
        f.row(tr("Header row"), |ui| {
            wizard::check(ui, &mut o.header, tr("Write"));
            ui.add_enabled_ui(o.header, |ui| {
                combo(
                    ui,
                    ("csv_header", id),
                    &mut o.header_style,
                    &[
                        (HeaderStyle::Label, tr("Column names").to_string()),
                        (HeaderStyle::Type, tr("Column types").to_string()),
                        (HeaderStyle::Both, tr("Names and types").to_string()),
                    ],
                );
            });
        });
        f.row(tr("NULL written as"), |ui| ui.add(line(&mut o.null_string).hint_text(tr("nothing")).desired_width(160.0)));
        f.row(tr("Line ending"), |ui| {
            parts::segmented(ui, &mut o.line_ending, &[(LineEnding::Lf, tr("LF (macOS, Linux)")), (LineEnding::CrLf, tr("CRLF (Windows)")), (LineEnding::Cr, tr("CR"))])
        });
    });
}

fn binary_options() -> Vec<(BinaryEncoding, String)> {
    vec![(BinaryEncoding::Hex, tr("Hexadecimal").to_string()), (BinaryEncoding::Base64, tr("Base64").to_string()), (BinaryEncoding::Text, tr("As text (UTF-8)").to_string())]
}

fn json_ui(ui: &mut Ui, id: u64, o: &mut JsonOptions, document: bool) {
    opts(ui, id, "json", |f| {
        if document {
            f.row(tr("Structure"), |ui| {
                combo(
                    ui,
                    ("json_layout", id),
                    &mut o.layout,
                    &[(JsonLayout::ArrayOfObjects, tr("Array of objects").to_string()), (JsonLayout::ColumnsAndRows, tr("Columns and rows").to_string())],
                )
            });
            f.row(tr("Indent"), |ui| wizard::check(ui, &mut o.pretty, tr("Pretty print")));
        }
        f.row(tr("Table name"), |ui| wizard::check(ui, &mut o.include_table_name, tr("Include the table name")));
        f.row(tr("Binary values"), |ui| combo(ui, ("json_bin", id), &mut o.binary, &binary_options()));
        f.row(tr("Dates"), |ui| wizard::check(ui, &mut o.format_dates, tr("Use the date formats of the value formatting")));
    });
}

fn sql_ui(ui: &mut Ui, id: u64, o: &mut SqlOptions, dialect: &mut Option<DbKind>, source: Option<DbKind>, kinds: &[DbKind]) {
    opts(ui, id, "sql", |f| {
        let mut options: Vec<(Option<DbKind>, String)> = vec![(
            None,
            match source {
                Some(k) => trf("Same as the source ({})", &[&k.display_name()]),
                None => tr("Same as the source").to_string(),
            },
        )];
        for k in kinds {
            options.push((Some(*k), k.display_name().to_string()));
        }
        options.push((Some(DbKind::Generic), tr("Standard SQL").to_string()));
        f.row(tr("Target database"), |ui| combo(ui, ("sql_dialect", id), dialect, &options));
        f.row(tr("Statements"), |ui| {
            combo(ui, ("sql_mode", id), &mut o.mode, &[(SqlInsertMode::Insert, tr("INSERT").to_string()), (SqlInsertMode::Upsert, tr("INSERT or UPDATE (upsert)").to_string())])
        });
        if o.mode == SqlInsertMode::Upsert {
            f.row(tr("Key columns"), |ui| {
                let mut keys = o.key_columns.join(", ");
                if ui.add(line(&mut keys).hint_text(tr("primary key")).desired_width(240.0)).changed() {
                    o.key_columns = keys.split(',').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
                }
            });
        }
        f.row(tr("Rows per INSERT"), |ui| ui.add(egui::DragValue::new(&mut o.rows_per_statement).range(1..=1000)));
        f.row(tr("Target table"), |ui| opt_text(ui, &mut o.table_name, tr("name of the source table"), 260.0));
        f.row(tr("Target schema"), |ui| opt_text(ui, &mut o.schema_name, tr("schema of the source table"), 260.0));
        f.row(tr("Qualify names"), |ui| wizard::check(ui, &mut o.qualify_with_schema, tr("Prefix the table with its schema")));
        f.row(tr("CREATE TABLE"), |ui| wizard::check(ui, &mut o.include_create_table, tr("Write the table definition before the data")));
        f.row(tr("Quote identifiers"), |ui| {
            combo(ui, ("sql_quote", id), &mut o.quoting, &[(IdentifierQuoting::AsNeeded, tr("When needed").to_string()), (IdentifierQuoting::Always, tr("Always").to_string())])
        });
        f.row(tr("Statement terminator"), |ui| ui.add(line(&mut o.terminator).font(egui::TextStyle::Monospace).desired_width(60.0)));
    });
}

fn xlsx_ui(ui: &mut Ui, id: u64, o: &mut XlsxOptions) {
    opts(ui, id, "xlsx", |f| {
        f.row(tr("Sheet name"), |ui| opt_text(ui, &mut o.sheet_name, tr("name of the table"), 260.0));
        f.row(tr("Header row"), |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            wizard::check(ui, &mut o.header, tr("Write"));
            ui.add_enabled_ui(o.header, |ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                wizard::check(ui, &mut o.bold_header, tr("Bold"));
                wizard::check(ui, &mut o.freeze_header, tr("Freeze"));
                wizard::check(ui, &mut o.autofilter, tr("Filter buttons"));
            });
        });
        f.row(tr("Column width"), |ui| {
            wizard::check(ui, &mut o.auto_column_width, tr("Fit to the content, at most"));
            ui.add_enabled(o.auto_column_width, egui::DragValue::new(&mut o.max_column_width).range(5..=255));
        });
        f.row(tr("Date format"), |ui| ui.add(line(&mut o.date_format).font(egui::TextStyle::Monospace).desired_width(200.0)));
        f.row(tr("Time format"), |ui| ui.add(line(&mut o.time_format).font(egui::TextStyle::Monospace).desired_width(200.0)));
        f.row(tr("Timestamp format"), |ui| ui.add(line(&mut o.datetime_format).font(egui::TextStyle::Monospace).desired_width(200.0)));
        f.row(tr("Number format"), |ui| opt_text(ui, &mut o.number_format, tr("General"), 200.0));
        f.row(tr("Integer format"), |ui| opt_text(ui, &mut o.integer_format, tr("General"), 200.0));
        f.row(tr("Rows per sheet"), |ui| ui.add(egui::DragValue::new(&mut o.max_rows_per_sheet).range(1..=1_048_575)));
    });
}

fn markdown_ui(ui: &mut Ui, id: u64, o: &mut MarkdownOptions) {
    opts(ui, id, "md", |f| {
        f.row(tr("Numbers"), |ui| wizard::check(ui, &mut o.right_align_numbers, tr("Align right")));
        f.row(tr("Pipes"), |ui| wizard::check(ui, &mut o.escape_pipes, tr("Escape | inside values")));
        f.row(tr("Line breaks become"), |ui| ui.add(line(&mut o.line_break).font(egui::TextStyle::Monospace).desired_width(100.0)));
    });
}

fn html_ui(ui: &mut Ui, id: u64, o: &mut HtmlOptions) {
    opts(ui, id, "html", |f| {
        f.row(tr("Title"), |ui| opt_text(ui, &mut o.title, tr("name of the table"), 260.0));
        f.row(tr("Style"), |ui| wizard::check(ui, &mut o.embed_css, tr("Embed a style sheet")));
        f.row(tr("Header row"), |ui| wizard::check(ui, &mut o.header, tr("Write")));
    });
}

fn xml_ui(ui: &mut Ui, id: u64, o: &mut XmlOptions) {
    opts(ui, id, "xml", |f| {
        f.row(tr("Root element"), |ui| ui.add(line(&mut o.root_element).font(egui::TextStyle::Monospace).desired_width(200.0)));
        f.row(tr("Row element"), |ui| ui.add(line(&mut o.record_element).font(egui::TextStyle::Monospace).desired_width(200.0)));
        f.row(tr("NULL values"), |ui| {
            combo(
                ui,
                ("xml_null", id),
                &mut o.null_mode,
                &[
                    (XmlNullMode::NullAttribute, tr("Element with null=\"true\"").to_string()),
                    (XmlNullMode::Omit, tr("Leave the element out").to_string()),
                    (XmlNullMode::Empty, tr("Empty element").to_string()),
                    (XmlNullMode::Text, tr("NULL text of the value formatting").to_string()),
                ],
            )
        });
        f.row(tr("Document type"), |ui| wizard::check(ui, &mut o.include_dtd, tr("Include a DTD")));
        f.row(tr("Indent"), |ui| wizard::check(ui, &mut o.indent, tr("Pretty print")));
    });
}

fn txt_ui(ui: &mut Ui, id: u64, o: &mut TxtOptions) {
    opts(ui, id, "txt", |f| {
        f.row(tr("Borders"), |ui| wizard::check(ui, &mut o.borders, tr("Draw borders")));
        f.row(tr("Header row"), |ui| wizard::check(ui, &mut o.header, tr("Write")));
        f.row(tr("Column width"), |ui| {
            wizard::muted(ui, tr("from"));
            ui.add(egui::DragValue::new(&mut o.min_column_width).range(1..=200));
            wizard::muted(ui, tr("to"));
            ui.add(egui::DragValue::new(&mut o.max_column_width).range(1..=1000));
        });
        f.row(tr("NULL values"), |ui| {
            wizard::check(ui, &mut o.show_nulls, tr("Show as"));
            ui.add_enabled(o.show_nulls, line(&mut o.null_text).desired_width(100.0));
        });
        f.row(tr("Numbers"), |ui| wizard::check(ui, &mut o.right_align_numbers, tr("Align right")));
        f.row(tr("Rows measured for widths"), |ui| ui.add(egui::DragValue::new(&mut o.width_sample_rows).range(1..=100_000)));
    });
}

fn dbunit_ui(ui: &mut Ui, id: u64, o: &mut DbUnitOptions) {
    opts(ui, id, "dbunit", |f| {
        f.row(tr("Table element"), |ui| opt_text(ui, &mut o.table_name, tr("name of the table"), 260.0));
        f.row(tr("Upper case"), |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            wizard::check(ui, &mut o.upper_case_table_name, tr("Table name"));
            wizard::check(ui, &mut o.upper_case_column_names, tr("Column names"));
        });
        f.row(tr("NULL values"), |ui| opt_text(ui, &mut o.null_value, tr("attribute left out"), 200.0));
    });
}

fn php_ui(ui: &mut Ui, id: u64, o: &mut PhpOptions) {
    opts(ui, id, "php", |f| {
        f.row(tr("Variable name"), |ui| opt_text(ui, &mut o.variable_name, tr("name of the table"), 260.0));
        f.row(tr("Arrays"), |ui| wizard::check(ui, &mut o.short_array_syntax, tr("Short syntax [ ]")));
        f.row(tr("Opening tag"), |ui| wizard::check(ui, &mut o.open_tag, tr("Start with <?php")));
    });
}

fn parquet_ui(ui: &mut Ui, id: u64, o: &mut ParquetOptions) {
    opts(ui, id, "parquet", |f| {
        f.row(tr("Compression"), |ui| {
            combo(ui, ("pq_comp", id), &mut o.compression, &[(ParquetCompression::Snappy, tr("Snappy").to_string()), (ParquetCompression::None, tr("None").to_string())])
        });
        f.row(tr("Rows per row group"), |ui| ui.add(egui::DragValue::new(&mut o.row_group_rows).range(1..=10_000_000)));
        f.row(tr("Row group size (MB)"), |ui| {
            let mut mb = (o.row_group_bytes / (1024 * 1024)).max(1);
            if ui.add(egui::DragValue::new(&mut mb).range(1..=4096)).changed() {
                o.row_group_bytes = mb * 1024 * 1024;
            }
        });
    });
}

fn values_ui(ui: &mut Ui, id: u64, v: &mut ValueFormat, format: ExportFormat) {
    if matches!(format, ExportFormat::Json | ExportFormat::JsonLines) {
        wizard::faint(ui, tr("JSON uses the date formats only with \"Use the date formats of the value formatting\"."));
    }
    opts(ui, id, "values", |f| {
        f.row(tr("NULL text"), |ui| ui.add(wizard::mono_line(&mut v.null_text, 120.0).hint_text(tr("empty"))));
        f.row(tr("Date format"), |ui| opt_text(ui, &mut v.date_format, tr("ISO, e.g. %d.%m.%Y"), 220.0));
        f.row(tr("Time format"), |ui| opt_text(ui, &mut v.time_format, tr("ISO, e.g. %H:%M"), 220.0));
        f.row(tr("Timestamp format"), |ui| opt_text(ui, &mut v.datetime_format, tr("ISO, e.g. %d.%m.%Y %H:%M:%S"), 220.0));
        f.row(tr("Timestamp with time zone"), |ui| opt_text(ui, &mut v.timestamptz_format, tr("ISO, e.g. %Y-%m-%d %H:%M %z"), 220.0));
        f.row(tr("Decimal separator"), |ui| opt_char_edit(ui, &mut v.decimal_separator, "."));
        f.row(tr("Thousands separator"), |ui| opt_char_edit(ui, &mut v.thousands_separator, ""));
        f.row(tr("Fraction digits"), |ui| opt_number(ui, &mut v.float_decimals, tr("all")));
        f.row(tr("Binary values"), |ui| combo(ui, ("val_bin", id), &mut v.binary, &binary_options()));
        f.row(tr("Booleans"), |ui| {
            ui.add(wizard::mono_line(&mut v.true_text, 80.0));
            ui.add(wizard::mono_line(&mut v.false_text, 80.0));
        });
        f.row(tr("Text"), |ui| {
            wizard::check(ui, &mut v.trim, tr("Trim spaces"));
            ui.add_space(8.0);
            wizard::muted(ui, tr("at most"));
            opt_number(ui, &mut v.max_length, tr("any length"));
            wizard::muted(ui, tr("characters"));
        });
    });
    wizard::faint(ui, tr("Date patterns use chrono syntax: %Y year, %m month, %d day, %H hour, %M minute, %S second."));
}

fn common_ui(ui: &mut Ui, id: u64, o: &mut ExportOptions, format: ExportFormat) {
    opts(ui, id, "common", |f| {
        if !format.is_binary() {
            f.row(tr("Encoding"), |ui| {
                egui::ComboBox::from_id_salt(("exp_enc", id)).icon(ternarygrid_ui::kit::combo_caret).selected_text(o.encoding.clone()).width(160.0).show_ui(ui, |ui| {
                    for e in ENCODINGS {
                        if ui.selectable_label(o.encoding.eq_ignore_ascii_case(e), e).clicked() {
                            o.encoding = e.to_string();
                        }
                    }
                });
                let bom = o.encoding.to_ascii_uppercase().starts_with("UTF");
                ui.add_enabled_ui(bom, |ui| wizard::check(ui, &mut o.insert_bom, tr("Byte order mark (BOM)")));
            });
        }
        f.row(tr("Rows fetched per batch"), |ui| ui.add(egui::DragValue::new(&mut o.fetch_size).range(1..=100_000)));
        f.row(tr("After a failure"), |ui| wizard::check(ui, &mut o.keep_partial_file, tr("Keep the partially written file")));
    });
}

impl TabView for ExportTab {
    fn take_done(&mut self) -> bool {
        std::mem::take(&mut self.done)
    }

    fn popup_size(&self) -> Option<egui::Vec2> {
        Some(egui::vec2(940.0, 660.0))
    }

    fn title(&self) -> String {
        let subject = self.subject();
        if subject.is_empty() { tr("Export data").to_string() } else { trf("Export {}", &[&subject]) }
    }

    fn icon(&self) -> &'static str {
        icon::EXPORT
    }

    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        self.poll(cx);
        self.settings.source = name_context(cx, &self.conn);
        let uid = self.uid;
        wizard::top_panel(ui, egui::Id::new(("exp_top", uid)), |ui| {
            // In a pop-up its title bar names the wizard and the connection.
            if !cx.popup {
                ui.horizontal(|ui| {
                    crate::widgets::icon_heading(ui, icon::EXPORT, tr("Export data"));
                    ui.weak(connection_name(cx, &self.conn));
                });
            }
            let steps = [tr("Source"), tr("Format"), tr("Output"), tr("Run")];
            let reachable = if self.run.is_some() { PAGE_RUN } else { self.reachable().max(PAGE_FORMAT.min(self.reachable())) };
            let finished = self.page == PAGE_RUN && matches!(self.result, Some(Ok(_)));
            if let Some(p) = wizard::steps(ui, &steps, self.page, reachable.max(self.page), finished)
                && self.run.is_none()
            {
                self.page = p;
            }
        });
        wizard::bottom_panel(ui, egui::Id::new(("exp_bottom", uid)), |ui| self.footer(ui, cx));
        wizard::body(ui, egui::Id::new(("exp_body", uid)), |ui| match self.page {
            PAGE_SOURCE => self.source_page(ui, cx),
            PAGE_FORMAT => self.format_page(ui, cx),
            PAGE_OUTPUT => self.output_page(ui),
            _ => self.run_page(ui, cx),
        });
        if let Some((name, folder)) = self.saver.ui(ui.ctx(), egui::Id::new(("exp_save", uid))) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_command_options_reach_the_settings() {
        let ternarygrid_sql::ClientCommand::Export { options } = ternarygrid_sql::parse_client_command(
            r#"@export {"type": "json", "consumer": {"outputFolder": "/tmp/out", "outputFilePattern": "${table}_x", "outputEncoding": "UTF-16LE", "outputEncodingBOM": true}, "processor": {"delimiter": ";", "header": "none", "extension": "json"}, "producer": {"selectedRowsOnly": false}}"#,
        )
        .unwrap()
        .unwrap() else {
            panic!("not @export")
        };
        let mut tab = ExportTab::new("c".into(), Vec::new(), Some("select 1".into()));
        let ignored = tab.apply_client_options(&options);
        let job = tab.job();
        assert_eq!(job.format, ExportFormat::Json);
        assert_eq!(job.output_folder, PathBuf::from("/tmp/out"));
        assert_eq!(job.file_name_pattern, "${table}_x");
        assert_eq!(job.options.encoding, "UTF-16LE");
        assert!(job.options.insert_bom);
        assert_eq!(job.options.csv.delimiter, ";");
        assert!(!job.options.csv.header);
        assert_eq!(ignored, vec!["producer.selectedRowsOnly".to_string()]);
        let mut tab = ExportTab::new("c".into(), Vec::new(), Some("select 1".into()));
        let opts = std::collections::BTreeMap::from([("type".to_string(), "markdown.table".to_string()), ("delimiter".to_string(), "\\t".to_string())]);
        assert!(tab.apply_client_options(&opts).is_empty());
        assert_eq!((tab.job().format, tab.job().options.csv.delimiter.as_str()), (ExportFormat::Markdown, "\t"));
    }

    #[test]
    fn constructor_builds_items() {
        let t = ObjectRef::table(Some("public".into()), "users");
        let tab = ExportTab::new("c".into(), vec![t.clone()], None);
        let job = tab.job();
        assert_eq!(job.items, vec![ExportItem::Table { table: t.clone(), where_clause: None }]);
        let q = ExportTab::new("c".into(), vec![t], Some("select * from users;".into()));
        assert_eq!(q.job().items, vec![ExportItem::Query { sql: "select * from users".into(), name: Some("users".into()) }]);
        assert!(q.validate().is_ok() || q.settings.output_folder.as_os_str().is_empty());
        let empty = ExportTab::new("c".into(), Vec::new(), None);
        assert!(empty.validate().is_err());
        assert_eq!(empty.reachable(), PAGE_SOURCE);
    }
}
