//! Import wizard (FR-T2): CSV / TSV / XLSX / XML / JSON files into an
//! existing or a new table, with a live preview, type guessing, column
//! mapping, conversion options and a report of the rows that failed.

use super::wizard::{self, Clicked, Form, LABEL_W, W, col, fact, line};
use super::{
    COMMON_KINDS, MessageLog, ProgressKind, Running, TaskSaver, char_edit, close_wizard, combo, combo_w, connection_name, current_schema, delimiter_choice, error_text, kind_label,
    lines_edit, next_uid, not_connected, on_error_options, opt_char_edit, opt_number, opt_text, outcome_tone, progress_ui, reload_table_def, schema_combo, schema_tables, section,
    table_def_of, transaction_options,
};
use crate::backend::Job;
use crate::command::AppCommand;
use crate::cx::AppContext;
use crate::tabs::TabView;
use crate::transferexec::{AppSessionFactory, database_of, import_task_config};
use egui::{RichText, Ui, vec2};
use egui_phosphor::regular as icon;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ternarygrid_core::meta::{ObjectRef, SchemaRef};
use ternarygrid_core::{ColumnMeta, DbError, DbResult, Row, tr, trf, trn};
use ternarygrid_tasks::TaskKind;
use ternarygrid_transfer::{
    BinaryEncoding, ColumnMapping, CsvHeader, CsvReadOptions, EscapeStyle, ImportJob, ImportMapping, ImportMode, ImportSourceConfig, ImportSummary, InferOptions, JsonInputFormat,
    JsonReadOptions, OnError, PeekableSource, RowSource, SessionFactory, SheetSelector, XlsxReadOptions, XmlReadOptions, infer_columns, run_import_job, xlsx_sheet_names,
};
use ternarygrid_ui::kit;
use ternarygrid_ui::parts::{self, Tone};

const PAGE_FILE: usize = 0;
const PAGE_TARGET: usize = 1;
const PAGE_OPTIONS: usize = 2;
const PAGE_RUN: usize = 3;

/// Rows shown in the preview table.
const PREVIEW_ROWS: usize = 50;

/// Encodings offered for input files (`auto` detects BOM / UTF-8 / windows-1252).
const ENCODINGS: [&str; 12] = ["auto", "UTF-8", "UTF-16LE", "UTF-16BE", "windows-1252", "ISO-8859-1", "ISO-8859-15", "windows-1250", "windows-1251", "Shift_JIS", "GBK", "Big5"];

/// Format of the file to import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFileFormat {
    Csv,
    Tsv,
    Xlsx,
    Xml,
    Json,
}

impl ImportFileFormat {
    pub const ALL: [ImportFileFormat; 5] = [ImportFileFormat::Csv, ImportFileFormat::Tsv, ImportFileFormat::Xlsx, ImportFileFormat::Xml, ImportFileFormat::Json];

    /// Format for a file name extension.
    pub fn from_path(path: &Path) -> Option<ImportFileFormat> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "csv" | "txt" => ImportFileFormat::Csv,
            "tsv" | "tab" => ImportFileFormat::Tsv,
            "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => ImportFileFormat::Xlsx,
            "xml" => ImportFileFormat::Xml,
            "json" | "jsonl" | "ndjson" => ImportFileFormat::Json,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            ImportFileFormat::Csv => tr("CSV"),
            ImportFileFormat::Tsv => tr("TSV"),
            ImportFileFormat::Xlsx => tr("Excel (XLSX)"),
            ImportFileFormat::Xml => tr("XML"),
            ImportFileFormat::Json => tr("JSON / JSON Lines"),
        }
    }
}

/// Existing or new target table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportTarget {
    Existing,
    New,
}

/// First rows of the file read in the background.
struct Preview {
    config: ImportSourceConfig,
    columns: Vec<ColumnMeta>,
    rows: Vec<Row>,
    warnings: Vec<String>,
}

/// What the current column mapping was built for.
#[derive(Clone, PartialEq)]
enum MappingKey {
    New { columns: Vec<String>, infer: InferOptions },
    Existing { table: ObjectRef, columns: Vec<String> },
}

/// The import wizard tab.
pub struct ImportTab {
    /// Finished successfully: the pop-up closes.
    done: bool,
    uid: u64,
    conn: String,
    page: usize,
    // ---------------------------------------------------------------- file
    path: String,
    format: ImportFileFormat,
    csv: CsvReadOptions,
    xlsx: XlsxReadOptions,
    xml: XmlReadOptions,
    json: JsonReadOptions,
    /// Rows read for the preview and the type guess.
    sample_rows: usize,
    sheets_job: Option<(String, Job<DbResult<Vec<String>>>)>,
    sheets: Option<(String, Vec<String>)>,
    preview_job: Option<Job<Result<Preview, (ImportSourceConfig, String)>>>,
    preview: Option<Preview>,
    preview_error: Option<(ImportSourceConfig, String)>,
    // -------------------------------------------------------------- target
    target: ImportTarget,
    schema: Option<SchemaRef>,
    table: Option<ObjectRef>,
    new_name: String,
    columns: Vec<ColumnMapping>,
    key_columns: Vec<String>,
    mapping_for: Option<MappingKey>,
    columns_edited: bool,
    mapping_error: Option<String>,
    // ------------------------------------------------------------- options
    /// Mode, batches, transactions, errors and conversion (target and
    /// columns are taken from the fields above).
    options: ImportMapping,
    infer: InferOptions,
    // ----------------------------------------------------------------- run
    run: Option<Running<ImportSummary>>,
    log: MessageLog,
    result: Option<Result<ImportSummary, String>>,
    error: Option<String>,
    report_job: Option<Job<std::io::Result<PathBuf>>>,
    saver: TaskSaver,
    /// The new table a run created: running again imports into it instead of creating it.
    created: Option<ObjectRef>,
}

impl ImportTab {
    /// Import into `table` (an existing table), or into a new table of
    /// `schema` when no table is given.
    pub fn new(conn: String, table: Option<ObjectRef>, schema: Option<SchemaRef>) -> Self {
        let table = table.filter(|t| t.kind.is_table_like());
        let schema = schema.or_else(|| table.as_ref().map(ObjectRef::schema_ref)).filter(|s| s.schema.is_some() || s.database.is_some());
        ImportTab {
            done: false,
            uid: next_uid(),
            conn,
            page: PAGE_FILE,
            path: String::new(),
            format: ImportFileFormat::Csv,
            csv: CsvReadOptions::default(),
            xlsx: XlsxReadOptions::default(),
            xml: XmlReadOptions::default(),
            json: JsonReadOptions::default(),
            sample_rows: 500,
            sheets_job: None,
            sheets: None,
            preview_job: None,
            preview: None,
            preview_error: None,
            target: if table.is_some() { ImportTarget::Existing } else { ImportTarget::New },
            schema,
            table,
            new_name: String::new(),
            columns: Vec::new(),
            key_columns: Vec::new(),
            mapping_for: None,
            columns_edited: false,
            mapping_error: None,
            options: ImportMapping::default(),
            infer: InferOptions::default(),
            run: None,
            log: MessageLog::default(),
            result: None,
            error: None,
            report_job: None,
            saver: TaskSaver::default(),
            created: None,
        }
    }

    // ------------------------------------------------------------ public API

