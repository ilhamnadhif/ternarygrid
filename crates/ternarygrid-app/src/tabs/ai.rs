//! AI assistant (PRD phase 3, DBeaver "AI smart assistance"): a chat with
//! streamed answers, quick actions (generate SQL, explain, fix an error,
//! optimize, describe an object), schema context from the metadata cache and
//! SQL blocks that can be copied or opened in a SQL editor.
//!
//! Privacy: nothing is sent while the assistant is disabled in the settings.
//! A request carries the instructions, the schema structure (only when
//! "include schema context" is on), sample rows (only when "send sample data"
//! is on, and only for "Describe object") and what the user typed. The API
//! key is read from the secret store when a request starts and is never shown
//! or logged.

// `DbError` carries the full server diagnostics (144 bytes); every `DbResult` triggers this lint.
#![allow(clippy::result_large_err)]

use crate::backend::Job;
use crate::command::AppCommand;
use crate::conn::session::SessionHandle;
use crate::cx::AppContext;
use crate::sqllang::SqlLanguage;
use crate::tabs::TabView;
use crate::tabs::ai_agent::{self, Agent, AgentEvent, ToolStep};
use crate::tabs::ai_history::{ConversationStore, SavedConversation, title_of};
use crate::tabs::table_designer::pick;
use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType, vec2};
use egui_phosphor::regular as icon;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use ternarygrid_ai::prompts;
use ternarygrid_ai::{AiClient, AiConfig, AiProviderKind, ChatMessage, ChatRequest, ChatResponse, SchemaContextBuilder, Usage};
use ternarygrid_config::settings::{AiProvider, AiSettings};
use ternarygrid_core::ddl::SchemaSnapshot;
use ternarygrid_core::meta::{ObjectKind, ObjectRef, SchemaRef};
use ternarygrid_core::{DbError, DbKind, DbResult, Dialect, ErrorKind, Secret, tr, trf};
use ternarygrid_editor::{EditorLanguage, EditorTheme};
use ternarygrid_ui::{kit, parts, style};
use tokio_util::sync::CancellationToken;

/// Character budget of the schema description put into the system prompt.
pub const SCHEMA_CONTEXT_BUDGET: usize = prompts::DEFAULT_CONTEXT_BUDGET;
/// Character budget of earlier messages repeated in a request (oldest dropped first).
pub const HISTORY_BUDGET: usize = 100_000;
/// Rows read for "Describe object" when sample data may be sent.
pub const SAMPLE_ROWS: u64 = 5;
/// Longest value text of a sample row.
const SAMPLE_VALUE_CHARS: usize = 120;

/// Height of the connection and schema selects at the top of the panel (as designed).
const PICKER_H: f32 = 28.0;

/// Fence tags treated as SQL (same list as `ternarygrid_ai::extract_sql_blocks`).
const SQL_TAGS: &[&str] = &[
    "sql",
    "postgresql",
    "postgres",
    "pgsql",
    "plpgsql",
    "mysql",
    "mariadb",
    "sqlite",
    "tsql",
    "t-sql",
    "mssql",
    "plsql",
    "oracle",
    "clickhouse",
    "cql",
    "redis",
    "javascript",
    "js",
    "mongodb",
    "http",
];

// ------------------------------------------------------------ quick actions

/// What the user asks the assistant to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum QuickAction {
    /// Free conversation.
    Chat,
    /// Write a query from a description.
    GenerateSql,
    /// Explain a statement.
    Explain,
    /// Explain an error and correct the statement.
    FixError,
    /// Make a query faster (optionally with its execution plan).
    Optimize,
    /// Describe a table, view or routine from its DDL.
    Describe,
}

impl QuickAction {
    pub const ALL: [QuickAction; 6] = [QuickAction::Chat, QuickAction::GenerateSql, QuickAction::Explain, QuickAction::FixError, QuickAction::Optimize, QuickAction::Describe];

    pub fn label(self) -> &'static str {
        match self {
            QuickAction::Chat => tr("Chat"),
            QuickAction::GenerateSql => tr("Generate SQL"),
            QuickAction::Explain => tr("Explain query"),
            QuickAction::FixError => tr("Fix error"),
            QuickAction::Optimize => tr("Optimize query"),
            QuickAction::Describe => tr("Describe object"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            QuickAction::Chat => icon::SPARKLE,
            QuickAction::GenerateSql => icon::FILE_SQL,
            QuickAction::Explain => icon::INFO,
            QuickAction::FixError => icon::WRENCH,
            QuickAction::Optimize => icon::LIGHTNING,
            QuickAction::Describe => icon::TABLE,
        }
    }

    /// The main input holds SQL (code editor style).
    fn takes_sql(self) -> bool {
        matches!(self, QuickAction::Explain | QuickAction::FixError | QuickAction::Optimize)
    }

    fn hint(self) -> &'static str {
        match self {
            QuickAction::Chat => tr("Ask anything about SQL or this database…"),
            QuickAction::GenerateSql => tr("Describe the data you need, e.g. \"customers with more than 3 orders last month\""),
            QuickAction::Explain => tr("Paste the SQL statement to explain"),
            QuickAction::FixError => tr("Paste the SQL statement that failed"),
            QuickAction::Optimize => tr("Paste the query to optimize"),
            QuickAction::Describe => tr("Paste a CREATE statement, or choose an object above"),
        }
    }
}

/// Inputs of a quick action: what the user typed plus what was read from the database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionInput {
    /// Chat text, description or SQL statement.
    pub text: String,
    /// "Fix error": the error message reported by the database.
    pub error: String,
    /// "Optimize": the execution plan as text.
    pub plan: Option<String>,
    /// "Describe": the object's DDL (read from the database, or `text`).
    pub ddl: Option<String>,
    /// "Describe": sample rows as text (only when sample data may be sent).
    pub sample: Option<String>,
}

/// A fenced block whose fence is longer than any backtick run in `content`.
fn fence(lang: &str, content: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in content.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let f = "`".repeat((longest + 1).max(3));
    format!("{f}{lang}\n{}\n{f}", content.trim_end())
}

/// The user message of a quick action, built with the `ternarygrid_ai::prompts` helpers.
pub fn user_message(action: QuickAction, input: &ActionInput) -> DbResult<ChatMessage> {
    let text = input.text.trim();
    let need = |ok: bool, what: &str| if ok { Ok(()) } else { Err(DbError::config(what.to_string())) };
    match action {
        QuickAction::Chat => {
            need(!text.is_empty(), tr("Type a question first."))?;
            Ok(prompts::chat(text))
        }
        QuickAction::GenerateSql => {
            need(!text.is_empty(), tr("Describe the query you need first."))?;
            Ok(prompts::generate_sql(text))
        }
        QuickAction::Explain => {
            need(!text.is_empty(), tr("Enter the SQL statement first."))?;
            Ok(prompts::explain_query(text))
        }
        QuickAction::FixError => {
            need(!text.is_empty(), tr("Enter the SQL statement first."))?;
            need(!input.error.trim().is_empty(), tr("Enter the error message first."))?;
            Ok(prompts::fix_error(text, input.error.trim()))
        }
        QuickAction::Optimize => {
            need(!text.is_empty(), tr("Enter the SQL statement first."))?;
            Ok(prompts::optimize_query(text, input.plan.as_deref()))
        }
        QuickAction::Describe => {
            let ddl = input.ddl.as_deref().map(str::trim).filter(|d| !d.is_empty()).unwrap_or(text);
            need(!ddl.is_empty(), tr("Choose an object or paste its definition first."))?;
            let mut m = prompts::describe_object(ddl);
            if let Some(sample) = input.sample.as_deref().filter(|s| !s.trim().is_empty()) {
                m.content.push_str("\n\nSample rows (the first rows of the table, for illustration only):\n");
                m.content.push_str(&fence("", sample));
            }
            Ok(m)
        }
    }
}

/// What the chat shows as the user's message (markdown-like; SQL in fences).
fn shown_text(action: QuickAction, input: &ActionInput, object: Option<&ObjectRef>) -> String {
    let text = input.text.trim();
    match action {
        QuickAction::Chat | QuickAction::GenerateSql => text.to_string(),
        QuickAction::Explain | QuickAction::Optimize => fence("sql", text),
        QuickAction::FixError => format!("{}\n\n{}", fence("sql", text), fence("text", input.error.trim())),
        QuickAction::Describe => match object {
            Some(o) => format!("{} {}", tr(o.kind.label()), o.display_path()),
            None => fence("sql", text),
        },
    }
}

// ------------------------------------------------------- request assembly

/// Database facts for the system prompt.
#[derive(Debug, Clone, Copy)]
pub struct RequestContext<'a> {
    /// Product of the connection (`Generic` without a connection).
    pub kind: DbKind,
    /// Server version string ("" when unknown).
    pub server_version: &'a str,
    /// Light snapshot of the selected schema.
    pub snapshot: Option<&'a SchemaSnapshot>,
    /// `AiSettings::include_schema_context`.
    pub include_schema: bool,
}

