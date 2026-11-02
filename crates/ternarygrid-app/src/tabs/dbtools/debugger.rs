//! Routine debugger (PostgreSQL `pldbgapi`): source view with line numbers
//! and breakpoints in the gutter, argument inputs, step into / step over /
//! resume / abort, the current line, variables (editable), the call stack
//! and the result once the routine finished.

use super::{SessionState, ToolSession};
use crate::backend::{Job, Loadable};
use crate::cx::AppContext;
use crate::tabs::TabView;
use crate::tabs::dbtools::look;
use egui::{Align2, RichText, Ui};
use egui_phosphor::regular as icon;
use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;
use ternarygrid_core::admin::{DebugSession, DebugState, DebugVariable, StackFrame, TabularData};
use ternarygrid_core::ddl::RoutineDef;
use ternarygrid_core::error::ErrorKind;
use ternarygrid_core::meta::{ObjectKind, ObjectRef};
use ternarygrid_core::{DbResult, Dialect, tr, trf};
use ternarygrid_editor::TokenStyle;
use ternarygrid_ui::{kit, parts};

type Shared = Arc<tokio::sync::Mutex<Box<dyn DebugSession>>>;

/// Messages kept in the messages list.
const MAX_MESSAGES: usize = 2000;

/// One argument of the routine and the value entered for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineArg {
    /// `IN`, `OUT`, `INOUT` or `VARIADIC`.
    pub mode: String,
    pub name: String,
    pub data_type: String,
    pub value: String,
    pub null: bool,
    /// The value is an SQL expression, passed as it is.
    pub expression: bool,
}

impl RoutineArg {
    /// SQL text passed for this argument.
    pub fn literal(&self, dialect: &dyn Dialect) -> String {
        let typed = |v: String| if self.data_type.is_empty() { v } else { format!("CAST({v} AS {})", self.data_type) };
        let v = if self.null {
            typed("NULL".to_string())
        } else if self.expression {
            self.value.trim().to_string()
        } else {
            typed(dialect.string_literal(&self.value))
        };
        if self.mode.eq_ignore_ascii_case("VARIADIC") { format!("VARIADIC {v}") } else { v }
    }

    fn is_out(&self) -> bool {
        self.mode.eq_ignore_ascii_case("OUT")
    }
}

/// First words of multi-word type names (an argument without a name).
const TYPE_STARTS: &[&str] = &["character", "double", "timestamp", "time", "bit", "interval", "national"];

fn parse_argument(part: &str) -> Option<RoutineArg> {
    let mut rest = part.trim();
    if rest.is_empty() {
        return None;
    }
    let mut mode = "IN".to_string();
    for m in ["INOUT", "IN", "OUT", "VARIADIC"] {
        if rest.get(..m.len()).is_some_and(|p| p.eq_ignore_ascii_case(m)) && rest[m.len()..].starts_with(char::is_whitespace) {
            mode = m.to_string();
            rest = rest[m.len()..].trim_start();
            break;
        }
    }
    let (name, data_type) = if let Some(tail) = rest.strip_prefix('"') {
        match tail.find('"') {
            Some(end) => (rest[..end + 2].to_string(), rest[end + 2..].trim().to_string()),
            None => (String::new(), rest.to_string()),
        }
    } else {
        match rest.split_once(char::is_whitespace) {
            Some((first, tail)) if !TYPE_STARTS.contains(&first.to_ascii_lowercase().as_str()) => (first.to_string(), tail.trim().to_string()),
            _ => (String::new(), rest.to_string()),
        }
    };
    Some(RoutineArg { mode, name, data_type, value: String::new(), null: false, expression: false })
}

/// Parse an argument list such as `a integer, OUT b text, character varying`.
pub fn parse_arguments(list: &str) -> Vec<RoutineArg> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let (mut depth, mut quoted) = (0i32, false);
    for c in list.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            '(' | '[' if !quoted => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' if !quoted => {
                depth -= 1;
                cur.push(c);
            }
            ',' if !quoted && depth <= 0 => parts.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    parts.push(cur);
    parts.iter().filter_map(|p| parse_argument(p)).collect()
}

/// Body of a `CREATE FUNCTION ... AS $tag$ body $tag$` definition: the text
/// whose lines the debugger reports.
pub fn routine_body(definition: &str) -> Option<String> {
    let upper = definition.to_ascii_uppercase();
    let mut from = 0;
    while let Some(pos) = upper[from..].find("AS $") {
        let dollar = from + pos + 3;
        let rest = &definition[dollar + 1..];
        let tag_len = rest.find('$')?;
        let tag = &rest[..tag_len];
        if tag.chars().all(|c| c.is_alphanumeric() || c == '_') {
            let delim = format!("${tag}$");
            let body_start = dollar + delim.len();
            if let Some(end) = definition[body_start..].find(&delim) {
                return Some(definition[body_start..body_start + end].to_string());
            }
        }
        from = dollar + 1;
    }
    None
}