    /// Choose the file; the format follows the extension.
    pub fn set_file(&mut self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        self.path = path.display().to_string();
        if let Some(f) = ImportFileFormat::from_path(path) {
            self.format = f;
        }
        if self.new_name.trim().is_empty()
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            self.new_name = table_name_from(stem);
        }
    }

    pub fn set_format(&mut self, format: ImportFileFormat) {
        self.format = format;
    }

    /// CSV / TSV reader options.
    pub fn csv_options_mut(&mut self) -> &mut CsvReadOptions {
        &mut self.csv
    }

    /// The preview of the current file and reader options is loaded.
    pub fn preview_ready(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| p.config == self.source_config())
    }

    /// Error of the last preview attempt.
    pub fn preview_error(&self) -> Option<&str> {
        self.preview_error.as_ref().map(|(_, e)| e.as_str())
    }

    /// Column names of the preview.
    pub fn preview_columns(&self) -> Vec<String> {
        self.preview.as_ref().map(|p| p.columns.iter().map(|c| c.name.clone()).collect()).unwrap_or_default()
    }

    /// Rows read by the preview (at most the sample size).
    pub fn preview_row_count(&self) -> usize {
        self.preview.as_ref().map(|p| p.rows.len()).unwrap_or(0)
    }

    /// Import into a new table `name` of `schema` (`None`: the default schema).
    pub fn set_new_table(&mut self, schema: Option<SchemaRef>, name: &str) {
        self.target = ImportTarget::New;
        self.schema = schema;
        self.new_name = name.to_string();
    }

    /// Import into an existing table.
    pub fn set_existing_table(&mut self, table: ObjectRef) {
        self.target = ImportTarget::Existing;
        self.schema = Some(table.schema_ref());
        self.table = Some(table);
    }

    /// The column mapping is built for the current preview and target.
    pub fn mapping_ready(&self) -> bool {
        self.preview_ready() && self.mapping_for == self.current_key() && !self.columns.is_empty()
    }

    /// Column mappings (editable).
    pub fn columns_mut(&mut self) -> &mut Vec<ColumnMapping> {
        self.columns_edited = true;
        &mut self.columns
    }

    /// Mode, batch size, transactions, error handling and conversion options.
    pub fn options_mut(&mut self) -> &mut ImportMapping {
        &mut self.options
    }

    /// The complete mapping as it would run now.
    pub fn mapping(&self) -> Option<ImportMapping> {
        let target = self.target_ref()?;
        let mut m = self.options.clone();
        m.create_table = self.target == ImportTarget::New && self.created.as_ref() != Some(&target);
        m.target = target;
        m.columns = self.columns.clone();
        m.key_columns = self.key_columns.clone();
        Some(m)
    }

    /// The job as it would run now.
    pub fn job(&self) -> Option<ImportJob> {
        Some(ImportJob { source: self.source_config(), mapping: self.mapping()?, infer: self.infer_options(), sample_rows: self.sample_rows })
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn set_page(&mut self, page: usize) {
        self.page = page.min(PAGE_RUN);
    }

    /// Run the import (the "Run" button).
    pub fn start(&mut self, cx: &mut AppContext) {
        if self.run.is_some() {
            return;
        }
        if let Err(e) = self.validate() {
            self.error = Some(e);
            return;
        }
        let Some(config) = cx.conns.config(&self.conn) else { return };
        if config.read_only {
            self.error = Some(trf("The connection \"{}\" is read-only.", &[&config.name]));
            return;
        }
        if cx.conns.live(&self.conn).is_none() {
            self.error = Some(not_connected(cx, &self.conn));
            return;
        }
        let Some(job) = self.job() else { return };
        let database = database_of([&job.mapping.target]);
        let Some(live) = cx.conns.live(&self.conn) else { return };
        let factory: Arc<dyn SessionFactory> = Arc::new(AppSessionFactory::from_live(live, database.as_deref()));
        self.error = None;
        self.result = None;
        self.log.clear();
        self.page = PAGE_RUN;
        let title = trf("Import into {}", &[&job.mapping.target.display_path()]);
        self.run = Some(Running::spawn(cx, title, move |progress, cancel| run_import_job(job, factory, progress, cancel)));
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    pub fn cancel(&self) {
        if let Some(r) = &self.run {
            r.cancel();
        }
    }

    /// Summary of the last finished run.
    pub fn last_summary(&self) -> Option<&ImportSummary> {
        self.result.as_ref().and_then(|r| r.as_ref().ok())
    }

    /// Error of the last run (or of the last attempt to start one).
    pub fn last_error(&self) -> Option<&str> {
        match &self.result {
            Some(Err(e)) => Some(e),
            _ => self.error.as_deref().or(self.mapping_error.as_deref()),
        }
    }

    /// Save the current configuration as a task (in the background).
    pub fn save_task(&mut self, cx: &mut AppContext, name: &str, folder: Option<String>) {
        match self.task_kind() {
            Ok(kind) => self.saver.save(cx, name.to_string(), folder, kind),
            Err(e) => self.saver.fail(e.full_text()),
        }
    }

    pub fn saved_task_id(&self) -> Option<&str> {
        self.saver.saved.as_deref()
    }

    // --------------------------------------------------------------- helpers

    fn task_kind(&self) -> DbResult<TaskKind> {
        self.validate().map_err(DbError::config)?;
        let job = self.job().ok_or_else(|| DbError::config(tr("Choose the target table.")))?;
        Ok(TaskKind::Import { connection_id: self.conn.clone(), config: import_task_config(&job)? })
    }

    fn source_config(&self) -> ImportSourceConfig {
        let path = PathBuf::from(self.path.trim());
        match self.format {
            ImportFileFormat::Csv => ImportSourceConfig::Csv { path, options: self.csv.clone() },
            ImportFileFormat::Tsv => {
                let mut options = self.csv.clone();
                options.delimiter = "\t".into();
                ImportSourceConfig::Csv { path, options }
            }
            ImportFileFormat::Xlsx => ImportSourceConfig::Xlsx { path, options: self.xlsx.clone() },
            ImportFileFormat::Xml => ImportSourceConfig::Xml { path, options: self.xml.clone() },
            ImportFileFormat::Json => ImportSourceConfig::Json { path, options: self.json.clone() },
        }
    }

    fn infer_options(&self) -> InferOptions {
        let c = &self.options.convert;
        InferOptions {
            date_formats: c.date_formats.clone(),
            datetime_formats: c.datetime_formats.clone(),
            decimal_separator: c.decimal_separator,
            thousands_separator: c.thousands_separator,
            null_values: c.null_values.clone(),
            ..self.infer.clone()
        }
    }

    fn target_ref(&self) -> Option<ObjectRef> {
        match self.target {
            ImportTarget::Existing => self.table.clone(),
            ImportTarget::New => {
                let name = self.new_name.trim();
                if name.is_empty() {
                    return None;
                }
                let schema = self.schema.clone().unwrap_or_default();
                Some(ObjectRef::table(schema.schema, name).with_database(schema.database))
            }
        }
    }

    fn current_key(&self) -> Option<MappingKey> {
        let preview = self.preview.as_ref()?;
        let columns: Vec<String> = preview.columns.iter().map(|c| c.name.clone()).collect();
        Some(match self.target {
            ImportTarget::New => MappingKey::New { columns, infer: self.infer_options() },
            ImportTarget::Existing => MappingKey::Existing { table: self.table.clone()?, columns },
        })
    }

    fn file_error(&self) -> Option<String> {
        if self.path.trim().is_empty() {
            return Some(tr("Choose the file to import.").to_string());
        }
        if let Some((config, e)) = &self.preview_error
            && *config == self.source_config()
        {
            return Some(e.clone());
        }
        if !self.preview_ready() {
            return Some(tr("Reading the file…").to_string());
        }
        None
    }

    fn target_error(&self) -> Option<String> {
        match self.target {
            ImportTarget::Existing if self.table.is_none() => return Some(tr("Choose the target table.").to_string()),
            ImportTarget::New if self.new_name.trim().is_empty() => {
                return Some(tr("Enter the name of the new table.").to_string());
            }
            _ => {}
        }
        if let Some(e) = &self.mapping_error {
            return Some(e.clone());
        }
        if self.mapping_for != self.current_key() {
            return Some(tr("Preparing the column mapping…").to_string());
        }
        let active: Vec<&ColumnMapping> = self.columns.iter().filter(|c| !c.skip).collect();
        if active.is_empty() {
            return Some(tr("No column is imported.").to_string());
        }
        if let Some(c) = active.iter().find(|c| c.target_name.trim().is_empty()) {
            let source = self.preview_columns().get(c.source_index).cloned().unwrap_or_default();
            return Some(trf("The source column {} has no target column.", &[&source]));
        }
        for (i, a) in active.iter().enumerate() {
            if active[..i].iter().any(|b| b.target_name.eq_ignore_ascii_case(&a.target_name)) {
                return Some(trf("The target column {} is used twice.", &[&a.target_name]));
            }
        }
        if self.options.mode == ImportMode::Replace && self.key_columns.is_empty() {
            return Some(tr("Insert or replace needs key columns.").to_string());
        }
        None
    }

    fn validate(&self) -> Result<(), String> {
        match self.file_error().or_else(|| self.target_error()) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn reachable(&self) -> usize {
        if self.file_error().is_some() {
            PAGE_FILE
        } else if self.target_error().is_some() {
            PAGE_TARGET
        } else {
            PAGE_RUN
        }
    }

    fn poll(&mut self, cx: &mut AppContext) {
        // Preview.
        if let Some(job) = &mut self.preview_job
            && let Some(result) = job.poll()
        {
            self.preview_job = None;
            match result {
                Ok(p) => {
                    self.preview_error = None;
                    self.preview = Some(p);
                }
                Err((config, e)) => self.preview_error = Some((config, e)),
            }
        }
        let config = self.source_config();
        let current = self.preview.as_ref().is_some_and(|p| p.config == config);
        let failed = self.preview_error.as_ref().is_some_and(|(c, _)| *c == config);
        if self.preview_job.is_none() && !current && !failed && !self.path.trim().is_empty() {
            self.request_preview(cx, config);
        }
        // Sheet names.
        if self.format == ImportFileFormat::Xlsx {
            if let Some((path, job)) = &mut self.sheets_job
                && let Some(result) = job.poll()
            {
                let path = path.clone();
                self.sheets_job = None;
                self.sheets = Some((path, result.unwrap_or_default()));
            }
            let path = self.path.trim().to_string();
            let known = self.sheets.as_ref().is_some_and(|(p, _)| *p == path);
            if !known && self.sheets_job.is_none() && !path.is_empty() {
                let p = path.clone();
                let job = cx.backend.spawn(async move { tokio::task::spawn_blocking(move || xlsx_sheet_names(&p)).await.map_err(|e| DbError::internal(e.to_string()))? });
                self.sheets_job = Some((path, job));
            }
        }
        self.ensure_mapping(cx);
        // Run.
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
                    if summary.rows_failed > 0 {
                        cx.toasts.warning(describe(&summary), summary.errors.first().map(|e| e.message.clone()));
                    } else {
                        cx.toasts.success(describe(&summary));
                        self.done = true;
                    }
                    self.refresh_metadata(cx, summary.created_table);
                    if summary.created_table {
                        self.created = self.target_ref();
                    }
                    self.result = Some(Ok(summary));
                }
                Err(e) => {
                    if e.kind != ternarygrid_core::ErrorKind::Cancelled {
                        cx.toasts.error(tr("Import failed"), Some(e.full_text()));
                    }
                    self.result = Some(Err(error_text(&e)));
                }
            }
        }
        // Failed rows report.
        if let Some(job) = &mut self.report_job
            && let Some(result) = job.poll()
        {
            self.report_job = None;
            match result {
                Ok(path) => cx.toasts.success(trf("Report written to {}", &[&path.display()])),
                Err(e) => cx.toasts.error(tr("Cannot write the file"), Some(e.to_string())),
            }
        }
        self.saver.poll(cx);
    }

    fn request_preview(&mut self, cx: &mut AppContext, config: ImportSourceConfig) {
        let sample = self.sample_rows.clamp(10, 100_000);
        self.preview_job = Some(cx.backend.spawn(async move {
            let read = async {
                let source = config.open().await?;
                let mut source = PeekableSource::new(source);
                let rows = source.peek(sample).await?.to_vec();
                let columns = source.columns().to_vec();
                let warnings = source.take_warnings();
                let _ = source.close().await;
                Ok::<_, DbError>((columns, rows, warnings))
            };
            match read.await {
                Ok((columns, rows, warnings)) => Ok(Preview { config, columns, rows, warnings }),
                Err(e) => Err((config, e.full_text())),
            }
        }));
    }

    /// Build the column mapping for the current preview and target.
    fn ensure_mapping(&mut self, cx: &mut AppContext) {
        let Some(key) = self.current_key() else { return };
        if self.mapping_for.as_ref() == Some(&key) {
            return;
        }
        let Some(preview) = &self.preview else { return };
        match &key {
            MappingKey::New { columns, infer } => {
                let same_columns = matches!(&self.mapping_for, Some(MappingKey::New { columns: c, .. }) if c == columns);
                if self.columns_edited && same_columns {
                    // Keep the user's names and types; "Guess types again" rebuilds.
                    self.mapping_for = Some(key);
                    return;
                }
                let Some(live) = cx.conns.live(&self.conn) else { return };
                let inferred = infer_columns(&preview.columns, &preview.rows, infer);
                let target = self.target_ref().unwrap_or_else(|| ObjectRef::table(None, "import"));
                let m = ImportMapping::from_inferred(target, &inferred, live.dialect.as_ref());
                self.columns = m.columns;
                self.key_columns.clear();
            }
            MappingKey::Existing { table, .. } => {
                let table = table.clone();
                let conn = self.conn.clone();
                match table_def_of(cx, &conn, &table) {
                    None => return,
                    Some(Err(e)) => {
                        self.mapping_error = Some(trf("Cannot read the table definition: {}", &[&e.full_text()]));
                        return;
                    }
                    Some(Ok(def)) => {
                        let Some(preview) = &self.preview else { return };
                        let m = ImportMapping::match_table(&preview.columns, &def);
                        self.columns = m.columns;
                        self.key_columns = m.key_columns;
                    }
                }
            }
        }
        self.mapping_error = None;
        self.columns_edited = false;
        self.mapping_for = Some(key);
    }

    fn refresh_metadata(&self, cx: &mut AppContext, created: bool) {
        let Some(target) = self.target_ref() else { return };
        let schema = match &target.schema {
            Some(_) => target.schema_ref(),
            None => current_schema(cx, &self.conn).unwrap_or_default(),
        };
        let cmd = if created {
            AppCommand::RefreshMetadata { connection: self.conn.clone(), object: None, schema: Some(schema) }
        } else {
            AppCommand::RefreshMetadata { connection: self.conn.clone(), object: Some(target), schema: None }
        };
        cx.push(cmd);
    }

    // ------------------------------------------------------------------ pages

    fn file_page(&mut self, ui: &mut Ui) {
        section(ui, tr("File"));
        let id = self.uid;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::secondary(ui, Some(icon::FOLDER_OPEN), tr("Browse…")).clicked()
                    && let Some(p) = rfd::FileDialog::new()
                        .set_title(tr("File to import"))
                        .add_filter(tr("Data files"), &["csv", "tsv", "txt", "tab", "xlsx", "xlsm", "xls", "ods", "xml", "json", "jsonl", "ndjson"])
                        .add_filter(tr("All files"), &["*"])
                        .pick_file()
                {
                    self.set_file(p);
                }
                // The file name always shows (its folder shortened in the middle).
                if wizard::path_field(ui, egui::Id::new(("imp_path", id)), &mut self.path, tr("path of the file"))
                    && let Some(f) = ImportFileFormat::from_path(Path::new(self.path.trim()))
                {
                    self.format = f;
                }
            });
        });
        let csv = matches!(self.format, ImportFileFormat::Csv | ImportFileFormat::Tsv);
        wizard::form(ui, ("imp_format", id), 1, LABEL_W, |f| {
            f.seg_row(tr("Format"), |ui| {
                let formats: Vec<(ImportFileFormat, &str)> = ImportFileFormat::ALL.iter().map(|f| (*f, f.label())).collect();
                parts::segmented(ui, &mut self.format, &formats);
            });
            if self.format == ImportFileFormat::Csv {
                f.row(tr("Delimiter"), |ui| delimiter_choice(ui, ("imp_delim", id), &mut self.csv.delimiter));
            }
        });
        wizard::form(ui, ("imp_reader", id), 2, LABEL_W, |f| match self.format {
            ImportFileFormat::Csv | ImportFileFormat::Tsv => csv_reader_ui(f, id, &mut self.csv, false),
            ImportFileFormat::Xlsx => {
                let sheets = match &self.sheets {
                    Some((p, names)) if *p == self.path.trim() => names.clone(),
                    _ => Vec::new(),
                };
                xlsx_reader_ui(f, id, &mut self.xlsx, &sheets);
            }
            ImportFileFormat::Xml => xml_reader_ui(f, id, &mut self.xml),
            ImportFileFormat::Json => json_reader_ui(f, id, &mut self.json),
        });
        // The other reading options, folded above the preview (which takes the rest of the page).
        let key = egui::Id::new(("imp_more", id));
        let mut open = wizard::remembered_open(ui, key, false);
        ui.add_space(16.0 - wizard::GAP);
        let rows = super::group(self.sample_rows as u64);
        let summary = if csv { trf("quotes, rows to skip, comments · {} rows sampled", &[&rows]) } else { trf("{} rows read for the preview and the type guess", &[&rows]) };
        if wizard::fold_card(ui, key, &mut open, tr("More options"), Some(&summary), true, |ui| {
            if csv {
                wizard::form(ui, ("imp_reader_more", id), 2, wizard::CARD_LABEL_W, |f| csv_reader_ui(f, id, &mut self.csv, true));
            }
            wizard::form(ui, ("imp_sample", id), 1, wizard::CARD_LABEL_W, |f| {
                f.row(tr("Rows read"), |ui| {
                    wizard::number(ui, &mut self.sample_rows, 10..=100_000, 120.0);
                    wizard::faint(ui, tr("for the preview and the type guess"));
                });
            });
        }) {
            wizard::remember_open(ui, key, open);
        }
        self.preview_ui(ui);
    }

    /// The first rows of the file, like the data grid, under the "Preview" title (the counts at
    /// its right).
    fn preview_ui(&mut self, ui: &mut Ui) {
        let id = self.uid;
        let counts = self.preview.as_ref().filter(|_| self.preview_ready()).map(|p| {
            let rows = trn(p.rows.len(), "1 row", "{} rows");
            let rows = if p.rows.len() >= self.sample_rows { format!("{rows}+") } else { rows };
            trf("{}, {} read", &[&trn(p.columns.len(), "1 column", "{} columns"), &rows])
        });
        wizard::section_with(ui, tr("Preview"), |ui| {
            if let Some(c) = &counts {
                wizard::faint(ui, c);
            }
        });
        if self.path.trim().is_empty() {
            wizard::muted(ui, tr("Choose a file to see its first rows."));
            return;
        }
        if let Some((config, e)) = &self.preview_error
            && *config == self.source_config()
        {
            crate::widgets::error_box(ui, e);
            if kit::secondary(ui, Some(icon::ARROW_CLOCKWISE), tr("Retry")).clicked() {
                self.preview_error = None;
            }
            return;
        }
        if self.preview_job.is_some() || !self.preview_ready() {
            parts::spinner_label(ui, tr("Reading the file…"));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        let Some(p) = &self.preview else { return };
        let pal = ternarygrid_ui::pal(ui);
        if !p.warnings.is_empty() {
            ui.scope(|ui| {
                ui.spacing_mut().interact_size.y = 16.0;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    for w in p.warnings.iter().take(5) {
                        ui.label(RichText::new(format!("{} {w}", icon::WARNING)).size(12.5).color(pal.warn));
                    }
                });
            });
        }
        // Like the data grid: row numbers, the column names with their types, NULL in italics.
        let font = egui::FontId::proportional(ternarygrid_ui::style::body_size(ui.ctx()));
        let text_w = |ui: &Ui, s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), egui::Color32::PLACEHOLDER).size().x;
        let rows: Vec<&Row> = p.rows.iter().take(PREVIEW_ROWS).collect();
        // Text files have text columns only.
        let text_file = matches!(self.format, ImportFileFormat::Csv | ImportFileFormat::Tsv);
        let type_of = |t: &str| if t.is_empty() && text_file { "text".to_string() } else { t.to_string() };
        let mut cols = vec![col("", W::Px(48.0)).right().grid()];
        for (i, c) in p.columns.iter().enumerate() {
            let head = text_w(ui, &c.name) + text_w(ui, &type_of(&c.type_name)) * 0.85 + 34.0;
            let widest = rows.iter().filter_map(|r| r.get(i)).map(|v| text_w(ui, preview_text(v).lines().next().unwrap_or(""))).fold(0.0, f32::max) + 24.0;
            // Numbers at the right, as in the design's grid.
            let mut values = rows.iter().filter_map(|r| r.get(i)).filter(|v| !v.is_null()).peekable();
            let number = values.peek().is_some() && values.all(|v| preview_text(v).trim().parse::<f64>().is_ok());
            let column = col(&c.name, W::Px(head.max(widest).clamp(80.0, 280.0))).grid();
            cols.push(if number { column.right() } else { column });
        }
        // The rest of the page (at least 4 rows); the rows scroll under the header.
        let rows_h = wizard::fit_height(ui, 29.0 * rows.len().max(1) as f32, 29.0 * 4.0, 35.0);
        wizard::table(
            ui,
            ("imp_preview", id),
            &cols,
            28.0,
            Some(rows_h),
            rows.len(),
            |ui, c| {
                if c > 0
                    && let Some(m) = p.columns.get(c - 1)
                {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(type_of(&m.type_name)).monospace().size(10.5).color(pal.text_faint));
                    });
                }
            },
            |ui, r, c| {
                if c == 0 {
                    ui.label(RichText::new((r + 1).to_string()).size(11.5).color(pal.text_faint));
                    return;
                }
                let Some(v) = rows[r].get(c - 1) else { return };
                if v.is_null() {
                    ui.label(RichText::new("NULL").italics().size(12.0).color(pal.text_faint));
                    return;
                }
                let full = preview_text(v);
                let first: String = full.chars().take(200).collect();
                let first = first.lines().next().unwrap_or("").to_string();
                let resp = ui.add(egui::Label::new(first.clone()).truncate());
                if full.len() > first.len() {
                    resp.on_hover_text(full.chars().take(2000).collect::<String>());
                }
            },
        );
    }

    fn target_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let id = self.uid;
        let conn = self.conn.clone();
        section(ui, tr("Target table"));
        if cx.conns.live(&conn).is_none() {
            ui.horizontal(|ui| {
                wizard::muted(ui, tr("Connect to choose the target table."));
                if kit::secondary(ui, Some(icon::PLUGS_CONNECTED), tr("Connect")).clicked() {
                    cx.push(AppCommand::Connect(conn.clone()));
                }
            });
            return;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            wizard::radio_value(ui, &mut self.target, ImportTarget::Existing, tr("Existing table"));
            ui.spacing_mut().item_spacing.x = 12.0;
            wizard::radio_value(ui, &mut self.target, ImportTarget::New, tr("New table"));
            kit::vdivider(ui, 18.0);
            wizard::muted(ui, tr("Schema"));
            if schema_combo(ui, cx, &conn, ("imp_schema", id), &mut self.schema, 140.0) && self.target == ImportTarget::Existing {
                self.table = None;
            }
            match self.target {
                ImportTarget::Existing => {
                    wizard::muted(ui, tr("Table"));
                    self.table_combo(ui, cx);
                }
                ImportTarget::New => {
                    wizard::muted(ui, tr("Name"));
                    ui.add(line(&mut self.new_name).hint_text(tr("new_table")).desired_width(220.0));
                }
            }
        });
        if let Some(e) = &self.mapping_error {
            crate::widgets::error_box(ui, e);
            if kit::secondary(ui, Some(icon::ARROW_CLOCKWISE), tr("Retry")).clicked()
                && let Some(t) = self.table.clone()
            {
                reload_table_def(cx, &conn, &t);
                self.mapping_error = None;
                self.mapping_for = None;
            }
        }
        section(ui, tr("Columns"));
        if self.preview.is_none() {
            wizard::muted(ui, tr("The columns appear once the file has been read."));
            return;
        }
        if self.mapping_for != self.current_key() {
            parts::spinner_label(ui, tr("Preparing the column mapping…"));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        match self.target {
            ImportTarget::New => self.new_columns_ui(ui, cx),
            ImportTarget::Existing => self.existing_columns_ui(ui, cx),
        }
    }

    fn table_combo(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let conn = self.conn.clone();
        let Some(schema) = self.schema.clone().or_else(|| current_schema(cx, &conn)) else { return };
        let tables = schema_tables(cx, &conn, &schema, false);
        let faint = ternarygrid_ui::pal(ui).text_faint;
        let label: egui::WidgetText = match &self.table {
            Some(t) => t.name.clone().into(),
            None => RichText::new(tr("Choose…")).color(faint).into(),
        };
        egui::ComboBox::from_id_salt(("imp_table", self.uid))
            .icon(kit::combo_caret)
            .popup_style(egui::containers::menu::menu_style.into())
            .selected_text(label)
            .width(200.0)
            .height(360.0)
            .show_ui(ui, |ui| {
                kit::menu_look(ui, 200.0);
                match &tables {
                    None => {
                        parts::spinner_label(ui, tr("Loading…"));
                    }
                    Some(Err(e)) => {
                        ui.label(e.full_text());
                    }
                    Some(Ok(list)) => {
                        for o in list.iter().filter(|o| !o.is_system) {
                            let selected = self.table.as_ref() == Some(&o.obj);
                            if wizard::choice(ui, selected, &o.obj.name) && !selected {
                                self.table = Some(o.obj.clone());
                            }
                        }
                    }
                }
            });
    }

    fn sample_values(&self, index: usize) -> String {
        let Some(p) = &self.preview else { return String::new() };
        p.rows.iter().filter_map(|r| r.get(index)).filter(|v| !v.is_null()).take(3).map(preview_text).collect::<Vec<_>>().join(", ")
    }

    fn new_columns_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let id = self.uid;
        let dialect = cx.conns.live(&self.conn).map(|l| l.dialect.clone());
        let names = self.preview_columns();
        wizard::tight_row(ui, |ui| {
            if wizard::action(ui, Some(icon::MAGIC_WAND), tr("Guess types again")).clicked() {
                self.columns_edited = false;
                self.mapping_for = None;
            }
            wizard::muted(ui, tr("Key columns become the primary key of the new table."));
        });
        let pal = ternarygrid_ui::pal(ui);
        let samples: Vec<String> = (0..self.columns.len()).map(|i| self.sample_values(self.columns[i].source_index)).collect();
        let states: Vec<bool> = self.columns.iter().map(|c| !c.skip).collect();
        let mut edited = false;
        let mut set_all = None;
        let cols = [
            col("", W::Px(32.0)),
            col(tr("Source column"), W::Px(130.0)),
            col(tr("Target column"), W::Fr(1.0)),
            col(tr("Type"), W::Fr(1.0)),
            col(tr("NULL"), W::Px(50.0)),
            col(tr("Key"), W::Px(44.0)),
            col(tr("Sample values"), W::Fr(1.6)),
        ];
        let (columns, keys) = (&mut self.columns, &mut self.key_columns);
        wizard::table(
            ui,
            ("imp_cols", id),
            &cols,
            38.0,
            None,
            columns.len(),
            |ui, c| {
                if c == 0
                    && let Some(on) = wizard::check_all(ui, &states)
                {
                    set_all = Some(on);
                }
            },
            |ui, i, k| {
                let c = &mut columns[i];
                match k {
                    0 => {
                        let mut import = !c.skip;
                        if wizard::check(ui, &mut import, "").changed() {
                            c.skip = !import;
                            edited = true;
                        }
                    }
                    1 => {
                        ui.add(egui::Label::new(RichText::new(names.get(c.source_index).cloned().unwrap_or_default()).color(pal.text_muted)).truncate());
                    }
                    2 => {
                        let old_name = c.target_name.clone();
                        if ui.add(wizard::fit(egui::TextEdit::singleline(&mut c.target_name), 26.0).desired_width(f32::INFINITY)).changed() {
                            edited = true;
                            if let Some(k) = keys.iter_mut().find(|k| **k == old_name) {
                                *k = c.target_name.clone();
                            }
                        }
                    }
                    3 => {
                        // An editable select: the type typed in, or picked from the list behind
                        // the caret inside the field's right end (as the kit's selects).
                        let caret_w = if dialect.is_some() { 26 } else { 0 };
                        let ty = wizard::fit(egui::TextEdit::singleline(&mut c.target_type), 26.0).font(egui::TextStyle::Monospace).margin(egui::Margin {
                            left: 13,
                            right: 10 + caret_w,
                            top: 0,
                            bottom: 0,
                        });
                        let field = ui.add(ty.desired_width(ui.available_width()));
                        if field.changed() {
                            c.target_kind = None;
                            edited = true;
                        }
                        if let Some(d) = &dialect {
                            let at = egui::Rect::from_center_size(egui::pos2(field.rect.right() - 15.0, field.rect.center().y), vec2(24.0, field.rect.height() - 4.0));
                            let r = ui.interact(at, ui.id().with(("imp_type_menu", i)), egui::Sense::click()).on_hover_text(tr("Types"));
                            let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&r));
                            let visuals = if r.hovered() { ui.visuals().widgets.hovered } else { ui.visuals().widgets.inactive };
                            kit::combo_caret(ui, at, &visuals, open);
                            if r.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            egui::Popup::menu(&r).show(|ui| {
                                kit::menu_look(ui, 260.0);
                                for kind in COMMON_KINDS {
                                    let t = d.ddl().default_type_for(kind);
                                    if kit::menu_item(ui, None, &t, kind_label(kind)).clicked() {
                                        c.target_type = t;
                                        c.target_kind = Some(kind);
                                        edited = true;
                                        ui.close();
                                    }
                                }
                            });
                        }
                    }
                    4 => {
                        if wizard::check(ui, &mut c.nullable, "").changed() {
                            edited = true;
                        }
                    }
                    5 => {
                        let mut key = keys.iter().any(|k| k.eq_ignore_ascii_case(&c.target_name));
                        if wizard::check(ui, &mut key, "").changed() {
                            edited = true;
                            if key {
                                keys.push(c.target_name.clone());
                            } else {
                                keys.retain(|k| !k.eq_ignore_ascii_case(&c.target_name));
                            }
                        }
                    }
                    _ => {
                        let s = samples.get(i).cloned().unwrap_or_default();
                        ui.add(egui::Label::new(RichText::new(&s).size(11.5).color(pal.text_faint)).truncate()).on_hover_text(s);
                    }
                }
            },
        );
        if let Some(on) = set_all {
            self.columns.iter_mut().for_each(|c| c.skip = !on);
            edited = true;
        }
        if edited {
            self.columns_edited = true;
        }
    }

    fn existing_columns_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let id = self.uid;
        let Some(table) = self.table.clone() else { return };
        let conn = self.conn.clone();
        let Some(Ok(def)) = table_def_of(cx, &conn, &table) else { return };
        let names = self.preview_columns();
        wizard::tight_row(ui, |ui| {
            if wizard::action(ui, Some(icon::REPEAT), tr("Match by name again")).clicked() {
                self.mapping_for = None;
            }
            if !self.key_columns.is_empty() {
                wizard::muted(ui, &trf("Key: {}", &[&self.key_columns.join(", ")]));
            }
        });
        let pal = ternarygrid_ui::pal(ui);
        let samples: Vec<String> = (0..self.columns.len()).map(|i| self.sample_values(self.columns[i].source_index)).collect();
        let cols = [col(tr("Source column"), W::Px(140.0)), col(tr("Target column"), W::Fr(1.2)), col(tr("Type"), W::Fr(0.8)), col(tr("Sample values"), W::Fr(1.6))];
        let columns = &mut self.columns;
        wizard::table(
            ui,
            ("imp_map", id),
            &cols,
            38.0,
            None,
            columns.len(),
            |_, _| {},
            |ui, i, k| {
                let c = &mut columns[i];
                match k {
                    0 => {
                        ui.add(egui::Label::new(RichText::new(names.get(c.source_index).cloned().unwrap_or_default()).color(pal.text_muted)).truncate());
                    }
                    1 => {
                        let label = if c.skip { tr("(not imported)").to_string() } else { c.target_name.clone() };
                        let w = ui.available_width();
                        egui::ComboBox::from_id_salt(("imp_map_col", id, i))
                            .icon(kit::combo_caret)
                            .popup_style(egui::containers::menu::menu_style.into())
                            .selected_text(label)
                            .width(w)
                            .show_ui(ui, |ui| {
                                kit::menu_look(ui, w);
                                if wizard::choice(ui, c.skip, tr("(not imported)")) {
                                    c.skip = true;
                                }
                                for col in &def.columns {
                                    let selected = !c.skip && c.target_name == col.name;
                                    if wizard::choice(ui, selected, &col.name) {
                                        c.skip = col.generated.is_some();
                                        c.target_name = col.name.clone();
                                        c.target_type = col.data_type.clone();
                                        c.target_kind = Some(col.kind);
                                        c.nullable = col.nullable;
                                    }
                                }
                            });
                    }
                    2 => {
                        wizard::mono(ui, if c.skip { "" } else { &c.target_type }, pal.text_muted, 12.0);
                    }
                    _ => {
                        let s = samples.get(i).cloned().unwrap_or_default();
                        ui.add(egui::Label::new(RichText::new(&s).size(11.5).color(pal.text_faint)).truncate()).on_hover_text(s);
                    }
                }
            },
        );
        let unmapped: Vec<&str> =
            def.columns.iter().filter(|col| col.generated.is_none() && !self.columns.iter().any(|c| !c.skip && c.target_name == col.name)).map(|col| col.name.as_str()).collect();
        if !unmapped.is_empty() {
            wizard::muted(ui, &trf("Table columns without a source (default value or NULL): {}", &[&unmapped.join(", ")]));
        }
    }

    fn options_page(&mut self, ui: &mut Ui, transactions: bool) {
        let id = self.uid;
        let (o, keys) = (&mut self.options, &self.key_columns);
        // Two columns 24 apart; the selects 300 wide when there is room.
        ui.spacing_mut().item_spacing.x = 24.0;
        ui.columns(2, |cols| {
            let [left, right] = cols else { return };
            left.spacing_mut().item_spacing = egui::vec2(8.0, wizard::GAP);
            right.spacing_mut().item_spacing = egui::vec2(8.0, wizard::GAP);
            section(left, tr("Writing"));
            wizard::form(left, ("imp_opts", id), 1, LABEL_W, |f| {
                f.row(tr("Existing rows"), |ui| {
                    let w = ui.available_width().min(300.0);
                    combo_w(
                        ui,
                        ("imp_mode", id),
                        &mut o.mode,
                        &[
                            (ImportMode::Append, tr("Keep them and add the new rows").to_string()),
                            (ImportMode::TruncateFirst, tr("Delete them first").to_string()),
                            (ImportMode::Replace, tr("Insert or replace rows by key").to_string()),
                        ],
                        w,
                    )
                });
                if o.mode == ImportMode::Replace {
                    f.row(tr("Key columns"), |ui| {
                        let p = ternarygrid_ui::pal(ui);
                        if keys.is_empty() {
                            ui.add(egui::Label::new(RichText::new(tr("None: mark key columns on the Target page.")).color(p.warn)).wrap());
                        } else {
                            ui.label(keys.join(", "));
                        }
                    });
                    if !transactions {
                        super::replace_without_transactions_ui(f);
                    }
                }
                f.row(tr("Rows per batch"), |ui| wizard::number(ui, &mut o.batch_size, 1..=1_000_000, 120.0));
                f.row(tr("Rows per INSERT"), |ui| wizard::number(ui, &mut o.rows_per_statement, 1..=1000, 120.0));
                f.row(tr("Transactions"), |ui| {
                    let w = ui.available_width().min(300.0);
                    combo_w(ui, ("imp_tx", id), &mut o.transaction, &transaction_options(), w)
                });
                f.row(tr("On error"), |ui| {
                    let w = ui.available_width().min(300.0);
                    combo_w(ui, ("imp_err", id), &mut o.on_error, &on_error_options(), w)
                });
                if o.on_error != OnError::Abort {
                    f.row(tr("Stop after"), |ui| {
                        opt_number(ui, &mut o.max_errors, tr("no limit"));
                        wizard::muted(ui, tr("failed rows"));
                    });
                }
            });

            section(right, tr("Reading values"));
            let c = &mut o.convert;
            // The lists: one entry per line (their hints on one line).
            wizard::form(right, ("imp_conv", id), 1, LABEL_W, |f| {
                f.row(tr("Date formats"), |ui| lines_edit(ui, &mut c.date_formats, tr("e.g. %d/%m/%Y"), f32::INFINITY));
                f.row(tr("Decimal separator"), |ui| char_edit(ui, &mut c.decimal_separator));
                f.row(tr("Thousands separator"), |ui| opt_char_edit(ui, &mut c.thousands_separator, ""));
                // The one NULL setting of the wizard: texts read as NULL, in every format.
                f.row(tr("NULL as"), |ui| lines_edit(ui, &mut c.null_values, tr("e.g. NULL or \\N"), f32::INFINITY));
                // Wrapped when the column is narrow (the window keeps the design's width).
                f.check_row(tr("Text"), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(16.0, 8.0);
                        wizard::check(ui, &mut c.trim_text, tr("Trim spaces"));
                        wizard::check(ui, &mut c.empty_text_as_null, tr("Empty text is NULL"));
                    });
                });
                f.row(tr("Time zone"), |ui| time_zone_ui(ui, ("imp_tz", id), &mut c.default_offset_minutes));
                f.row(tr("Timestamp formats"), |ui| lines_edit(ui, &mut c.datetime_formats, tr("e.g. %d/%m/%Y %H:%M"), f32::INFINITY));
                f.row(tr("Time formats"), |ui| lines_edit(ui, &mut c.time_formats, tr("e.g. %H.%M"), f32::INFINITY));
                f.row(tr("Binary values"), |ui| {
                    combo(
                        ui,
                        ("imp_bin", id),
                        &mut c.binary,
                        &[
                            (None, tr("Detect").to_string()),
                            (Some(BinaryEncoding::Hex), tr("Hexadecimal").to_string()),
                            (Some(BinaryEncoding::Base64), tr("Base64").to_string()),
                            (Some(BinaryEncoding::Text), tr("As text (UTF-8)").to_string()),
                        ],
                    )
                });
            });
        });

        section(ui, tr("Type guessing (new tables)"));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            wizard::check(ui, &mut self.infer.detect_bool, tr("Booleans"));
            wizard::check(ui, &mut self.infer.detect_json, tr("JSON"));
            wizard::check(ui, &mut self.infer.detect_uuid, tr("UUIDs"));
        });
    }

    fn run_page(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if let Some(run) = &self.run {
            wizard::title(ui, tr("Importing…"));
            progress_ui(ui, &run.progress, ProgressKind { label: tr("Objects"), items: (tr("object"), tr("objects")), files: false });
            self.log.ui(ui, ("imp_log", self.uid), 260.0, false);
            return;
        }
        match &self.result {
            Some(Ok(summary)) => {
                let summary = summary.clone();
                self.result_ui(ui, cx, &summary);
            }
            Some(Err(e)) => {
                wizard::outcome(ui, Tone::Error, tr("The import failed"), None);
                crate::widgets::error_box(ui, e);
                if kit::secondary(ui, Some(icon::PENCIL_SIMPLE), tr("Change settings")).clicked() {
                    self.result = None;
                    self.page = PAGE_FILE;
                }
            }
            None => self.summary_ui(ui, cx),
        }
        if self.result.is_some() {
            // The counters and the failed rows tell what happened: the log is folded away.
            let folded = matches!(self.result, Some(Ok(_)));
            self.log.ui(ui, ("imp_log", self.uid), 220.0, folded);
        }
    }

    /// The tone of a finished run (the outcome's icon, the stepper's last step).
    fn outcome(&self) -> Option<Tone> {
        match &self.result {
            Some(Ok(s)) => Some(outcome_tone(s.rows_written, s.rows_failed)),
            Some(Err(_)) => Some(Tone::Error),
            None => None,
        }
    }

    fn summary_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        section(ui, tr("Summary"));
        let p = ternarygrid_ui::pal(ui);
        let mapped = self.columns.iter().filter(|c| !c.skip).count();
        let creates = self.mapping().is_some_and(|m| m.create_table);
        let target = self.target_ref().map(|t| t.display_path()).unwrap_or_default();
        let target = if creates { trf("{} (new)", &[&target]) } else { target };
        let o = &self.options;
        let mode = match o.mode {
            ImportMode::Append => tr("Keep them and add the new rows"),
            ImportMode::TruncateFirst => tr("Delete them first"),
            ImportMode::Replace => tr("Insert or replace rows by key"),
        };
        let mut rows = vec![
            fact(tr("File"), self.path.trim()).path(),
            fact(tr("Format"), self.format.label()),
            fact(tr("Connection"), connection_name(cx, &self.conn)),
            fact(tr("Target table"), target),
            fact(tr("Columns"), trf("{} of {}", &[&mapped, &self.columns.len()])),
        ];
        if !creates {
            // Deleting the rows first is the one that loses data: in the warning colour.
            rows.push(fact(tr("Existing rows"), mode).color((o.mode == ImportMode::TruncateFirst).then_some(p.warn)));
        }
        if o.mode == ImportMode::Replace || (creates && !self.key_columns.is_empty()) {
            let keys = if self.key_columns.is_empty() { tr("None").to_string() } else { self.key_columns.join(", ") };
            rows.push(fact(tr("Key columns"), keys));
        }
        let on_error = on_error_options().into_iter().find(|(v, _)| *v == o.on_error).map(|(_, l)| l).unwrap_or_default();
        let on_error = match o.max_errors {
            Some(n) if o.on_error != OnError::Abort => format!("{on_error}, {}", trn(n as usize, "stop after 1 failed row", "stop after {} failed rows")),
            _ => on_error,
        };
        rows.push(fact(tr("On error"), on_error));
        wizard::facts(ui, ("imp_sum", self.uid), 8.0, &rows);
        if let Err(e) = self.validate() {
            crate::widgets::error_box(ui, &e);
        }
    }

    fn result_ui(&mut self, ui: &mut Ui, cx: &mut AppContext, s: &ImportSummary) {
        let p = ternarygrid_ui::pal(ui);
        wizard::outcome(ui, outcome_tone(s.rows_written, s.rows_failed), &describe(s), None);
        let mut facts = vec![
            fact(tr("Rows read"), super::group(s.rows_read)),
            fact(tr("Rows written"), super::group(s.rows_written)).color((s.rows_written > 0).then_some(p.ok)),
            fact(tr("Rows failed"), super::group(s.rows_failed)).color((s.rows_failed > 0).then_some(p.err)),
        ];
        if s.created_table {
            facts.push(fact(tr("Table"), tr("Created")));
        }
        facts.push(fact(tr("Statements"), super::group(s.statements)));
        facts.push(fact(tr("Time"), crate::widgets::format_duration(std::time::Duration::from_millis(s.duration_ms))));
        let mut save_report = false;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            let left_w = if s.errors.is_empty() { 300.0_f32.min(ui.available_width()) } else { 300.0 };
            ui.allocate_ui_with_layout(vec2(left_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(left_w);
                wizard::facts(ui, ("imp_res", self.uid), 6.0, &facts);
            });
            if !s.errors.is_empty() {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    parts::section_header(ui, tr("Failed rows"), |ui| {
                        if ui.add_enabled_ui(self.report_job.is_none(), |ui| wizard::action(ui, Some(icon::FLOPPY_DISK), tr("Save report…"))).inner.clicked() {
                            save_report = true;
                        }
                    });
                    if s.errors_truncated {
                        wizard::muted(ui, &trf("Only the first {} failed rows are listed.", &[&s.errors.len()]));
                    }
                    let failures: Vec<wizard::Failure> =
                        s.errors.iter().map(|e| wizard::Failure { object: None, row: Some(e.row), column: e.column.clone(), message: e.message.clone() }).collect();
                    wizard::failures_table(ui, ("imp_errors", self.uid), &failures, 240.0);
                });
            }
        });
        if save_report && let Some(path) = rfd::FileDialog::new().set_file_name("import-errors.csv").save_file() {
            let text = error_report(s);
            self.report_job = Some(cx.backend.spawn(async move { tokio::fs::write(&path, text).await.map(|_| path) }));
        }
        for w in &s.warnings {
            ui.label(RichText::new(format!("{} {w}", icon::WARNING)).color(p.warn));
        }
        wizard::tight_row(ui, |ui| {
            if let Some(target) = self.target_ref()
                && kit::secondary(ui, Some(icon::TABLE), tr("Open table")).clicked()
            {
                cx.push(AppCommand::OpenObject { connection: self.conn.clone(), object: target, page: crate::command::ObjectPage::Data });
            }
            if kit::secondary(ui, Some(icon::PENCIL_SIMPLE), tr("Change settings")).clicked() {
                self.result = None;
                self.page = PAGE_FILE;
                self.import_into_created();
            }
        });
    }

    /// A table the last run created exists now: the next run imports into it.
    fn import_into_created(&mut self) {
        if self.target == ImportTarget::New
            && let Some(t) = self.target_ref()
            && self.created.as_ref() == Some(&t)
        {
            self.table = Some(t);
            self.target = ImportTarget::Existing;
        }
    }

    fn footer(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let running = self.run.is_some();
        let reachable = self.reachable();
        let ok = reachable == PAGE_RUN && !running;
        // Saving as a task is offered with the summary (the whole configuration) and after a run.
        let invalid = self.validate().err();
        let footer = wizard::Footer {
            back: (self.page > PAGE_FILE).then_some(!running),
            next: (self.page < PAGE_RUN).then_some(self.page < reachable && !running),
            run: (tr("Run import"), ok),
            save: (self.page == PAGE_RUN).then_some((ok && !self.saver.is_saving(), invalid.as_deref())),
            error: self.error.as_deref(),
            running: self.run.as_ref().map(|r| r.is_cancelling()),
            finished: self.page == PAGE_RUN && self.result.is_some(),
        };
        match wizard::footer(ui, &footer) {
            Some(Clicked::Back) => self.page -= 1,
            Some(Clicked::Next) => self.page += 1,
            // Run again into a table the last run created: `created` keeps it from being created twice.
            Some(Clicked::Run) => self.start(cx),
            Some(Clicked::Save) => {
                let target = self.target_ref().map(|t| t.name).unwrap_or_default();
                self.saver.open(trf("Import {}", &[&target]));
            }
            Some(Clicked::Cancel) => self.cancel(),
            Some(Clicked::Close) => close_wizard(cx, &mut self.done),
            None => {}
        }
    }
}