/// The complete request: system prompt (instructions, dialect, schema
/// context ranked by `question`), earlier exchanges that fit
/// [`HISTORY_BUDGET`] and the new user message last.
pub fn build_request(ctx: &RequestContext<'_>, history: &[(ChatMessage, String)], message: ChatMessage, question: &str) -> ChatRequest {
    let schema_text = match (ctx.include_schema, ctx.snapshot) {
        (true, Some(s)) => SchemaContextBuilder::new().snapshot(s).question(question).budget(SCHEMA_CONTEXT_BUDGET).build(),
        _ => String::new(),
    };
    let system = prompts::system_prompt(ctx.kind, ctx.server_version, &schema_text);
    let mut used = message.content.len();
    let mut kept: Vec<(&ChatMessage, &String)> = Vec::new();
    for (q, a) in history.iter().rev() {
        if q.content.trim().is_empty() || a.trim().is_empty() {
            continue;
        }
        let size = q.content.len() + a.len();
        if used + size > HISTORY_BUDGET {
            break;
        }
        used += size;
        kept.push((q, a));
    }
    kept.reverse();
    let mut messages = Vec::with_capacity(kept.len() * 2 + 1);
    for (q, a) in kept {
        messages.push(ChatMessage::user(q.content.clone()));
        messages.push(ChatMessage::assistant(a.clone()));
    }
    messages.push(message);
    ChatRequest::new(system, messages)
}

/// Client configuration from the preferences and the stored API key.
pub fn ai_config(settings: &AiSettings, api_key: Option<Secret>) -> AiConfig {
    let provider = match settings.provider {
        AiProvider::Anthropic => AiProviderKind::Anthropic,
        AiProvider::OpenAiCompatible => AiProviderKind::OpenAiCompatible,
    };
    let anthropic = provider == AiProviderKind::Anthropic;
    AiConfig {
        provider,
        base_url: settings.base_url.clone().map(|u| u.trim().to_string()).filter(|u| !u.is_empty()),
        model: settings.model.trim().to_string(),
        api_key: api_key.filter(|k| !k.expose().trim().is_empty()),
        max_tokens: settings.max_tokens,
        effort: anthropic.then(|| settings.effort.as_str().to_string()),
        server_fallback: anthropic,
        ..AiConfig::default()
    }
}

fn provider_label(p: AiProvider) -> &'static str {
    match p {
        AiProvider::Anthropic => "Anthropic Claude",
        AiProvider::OpenAiCompatible => tr("OpenAI compatible"),
    }
}

// ------------------------------------------------------------- answer text

/// A piece of an answer: prose or a fenced code block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerSegment {
    Text(String),
    Code {
        /// Fence info word, lower case ("sql", "json", "" ...).
        lang: String,
        code: String,
        /// The block is SQL: tagged with a SQL dialect, or untagged when the
        /// answer has no tagged SQL block (as `ternarygrid_ai::extract_sql_blocks`).
        sql: bool,
    },
}

fn is_sql_tag(tag: &str) -> bool {
    SQL_TAGS.contains(&tag)
}

/// Split an answer into prose and code blocks. An unterminated last fence
/// (while streaming) is a code block too.
pub fn answer_segments(text: &str) -> Vec<AnswerSegment> {
    let mut out: Vec<AnswerSegment> = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let flush = |prose: &mut Vec<&str>, out: &mut Vec<AnswerSegment>| {
        let t = prose.join("\n");
        prose.clear();
        let t = t.trim_matches('\n').trim_end();
        if !t.trim().is_empty() {
            out.push(AnswerSegment::Text(t.to_string()));
        }
    };
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let fence_char = match trimmed.chars().next() {
            Some(c @ ('`' | '~')) => c,
            _ => {
                prose.push(line);
                continue;
            }
        };
        let fence_len = trimmed.chars().take_while(|c| *c == fence_char).count();
        if fence_len < 3 {
            prose.push(line);
            continue;
        }
        let info = trimmed[fence_len * fence_char.len_utf8()..].trim();
        let closing = fence_char.to_string().repeat(fence_len);
        if let Some(inner) = info.strip_suffix(closing.as_str()) {
            // single line block: ```sql SELECT 1```
            let mut parts = inner.trim().splitn(2, char::is_whitespace);
            let tag = parts.next().unwrap_or("").to_lowercase();
            let body = parts.next().unwrap_or("").trim().to_string();
            if body.is_empty() {
                prose.push(line);
                continue;
            }
            flush(&mut prose, &mut out);
            out.push(AnswerSegment::Code { lang: tag, code: body, sql: false });
            continue;
        }
        flush(&mut prose, &mut out);
        let tag = info.split_whitespace().next().unwrap_or("").to_lowercase();
        let mut body = Vec::new();
        for l in lines.by_ref() {
            let t = l.trim();
            if t.chars().count() >= fence_len && t.chars().all(|c| c == fence_char) {
                break;
            }
            body.push(l);
        }
        let code = body.join("\n").trim().to_string();
        if !code.is_empty() {
            out.push(AnswerSegment::Code { lang: tag, code, sql: false });
        }
    }
    flush(&mut prose, &mut out);
    let any_tagged = out.iter().any(|s| matches!(s, AnswerSegment::Code { lang, .. } if is_sql_tag(lang)));
    for s in &mut out {
        if let AnswerSegment::Code { lang, sql, .. } = s {
            *sql = is_sql_tag(lang) || (lang.is_empty() && !any_tagged);
        }
    }
    out
}

/// The SQL blocks of an answer, in order.
pub fn sql_blocks(text: &str) -> Vec<String> {
    answer_segments(text)
        .into_iter()
        .filter_map(|s| match s {
            AnswerSegment::Code { code, sql: true, .. } => Some(code),
            _ => None,
        })
        .collect()
}

/// Highlighted, non-wrapping layout of SQL text, 20 points a line.
fn highlight_job(code: &str, lang: Option<&SqlLanguage>, theme: &EditorTheme, font: egui::FontId) -> egui::text::LayoutJob {
    let tokens = lang.map(|l| l.highlight(code)).unwrap_or_default();
    let mut job = crate::sqllang::highlight_job(code, &tokens, theme, font);
    for s in &mut job.sections {
        s.format.line_height = Some(20.0);
    }
    job
}

// -------------------------------------------------------------- the chat

/// State of one answer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TurnState {
    /// Reading the plan / definition / sample rows from the database.
    Preparing,
    Streaming,
    Done,
    /// Stopped by the user; the partial answer is kept.
    Stopped,
    Failed(String),
    /// The model declined; streamed text was discarded.
    Refused(String),
}

/// One question and its answer (saved with the conversation).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Turn {
    pub action: QuickAction,
    /// What the chat shows as the question.
    pub shown: String,
    /// The message sent to the model (known once prepared).
    pub message: Option<ChatMessage>,
    pub answer: String,
    pub state: TurnState,
    pub model: Option<String>,
    pub usage: Option<Usage>,
    pub truncated: bool,
    pub fallback_from: Option<String>,
    /// Queries the assistant ran, each after `answer[..offset]`.
    pub steps: Vec<(usize, ToolStep)>,
    /// The query running now.
    #[serde(skip)]
    pub running: Option<String>,
    /// Parsed `answer` in pieces split at the steps, keyed by its length
    /// and the number of steps (both only grow while streaming).
    #[serde(skip)]
    segments: Option<((usize, usize), Vec<Vec<AnswerSegment>>)>,
}

impl Turn {
    fn new(action: QuickAction, shown: String) -> Turn {
        Turn {
            action,
            shown,
            message: None,
            answer: String::new(),
            state: TurnState::Preparing,
            model: None,
            usage: None,
            truncated: false,
            fallback_from: None,
            steps: Vec::new(),
            running: None,
            segments: None,
        }
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Prepared(m) => {
                self.message = Some(m);
                self.state = TurnState::Streaming;
            }
            Event::Delta(d) | Event::Agent(AgentEvent::Text(d)) => self.answer.push_str(&d),
            Event::Agent(AgentEvent::Running(sql)) => self.running = Some(sql),
            Event::Agent(AgentEvent::Step(step)) => {
                self.running = None;
                self.steps.push((self.answer.len(), step));
            }
        }
    }

    /// Part of the conversation sent with later questions.
    fn in_history(&self) -> bool {
        self.message.is_some() && matches!(self.state, TurnState::Done | TurnState::Stopped) && !self.answer.trim().is_empty()
    }

    /// The answer's segments before the first step, between the steps and
    /// after the last.
    fn parts(&mut self) -> &[Vec<AnswerSegment>] {
        let key = (self.answer.len(), self.steps.len());
        if self.segments.as_ref().is_none_or(|(k, _)| *k != key) {
            let mut parts = Vec::with_capacity(self.steps.len() + 1);
            let mut from = 0;
            for (at, _) in &self.steps {
                let mut at = (*at).clamp(from, self.answer.len());
                while !self.answer.is_char_boundary(at) {
                    at += 1;
                }
                parts.push(answer_segments(&self.answer[from..at]));
                from = at;
            }
            parts.push(answer_segments(&self.answer[from..]));
            self.segments = Some((key, parts));
        }
        &self.segments.as_ref().expect("just set").1
    }
}

enum Event {
    Prepared(ChatMessage),
    Delta(String),
    Agent(AgentEvent),
}

struct Pending {
    turn: usize,
    job: Job<DbResult<ChatResponse>>,
    rx: mpsc::Receiver<Event>,
    cancel: CancellationToken,
}

/// Database work done before a request (in the background).
enum Prep {
    None,
    Plan { session: SessionHandle, sql: String },
    Describe { session: SessionHandle, dialect: Arc<dyn Dialect>, object: ObjectRef, sample: bool },
}

/// `sql` as the text of a new editor: ended with `;` and a new line.
fn editor_text(sql: &str) -> String {
    let mut text = sql.trim_end().to_string();
    if !text.ends_with(';') {
        text.push(';');
    }
    text.push('\n');
    text
}

fn short_value(s: &str) -> String {
    let one_line = s.replace(['\n', '\r', '\t'], " ");
    match one_line.char_indices().nth(SAMPLE_VALUE_CHARS) {
        Some((i, _)) => format!("{}…", &one_line[..i]),
        None => one_line,
    }
}