/// State after a debugger command, with the frame data.
struct Step {
    state: DebugState,
    variables: Vec<DebugVariable>,
    stack: Vec<StackFrame>,
    notes: Vec<String>,
}

async fn snapshot(d: &mut Box<dyn DebugSession>, state: DebugState) -> Step {
    let mut notes = Vec::new();
    if state.finished {
        // release the proxy and target connections
        if let Err(e) = d.abort().await {
            notes.push(e.full_text());
        }
        return Step { state, variables: Vec::new(), stack: Vec::new(), notes };
    }
    let variables = d.variables().await.unwrap_or_else(|e| {
        notes.push(e.full_text());
        Vec::new()
    });
    let stack = d.stack().await.unwrap_or_else(|e| {
        notes.push(e.full_text());
        Vec::new()
    });
    Step { state, variables, stack, notes }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    StepInto,
    StepOver,
    Resume,
}

enum Side {
    Breakpoint { line: u32, set: bool, error: Option<String> },
    Variable { name: String, result: Result<Vec<DebugVariable>, String> },
    Aborted(Option<String>),
}

/// Debugger of one routine.
pub struct DebuggerTab {
    conn: String,
    routine: ObjectRef,
    session: ToolSession,
    def: Loadable<RoutineDef>,
    args: Vec<RoutineArg>,
    source: String,
    /// The displayed source has the debugger's line numbering.
    lines_ok: bool,
    max_cols: usize,
    breakpoints: BTreeSet<u32>,
    dbg: Option<Shared>,
    start_job: Option<Job<DbResult<(Shared, String, Step)>>>,
    step_job: Option<Job<DbResult<Step>>>,
    side_jobs: Vec<Job<Side>>,
    pending_start: bool,
    state: Option<DebugState>,
    variables: Vec<DebugVariable>,
    stack: Vec<StackFrame>,
    messages: Vec<String>,
    result: Option<TabularData>,
    error: Option<String>,
    unsupported: Option<String>,
    editing: Option<(String, String)>,
    scroll_to: Option<u32>,
    visible: Range<usize>,
    runtime: Option<tokio::runtime::Handle>,
    /// Byte offset of each line of `source`.
    line_starts: Vec<usize>,
    /// Highlighting of `source` (the connection's dialect).
    tokens: Vec<(Range<usize>, TokenStyle)>,
    /// Length of the source `tokens` were made for.
    tokens_for: Option<usize>,
}

impl DebuggerTab {
    pub fn new(conn: String, routine: ObjectRef) -> Self {
        let database = routine.database.clone();
        DebuggerTab {
            conn,
            routine,
            session: ToolSession::new(database),
            def: Loadable::NotLoaded,
            args: Vec::new(),
            source: String::new(),
            lines_ok: false,
            max_cols: 0,
            breakpoints: BTreeSet::new(),
            dbg: None,
            start_job: None,
            step_job: None,
            side_jobs: Vec::new(),
            pending_start: false,
            state: None,
            variables: Vec::new(),
            stack: Vec::new(),
            messages: Vec::new(),
            result: None,
            error: None,
            unsupported: None,
            editing: None,
            scroll_to: None,
            visible: 0..0,
            runtime: None,
            line_starts: Vec::new(),
            tokens: Vec::new(),
            tokens_for: None,
        }
    }

    /// The driver cannot debug (message for the user).
    pub fn unsupported_message(&self) -> Option<&str> {
        self.unsupported.as_deref()
    }

    /// Error of the last command.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The routine definition is loaded (or failed to load).
    pub fn definition_loaded(&self) -> bool {
        matches!(self.def, Loadable::Loaded(_) | Loadable::Failed(_))
    }

    /// Arguments of the routine (values can be edited).
    pub fn arguments_mut(&mut self) -> &mut Vec<RoutineArg> {
        &mut self.args
    }

    /// A start or a step is in progress.
    pub fn is_busy(&self) -> bool {
        self.pending_start || self.start_job.is_some() || self.step_job.is_some()
    }

    /// A debugging session is open (stopped or running).
    pub fn is_active(&self) -> bool {
        self.dbg.is_some()
    }

    /// Line the routine is stopped at.
    pub fn current_line(&self) -> Option<u32> {
        self.state.as_ref().filter(|s| !s.finished).and_then(|s| s.line)
    }

    fn set_source(&mut self, source: String, lines_ok: bool) {
        self.max_cols = source.lines().map(|l| l.chars().count() + l.matches('\t').count() * 3).max().unwrap_or(0);
        self.line_starts = std::iter::once(0).chain(source.match_indices('\n').map(|(i, _)| i + 1)).collect();
        self.source = source;
        self.lines_ok = lines_ok;
        self.tokens.clear();
        self.tokens_for = None;
    }

    fn push_messages(&mut self, msgs: impl IntoIterator<Item = String>) {
        self.messages.extend(msgs);
        if self.messages.len() > MAX_MESSAGES {
            let cut = self.messages.len() - MAX_MESSAGES;
            self.messages.drain(..cut);
        }
    }