fn describe(s: &ImportSummary) -> String {
    let written = super::rows_text(s.rows_written);
    if s.rows_failed > 0 { trf("{} imported, {} failed", &[&written, &super::group(s.rows_failed)]) } else { trf("{} imported", &[&written]) }
}

/// Text of a preview cell (canonical text, NULL marked).
fn preview_text(v: &ternarygrid_core::Value) -> String {
    if v.is_null() { "NULL".to_string() } else { v.to_text().into_owned() }
}

/// CSV with the failed rows of an import.
fn error_report(s: &ImportSummary) -> String {
    let quote = |t: &str| format!("\"{}\"", t.replace('"', "\"\""));
    let mut out = String::from("row,column,error\n");
    for e in &s.errors {
        out.push_str(&format!("{},{},{}\n", e.row, quote(e.column.as_deref().unwrap_or("")), quote(&e.message)));
    }
    out
}

/// A table name derived from a file name (`Sales 2024.csv` → `sales_2024`).
fn table_name_from(stem: &str) -> String {
    let mut out = String::new();
    for c in stem.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_string();
    match out.chars().next() {
        None => "import".to_string(),
        Some(c) if c.is_ascii_digit() => format!("t_{out}"),
        Some(_) => out,
    }
}

/// The time zone given to timestamps without an offset (going into time zone aware columns):
/// the machine's, UTC, or a fixed offset typed as ±HH:MM.
fn time_zone_ui(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug + Copy, value: &mut Option<i32>) {
    #[derive(Clone, Copy, PartialEq)]
    enum Zone {
        Local,
        Utc,
        Fixed,
    }
    let fixed_key = ui.id().with(("tz_fixed", id));
    let fixed = ui.data(|d| d.get_temp::<bool>(fixed_key)).unwrap_or(false) || matches!(value, Some(m) if *m != 0);
    let mut zone = match value {
        None => Zone::Local,
        Some(0) if !fixed => Zone::Utc,
        _ => Zone::Fixed,
    };
    let local = chrono::Local::now().offset().local_minus_utc() / 60;
    let options = [(Zone::Local, trf("Local (UTC{})", &[&offset_text(local, true)])), (Zone::Utc, tr("UTC").to_string()), (Zone::Fixed, tr("Fixed offset…").to_string())];
    if combo(ui, ("tz", id), &mut zone, &options) {
        *value = match zone {
            Zone::Local => None,
            Zone::Utc => Some(0),
            Zone::Fixed => Some(value.unwrap_or(local)),
        };
        ui.data_mut(|d| d.insert_temp(fixed_key, zone == Zone::Fixed));
    }
    if zone == Zone::Fixed {
        super::parsed_edit(ui, offset_text(value.unwrap_or(0), false), "+07:00", 80.0, false, |t| {
            if let Some(m) = parse_offset(t) {
                *value = Some(m);
            }
        });
    }
}