impl Prep {
    async fn run(self, input: &mut ActionInput) -> DbResult<()> {
        match self {
            Prep::None => Ok(()),
            Prep::Plan { session, sql } => {
                // Without a plan the request is still useful: ignore errors.
                if let Ok(plan) = session.lock().await.explain(&sql, false).await {
                    let text = if plan.text.trim().is_empty() { plan.to_text_tree() } else { plan.text.clone() };
                    input.plan = Some(text).filter(|t| !t.trim().is_empty());
                }
                Ok(())
            }
            Prep::Describe { session, dialect, object, sample } => {
                let ddl = session.lock().await.object_ddl(&object).await.map_err(|e| e.with_context(trf("Reading the definition of {}", &[&object.display_path()])))?;
                input.ddl = Some(ddl);
                if sample && object.kind.has_data() {
                    let sql = dialect.limit_query(&dialect.select_table(&object, None, None, None, None), SAMPLE_ROWS, 0);
                    if let Ok((cols, rows)) = session.query_all(&sql, &[]).await {
                        let mut text = cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join("\t");
                        for r in rows {
                            text.push('\n');
                            text.push_str(&r.iter().map(|v| if v.is_null() { "NULL".to_string() } else { short_value(&v.to_text()) }).collect::<Vec<_>>().join("\t"));
                        }
                        input.sample = Some(text);
                    }
                }
                Ok(())
            }
        }
    }
}

/// Whether requests can be sent with the current preferences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiStatus {
    Disabled,
    MissingKey(String),
    Invalid(String),
    Ready,
}

pub struct AiAssistantTab {
    uid: u64,
    conn: Option<String>,
    schema: Option<SchemaRef>,
    action: QuickAction,
    input: String,
    error_input: String,
    include_plan: bool,
    object: Option<ObjectRef>,
    turns: Vec<Turn>,
    pending: Option<Pending>,
    /// Provider whose key presence was checked, the result and when.
    key_check: Option<(AiProvider, Result<bool, String>, Instant)>,
    notice: Option<String>,
    auto_send: bool,
    auto_since: Option<Instant>,
    connect_requested: bool,
    dedupe: bool,
    lang: Option<(DbKind, SqlLanguage)>,
    focus_input: bool,
    /// Id the conversation is saved under.
    conversation: String,
    /// Saved conversations, read when the history opens.
    saved: Option<Vec<SavedConversation>>,
    /// An answer finished since the conversation was saved.
    unsaved: bool,
    /// The history's popup was open in the last frame (its button shows it on).
    history_open: bool,
    /// "Clear history…" asks first: the number of conversations it deletes.
    confirm_clear: Option<usize>,
}

/// Shift+Enter starts a new line in the assistant's inputs; Enter sends.
const NEW_LINE: egui::KeyboardShortcut = egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter);

/// Enter (or ⌘Enter) without Shift was pressed: send the message.
fn enter_pressed(ui: &mut Ui) -> bool {
    ui.input_mut(|i| {
        let at = i.events.iter().position(|e| matches!(e, egui::Event::Key { key: egui::Key::Enter, pressed: true, modifiers, .. } if !modifiers.shift && !modifiers.alt));
        at.map(|at| i.events.remove(at)).is_some()
    })
}

impl AiAssistantTab {
    pub fn new(conn: Option<String>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        AiAssistantTab {
            uid: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            conn,
            schema: None,
            action: QuickAction::Chat,
            input: String::new(),
            error_input: String::new(),
            include_plan: true,
            object: None,
            turns: Vec::new(),
            pending: None,
            key_check: None,
            notice: None,
            auto_send: false,
            auto_since: None,
            connect_requested: false,
            dedupe: true,
            lang: None,
            focus_input: true,
            conversation: uuid::Uuid::new_v4().to_string(),
            saved: None,
            unsaved: false,
            history_open: false,
            confirm_clear: None,
        }
    }

    /// A new conversation about `sql`: "Fix error" when `error` is given,
    /// "Explain query" otherwise. The question is sent as soon as the
    /// assistant is enabled and configured (the user asked for it).
    pub fn ask_about(conn: Option<String>, sql: String, error: Option<String>) -> Self {
        let mut t = AiAssistantTab::new(conn);
        t.dedupe = false;
        t.action = if error.is_some() { QuickAction::FixError } else { QuickAction::Explain };
        t.input = sql;
        t.error_input = error.unwrap_or_default();
        t.auto_send = true;
        t
    }

    /// Like [`AiAssistantTab::ask_about`] with a chosen action, without sending.
    pub fn with_action(conn: Option<String>, action: QuickAction, text: String) -> Self {
        let mut t = AiAssistantTab::new(conn);
        t.dedupe = false;
        t.action = action;
        t.input = text;
        t
    }

    /// Opened again for `conn`: an empty conversation follows the connection
    /// (one under way keeps its own); the question box gets the focus.
    pub fn follow(&mut self, conn: Option<String>) {
        if self.turns.is_empty() && conn.is_some() && conn != self.conn {
            self.conn = conn;
            self.schema = None;
            self.object = None;
            self.lang = None;
        }
        self.focus_input = true;
    }

    fn kind(&self, cx: &AppContext) -> DbKind {
        self.conn.as_ref().and_then(|c| cx.conns.live(c)).map(|l| l.kind()).unwrap_or(DbKind::Generic)
    }

    /// Readiness from the preferences and the stored key (checked at most every few seconds).
    pub fn status(&mut self, cx: &AppContext) -> AiStatus {
        let ai = &cx.settings.ai;
        if !ai.enabled {
            return AiStatus::Disabled;
        }
        let stale = match &self.key_check {
            Some((p, r, at)) => *p != ai.provider || (!matches!(r, Ok(true)) && at.elapsed() > Duration::from_secs(2)),
            None => true,
        };
        if stale {
            let r = match cx.services.secrets.read().ai_api_key(ai.provider.id()) {
                Ok(k) => Ok(k.is_some_and(|k| !k.expose().trim().is_empty())),
                Err(e) => Err(e.full_text()),
            };
            self.key_check = Some((ai.provider, r, Instant::now()));
        }
        let present = match &self.key_check {
            Some((_, Ok(p), _)) => *p,
            Some((_, Err(e), _)) => return AiStatus::Invalid(e.clone()),
            None => false,
        };
        // A placeholder key only for validation: the real key is read on send.
        let cfg = ai_config(ai, present.then(|| Secret::new("present")));
        match cfg.validate() {
            Ok(()) => AiStatus::Ready,
            Err(e) if e.kind == ErrorKind::Auth => AiStatus::MissingKey(provider_label(ai.provider).to_string()),
            Err(e) => AiStatus::Invalid(e.full_text()),
        }
    }

    /// Earlier exchanges sent with the next question.
    pub fn history(&self) -> Vec<(ChatMessage, String)> {
        self.turns.iter().filter(|t| t.in_history()).filter_map(|t| t.message.clone().map(|m| (m, t.answer.clone()))).collect()
    }

    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Start a new conversation (the current one stays in the history).
    pub fn reset(&mut self) {
        self.stop();
        self.pending = None;
        self.turns.clear();
        self.notice = None;
        self.conversation = uuid::Uuid::new_v4().to_string();
    }

    fn store(cx: &AppContext) -> ConversationStore {
        ConversationStore::new(cx.services.paths.ai_conversations_dir())
    }

    /// Save the conversation's finished turns.
    fn save(&mut self, cx: &AppContext) {
        let turns: Vec<Turn> = self.turns.iter().filter(|t| !matches!(t.state, TurnState::Preparing | TurnState::Streaming)).cloned().collect();
        let Some(first) = turns.first() else { return };
        let c = SavedConversation {
            id: self.conversation.clone(),
            title: title_of(&first.shown),
            connection: self.conn.clone(),
            schema: self.schema.clone(),
            updated: chrono::Utc::now().timestamp(),
            turns,
        };
        if let Err(e) = Self::store(cx).save(&c) {
            tracing::warn!("cannot save the AI conversation: {e}");
        }
        self.saved = None;
    }

    /// Continue the saved conversation `c`.
    pub fn open_saved(&mut self, c: SavedConversation, cx: &AppContext) {
        self.reset();
        self.conversation = c.id;
        self.turns = c.turns;
        if let Some(conn) = c.connection.filter(|id| cx.conns.config(id).is_some()) {
            self.conn = Some(conn);
            self.schema = c.schema;
            self.object = None;
            self.lang = None;
        }
    }

    pub fn stop(&mut self) {
        if let Some(p) = &self.pending {
            p.cancel.cancel();
        }
    }

    fn snapshot(&self, cx: &AppContext) -> Option<Arc<SchemaSnapshot>> {
        let live = cx.conns.live(self.conn.as_ref()?)?;
        live.meta.snapshot(self.schema.as_ref()?)
    }