    fn apply_step(&mut self, step: Step) {
        let Step { state, variables, stack, notes } = step;
        self.push_messages(state.messages.iter().cloned());
        self.push_messages(notes);
        if state.finished {
            self.result = state.result.clone();
            self.dbg = None;
            self.variables.clear();
            self.stack.clear();
            self.push_messages([tr("The routine finished.").to_string()]);
        } else {
            self.variables = variables;
            self.stack = stack;
            if let Some(l) = state.line {
                let idx = l.saturating_sub(1) as usize;
                if !(self.visible.start + 1..self.visible.end.saturating_sub(1)).contains(&idx) {
                    self.scroll_to = Some(l);
                }
            }
        }
        self.state = Some(state);
    }

    fn load_definition(&mut self, cx: &mut AppContext) {
        if !self.def.is_not_loaded() {
            return;
        }
        let SessionState::Ready(s) = self.session.state(cx, &self.conn, "debugger") else { return };
        let routine = self.routine.clone();
        self.def = Loadable::Loading(cx.backend.spawn(async move { s.lock().await.routine_definition(&routine).await }));
    }

    fn poll(&mut self, cx: &mut AppContext) {
        self.runtime.get_or_insert_with(|| cx.backend.handle());
        if self.def.poll() {
            if let Some(d) = self.def.get().cloned() {
                let is_function = d.kind.unwrap_or(self.routine.kind) != ObjectKind::Procedure;
                let mut args = parse_arguments(&d.arguments);
                if is_function {
                    args.retain(|a| !a.is_out());
                }
                for a in &mut args {
                    a.null = a.is_out();
                }
                self.args = args;
                match routine_body(&d.definition) {
                    Some(body) => self.set_source(body, true),
                    None => self.set_source(d.definition, false),
                }
            } else if let Some(e) = self.def.error().filter(|e| e.kind == ErrorKind::Unsupported) {
                self.unsupported = Some(e.message.clone());
            }
        }
        if self.pending_start {
            self.try_start(cx);
        }
        if let Some(r) = self.start_job.as_mut().and_then(|j| j.poll()) {
            self.start_job = None;
            match r {
                Ok((dbg, source, step)) => {
                    self.set_source(source, true);
                    if !step.state.finished {
                        self.dbg = Some(dbg);
                    }
                    self.apply_step(step);
                }
                Err(e) if e.kind == ErrorKind::Unsupported => self.unsupported = Some(e.message),
                Err(e) => self.error = Some(e.full_text()),
            }
        }
        if let Some(r) = self.step_job.as_mut().and_then(|j| j.poll()) {
            self.step_job = None;
            match r {
                Ok(step) => self.apply_step(step),
                Err(e) => self.error = Some(e.full_text()),
            }
        }
        let mut done = Vec::new();
        self.side_jobs.retain_mut(|j| match j.poll() {
            Some(r) => {
                done.push(r);
                false
            }
            None => true,
        });
        for r in done {
            match r {
                Side::Breakpoint { line, set, error: Some(e) } => {
                    // revert the optimistic toggle
                    if set {
                        self.breakpoints.remove(&line);
                    } else {
                        self.breakpoints.insert(line);
                    }
                    self.error = Some(e);
                }
                Side::Breakpoint { .. } => {}
                Side::Variable { result: Ok(vars), .. } => self.variables = vars,
                Side::Variable { name, result: Err(e) } => self.error = Some(format!("{name}: {e}")),
                Side::Aborted(e) => {
                    self.dbg = None;
                    if let Some(e) = e {
                        self.push_messages([e]);
                    }
                    self.push_messages([tr("Debugging aborted.").to_string()]);
                    self.state = Some(DebugState { line: None, finished: true, result: None, messages: Vec::new() });
                    self.variables.clear();
                    self.stack.clear();
                }
            }
        }
    }

    /// Start debugging with the entered arguments (once the session is open).
    pub fn start(&mut self, cx: &mut AppContext) {
        if self.is_busy() || self.dbg.is_some() {
            return;
        }
        self.error = None;
        self.unsupported = None;
        self.pending_start = true;
        self.try_start(cx);
    }