/// An offset in minutes as `+07:00` (`short`: `+7`, `+5:30`).
fn offset_text(minutes: i32, short: bool) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let (h, m) = (minutes.abs() / 60, minutes.abs() % 60);
    match (short, m) {
        (true, 0) => format!("{sign}{h}"),
        (true, _) => format!("{sign}{h}:{m:02}"),
        _ => format!("{sign}{h:02}:{m:02}"),
    }
}

/// `+07:00`, `-5`, `+0530`, `UTC+7` → minutes east of UTC.
fn parse_offset(text: &str) -> Option<i32> {
    let t = text.trim();
    let t = t.strip_prefix("UTC").unwrap_or(t).trim();
    let (sign, rest) = match t.chars().next()? {
        '-' => (-1, &t[1..]),
        '+' => (1, &t[1..]),
        _ => (1, t),
    };
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit() || c == ':') {
        return None;
    }
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None if rest.len() > 2 => (rest[..rest.len() - 2].parse().ok()?, rest[rest.len() - 2..].parse().ok()?),
        None => (rest.parse().ok()?, 0),
    };
    (h <= 14 && m < 60).then_some(sign * (h * 60 + m))
}

// ------------------------------------------------------------ reader options

/// A choice of text encodings (export and import), 160 wide.
pub(crate) fn encoding_combo(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, value: &mut String, options: &[&str]) {
    egui::ComboBox::from_id_salt(id).icon(kit::combo_caret).popup_style(egui::containers::menu::menu_style.into()).selected_text(value.clone()).width(160.0).show_ui(ui, |ui| {
        kit::menu_look(ui, 160.0);
        for e in options {
            if wizard::choice(ui, value.eq_ignore_ascii_case(e), e) {
                *value = e.to_string();
            }
        }
    });
}