    /// Validate the inputs, start the background request and add the turn.
    pub fn send(&mut self, cx: &mut AppContext) {
        self.notice = None;
        if self.pending.is_some() {
            return;
        }
        if self.status(cx) != AiStatus::Ready {
            return;
        }
        let ai = cx.settings.ai.clone();
        let key = match cx.services.secrets.read().ai_api_key(ai.provider.id()) {
            Ok(k) => k,
            Err(e) => {
                self.notice = Some(e.full_text());
                return;
            }
        };
        let client = match AiClient::new(ai_config(&ai, key)) {
            Ok(c) => c,
            Err(e) => {
                self.key_check = None;
                self.notice = Some(e.full_text());
                return;
            }
        };
        let action = self.action;
        let mut input = ActionInput { text: self.input.clone(), error: self.error_input.clone(), ..Default::default() };
        let live = self.conn.as_ref().and_then(|c| cx.conns.live(c));
        let session = live.map(|l| l.main.clone());
        let object = if action == QuickAction::Describe { self.object.clone() } else { None };
        let prep = match (action, &session) {
            (QuickAction::Optimize, Some(s)) if self.include_plan && !input.text.trim().is_empty() => Prep::Plan { session: s.clone(), sql: input.text.trim().to_string() },
            (QuickAction::Describe, Some(s)) if object.is_some() => {
                Prep::Describe { session: s.clone(), dialect: s.dialect(), object: object.clone().expect("checked"), sample: ai.send_sample_data }
            }
            _ => Prep::None,
        };
        // Check the inputs now (the real message is built after the database lookups).
        let mut probe = input.clone();
        if matches!(prep, Prep::Describe { .. }) {
            probe.ddl = Some("-".into());
        }
        if let Err(e) = user_message(action, &probe) {
            self.notice = Some(e.message);
            return;
        }
        let kind = live.map(|l| l.kind()).unwrap_or(DbKind::Generic);
        let version = live.map(|l| l.server.version.clone()).unwrap_or_default();
        let snapshot = if ai.include_schema_context { self.snapshot(cx) } else { None };
        let history = self.history();
        let question = format!("{} {}", input.text, input.error);
        let include_schema = ai.include_schema_context;
        let preparing = !matches!(prep, Prep::None);
        let mut agent = match live {
            Some(l) if ai.agent_queries => {
                let schema = self.schema.clone().unwrap_or_default();
                // Read-only for the driver too (ClickHouse `readonly`, the NoSQL drivers), besides the agent's own checks.
                let opener = l.open_read_only_session_future(format!("{} ({})", l.config.name, tr("AI assistant")), schema.database);
                Some(Agent::new(l.kind(), l.main.dialect().syntax().clone(), schema.schema, Box::pin(opener)))
            }
            _ => None,
        };
        let mut turn = Turn::new(action, shown_text(action, &input, object.as_ref()));
        turn.state = if preparing { TurnState::Preparing } else { TurnState::Streaming };
        let (tx, rx) = mpsc::channel();
        let repaint = cx.backend.repainter();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let job = cx.backend.spawn(async move {
            prep.run(&mut input).await?;
            if token.is_cancelled() {
                return Err(DbError::cancelled());
            }
            let message = user_message(action, &input)?;
            let ctx = RequestContext { kind, server_version: &version, snapshot: snapshot.as_deref(), include_schema };
            let req = build_request(&ctx, &history, message.clone(), &question);
            let _ = tx.send(Event::Prepared(message));
            repaint.request();
            let (client_ref, tx_ref, rp_ref, token_ref) = (&client, &tx, &repaint, &token);
            let stream = move |req| -> std::pin::Pin<Box<dyn std::future::Future<Output = DbResult<ChatResponse>> + Send + '_>> {
                let (tx, rp) = (tx_ref.clone(), rp_ref.clone());
                let on_delta = move |d: &str| {
                    let _ = tx.send(Event::Delta(d.to_string()));
                    rp.request();
                };
                Box::pin(client_ref.chat_stream(req, on_delta, token_ref.clone()))
            };
            let Some(agent) = &mut agent else { return stream(req).await };
            let on_event = |e| {
                let _ = tx.send(Event::Agent(e));
                repaint.request();
            };
            ai_agent::converse(req, agent, &token, stream, on_event).await
        });
        self.turns.push(turn);
        self.pending = Some(Pending { turn: self.turns.len() - 1, job, rx, cancel });
        // Keep the SQL for follow-ups; clear free text.
        if !action.takes_sql() {
            self.input.clear();
        }
    }

    /// Take what the request sent; true when its answer finished.
    fn poll(&mut self) -> bool {
        let Some(p) = &mut self.pending else { return false };
        let Some(turn) = self.turns.get_mut(p.turn) else {
            self.pending = None;
            return false;
        };
        while let Ok(ev) = p.rx.try_recv() {
            turn.apply(ev);
        }
        let Some(result) = p.job.poll() else { return false };
        // Deltas sent before the result arrived.
        while let Ok(ev) = p.rx.try_recv() {
            turn.apply(ev);
        }
        turn.running = None;
        match result {
            Ok(r) => {
                turn.answer = r.text;
                turn.state = TurnState::Done;
                turn.model = Some(r.model).filter(|m| !m.is_empty());
                turn.usage = Some(r.usage);
                turn.truncated = r.truncated;
                turn.fallback_from = r.fallback_from;
            }
            Err(e) if e.code.as_deref() == Some("refusal") => {
                turn.answer.clear();
                turn.state = TurnState::Refused(e.full_text());
            }
            Err(e) if e.kind == ErrorKind::Cancelled => turn.state = TurnState::Stopped,
            Err(e) => {
                if e.kind == ErrorKind::Auth {
                    self.key_check = None;
                }
                turn.state = TurnState::Failed(e.full_text());
            }
        }
        self.pending = None;
        true
    }

    /// The SQL blocks of the answer of turn `turn`.
    pub fn answer_sql(&self, turn: usize) -> Vec<String> {
        self.turns.get(turn).map(|t| sql_blocks(&t.answer)).unwrap_or_default()
    }

    /// Open SQL in a new editor bound to the assistant's connection and schema.
    pub fn open_in_editor(&self, cx: &mut AppContext, sql: &str) {
        cx.push(AppCommand::NewSqlEditor { connection: self.conn.clone(), schema: self.schema.clone(), text: Some(editor_text(sql)) });
    }

    /// Test helper: add a finished exchange without contacting any service.
    #[doc(hidden)]
    pub fn push_exchange(&mut self, action: QuickAction, question: &str, answer: &str) {
        let mut t = Turn::new(action, question.to_string());
        t.message = Some(ChatMessage::user(question));
        t.answer = answer.to_string();
        t.state = TurnState::Done;
        self.turns.push(t);
        self.unsaved = true;
    }

    /// Test helper: the model and the token counts of the last answer.
    #[doc(hidden)]
    pub fn set_answered_by(&mut self, model: &str, input: u64, output: u64) {
        if let Some(t) = self.turns.last_mut() {
            t.model = Some(model.to_string());
            t.usage = Some(Usage { input_tokens: input, output_tokens: output, ..Default::default() });
        }
    }

    /// Test helper: a query the assistant ran before the answer of the last turn.
    #[doc(hidden)]
    pub fn push_step(&mut self, step: ToolStep) {
        if let Some(t) = self.turns.last_mut() {
            t.steps.push((0, step));
            self.unsaved = true;
        }
    }

    /// Test helper: a question whose answer is running `sql` now (nothing is sent).
    #[doc(hidden)]
    pub fn push_running(&mut self, question: &str, sql: &str) {
        let mut t = Turn::new(QuickAction::Chat, question.to_string());
        t.message = Some(ChatMessage::user(question));
        t.state = TurnState::Streaming;
        t.running = Some(sql.to_string());
        self.turns.push(t);
    }

    #[doc(hidden)]
    pub fn set_input(&mut self, action: QuickAction, text: &str, error: &str) {
        self.action = action;
        self.input = text.to_string();
        self.error_input = error.to_string();
    }

    #[doc(hidden)]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    // ----------------------------------------------------------------- UI

    fn sync_language(&mut self, cx: &AppContext) {
        let kind = self.kind(cx);
        if self.lang.as_ref().is_some_and(|(k, _)| *k == kind) {
            return;
        }
        let dialect: Arc<dyn Dialect> = match self.conn.as_ref().and_then(|c| cx.conns.live(c)) {
            Some(l) => l.dialect.clone(),
            None => Arc::new(ternarygrid_core::dialect::GenericDialect::new()),
        };
        self.lang = Some((kind, SqlLanguage::new(dialect)));
    }

    /// The connection and schema the assistant works with, what is sent with a question
    /// and the actions (settings, new conversation, history), above the conversation.
    fn header(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let p = ternarygrid_ui::pal(ui);
        egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.set_min_height(PICKER_H);
                if self.connection_picker(ui, cx) {
                    self.schema = None;
                    self.object = None;
                    self.lang = None;
                }
                if let Some(c) = self.conn.clone()
                    && cx.conns.is_connected(&c)
                {
                    self.schema_picker(ui, cx, &c);
                }
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.set_min_height(24.0);
                icon_box(ui, icon::DATABASE, 13.0, p.text_muted);
                self.context_status(ui, cx);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // 8 apart, as the design's row.
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.add(egui::Label::new(RichText::new(cx.settings.ai.model.clone()).size(11.0).color(p.text_faint)).truncate());
                    self.history_button(ui, cx);
                    if ui.add_enabled_ui(!self.turns.is_empty(), |ui| kit::icon_button_ex(ui, icon::PLUS, tr("New conversation"), false, 24.0)).inner.clicked() {
                        self.reset();
                    }
                    if kit::icon_button_ex(ui, icon::GEAR, tr("AI settings"), false, 24.0).clicked() {
                        cx.push(AppCommand::OpenSettings);
                    }
                });
            });
        });
        parts::hline(ui);
    }

    /// The connection, its database's logo in front; true when another one was chosen.
    fn connection_picker(&mut self, ui: &mut Ui, cx: &mut AppContext) -> bool {
        let names: Vec<(String, String, bool, DbKind)> = cx.conns.configs().iter().map(|c| (c.id.clone(), c.name.clone(), cx.conns.is_connected(&c.id), c.kind)).collect();
        let current = self.conn.as_ref().and_then(|c| names.iter().find(|(i, ..)| i == c));
        if let Some((.., kind)) = current {
            let dark = ui.visuals().dark_mode;
            if let Some(img) = crate::dblogo::image(ui.ctx(), *kind, 15.0, crate::theme::logo_color(*kind, dark)) {
                ui.add(img);
            }
        }
        let text = match (current, &self.conn) {
            (Some((_, name, ..)), _) => name.clone(),
            (None, None) => tr("No connection").to_string(),
            (None, Some(_)) => tr("Choose a connection…").to_string(),
        };
        let mut chosen: Option<Option<String>> = None;
        let none = self.conn.is_none();
        // As wide as its text (10 and 8 of padding, the chevron 8 from it), at most 220.
        let w = (ui.painter().layout_no_wrap(text.clone(), egui::FontId::proportional(13.0), Color32::PLACEHOLDER).size().x + 41.0).clamp(80.0, 220.0);
        crate::tabs::table_designer::select(ui, ("ai_conn", self.uid), text, w, PICKER_H, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            if pick(ui, none, tr("No connection")) && !none {
                chosen = Some(None);
            }
            for (id, name, connected, _) in &names {
                // The chosen one checked, the others with their state (connected or not).
                let glyph = if *connected { icon::PLUGS_CONNECTED } else { icon::PLUGS };
                let on = self.conn.as_deref() == Some(id.as_str());
                let clicked = if on { pick(ui, true, name) } else { kit::menu_item(ui, Some(glyph), name, "").clicked() };
                if clicked && !on {
                    chosen = Some(Some(id.clone()));
                }
            }
        });
        let Some(conn) = chosen else { return false };
        if let Some(c) = &conn
            && !cx.conns.is_connected(c)
        {
            cx.push(AppCommand::Connect(c.clone()));
        }
        self.conn = conn;
        true
    }

    /// The schema of a connected connection (its current one until another is chosen).
    fn schema_picker(&mut self, ui: &mut Ui, cx: &mut AppContext, conn: &str) {
        let backend = cx.backend;
        let fallback = crate::tabs::data_compare::default_schema(cx, conn);
        let Some(live) = cx.conns.live_mut(conn) else { return };
        let main = live.main.clone();
        live.meta.request_schemas(backend, &main, None);
        let loading = live.meta.schemas_of(None).is_loading();
        let mut names: Vec<String> = live.meta.schemas_of(None).get().map(|v| v.iter().filter(|s| !s.is_system).map(|s| s.name.clone()).collect()).unwrap_or_default();
        if let Some(f) = fallback.as_ref().and_then(|f| f.schema.clone())
            && !names.contains(&f)
        {
            names.insert(0, f);
        }
        if self.schema.is_none()
            && let Some(f) = fallback.or_else(|| names.first().map(|n| SchemaRef::new(None, Some(n.clone()))))
        {
            self.schema = Some(f);
            self.object = None;
        }
        let label = self.schema.as_ref().and_then(|s| s.schema.clone()).unwrap_or_else(|| tr("Schema…").to_string());
        let mut chosen: Option<String> = None;
        crate::tabs::table_designer::select(ui, ("ai_schema", self.uid), label, 130.0, PICKER_H, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            for n in &names {
                let on = self.schema.as_ref().and_then(|s| s.schema.as_deref()) == Some(n.as_str());
                if pick(ui, on, n) && !on {
                    chosen = Some(n.clone());
                }
            }
        });
        if let Some(n) = chosen {
            self.schema = Some(SchemaRef::new(None, Some(n)));
            self.object = None;
        }
        if loading {
            ui.add(egui::Spinner::new().size(14.0));
        }
    }

    /// The saved conversations: one opens with a click, the trash deletes it.
    fn history_button(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let p = ternarygrid_ui::pal(ui);
        let resp = kit::icon_button_ex(ui, icon::CLOCK_COUNTER_CLOCKWISE, tr("Conversation history"), self.history_open, 24.0);
        let popup_id = egui::Popup::default_response_id(&resp);
        if resp.clicked() || self.saved.is_none() && egui::Popup::is_id_open(ui.ctx(), popup_id) {
            self.saved = Some(Self::store(cx).list());
        }
        let mut open: Option<SavedConversation> = None;
        let mut delete: Option<String> = None;
        let mut clear = false;
        // As designed: 60 points from the panel's left edge, 9 under the button (on the
        // context's hairline).
        let at = egui::pos2(ui.clip_rect().left() + 60.0, resp.rect.bottom() + 9.0);
        egui::Popup::from_toggle_button_response(&resp)
            .anchor(at)
            .align(egui::RectAlign::BOTTOM_START)
            .gap(0.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_width(340.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                let saved = self.saved.as_deref().unwrap_or_default();
                if saved.is_empty() {
                    egui::Frame::new().inner_margin(Margin::same(8)).show(ui, |ui| {
                        ui.label(RichText::new(tr("No saved conversations")).color(p.text_faint));
                    });
                    return;
                }
                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                    for c in saved {
                        let when = chrono::DateTime::from_timestamp(c.updated, 0).map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                        let conn = c.connection.as_deref().and_then(|id| cx.conns.config(id)).map(|c| c.name.clone()).unwrap_or_default();
                        let detail = [conn, when].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                        match history_item(ui, &c.title, &detail, c.id == self.conversation) {
                            Some(HistoryClick::Open) => open = Some(c.clone()),
                            Some(HistoryClick::Delete) => delete = Some(c.id.clone()),
                            None => {}
                        }
                    }
                });
                let y = ui.cursor().top() + 3.0;
                ui.painter().hline(ui.max_rect().x_range().shrink(4.0), y, Stroke::new(1.0, p.border));
                ui.add_space(4.0);
                // Deletes every conversation: red, and it asks first.
                ui.scope(|ui| {
                    // A menu item's look (no frame at rest) in this pop-up.
                    egui::containers::menu::menu_style(ui.style_mut());
                    kit::menu_look(ui, 340.0);
                    if kit::menu_item_danger(ui, Some(icon::TRASH), tr("Clear history…"), "").clicked() {
                        self.confirm_clear = Some(saved.len());
                        ui.close();
                    }
                });
            });
        self.history_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        if let Some(n) = self.confirm_clear {
            let body = ternarygrid_core::trn(n, "The saved conversation is deleted. This cannot be undone.", "The {} saved conversations are deleted. This cannot be undone.");
            let (mut yes, mut no) = (false, false);
            let (resp, ()) = ternarygrid_ui::parts::Dialog::new(tr("Clear the AI history?")).tone(ternarygrid_ui::parts::Tone::Error).icon(icon::TRASH).width(440.0).show_modal(
                ui.ctx(),
                egui::Id::new(("ai_clear_history", self.uid)),
                |ui| {
                    ui.add(egui::Label::new(RichText::new(body).color(p.text_muted)).wrap());
                },
                |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        yes = kit::button_ex(ui, kit::Kind::DangerFilled, Some(icon::TRASH), tr("Clear history"), false).clicked();
                        no = kit::button_ex(ui, kit::Kind::Ghost, None, tr("Cancel"), false).clicked();
                    });
                },
            );
            if yes {
                clear = true;
            }
            if yes || no || resp.close {
                self.confirm_clear = None;
            }
        }
        let store = Self::store(cx);
        let done = if clear {
            store.clear()
        } else if let Some(id) = &delete {
            store.delete(id)
        } else {
            Ok(())
        };
        if let Err(e) = done {
            self.notice = Some(e.to_string());
        }
        if clear || delete.is_some() {
            self.saved = Some(store.list());
        }
        if let Some(c) = open {
            egui::Popup::close_id(ui.ctx(), popup_id);
            self.open_saved(c, cx);
        }
    }

    /// What is sent besides the question.
    fn context_status(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let p = ternarygrid_ui::pal(ui);
        let faint = |text: &str| RichText::new(text).size(12.0).color(p.text_faint);
        if !cx.settings.ai.include_schema_context {
            ui.label(faint(tr("Schema context: off")));
            return;
        }
        let (Some(conn), Some(schema)) = (self.conn.clone(), self.schema.clone()) else {
            ui.label(faint(tr("Schema context: none")));
            return;
        };
        let backend = cx.backend;
        let Some(live) = cx.conns.live_mut(&conn) else { return };
        let main = live.main.clone();
        live.meta.request_snapshot(backend, &main, &schema);
        match live.meta.snapshots.get(&schema) {
            Some(l) if l.is_loading() => {
                ui.add(egui::Spinner::new().size(12.0).color(p.accent_text));
                ui.label(faint(tr("Reading the schema…")));
            }
            Some(l) if l.error().is_some() => {
                let e = l.error().map(|e| e.full_text()).unwrap_or_default();
                ui.label(RichText::new(kit::with_icon(icon::WARNING, tr("No schema context"))).size(12.0).color(p.warn)).on_hover_text(e);
            }
            Some(l) => {
                if let Some(s) = l.get() {
                    let tables = ternarygrid_core::trn(s.tables.len(), "1 table", "{} tables");
                    let views = ternarygrid_core::trn(s.views.len(), "1 view", "{} views");
                    ui.add(egui::Label::new(faint(&trf("Schema context: {}, {}", &[&tables, &views]))).truncate())
                        .on_hover_text(tr("Table and column names of this schema are sent with each question. No data rows are sent."));
                }
            }
            None => {}
        }
        if cx.settings.ai.send_sample_data {
            ui.label(faint(tr("· sample rows on")));
        }
    }

    /// Nothing can be sent yet: why, and the way to the settings.
    fn setup_ui(ui: &mut Ui, cx: &mut AppContext, status: &AiStatus) {
        let p = ternarygrid_ui::pal(ui);
        let (title, body) = match status {
            AiStatus::Disabled => (
                tr("The AI assistant is turned off.").to_string(),
                Some(tr(
                    "Turn it on in the settings and enter the API key of your provider. Only the schema structure and what you type are sent, and only to the provider you choose.",
                )),
            ),
            AiStatus::MissingKey(provider) => (
                trf("No API key is saved for {}.", &[provider]),
                Some(tr("Enter the key in the AI section of the settings. It is kept in the system keychain or the encrypted vault.")),
            ),
            AiStatus::Invalid(_) => (tr("The AI settings are incomplete.").to_string(), None),
            AiStatus::Ready => (String::new(), None),
        };
        // In the middle of the panel: the block's height is measured on the frame before.
        let height_id = ui.id().with("ai_setup_height");
        let block_h = ui.data(|d| d.get_temp::<f32>(height_id)).unwrap_or(170.0);
        ui.vertical_centered(|ui| {
            ui.add_space(((ui.available_height() - block_h) / 2.0).max(20.0));
            let top = ui.cursor().top();
            ui.spacing_mut().item_spacing.y = 10.0;
            let (r, _) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 26.0, p.accent_soft);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, icon::SPARKLE, style::icon_font(24.0), p.accent_text);
            kit::semibold_label(ui, &title, 14.0, p.text);
            if let Some(body) = body {
                ui.scope(|ui| {
                    ui.set_max_width(ui.available_width().min(320.0));
                    ui.add(egui::Label::new(RichText::new(body).size(13.0).color(p.text_muted).line_height(Some(19.5))).wrap());
                });
            }
            if let AiStatus::Invalid(e) = status {
                egui::Frame::new().inner_margin(Margin::symmetric(16, 0)).show(ui, |ui| crate::widgets::error_box(ui, e));
            }
            if kit::primary_icon(ui, icon::GEAR, tr("Open settings")).clicked() {
                cx.push(AppCommand::OpenSettings);
            }
            let h = ui.min_rect().bottom() - top;
            ui.data_mut(|d| d.insert_temp(height_id, h));
        });
    }

    /// A code block of an answer: its language and actions on a header band, the code under it
    /// (SQL can be run unless it is the query running now).
    #[allow(clippy::too_many_arguments)]
    fn code_block(&self, ui: &mut Ui, cx: &mut AppContext, id: (u64, usize, usize, usize), lang_tag: &str, code: &str, sql: bool, runnable: bool) {
        let p = ternarygrid_ui::pal(ui);
        let theme = EditorTheme::from_visuals(ui.visuals());
        let label = if sql {
            "SQL".to_string()
        } else if lang_tag.is_empty() {
            tr("Code").to_string()
        } else {
            lang_tag.to_string()
        };
        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::Frame::new().fill(p.header).corner_radius(CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }).inner_margin(Margin { left: 10, right: 6, top: 4, bottom: 4 }).show(
                ui,
                |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.set_min_height(24.0);
                        ui.label(RichText::new(label).size(11.5).color(p.text_faint));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if kit::button_ex(ui, kit::Kind::Ghost, Some(icon::COPY), tr("Copy"), true).clicked() {
                                ui.ctx().copy_text(code.to_string());
                            }
                            if sql && kit::button_ex(ui, kit::Kind::Ghost, Some(icon::FILE_CODE), tr("Open in SQL editor"), true).clicked() {
                                self.open_in_editor(cx, code);
                            }
                            if sql
                                && runnable
                                && self.conn.is_some()
                                && kit::button_full(ui, kit::Kind::Ghost, Some(icon::PLAY), tr("Run"), true, true).on_hover_text(tr("Run in a new SQL editor")).clicked()
                            {
                                cx.push(AppCommand::RunSqlInEditor { connection: self.conn.clone(), schema: self.schema.clone(), text: editor_text(code) });
                            }
                        });
                    });
                },
            );
            parts::hline(ui);
            // Long lines scroll sideways without a bar (as designed, the code is cut at the edge).
            egui::Frame::new().inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt(("ai_code", id))
                    .auto_shrink([false, true])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        let lang = if sql { self.lang.as_ref().map(|(_, l)| l) } else { None };
                        ui.add(egui::Label::new(highlight_job(code, lang, &theme, FontId::monospace(12.5))).selectable(true).extend());
                    });
            });
        });
    }

    /// A query the assistant ran: one line ("Ran a query · 3 rows") over its result (open at
    /// first; the line folds it). Its SQL shows with "SQL" at the right of the line (at once
    /// when the query failed or returned no columns).
    fn step_ui(&self, ui: &mut Ui, cx: &mut AppContext, step: &ToolStep, id: (u64, usize, usize)) {
        let p = ternarygrid_ui::pal(ui);
        let open_id = egui::Id::new(("ai_step_open", id));
        let sql_id = egui::Id::new(("ai_step_sql", id));
        let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(true);
        let mut show_sql = ui.data(|d| d.get_temp::<bool>(sql_id)).unwrap_or(!step.ok || step.columns.is_empty());
        egui::Frame::new().stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            let title = if step.ok { format!("{} · {}", tr("Ran a query"), step.summary) } else { tr("The query did not run").to_string() };
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
            resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, open, &title));
            // "SQL" at the right end of the line: shows or hides the statement.
            let sql_g = ui.painter().layout_no_wrap("SQL".to_string(), FontId::proportional(11.5), p.text_faint);
            let sql_rect = Rect::from_min_max(egui::pos2(rect.right() - sql_g.size().x - 8.0, rect.top()), rect.max);
            let sql_resp = ui.interact(sql_rect, ui.id().with(("ai_step_sql_btn", id)), Sense::click()).on_hover_text(tr("Show the query"));
            sql_resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, show_sql, "SQL"));
            if sql_resp.clicked() {
                show_sql = !show_sql;
                open = open || show_sql;
                ui.data_mut(|d| {
                    d.insert_temp(sql_id, show_sql);
                    d.insert_temp(open_id, open);
                });
            } else if resp.clicked() {
                open = !open;
                ui.data_mut(|d| d.insert_temp(open_id, open));
            }
            let sql_color = if show_sql {
                p.accent_text
            } else if sql_resp.hovered() {
                p.text
            } else {
                p.text_faint
            };
            ui.painter().galley(egui::pos2(sql_rect.right() - sql_g.size().x, rect.center().y - sql_g.size().y / 2.0), sql_g, sql_color);
            let painter = ui.painter();
            let cy = rect.center().y;
            let caret = if open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
            painter.text(egui::pos2(rect.left() + 5.0, cy), Align2::CENTER_CENTER, caret, style::icon_font(11.0), p.text_faint);
            let (glyph, color) = if step.ok { (icon::DATABASE, p.text_muted) } else { (icon::WARNING, p.warn) };
            // The design's row: the caret (11), 8, the icon (13), 8, the text.
            painter.text(egui::pos2(rect.left() + 25.5, cy), Align2::CENTER_CENTER, glyph, style::icon_font(13.0), color);
            let text_color = if !step.ok {
                p.warn
            } else if resp.hovered() {
                p.text
            } else {
                p.text_muted
            };
            painter.text(egui::pos2(rect.left() + 40.0, cy), Align2::LEFT_CENTER, &title, FontId::proportional(12.5), text_color);
            if resp.has_focus() {
                painter.rect_stroke(rect.expand(2.0), CornerRadius::same(style::RADIUS), Stroke::new(1.5, p.accent), StrokeKind::Outside);
            }
            if !open {
                return;
            }
            if !step.ok {
                crate::widgets::error_box(ui, &step.summary);
            } else if !step.columns.is_empty() {
                result_table(ui, ("ai_step_rows", id), &step.columns, &step.rows);
            }
            if show_sql {
                self.code_block(ui, cx, (id.0, id.1, usize::MAX - 1 - id.2, 0), "", &step.sql, true, true);
            }
        });
    }

    fn segments_ui(&self, ui: &mut Ui, cx: &mut AppContext, segments: &[AnswerSegment], id: (u64, usize, usize)) {
        let p = ternarygrid_ui::pal(ui);
        for (i, s) in segments.iter().enumerate() {
            match s {
                AnswerSegment::Text(t) => {
                    ui.add(egui::Label::new(RichText::new(t.as_str()).size(13.0).color(p.text).line_height(Some(19.5))).wrap().selectable(true));
                }
                AnswerSegment::Code { lang, code, sql } => self.code_block(ui, cx, (id.0, id.1, id.2, i), lang, code, *sql, true),
            }
        }
    }

    fn conversation_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let p = ternarygrid_ui::pal(ui);
        if self.turns.is_empty() {
            egui::Frame::new().inner_margin(Margin::symmetric(24, 30)).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(tr("Ask a question or choose a quick action below. SQL in answers can be copied or opened in a SQL editor."))
                                .color(p.text_faint)
                                .line_height(Some(19.5)),
                        )
                        .wrap(),
                    );
                });
            });
            return;
        }
        egui::ScrollArea::vertical().id_salt(("ai_chat", self.uid)).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            egui::Frame::new().inner_margin(Margin::same(14)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 14.0;
                for ti in 0..self.turns.len() {
                    self.turn_ui(ui, cx, ti);
                }
            });
        });
    }

    /// One question (a bubble at the right) and its answer.
    fn turn_ui(&mut self, ui: &mut Ui, cx: &mut AppContext, ti: usize) {
        let p = ternarygrid_ui::pal(ui);
        let (shown, state, model, usage, truncated, fallback, answer_empty) = {
            let t = &self.turns[ti];
            (t.shown.clone(), t.state.clone(), t.model.clone(), t.usage, t.truncated, t.fallback_from.clone(), t.answer.trim().is_empty())
        };
        let (steps, running, after_step) = {
            let t = &self.turns[ti];
            (t.steps.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(), t.running.clone(), t.steps.last().is_some_and(|(at, _)| *at >= t.answer.len()))
        };
        // The question (88 % as wide as the panel, as the design measures it).
        let bubble_w = (ui.available_width() + 28.0) * 0.88;
        ui.with_layout(Layout::top_down(Align::Max), |ui| {
            egui::Frame::new().fill(p.subtle).corner_radius(CornerRadius::same(style::RADIUS_LG)).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                ui.set_max_width(bubble_w - 24.0);
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        icon_box(ui, icon::USER, 13.0, p.text_muted);
                        // The question's text names its action ("Fix error: …").
                        kit::semibold_label(ui, tr("You"), 12.5, p.text);
                    });
                    let segs = answer_segments(&shown);
                    self.segments_ui(ui, cx, &segs, (self.uid, ti, 0));
                });
            });
        });
        // The answer.
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                icon_box(ui, icon::SPARKLE, 13.0, p.accent_text);
                kit::semibold_label(ui, tr("Assistant"), 12.5, p.text);
                if let Some(m) = &model {
                    ui.label(RichText::new(m).size(11.0).color(p.text_faint));
                }
            });
            match &state {
                TurnState::Preparing => {
                    parts::spinner_label(ui, tr("Reading from the database…"));
                }
                TurnState::Streaming if answer_empty && steps.is_empty() && running.is_none() => {
                    parts::spinner_label(ui, tr("Thinking…"));
                }
                _ => {}
            }
            let parts_ = self.turns[ti].parts().to_vec();
            for (pi, segs) in parts_.iter().enumerate() {
                self.segments_ui(ui, cx, segs, (self.uid, ti, 1 + pi));
                if let Some(step) = steps.get(pi) {
                    self.step_ui(ui, cx, step, (self.uid, ti, pi));
                }
            }
            if state == TurnState::Streaming {
                if let Some(sql) = &running {
                    parts::spinner_label(ui, tr("Running a query…"));
                    self.code_block(ui, cx, (self.uid, ti, usize::MAX, 0), "", sql, true, false);
                } else if after_step {
                    parts::spinner_label(ui, tr("Thinking…"));
                }
            }
            let faint = |text: String| RichText::new(text).size(11.5).color(p.text_faint);
            match &state {
                TurnState::Stopped => {
                    ui.label(faint(kit::with_icon(icon::STOP, tr("Stopped."))));
                }
                TurnState::Failed(e) => crate::widgets::error_box(ui, e),
                TurnState::Refused(e) => {
                    ui.label(RichText::new(kit::with_icon(icon::WARNING, tr("The model declined to answer this request."))).color(p.warn));
                    ui.label(faint(e.clone()));
                }
                TurnState::Done => {
                    if truncated {
                        ui.label(RichText::new(kit::with_icon(icon::WARNING, tr("The answer was cut off at the token limit."))).color(p.warn));
                    }
                    if let (Some(from), Some(m)) = (&fallback, &model) {
                        ui.label(faint(trf("Answered by {} because {} declined.", &[m, from])));
                    }
                    if let Some(u) = usage {
                        let input = u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens;
                        ui.label(faint(trf("{} input / {} output tokens", &[&grouped(input), &grouped(u.output_tokens)])));
                    }
                }
                _ => {}
            }
        });
    }

    fn composer_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let p = ternarygrid_ui::pal(ui);
        egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for a in QuickAction::ALL {
                    if action_chip(ui, a.icon(), a.label(), self.action == a).clicked() {
                        self.action = a;
                    }
                }
            });
            let connected = self.conn.as_ref().is_some_and(|c| cx.conns.is_connected(c));
            match self.action {
                QuickAction::Describe if connected => self.object_picker(ui, cx),
                QuickAction::Optimize if connected => {
                    kit::checkbox(ui, &mut self.include_plan, tr("Include the execution plan (EXPLAIN runs on the connection, the query is not executed)"));
                }
                _ => {}
            }
            let busy = self.pending.is_some();
            let mut send = false;
            let code = self.action.takes_sql() || self.action == QuickAction::Describe;
            let rows = if code { 4 } else { 2 };
            // Fixing an error takes two texts: each under its name.
            if self.action == QuickAction::FixError {
                field_label(ui, tr("Statement"));
            }
            let resp = input_box(ui, &mut self.input, ("ai_input", self.uid), self.action.hint(), rows, code);
            if self.focus_input {
                self.focus_input = false;
                resp.request_focus();
            }
            if resp.has_focus() && enter_pressed(ui) {
                send = true;
            }
            if self.action == QuickAction::FixError {
                field_label(ui, tr("Error message"));
                let r = input_box(ui, &mut self.error_input, ("ai_error", self.uid), tr("Error message from the database"), 2, false);
                if r.has_focus() && enter_pressed(ui) {
                    send = true;
                }
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if busy {
                    if kit::button_ex(ui, kit::Kind::Danger, Some(icon::STOP), tr("Stop"), true).clicked() {
                        self.stop();
                    }
                    ui.add(egui::Spinner::new().size(14.0).color(p.accent_text));
                } else if kit::button_ex(ui, kit::Kind::Primary, Some(icon::PAPER_PLANE_TILT), tr("Send"), true).clicked() {
                    send = true;
                }
                let (enter, shift_enter) = if crate::keys::IS_MAC { ("↩", "⇧↩") } else { ("Enter", "Shift+Enter") };
                ui.label(RichText::new(trf("{} to send, {} for a new line", &[&enter, &shift_enter])).size(11.5).color(p.text_faint));
            });
            if let Some(n) = &self.notice {
                ui.add(egui::Label::new(RichText::new(kit::with_icon(icon::WARNING, n)).size(12.5).color(p.warn)).wrap());
            }
            if send && !busy {
                self.send(cx);
            }
        });
    }

    /// Send the question of [`AiAssistantTab::ask_about`] once the connection
    /// and its schema context are available (or after a few seconds without).
    fn maybe_auto_send(&mut self, cx: &mut AppContext) {
        if !self.auto_send {
            return;
        }
        let since = *self.auto_since.get_or_insert_with(Instant::now);
        let ready = match self.conn.clone() {
            None => true,
            Some(c) if !cx.conns.is_connected(&c) => {
                if !cx.conns.is_connecting() && !self.connect_requested {
                    self.connect_requested = true;
                    cx.push(AppCommand::Connect(c));
                }
                false
            }
            Some(c) => {
                !cx.settings.ai.include_schema_context
                    || match (&self.schema, cx.conns.live(&c)) {
                        (Some(s), Some(l)) => l.meta.snapshots.get(s).is_some_and(|x| !x.is_loading()),
                        _ => false,
                    }
            }
        };
        if ready || since.elapsed() > Duration::from_secs(10) {
            self.auto_send = false;
            self.send(cx);
        }
    }

    fn object_picker(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let p = ternarygrid_ui::pal(ui);
        let Some(snap) = self.snapshot(cx) else {
            ui.label(RichText::new(tr("Choose a schema to pick an object, or paste its definition below.")).size(12.5).color(p.text_faint));
            return;
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.set_min_height(style::CONTROL_H);
            ui.label(RichText::new(tr("Object:")).color(p.text_muted));
            let label = self.object.as_ref().map(|o| o.name.clone()).unwrap_or_else(|| tr("(paste a definition)").to_string());
            crate::tabs::table_designer::select(ui, ("ai_obj", self.uid), label, 240.0, style::CONTROL_H, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                if pick(ui, self.object.is_none(), tr("(paste a definition)")) {
                    self.object = None;
                }
                let db = self.schema.as_ref().and_then(|s| s.database.clone());
                let items = snap.tables.iter().map(|t| ObjectRef::table(snap.schema.clone(), t.name.clone())).chain(
                    snap.views.iter().map(|v| ObjectRef::new(if v.materialized { ObjectKind::MaterializedView } else { ObjectKind::View }, snap.schema.clone(), v.name.clone())),
                );
                for o in items {
                    let o = o.with_database(db.clone());
                    let on = self.object.as_ref() == Some(&o);
                    let clicked = if on { pick(ui, true, &o.name) } else { kit::menu_item(ui, Some(crate::nav::kind_icon(o.kind)), &o.name, "").clicked() };
                    if clicked {
                        self.object = Some(o);
                    }
                }
            });
            if cx.settings.ai.send_sample_data && self.object.as_ref().is_some_and(|o| o.kind.has_data()) {
                ui.label(RichText::new(trf("The first {} rows are sent as a sample.", &[&SAMPLE_ROWS])).size(12.0).color(p.text_faint));
            }
        });
    }
}