    fn try_start(&mut self, cx: &mut AppContext) {
        let session = match self.session.state(cx, &self.conn, "debugger") {
            SessionState::Ready(s) => s,
            SessionState::Opening => return,
            SessionState::Closed => {
                self.pending_start = false;
                self.error = Some(tr("The connection is closed.").to_string());
                return;
            }
            SessionState::Failed(e) => {
                self.pending_start = false;
                self.error = Some(e);
                return;
            }
        };
        self.pending_start = false;
        if let Some(live) = cx.conns.live(&self.conn)
            && !live.driver.info().capabilities.debugger
        {
            self.unsupported = Some(tr("The debugger is not available for this database.").to_string());
            return;
        }
        let dialect = session.dialect();
        let args: Vec<String> = self.args.iter().map(|a| a.literal(dialect.as_ref())).collect();
        let breakpoints: Vec<u32> = if self.lines_ok { self.breakpoints.iter().copied().collect() } else { Vec::new() };
        let routine = self.routine.clone();
        self.state = None;
        self.result = None;
        self.variables.clear();
        self.stack.clear();
        self.messages.clear();
        self.push_messages([trf("Starting {} with ({})", &[&routine.display_path(), &args.join(", ")])]);
        self.start_job = Some(cx.backend.spawn(async move {
            let mut dbg = {
                let mut s = session.lock().await;
                s.debug_routine(&routine, &args).await?
            };
            let source = dbg.source().to_string();
            let state = dbg.wait().await?;
            let mut notes = Vec::new();
            if !state.finished {
                for line in breakpoints {
                    if let Err(e) = dbg.set_breakpoint(line).await {
                        notes.push(trf("Breakpoint at line {}: {}", &[&line, &e.message]));
                    }
                }
            }
            let mut step = snapshot(&mut dbg, state).await;
            notes.append(&mut step.notes);
            step.notes = notes;
            let shared: Shared = Arc::new(tokio::sync::Mutex::new(dbg));
            Ok((shared, source, step))
        }));
    }

    fn command(&mut self, cx: &AppContext, cmd: Command) {
        let Some(dbg) = self.dbg.clone() else { return };
        if self.is_busy() {
            return;
        }
        self.error = None;
        self.editing = None;
        if let Some(s) = self.state.as_mut() {
            // running until the next stop
            s.line = None;
        }
        self.step_job = Some(cx.backend.spawn(async move {
            let mut d = dbg.lock().await;
            let state = match cmd {
                Command::StepInto => d.step_into().await?,
                Command::StepOver => d.step_over().await?,
                Command::Resume => d.resume().await?,
            };
            Ok(snapshot(&mut d, state).await)
        }));
    }

    /// Abort the debugging session.
    pub fn abort(&mut self, cx: &AppContext) {
        self.pending_start = false;
        let Some(dbg) = self.dbg.clone() else { return };
        if let Some(j) = self.step_job.as_mut() {
            // releases the session lock held by a running step
            j.abort();
        }
        self.step_job = None;
        self.side_jobs.push(cx.backend.spawn(async move {
            let mut d = dbg.lock().await;
            Side::Aborted(d.abort().await.err().map(|e| e.full_text()))
        }));
    }

    fn toggle_breakpoint(&mut self, cx: &AppContext, line: u32) {
        let set = !self.breakpoints.contains(&line);
        if set {
            self.breakpoints.insert(line);
        } else {
            self.breakpoints.remove(&line);
        }
        if let Some(dbg) = self.dbg.clone() {
            self.side_jobs.push(cx.backend.spawn(async move {
                let mut d = dbg.lock().await;
                let r = if set { d.set_breakpoint(line).await } else { d.clear_breakpoint(line).await };
                Side::Breakpoint { line, set, error: r.err().map(|e| e.full_text()) }
            }));
        }
    }

    fn set_variable(&mut self, cx: &AppContext, name: String, value: String) {
        let Some(dbg) = self.dbg.clone() else { return };
        self.side_jobs.push(cx.backend.spawn(async move {
            let mut d = dbg.lock().await;
            let result = match d.set_variable(&name, &value).await {
                Ok(()) => d.variables().await.map_err(|e| e.full_text()),
                Err(e) => Err(e.full_text()),
            };
            Side::Variable { name, result }
        }));
    }