/// The CSV reader's options (the delimiter is chosen with the format): the common ones, or
/// (`more`) the others, folded under "More options". NULL markers are set once, with the
/// options of reading values.
fn csv_reader_ui(f: &mut Form<'_>, id: u64, o: &mut CsvReadOptions, more: bool) {
    if more {
        f.row(tr("Quote character"), |ui| opt_char_edit(ui, &mut o.quote_char, tr("none")));
        f.row(tr("Skip rows"), |ui| wizard::number(ui, &mut o.skip_rows, 0..=1_000_000, 70.0));
        f.row(tr("Quotes inside values"), |ui| {
            combo(
                ui,
                ("csv_rd_escape", id),
                &mut o.escape,
                &[(EscapeStyle::Doubled, tr("Doubled (\"\")").to_string()), (EscapeStyle::Backslash, tr("Backslash (\\\")").to_string())],
            )
        });
        f.row(tr("Comment lines"), |ui| opt_char_edit(ui, &mut o.comment_char, tr("none")));
        f.check_row(tr("Spaces"), |ui| wizard::check(ui, &mut o.trim, tr("Trim spaces around values")));
        return;
    }
    f.row(tr("Header"), |ui| {
        let w = ui.available_width();
        combo_w(
            ui,
            ("csv_rd_header", id),
            &mut o.header,
            &[
                (CsvHeader::FirstRow, tr("The first row holds the column names").to_string()),
                (CsvHeader::None, tr("No header row").to_string()),
                (CsvHeader::Auto, tr("Detect").to_string()),
            ],
            w,
        )
    });
    f.row(tr("Encoding"), |ui| encoding_combo(ui, ("csv_rd_enc", id), &mut o.encoding, &ENCODINGS));
}