/// `n` with its thousands grouped by commas (token counts).
fn grouped(n: impl std::fmt::Display) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) && c.is_ascii_digit() {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A quick action chip: its icon and name, the chosen one on the accent tint.
fn action_chip(ui: &mut Ui, glyph: &str, text: &str, on: bool) -> Response {
    let p = ternarygrid_ui::pal(ui);
    let label = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(12.0), Color32::PLACEHOLDER);
    let icon_g = ui.painter().layout_no_wrap(glyph.to_string(), style::icon_font(12.5), Color32::PLACEHOLDER);
    let width = 10.0 + icon_g.size().x + 5.0 + label.size().x + 10.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 26.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, on, text));
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        let (fill, color) = if on {
            (p.accent_soft, p.accent_text)
        } else if hovered {
            (p.subtle, p.text)
        } else {
            (p.subtle, p.text_muted)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(13), fill);
        let iw = icon_g.size().x;
        painter.galley(egui::pos2(rect.left() + 10.0, rect.center().y - icon_g.size().y / 2.0), icon_g, color);
        painter.galley(egui::pos2(rect.left() + 15.0 + iw, rect.center().y - label.size().y / 2.0), label, color);
        if resp.has_focus() {
            style::focus_halo(ui, rect, 13);
        }
    }
    resp
}