    fn stopped(&self) -> bool {
        self.dbg.is_some() && !self.is_busy() && self.state.as_ref().is_some_and(|s| !s.finished)
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let busy = self.is_busy();
        let stopped = self.stopped();
        let p = ternarygrid_ui::pal(ui);
        parts::toolbar(ui, |ui| {
            ui.label(RichText::new(crate::nav::kind_icon(self.routine.kind)).font(ternarygrid_ui::style::icon_font(15.0)).color(p.text_muted));
            ui.label(RichText::new(self.routine.display_path()).font(ternarygrid_ui::style::semibold(ui.ctx(), 13.0)).color(p.text));
            if let Loadable::Loaded(d) = &self.def
                && let Some(l) = &d.language
            {
                ui.label(RichText::new(l.as_str()).size(12.0).color(p.text_faint));
            }
            kit::vdivider(ui, 18.0);
            let can_start = self.dbg.is_none() && !busy && self.unsupported.is_none();
            let clicked = ui
                .add_enabled_ui(can_start, |ui| {
                    if self.state.is_some() {
                        look::small(ui, kit::Kind::Ghost, icon::ARROW_CLOCKWISE, tr("Restart"))
                    } else {
                        look::small(ui, kit::Kind::Primary, icon::PLAY, tr("Start"))
                    }
                })
                .inner
                .clicked();
            if clicked {
                self.start(cx);
            }
            ui.add_enabled_ui(stopped, |ui| {
                if kit::icon_button(ui, icon::ARROW_ELBOW_DOWN_RIGHT, &format!("{} (F11)", tr("Step into"))).clicked() {
                    self.command(cx, Command::StepInto);
                }
                if kit::icon_button(ui, icon::ARROW_ARC_RIGHT, &format!("{} (F10)", tr("Step over"))).clicked() {
                    self.command(cx, Command::StepOver);
                }
                if look::small(ui, kit::Kind::Secondary, icon::FAST_FORWARD, tr("Resume")).on_hover_text("F8").clicked() {
                    self.command(cx, Command::Resume);
                }
            });
            if ui.add_enabled_ui(self.dbg.is_some(), |ui| look::small(ui, kit::Kind::Danger, icon::STOP, tr("Abort"))).inner.clicked() {
                self.abort(cx);
            }
            kit::vdivider(ui, 18.0);
            if busy {
                parts::spinner_label(ui, if self.start_job.is_some() || self.pending_start { tr("Waiting for the routine…") } else { tr("Running…") });
                ui.ctx().request_repaint_after(Duration::from_millis(150));
            } else if let Some(s) = &self.state {
                if s.finished {
                    ui.label(RichText::new(tr("Finished")).color(p.text_muted));
                } else if let Some(l) = s.line {
                    look::tone(ui, &trf("Stopped at line {}", &[&l]), p.warn, true);
                }
            }
        });
        if cx.tab_active && !ui.ctx().text_edit_focused() && stopped {
            let pressed = |k: egui::Key| ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k));
            if pressed(egui::Key::F11) {
                self.command(cx, Command::StepInto);
            } else if pressed(egui::Key::F10) {
                self.command(cx, Command::StepOver);
            } else if pressed(egui::Key::F8) {
                self.command(cx, Command::Resume);
            }
        }
    }

    fn arguments_ui(&mut self, ui: &mut Ui) {
        look::title(ui, tr("Arguments"));
        if self.args.is_empty() {
            look::faint(ui, if self.definition_loaded() { tr("The routine has no arguments.") } else { tr("Loading…") });
            return;
        }
        let editable = self.dbg.is_none() && !self.is_busy();
        let p = ternarygrid_ui::pal(ui);
        let id = ("dbg_args", self.conn.clone(), self.routine.name.clone());
        let args = &mut self.args;
        ui.add_enabled_ui(editable, |ui| {
            let n = args.len();
            look::widget_table(ui, id, &[tr("Name"), tr("Value"), "NULL", tr("Expr.")], &[92.0, 104.0, 48.0, 48.0], n, 32.0, |ui, r, c| {
                let a = &mut args[r];
                match c {
                    0 => {
                        let mode = if a.mode == "IN" { String::new() } else { format!("{} ", a.mode) };
                        let name = if a.name.is_empty() { format!("${}", r + 1) } else { a.name.clone() };
                        ui.add(egui::Label::new(RichText::new(format!("{mode}{name}")).font(look::mono(12.0)).color(p.text)).truncate()).on_hover_text(a.data_type.as_str());
                    }
                    1 => {
                        let w = (ui.available_width() - 8.0).max(40.0);
                        ui.add_enabled(!a.null, egui::TextEdit::singleline(&mut a.value).desired_width(w).font(look::mono(12.0)).hint_text(a.data_type.as_str()));
                    }
                    2 => {
                        look::checkbox(ui, &mut a.null, "").on_hover_text("NULL");
                    }
                    _ => {
                        look::checkbox(ui, &mut a.expression, "").on_hover_text(tr("Use the value as an SQL expression instead of a literal"));
                    }
                }
            });
        });
    }

    /// The highlighted text of line `i` (`start..end` in the source).
    fn line_job(&self, i: usize, theme: &ternarygrid_editor::EditorTheme, font: &egui::FontId) -> egui::text::LayoutJob {
        let start = self.line_starts.get(i).copied().unwrap_or(0);
        let end = self.line_starts.get(i + 1).map(|e| e.saturating_sub(1)).unwrap_or(self.source.len()).max(start);
        let line = &self.source[start..end];
        let mut job = egui::text::LayoutJob::default();
        let fmt = |style| egui::TextFormat { font_id: font.clone(), color: theme.token_color(style), ..Default::default() };
        let mut pos = start;
        let push = |job: &mut egui::text::LayoutJob, from: usize, to: usize, style| {
            if to > from {
                job.append(&self.source[from..to].replace('\t', "    "), 0.0, fmt(style));
            }
        };
        for (r, style) in self.tokens.iter().filter(|(r, _)| r.end > start && r.start < end) {
            let (a, b) = (r.start.max(start), r.end.min(end));
            if a < pos || !self.source.is_char_boundary(a) || !self.source.is_char_boundary(b) {
                continue;
            }
            push(&mut job, pos, a, TokenStyle::Plain);
            push(&mut job, a, b, *style);
            pos = b;
        }
        push(&mut job, pos, end.max(pos), TokenStyle::Plain);
        if line.is_empty() {
            job.append(" ", 0.0, fmt(TokenStyle::Plain));
        }
        job
    }

    /// Source with line numbers; returns the line whose gutter was clicked.
    fn source_ui(&mut self, ui: &mut Ui) -> Option<u32> {
        if self.source.is_empty() {
            if !self.definition_loaded() {
                egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| parts::spinner_label(ui, tr("Loading the routine…")));
            }
            return None;
        }
        let p = ternarygrid_ui::pal(ui);
        if !self.lines_ok {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 8)).show(ui, |ui| look::faint(ui, tr("Breakpoints can be set once the debugging session has started.")));
        }
        let font = look::mono(13.0);
        let theme = ternarygrid_editor::EditorTheme::from_visuals(ui.visuals());
        let row_h = 21.0;
        let char_w = ui.ctx().fonts_mut(|f| f.glyph_width(&font, '0')).max(1.0);
        let n = self.line_starts.len().max(1);
        let digits = n.to_string().len().max(2);
        let number_w = (digits as f32 * char_w).max(18.0) + 14.0;
        let gutter_w = 18.0 + 18.0 + number_w;
        let width = gutter_w + self.max_cols as f32 * char_w + 24.0;
        let current = self.state.as_ref().filter(|s| !s.finished).and_then(|s| s.line);
        let dark = ui.visuals().dark_mode;
        let current_bg = if dark { egui::Color32::from_rgb(0x4b, 0x44, 0x3b) } else { egui::Color32::from_rgb(0xf3, 0xea, 0xd6) };
        let bp_color = egui::Color32::from_rgb(0xf2, 0x66, 0x5a);
        let can_toggle = self.lines_ok;
        let mut toggled = None;
        let mut visible = 0..0;
        let scroll_to = self.scroll_to.take();
        let jobs: Vec<egui::text::LayoutJob> = (0..n).map(|i| self.line_job(i, &theme, &font)).collect();
        let breakpoints = &self.breakpoints;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut area = egui::ScrollArea::both().id_salt(("dbg_source", &self.conn, &self.routine.name)).auto_shrink([false, false]);
            if let Some(l) = scroll_to {
                area = area.vertical_scroll_offset((l as f32 - 6.0).max(0.0) * row_h);
            }
            ui.add_space(10.0);
            area.show_rows(ui, row_h, n, |ui, range| {
                visible = range.clone();
                for i in range {
                    let line_no = i as u32 + 1;
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width.max(ui.available_width()), row_h), egui::Sense::click());
                    let painter = ui.painter();
                    if current == Some(line_no) {
                        painter.rect_filled(rect, 0.0, current_bg);
                    }
                    let cy = rect.center().y;
                    if breakpoints.contains(&line_no) {
                        painter.circle_filled(egui::pos2(rect.left() + 9.0, cy), 4.5, bp_color);
                    }
                    if current == Some(line_no) {
                        painter.text(egui::pos2(rect.left() + 27.0, cy), Align2::CENTER_CENTER, icon::ARROW_RIGHT, ternarygrid_ui::style::icon_font(13.0), p.warn);
                    }
                    painter.text(egui::pos2(rect.left() + gutter_w - 14.0, cy), Align2::RIGHT_CENTER, line_no.to_string(), font.clone(), p.text_faint);
                    let galley = ui.fonts_mut(|f| f.layout_job(jobs[i].clone()));
                    ui.painter().galley(egui::pos2(rect.left() + gutter_w, cy - galley.size().y / 2.0), galley, p.text);
                    let gutter = rect.left() + gutter_w;
                    let in_gutter = resp.hover_pos().is_some_and(|pos| pos.x <= gutter);
                    if in_gutter && can_toggle {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if can_toggle && resp.clicked() && resp.interact_pointer_pos().is_some_and(|pos| pos.x <= gutter) {
                        toggled = Some(line_no);
                    }
                }
            });
        });
        self.visible = visible;
        toggled
    }

    fn frame_ui(&mut self, ui: &mut Ui, cx: &AppContext) {
        let p = ternarygrid_ui::pal(ui);
        look::title(ui, tr("Variables"));
        let editable = self.stopped();
        let mut commit: Option<(String, String)> = None;
        let mut cancel_edit = false;
        let mut start_edit: Option<(String, String)> = None;
        if self.variables.is_empty() {
            look::faint(ui, if self.dbg.is_some() { tr("No variables in this frame.") } else { tr("Variables appear when the routine stops.") });
        } else {
            let vars = &self.variables;
            let editing = &mut self.editing;
            let id = ("dbg_vars", self.conn.clone());
            egui::ScrollArea::vertical().id_salt(("dbg_vars_scroll", &self.conn)).max_height(ui.available_height() * 0.6).show(ui, |ui| {
                look::widget_table(ui, id, &[tr("Name"), tr("Type"), tr("Value")], &[116.0, 72.0, 100.0], vars.len(), 30.0, |ui, r, c| {
                    let v = &vars[r];
                    match c {
                        0 => {
                            let name = if v.is_argument { format!("→ {}", v.name) } else { v.name.clone() };
                            ui.add(egui::Label::new(RichText::new(name).font(look::mono(12.0)).color(p.text)).truncate());
                        }
                        1 => {
                            ui.add(egui::Label::new(RichText::new(v.data_type.as_str()).size(12.0).color(p.text_faint)).truncate());
                        }
                        _ => match editing.as_mut() {
                            Some((n, text)) if *n == v.name => {
                                let r = ui.add(egui::TextEdit::singleline(text).desired_width(ui.available_width() - 8.0).font(look::mono(12.0)));
                                if r.lost_focus() {
                                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                        commit = Some((n.clone(), text.clone()));
                                    } else {
                                        cancel_edit = true;
                                    }
                                } else if !r.has_focus() {
                                    r.request_focus();
                                }
                            }
                            _ => {
                                let color = if v.value.eq_ignore_ascii_case("null") { p.text_faint } else { p.text };
                                let r = ui.add(egui::Label::new(RichText::new(&v.value).font(look::mono(12.0)).color(color)).sense(egui::Sense::click()).truncate());
                                let r = if editable { r.on_hover_text(tr("Double-click to change the value")) } else { r };
                                if editable && r.double_clicked() {
                                    start_edit = Some((v.name.clone(), v.value.clone()));
                                }
                            }
                        },
                    }
                });
            });
        }
        if let Some(e) = start_edit {
            self.editing = Some(e);
        }
        if let Some((name, value)) = commit {
            self.editing = None;
            self.set_variable(cx, name, value);
        } else if cancel_edit {
            self.editing = None;
        }
        ui.add_space(4.0);
        look::title(ui, tr("Call stack"));
        egui::ScrollArea::vertical().id_salt(("dbg_stack", &self.conn)).show(ui, |ui| {
            if self.stack.is_empty() {
                ui.label(RichText::new("—").color(p.text_faint));
            }
            for f in &self.stack {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("#{}", f.level)).size(12.0).color(p.text_faint));
                    ui.label(RichText::new(&f.routine).font(look::mono(12.0)).color(p.text));
                    ui.label(RichText::new(trf("line {}", &[&f.line])).size(12.0).color(p.text_faint));
                });
                if !f.args.is_empty() {
                    ui.indent(("dbg_frame", f.level), |ui| {
                        ui.add(egui::Label::new(RichText::new(&f.args).size(11.5).color(p.text_muted)).wrap());
                    });
                }
            }
        });
    }

    fn output_ui(&mut self, ui: &mut Ui, cx: &AppContext) {
        let p = ternarygrid_ui::pal(ui);
        let rect = ui.max_rect();
        ui.painter().vline(rect.center().x, rect.y_range(), egui::Stroke::new(1.0, p.border));
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(RichText::new(tr("Messages")).font(ternarygrid_ui::style::semibold(ui.ctx(), 12.5)).color(p.text));
            });
            egui::ScrollArea::both().id_salt(("dbg_msgs", &self.conn)).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for m in &self.messages {
                    let color = if m.starts_with("ERROR") {
                        p.err
                    } else if m.starts_with("WARNING") {
                        p.warn
                    } else {
                        p.text_muted
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.add(egui::Label::new(RichText::new(m).font(look::mono(11.5)).color(color)).extend());
                    });
                }
            });
            let ui = &mut cols[1];
            ui.add_space(10.0);
            egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 0, bottom: 8 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(RichText::new(tr("Result")).font(ternarygrid_ui::style::semibold(ui.ctx(), 12.5)).color(p.text));
                match &self.result {
                    Some(data) if !data.columns.is_empty() => {
                        let fmt = cx.settings.results.format.clone();
                        let headers = look::headers(&data.columns.iter().map(String::as_str).collect::<Vec<_>>());
                        let rows: Vec<Vec<look::Cell>> = data.rows.iter().map(|r| r.iter().map(|v| look::Cell::new(v.display(&fmt).into_owned())).collect()).collect();
                        look::table(ui, ("dbg_result", &self.conn), &headers, &rows, None);
                    }
                    _ => {
                        look::faint(ui, tr("The result appears when the routine finishes."));
                    }
                }
            });
        });
    }

    /// Show a debugging session stopped at `line` (for screenshots): the definition, the
    /// breakpoints, the variables, the call stack and the messages, without a server.
    #[doc(hidden)]
    pub fn show_sample(&mut self, def: RoutineDef, line: u32, breakpoints: &[u32], variables: Vec<DebugVariable>, stack: Vec<StackFrame>, messages: Vec<String>) {
        let mut args = parse_arguments(&def.arguments);
        args.retain(|a| !a.is_out());
        for a in &mut args {
            a.value = "10423".into();
        }
        self.args = args;
        let body = routine_body(&def.definition);
        match body {
            Some(b) => self.set_source(b, true),
            None => self.set_source(def.definition.clone(), true),
        }
        self.def = Loadable::Loaded(def);
        self.unsupported = None;
        self.error = None;
        self.breakpoints = breakpoints.iter().copied().collect();
        self.state = Some(DebugState { line: Some(line), finished: false, result: None, messages: Vec::new() });
        self.variables = variables;
        self.stack = stack;
        self.messages = messages;
    }
}