fn xlsx_reader_ui(f: &mut Form<'_>, id: u64, o: &mut XlsxReadOptions, sheets: &[String]) {
    f.row(tr("Sheet"), |ui| {
        let current = match &o.sheet {
            SheetSelector::Name(n) => n.clone(),
            SheetSelector::Index(i) => sheets.get(*i).cloned().unwrap_or_else(|| trf("Sheet {}", &[&(i + 1)])),
        };
        egui::ComboBox::from_id_salt(("xlsx_sheet", id))
            .icon(kit::combo_caret)
            .popup_style(egui::containers::menu::menu_style.into())
            .selected_text(current.clone())
            .width(180.0)
            .show_ui(ui, |ui| {
                kit::menu_look(ui, 180.0);
                if sheets.is_empty() {
                    kit::menu_caption(ui, tr("Loading…"));
                }
                for s in sheets {
                    if wizard::choice(ui, *s == current, s) {
                        o.sheet = SheetSelector::Name(s.clone());
                    }
                }
            });
    });
    f.check_row(tr("Header"), |ui| wizard::check(ui, &mut o.header, tr("The first row holds the column names")));
    f.row(tr("Skip rows"), |ui| wizard::number(ui, &mut o.skip_rows, 0..=1_000_000, 70.0));
    f.check_row(tr("Empty rows"), |ui| wizard::check(ui, &mut o.skip_empty_rows, tr("Ignore")));
}