/// A multi-line input on a rounded surface (the composer's question and error message);
/// Enter is left to the caller (it sends), Shift+Enter starts a new line.
fn input_box(ui: &mut Ui, text: &mut String, id: impl std::hash::Hash + std::fmt::Debug, hint: &str, rows: usize, code: bool) -> Response {
    let p = ternarygrid_ui::pal(ui);
    let out = egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut edit = egui::TextEdit::multiline(text)
            .frame(egui::Frame::NONE)
            .margin(Margin::ZERO)
            .hint_text(RichText::new(hint).color(p.text_faint))
            .desired_rows(rows)
            // The design's question box is 64 high (its text 46).
            .min_size(vec2(0.0, if code { 0.0 } else { 46.0 }))
            .desired_width(f32::INFINITY)
            .return_key(NEW_LINE)
            .id_salt(id);
        if code {
            edit = edit.code_editor();
        }
        ui.add(edit)
    });
    if out.inner.has_focus() {
        style::focus_halo(ui, out.response.rect, 8);
    }
    out.inner
}

/// The name over a text box (11.5 points, muted), close to it.
fn field_label(ui: &mut Ui, text: &str) {
    let p = ternarygrid_ui::pal(ui);
    let gap = ui.spacing().item_spacing.y;
    ui.label(RichText::new(text).size(11.5).color(p.text_muted));
    ui.add_space(4.0 - gap);
}