impl TabView for DebuggerTab {
    fn title(&self) -> String {
        trf("Debug {}", &[&self.routine.name])
    }
    fn icon(&self) -> &'static str {
        icon::BUG
    }
    fn connection_id(&self) -> Option<&str> {
        Some(&self.conn)
    }
    fn dedupe_key(&self) -> Option<String> {
        Some(format!("debug:{}:{}", self.conn, self.routine.display_path()))
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        self.poll(cx);
        let conn = self.conn.clone();
        let opened = egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 0)).show(ui, |ui| self.session.ui(ui, cx, &conn, "debugger").is_some()).inner;
        if !opened && self.dbg.is_none() && self.def.is_not_loaded() {
            return;
        }
        self.load_definition(cx);
        if self.tokens_for != Some(self.source.len()) {
            let dialect = cx.conns.live(&self.conn).map(|l| l.dialect.clone());
            if let Some(d) = dialect {
                let lang = crate::sqllang::SqlLanguage::new(d);
                self.tokens = ternarygrid_editor::EditorLanguage::highlight(&lang, &self.source);
                self.tokens_for = Some(self.source.len());
            }
        }
        let p = ternarygrid_ui::pal(ui);
        ui.spacing_mut().item_spacing.y = 0.0;
        self.toolbar(ui, cx);
        if self.unsupported.is_some() || self.error.is_some() {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                if let Some(msg) = &self.unsupported {
                    look::note(ui, parts::Tone::Info, Some(tr("The routine cannot be debugged")), msg);
                }
                if let Some(e) = &self.error {
                    look::error(ui, e);
                }
            });
        }
        let base = egui::Id::new(("dbg", &self.conn, &self.routine.name));
        egui::Panel::bottom(base.with("out"))
            .resizable(true)
            .default_size(150.0)
            .size_range(80.0..=f32::INFINITY)
            .frame(egui::Frame::new().fill(p.canvas))
            .show(ui, |ui| self.output_ui(ui, cx));
        egui::Panel::right(base.with("frame")).resizable(true).default_size(320.0).frame(egui::Frame::new().fill(p.canvas).inner_margin(egui::Margin::symmetric(14, 12))).show(
            ui,
            |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                self.arguments_ui(ui);
                ui.add_space(4.0);
                self.frame_ui(ui, cx);
            },
        );
        let toggled = egui::CentralPanel::default().frame(egui::Frame::new().fill(p.surface)).show(ui, |ui| self.source_ui(ui)).inner;
        if let Some(line) = toggled {
            self.toggle_breakpoint(cx, line);
        }
        if !self.side_jobs.is_empty() || self.def.is_loading() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    crate::impl_tab_any!();
}