fn xml_reader_ui(f: &mut Form<'_>, id: u64, o: &mut XmlReadOptions) {
    f.row(tr("Row element"), |ui| opt_text(ui, &mut o.record_element, tr("every child of the root element"), 220.0));
    f.check_row(tr("Attributes"), |ui| wizard::check(ui, &mut o.attributes_as_columns, tr("Attributes of the row element are columns")));
    f.check_row(tr("Empty elements"), |ui| wizard::check(ui, &mut o.empty_as_null, tr("Read as NULL")));
    f.row(tr("Encoding"), |ui| encoding_combo(ui, ("xml_rd_enc", id), &mut o.encoding, &ENCODINGS));
}

fn json_reader_ui(f: &mut Form<'_>, id: u64, o: &mut JsonReadOptions) {
    f.row(tr("Structure"), |ui| {
        combo(
            ui,
            ("json_rd_fmt", id),
            &mut o.format,
            &[
                (JsonInputFormat::Auto, tr("Detect").to_string()),
                (JsonInputFormat::Array, tr("Array of objects").to_string()),
                (JsonInputFormat::Lines, tr("One object per line").to_string()),
            ],
        )
    });
    f.row(tr("Encoding"), |ui| encoding_combo(ui, ("json_rd_enc", id), &mut o.encoding, &ENCODINGS));
}