/// What was clicked in an entry of the history.
enum HistoryClick {
    Open,
    Delete,
}

/// An entry of the history: its title, where and when, and a trash button; the current
/// conversation on the accent tint.
fn history_item(ui: &mut Ui, title: &str, detail: &str, current: bool) -> Option<HistoryClick> {
    let p = ternarygrid_ui::pal(ui);
    // 8 × 10 of padding, the title's line as tall as the trash button (22), 2, the detail.
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, current, title));
    let trash_rect = Rect::from_center_size(egui::pos2(rect.right() - 21.0, rect.top() + 19.0), Vec2::splat(24.0));
    let trash = ui.interact(trash_rect, resp.id.with("delete"), Sense::click()).on_hover_text(tr("Delete the conversation"));
    trash.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, tr("Delete the conversation")));
    let resp = resp.on_hover_text(title);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if current {
            painter.rect_filled(rect, CornerRadius::same(style::RADIUS), p.accent_soft);
        } else if resp.hovered() || trash.hovered() {
            painter.rect_filled(rect, CornerRadius::same(style::RADIUS), p.hover);
        }
        // The trash while the entry is hovered: 14 points, muted (the text on its own hover).
        if trash.hovered() {
            painter.rect_filled(trash_rect, CornerRadius::same(style::RADIUS), p.subtle);
        }
        if resp.hovered() || trash.hovered() || trash.has_focus() {
            let trash_color = if trash.hovered() { p.text } else { p.text_muted };
            painter.text(trash_rect.center(), Align2::CENTER_CENTER, icon::TRASH, style::icon_font(14.0), trash_color);
        }
        let font = if current { style::medium(ui.ctx(), 13.0) } else { FontId::proportional(13.0) };
        let max_w = (rect.width() - 50.0).max(40.0);
        let mut job = egui::text::LayoutJob::simple_singleline(title.to_string(), font, p.text);
        job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
        let g = painter.layout_job(job);
        painter.galley(egui::pos2(rect.left() + 10.0, rect.top() + 19.0 - g.size().y / 2.0), g, p.text);
        let mut job = egui::text::LayoutJob::simple_singleline(detail.to_string(), FontId::proportional(11.5), p.text_faint);
        job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
        painter.galley(egui::pos2(rect.left() + 10.0, rect.top() + 32.0), painter.layout_job(job), p.text_faint);
    }
    if trash.clicked() {
        Some(HistoryClick::Delete)
    } else if resp.clicked() {
        Some(HistoryClick::Open)
    } else {
        None
    }
}