impl Drop for DebuggerTab {
    fn drop(&mut self) {
        if let Some(j) = self.step_job.as_mut() {
            j.abort();
        }
        if let (Some(dbg), Some(rt)) = (self.dbg.take(), self.runtime.as_ref()) {
            rt.spawn(async move {
                let mut d = dbg.lock().await;
                let _ = d.abort().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_parsed() {
        let a = parse_arguments("a integer, OUT b text, INOUT \"My Arg\" numeric(10,2), character varying, VARIADIC vals integer[]");
        assert_eq!(a.len(), 5);
        assert_eq!((a[0].mode.as_str(), a[0].name.as_str(), a[0].data_type.as_str()), ("IN", "a", "integer"));
        assert_eq!((a[1].mode.as_str(), a[1].name.as_str()), ("OUT", "b"));
        assert_eq!((a[2].mode.as_str(), a[2].name.as_str(), a[2].data_type.as_str()), ("INOUT", "\"My Arg\"", "numeric(10,2)"));
        assert_eq!((a[3].name.as_str(), a[3].data_type.as_str()), ("", "character varying"));
        assert_eq!(a[4].mode, "VARIADIC");
        assert!(parse_arguments("").is_empty());
        let d = ternarygrid_core::dialect::GenericDialect::new();
        let mut x = a[0].clone();
        x.value = "4'2".into();
        assert_eq!(x.literal(&d), "CAST('4''2' AS integer)");
        x.null = true;
        assert_eq!(x.literal(&d), "CAST(NULL AS integer)");
        x.null = false;
        x.expression = true;
        x.value = " 6 * 7 ".into();
        assert_eq!(x.literal(&d), "6 * 7");
    }

    #[test]
    fn body_is_extracted() {
        let def = "CREATE OR REPLACE FUNCTION public.f(a integer DEFAULT 1)\n RETURNS integer\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n  RETURN a + 1;\nEND;\n$function$";
        assert_eq!(routine_body(def).as_deref(), Some("\nBEGIN\n  RETURN a + 1;\nEND;\n"));
        assert_eq!(routine_body("CREATE FUNCTION f() AS $$ select 1 $$").as_deref(), Some(" select 1 "));
        assert!(routine_body("CREATE FUNCTION f() RETURNS int AS 'select 1' LANGUAGE sql").is_none());
    }
}