impl TabView for ImportTab {
    fn take_done(&mut self) -> bool {
        std::mem::take(&mut self.done)
    }

    fn popup_size(&self) -> Option<egui::Vec2> {
        Some(wizard::WINDOW)
    }

    fn title(&self) -> String {
        // An existing table is named; a new one once the import has run.
        let named = match self.target {
            ImportTarget::Existing => self.page > PAGE_FILE,
            ImportTarget::New => self.run.is_some() || self.result.is_some(),
        };
        match self.target_ref() {
            Some(t) if named => trf("Import into {}", &[&t.name]),
            _ => tr("Import data").to_string(),
        }
    }

    fn icon(&self) -> &'static str {
        icon::DOWNLOAD
    }

    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        self.poll(cx);
        let uid = self.uid;
        wizard::top_panel(ui, egui::Id::new(("imp_top", uid)), |ui| {
            // In a pop-up its title bar names the wizard and the connection.
            if !cx.popup {
                ui.horizontal(|ui| {
                    crate::widgets::icon_heading(ui, icon::DOWNLOAD, tr("Import data"));
                    ui.weak(connection_name(cx, &self.conn));
                });
            }
            let steps = [tr("File"), tr("Target"), tr("Options"), tr("Run")];
            let reachable = if self.run.is_some() { self.page } else { self.reachable() };
            let outcome = if self.page == PAGE_RUN { self.outcome() } else { None };
            if let Some(p) = wizard::steps(ui, &steps, self.page, reachable.max(self.page), outcome)
                && self.run.is_none()
            {
                self.page = p;
            }
        });
        wizard::bottom_panel(ui, egui::Id::new(("imp_bottom", uid)), |ui| self.footer(ui, cx));
        wizard::body(ui, egui::Id::new(("imp_body", uid)), |ui| match self.page {
            PAGE_FILE => self.file_page(ui),
            PAGE_TARGET => self.target_page(ui, cx),
            PAGE_OPTIONS => self.options_page(ui, super::has_transactions(cx, &self.conn)),
            _ => self.run_page(ui, cx),
        });
        if let Some((name, folder)) = self.saver.ui(ui.ctx(), egui::Id::new(("imp_save", uid))) {
            self.save_task(cx, &name, folder);
        }
        if self.preview_job.is_some() || self.sheets_job.is_some() || self.saver.is_saving() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
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
    fn names_and_formats() {
        assert_eq!(table_name_from("Sales 2024"), "sales_2024");
        assert_eq!(table_name_from("2024-orders"), "t_2024_orders");
        assert_eq!(table_name_from("***"), "import");
        assert_eq!(ImportFileFormat::from_path(Path::new("a/b.JSONL")), Some(ImportFileFormat::Json));
        assert_eq!(ImportFileFormat::from_path(Path::new("a.tsv")), Some(ImportFileFormat::Tsv));
        assert_eq!(ImportFileFormat::from_path(Path::new("a.bin")), None);
    }

    #[test]
    fn time_zone_offsets() {
        assert_eq!(parse_offset("+07:00"), Some(420));
        assert_eq!(parse_offset("UTC-5"), Some(-300));
        assert_eq!(parse_offset("+0530"), Some(330));
        assert_eq!(parse_offset("7"), Some(420));
        assert_eq!(parse_offset("+25:00"), None);
        assert_eq!(parse_offset("+-5"), None);
        assert_eq!(parse_offset("é"), None);
        assert_eq!(offset_text(420, true), "+7");
        assert_eq!(offset_text(-330, true), "-5:30");
        assert_eq!(offset_text(420, false), "+07:00");
    }

    #[test]
    fn a_created_table_is_not_created_again() {
        let mut tab = ImportTab::new("c".into(), None, Some(SchemaRef::schema("public")));
        tab.set_file("/tmp/sales.csv");
        assert!(tab.mapping().unwrap().create_table);
        tab.created = tab.target_ref();
        assert!(!tab.mapping().unwrap().create_table, "run again: into the table the last run created");
        tab.new_name = "sales_2".into();
        assert!(tab.mapping().unwrap().create_table, "another name: a new table");
    }

    #[test]
    fn mapping_follows_target() {
        let mut tab = ImportTab::new("c".into(), None, Some(SchemaRef::schema("public")));
        tab.set_file("/tmp/My Data.csv");
        assert_eq!(tab.new_name, "my_data");
        let m = tab.mapping().unwrap();
        assert!(m.create_table);
        assert_eq!(m.target, ObjectRef::table(Some("public".into()), "my_data"));
        tab.set_existing_table(ObjectRef::table(Some("s".into()), "t"));
        let m = tab.mapping().unwrap();
        assert!(!m.create_table);
        assert_eq!(m.target.name, "t");
        let report =
            error_report(&ImportSummary { errors: vec![ternarygrid_transfer::RowError { row: 3, column: Some("a".into()), message: "bad \"x\"".into() }], ..Default::default() });
        assert_eq!(report, "row,column,error\n3,\"a\",\"bad \"\"x\"\"\"\n");
    }
}