/// An icon in a `size` square (as the design's SVG icons take their size).
fn icon_box(ui: &mut Ui, glyph: &str, size: f32, color: Color32) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().text(r.center(), Align2::CENTER_CENTER, glyph, style::icon_font(size), color);
}

/// The rows a query of the assistant returned, as a small bordered table (numbers at the right).
fn result_table(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, columns: &[String], rows: &[Vec<String>]) {
    let p = ternarygrid_ui::pal(ui);
    let numeric: Vec<bool> = (0..columns.len()).map(|i| !rows.is_empty() && rows.iter().all(|r| r.get(i).is_some_and(|v| looks_numeric(v)))).collect();
    // The design's table: a 32 point header and rows of 26, each with its hairline.
    let (head_h, row_h) = (33.0, 27.0);
    // The design stripes the second, fourth… row; egui the first, third…: the frame shows the
    // stripe through the rows egui leaves bare, and egui's stripes are the surface.
    parts::bordered(ui).fill(p.stripe).show(ui, |ui| {
        let r = ui.max_rect();
        let band = Rect::from_min_size(r.min, vec2(r.width(), head_h));
        ui.painter().rect_filled(band, CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, p.header);
        ui.painter().hline(band.x_range(), band.bottom() - 0.5, Stroke::new(1.0, p.border));
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.visuals_mut().faint_bg_color = p.surface;
        ui.visuals_mut().widgets.noninteractive.bg_stroke = Stroke::NONE;
        egui::ScrollArea::horizontal().id_salt(("ai_rows_h", &id)).auto_shrink([false, true]).show(ui, |ui| {
            let room = ui.available_width();
            let mut tb = egui_extras::TableBuilder::new(ui)
                .id_salt(("ai_rows", &id))
                .striped(true)
                .auto_shrink([false, true])
                .max_scroll_height(220.0)
                .vscroll(rows.len() > 8)
                .cell_layout(Layout::left_to_right(Align::Center));
            // The first column 1.4 shares, numbers in the middle 0.6, the others 1 (as designed).
            let weights: Vec<f32> = (0..columns.len())
                .map(|i| {
                    if i == 0 {
                        1.4
                    } else if numeric[i] && i + 1 < columns.len() {
                        0.6
                    } else {
                        1.0
                    }
                })
                .collect();
            let total: f32 = weights.iter().sum();
            for (i, w) in weights.iter().enumerate() {
                tb = if i + 1 == weights.len() {
                    tb.column(egui_extras::Column::remainder().at_least(60.0).clip(true))
                } else {
                    tb.column(egui_extras::Column::exact((room * w / total).floor().max(60.0)).clip(true))
                };
            }
            let cell = |ui: &mut Ui, text: RichText, right: bool| {
                let layout = if right { Layout::right_to_left(Align::Center) } else { Layout::left_to_right(Align::Center) };
                ui.with_layout(layout, |ui| {
                    ui.add_space(10.0);
                    ui.add(egui::Label::new(text).truncate());
                    ui.add_space(10.0);
                });
            };
            tb.header(head_h, |mut h| {
                for (i, c) in columns.iter().enumerate() {
                    h.col(|ui| cell(ui, RichText::new(c).font(style::semibold(ui.ctx(), 12.0)).color(p.text_muted), numeric[i]));
                }
            })
            .body(|body| {
                body.rows(row_h, rows.len(), |mut row| {
                    let Some(values) = rows.get(row.index()) else { return };
                    for (i, _) in columns.iter().enumerate() {
                        let v = values.get(i).map(String::as_str).unwrap_or("");
                        row.col(|ui| {
                            let r = ui.max_rect();
                            ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, p.line));
                            cell(ui, RichText::new(v).size(13.0).color(p.text), numeric[i]);
                        });
                    }
                });
            });
        });
    });
}

/// A value shown at the right of its column: a number, with grouping commas or a decimal point.
fn looks_numeric(v: &str) -> bool {
    let t = v.trim();
    !t.is_empty() && t.chars().any(|c| c.is_ascii_digit()) && t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+' | 'e' | 'E'))
}

impl TabView for AiAssistantTab {
    fn title(&self) -> String {
        tr("AI assistant").to_string()
    }

    fn icon(&self) -> &'static str {
        icon::SPARKLE
    }

    fn connection_id(&self) -> Option<&str> {
        self.conn.as_deref()
    }

    fn dedupe_key(&self) -> Option<String> {
        self.dedupe.then(|| "ai-assistant".to_string())
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if self.poll() || self.unsaved {
            self.unsaved = false;
            self.save(cx);
        }
        self.sync_language(cx);
        let status = self.status(cx);
        ui.spacing_mut().item_spacing.y = 0.0;
        self.header(ui, cx);
        if status != AiStatus::Ready {
            Self::setup_ui(ui, cx, &status);
            return;
        }
        self.maybe_auto_send(cx);
        let p = ternarygrid_ui::pal(ui);
        egui::Panel::bottom(egui::Id::new(("ai_composer", self.uid)))
            .resizable(true)
            .default_size(200.0)
            .size_range(110.0..=600.0)
            .frame(egui::Frame::new().fill(p.canvas))
            .show(ui, |ui| self.composer_ui(ui, cx));
        egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| self.conversation_ui(ui, cx));
        if self.pending.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    fn on_close(&mut self, _cx: &mut AppContext) -> bool {
        self.stop();
        true
    }

    crate::impl_tab_any!();
}

#[cfg(test)]
mod look_tests {
    use super::grouped;

    #[test]
    fn token_counts_are_grouped() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(318), "318");
        assert_eq!(grouped(2104), "2,104");
        assert_eq!(grouped(1_234_567u64), "1,234,567");
    }
}
