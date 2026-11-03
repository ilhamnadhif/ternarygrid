//! Task manager (FR-A3): saved tasks in folders with their schedules, next
//! and last runs; the task editor; manual runs and cancellation; export and
//! import of task definitions; the run history with live output; and the
//! option to register a task with the operating system scheduler so that it
//! also runs while TernaryGrid is closed.
//!
//! Store writes, run log reads and operating system commands run in the
//! background; scheduler events are forwarded to the UI as they happen.

mod draft;
mod editor;
pub(crate) mod look;
mod system;

pub use draft::{
    CompareDraft, IntervalUnit, KindDraft, NewTaskKind, ScheduleDraft, ScheduleMode, SqlScriptDraft, TaskDraft, kind_icon, kind_label, parse_local_time, weekday_names,
};
pub use system::{Synced, entry_preview, scheduler_name};

use crate::backend::Job;
use crate::command::AppCommand;
use crate::cx::AppContext;
use crate::tabs::TabView;
use crate::tabs::dbtools::spawn_blocking;
use chrono::{DateTime, Local};
use editor::{EditorAction, TaskEditor};
use egui::{Color32, RichText, Ui};
use egui_phosphor::regular as icon;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use ternarygrid_core::{DbResult, tr, trf, trn};
use ternarygrid_tasks::{MessageLevel, Progress, RunLog, RunStatus, RunningInfo, TaskDef, TaskEvent, TaskRun, Trigger};
use ternarygrid_ui::kit::{self, Kind};
use ternarygrid_ui::parts::Tone;
use tokio::sync::broadcast::error::TryRecvError;
use tokio_util::sync::CancellationToken;

/// Runs shown in the history of a task.
const HISTORY_LIMIT: usize = 200;
/// Reload the run log this often while tasks are registered with the
/// operating system scheduler (their runs happen in other processes).
const EXTERNAL_REFRESH: Duration = Duration::from_secs(60);

/// Localised label of a run status.
pub fn status_label(s: RunStatus) -> &'static str {
    match s {
        RunStatus::Running => tr("Running"),
        RunStatus::Succeeded => tr("Succeeded"),
        RunStatus::Failed => tr("Failed"),
        RunStatus::Cancelled => tr("Cancelled"),
        RunStatus::Skipped => tr("Skipped"),
    }
}

/// Localised label of what started a run.
pub fn trigger_label(t: Trigger) -> &'static str {
    match t {
        Trigger::Manual => tr("Manual"),
        Trigger::Schedule => tr("Scheduled"),
        Trigger::Cli => tr("Command line"),
        Trigger::Composite => tr("Composite step"),
    }
}

/// The colour of a run's status text.
fn status_color(s: RunStatus, p: &ternarygrid_ui::Palette) -> Color32 {
    match s {
        RunStatus::Running => p.accent_text,
        RunStatus::Succeeded => p.ok,
        RunStatus::Failed => p.err,
        RunStatus::Cancelled => p.warn,
        RunStatus::Skipped => p.text_faint,
    }
}

/// A time in the lists, relative to `now`: "Today 14:00", "Tomorrow 02:00", "Yesterday
/// 12:50", the weekday within a week ("Wed 09:00"), else the date ("2026-11-20 09:00").
fn when(t: DateTime<Local>, now: DateTime<Local>) -> String {
    use chrono::Datelike;
    let hm = t.format("%H:%M").to_string();
    match (t.date_naive() - now.date_naive()).num_days() {
        0 => trf("Today {}", &[&hm]),
        1 => trf("Tomorrow {}", &[&hm]),
        -1 => trf("Yesterday {}", &[&hm]),
        -6..=6 => format!("{} {hm}", weekday_names()[t.weekday().num_days_from_monday() as usize]),
        _ => t.format("%Y-%m-%d %H:%M").to_string(),
    }
}

/// The full time of a run (tooltips, the run history).
fn full_time(t: DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn run_duration(run: &TaskRun) -> String {
    let end = run.finished.unwrap_or_else(chrono::Utc::now);
    short_duration((end - run.started).to_std().unwrap_or_default())
}

/// A run's length as the task list shows it (as the query history's: "850 ms", "41 s",
/// "1m 12s", "2h 05m").
fn short_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0 => format!("{} ms", d.as_millis()),
        1..60 => format!("{s} s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

fn in_folder(t: &TaskDef, folder: &str) -> bool {
    t.folder.as_deref().is_some_and(|f| f == folder || f.starts_with(&format!("{folder}/")))
}

/// What the background run log reader found.
struct RunSnapshot {
    focus: Option<String>,
    latest: HashMap<String, TaskRun>,
    history: Vec<TaskRun>,
    children: HashMap<String, Vec<TaskRun>>,
}

fn load_runs(log: &RunLog, focus: Option<&str>) -> DbResult<RunSnapshot> {
    let runs = log.runs()?;
    let mut latest: HashMap<String, TaskRun> = HashMap::new();
    for r in &runs {
        if !latest.contains_key(&r.task_id) {
            let mut l = r.clone();
            l.messages.clear();
            latest.insert(r.task_id.clone(), l);
        }
    }
    let history: Vec<TaskRun> = match focus {
        Some(f) => runs.iter().filter(|r| r.task_id == f).take(HISTORY_LIMIT).cloned().collect(),
        None => Vec::new(),
    };
    let ids: HashSet<&str> = history.iter().map(|r| r.run_id.as_str()).collect();
    let mut children: HashMap<String, Vec<TaskRun>> = HashMap::new();
    for r in runs.iter().filter(|r| r.parent_run_id.as_deref().is_some_and(|p| ids.contains(p))) {
        children.entry(r.parent_run_id.clone().unwrap_or_default()).or_default().push(r.clone());
    }
    for v in children.values_mut() {
        v.sort_by_key(|a| a.started);
    }
    Ok(RunSnapshot { focus: focus.map(str::to_string), latest, history, children })
}

/// Result of a background store operation.
enum StoreDone {
    Saved { task: Box<TaskDef>, previous: Option<Box<TaskDef>> },
    Deleted(Vec<TaskDef>),
    Duplicated(Vec<TaskDef>),
    Toggled(Vec<TaskDef>),
    Imported(Vec<TaskDef>),
    Exported { path: PathBuf, count: usize },
    Moved(usize),
    Folder,
    Reloaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobKind {
    /// Saving from the editor: errors are shown in the editor.
    Editor,
    Other,
}

/// Result of a system scheduler change.
enum OsDone {
    Synced(TaskDef, Synced),
    /// Entry of a deleted task removed (`true`) or absent.
    Removed(String, bool),
    Failed {
        task: Option<TaskDef>,
        error: String,
    },
}

/// Folder name prompt (new folder or rename).
struct FolderPrompt {
    /// Folder being renamed.
    rename: Option<String>,
    /// Parent of the new folder.
    parent: Option<String>,
    name: String,
}

enum RowAction {
    Click(usize, String),
    Edit(String),
    Enable(Vec<String>, bool),
    Run(Vec<String>),
    Cancel(Vec<String>),
    Duplicate(Vec<String>),
    Delete(Vec<String>),
    Move(Vec<String>, Option<String>),
    Export(Vec<String>),
}

/// The task manager tab.
pub struct TasksTab {
    folder: Option<String>,
    collapsed: HashSet<String>,
    filter: String,
    tasks: Vec<TaskDef>,
    folders: Vec<String>,
    store_rx: Option<tokio::sync::watch::Receiver<u64>>,
    initialized: bool,
    selected: Vec<String>,
    focus: Option<String>,
    anchor: Option<usize>,
    events: Option<tokio::sync::broadcast::Receiver<TaskEvent>>,
    forwarder: Option<CancellationToken>,
    latest: HashMap<String, TaskRun>,
    history_for: Option<String>,
    history: Vec<TaskRun>,
    children: HashMap<String, Vec<TaskRun>>,
    progress: HashMap<String, Progress>,
    runs_job: Option<Job<DbResult<RunSnapshot>>>,
    runs_error: Option<String>,
    buffered: Vec<TaskEvent>,
    reload_runs: bool,
    last_runs_load: Option<Instant>,
    selected_run: Option<String>,
    jobs: Vec<(JobKind, Job<DbResult<StoreDone>>)>,
    run_jobs: Vec<(String, Job<DbResult<String>>)>,
    os_jobs: Vec<Job<OsDone>>,
    /// Runs started from this tab (a notification is shown when they end).
    my_runs: HashSet<String>,
    /// Recently finished runs (for runs that end before their start is confirmed).
    recent_finished: Vec<TaskRun>,
    editor: Option<TaskEditor>,
    confirm_delete: Option<Vec<String>>,
    folder_prompt: Option<FolderPrompt>,
}

impl Default for TasksTab {
    fn default() -> Self {
        Self::new()
    }
}

impl TasksTab {
    pub fn new() -> Self {
        TasksTab {
            folder: None,
            collapsed: HashSet::new(),
            filter: String::new(),
            tasks: Vec::new(),
            folders: Vec::new(),
            store_rx: None,
            initialized: false,
            selected: Vec::new(),
            focus: None,
            anchor: None,
            events: None,
            forwarder: None,
            latest: HashMap::new(),
            history_for: None,
            history: Vec::new(),
            children: HashMap::new(),
            progress: HashMap::new(),
            runs_job: None,
            runs_error: None,
            buffered: Vec::new(),
            reload_runs: true,
            last_runs_load: None,
            selected_run: None,
            jobs: Vec::new(),
            run_jobs: Vec::new(),
            os_jobs: Vec::new(),
            my_runs: HashSet::new(),
            recent_finished: Vec::new(),
            editor: None,
            confirm_delete: None,
            folder_prompt: None,
        }
    }

    // ------------------------------------------------------------ test API

    /// Ids of the tasks shown in the list.
    pub fn task_ids(&self) -> Vec<String> {
        self.visible().iter().map(|t| t.id.clone()).collect()
    }

    /// Select one task (it becomes the focused task of the history panel).
    pub fn select_task(&mut self, id: &str) {
        self.selected = vec![id.to_string()];
        self.focus = Some(id.to_string());
    }

    /// Add a task to the selection (as a ⌘-click; the history stays on the focused one).
    pub fn add_to_selection(&mut self, id: &str) {
        if !self.selected.iter().any(|s| s == id) {
            self.selected.push(id.to_string());
        }
    }

    /// The task whose history is shown.
    pub fn focused_task(&self) -> Option<&str> {
        self.focus.as_deref()
    }

    /// Open the editor for a new task.
    pub fn open_new_task(&mut self, cx: &AppContext, kind: NewTaskKind) {
        let conn = if kind.uses_connection() || kind == NewTaskKind::SchemaCompare { cx.conns.configs().first().map(|c| c.id.clone()) } else { None };
        self.editor = Some(TaskEditor::new(TaskDraft::new(kind, conn, self.folder.clone())));
    }

    /// Open the editor for an existing task.
    pub fn edit_task(&mut self, id: &str) {
        if let Some(t) = self.tasks.iter().find(|t| t.id == id) {
            self.editor = Some(TaskEditor::new(TaskDraft::from_task(t)));
        }
    }

    /// The task editor is open.
    pub fn editor_open(&self) -> bool {
        self.editor.is_some()
    }

    /// Form of the open editor.
    pub fn editor_draft_mut(&mut self) -> Option<&mut TaskDraft> {
        self.editor.as_mut().map(|e| &mut e.draft)
    }

    /// Error shown in the open editor (failed save).
    pub fn editor_error(&self) -> Option<&str> {
        self.editor.as_ref().and_then(|e| e.error.as_deref())
    }

    /// Save the open editor (like its Save button).
    pub fn save_editor(&mut self, cx: &mut AppContext) -> Result<(), String> {
        let task = self.editor.as_ref().ok_or_else(|| tr("The task editor is not open").to_string())?.draft.to_task()?;
        self.save_task(cx, task);
        Ok(())
    }

    /// Start the selected tasks now.
    pub fn run_selected(&mut self, cx: &mut AppContext) {
        let ids = self.selected.clone();
        self.run_tasks(cx, ids);
    }

    /// Cancel the runs of the selected tasks.
    pub fn cancel_selected(&mut self, cx: &mut AppContext) {
        let ids = self.selected.clone();
        self.cancel_runs(cx, &ids);
    }

    /// Status of the most recent run of a task.
    pub fn latest_status(&self, task_id: &str) -> Option<RunStatus> {
        self.latest.get(task_id).map(|r| r.status)
    }

    /// Runs of the focused task, newest first.
    pub fn history(&self) -> &[TaskRun] {
        &self.history
    }

    /// Background work is pending (store writes, run log reads, starts).
    pub fn is_busy(&self) -> bool {
        !self.jobs.is_empty() || !self.run_jobs.is_empty() || !self.os_jobs.is_empty() || self.runs_job.is_some()
    }

    // ------------------------------------------------------------ model

    fn visible(&self) -> Vec<&TaskDef> {
        let f = self.filter.trim().to_lowercase();
        self.tasks
            .iter()
            .filter(|t| self.folder.as_deref().is_none_or(|sel| in_folder(t, sel)))
            .filter(|t| {
                f.is_empty()
                    || t.name.to_lowercase().contains(&f)
                    || t.description.as_deref().is_some_and(|d| d.to_lowercase().contains(&f))
                    || kind_label(&t.kind).to_lowercase().contains(&f)
            })
            .collect()
    }

    fn task(&self, id: &str) -> Option<&TaskDef> {
        self.tasks.iter().find(|t| t.id == id)
    }

    fn refresh_tasks(&mut self, cx: &AppContext) {
        let Some(store) = cx.services.tasks.as_ref() else { return };
        self.tasks = store.list();
        self.folders = store.folders();
        let ids: HashSet<&str> = self.tasks.iter().map(|t| t.id.as_str()).collect();
        self.selected.retain(|s| ids.contains(s.as_str()));
        if self.focus.as_deref().is_some_and(|f| !ids.contains(f)) {
            self.focus = None;
        }
        if let Some(f) = &self.folder
            && !self.folders.contains(f)
        {
            self.folder = None;
        }
    }

    fn ensure_subscribed(&mut self, cx: &AppContext) {
        if self.events.is_some() {
            return;
        }
        let Some(s) = cx.services.scheduler.clone() else { return };
        self.events = Some(s.subscribe());
        // wake the UI for every event; the tab drains them each frame
        let mut rx = s.subscribe();
        let token = CancellationToken::new();
        let stop = token.clone();
        let repaint = cx.backend.repainter();
        cx.backend.spawn_detached(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    r = rx.recv() => match r {
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => repaint.request(),
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                }
            }
        });
        self.forwarder = Some(token);
    }

    fn start_runs_job(&mut self, cx: &AppContext) {
        if self.runs_job.is_some() {
            return;
        }
        let Some(log) = cx.services.runlog.clone() else {
            self.reload_runs = false;
            return;
        };
        self.reload_runs = false;
        self.last_runs_load = Some(Instant::now());
        let focus = self.history_for.clone();
        self.runs_job = Some(spawn_blocking(cx.backend, move || load_runs(&log, focus.as_deref())));
    }

    fn find_run_mut(&mut self, run_id: &str) -> Option<&mut TaskRun> {
        if let Some(i) = self.history.iter().position(|r| r.run_id == run_id) {
            return self.history.get_mut(i);
        }
        self.children.values_mut().flat_map(|v| v.iter_mut()).find(|r| r.run_id == run_id)
    }

    fn upsert_run(&mut self, run: TaskRun) {
        if let Some(parent) = run.parent_run_id.clone()
            && self.history.iter().any(|r| r.run_id == parent)
        {
            let list = self.children.entry(parent).or_default();
            match list.iter_mut().find(|r| r.run_id == run.run_id) {
                Some(slot) => *slot = run.clone(),
                None => list.push(run.clone()),
            }
        }
        if self.history_for.as_deref() == Some(run.task_id.as_str()) {
            match self.history.iter_mut().find(|r| r.run_id == run.run_id) {
                Some(slot) => *slot = run,
                None => {
                    self.history.insert(0, run);
                    self.history.truncate(HISTORY_LIMIT);
                }
            }
        }
    }

    fn notify(cx: &mut AppContext, run: &TaskRun) {
        match run.status {
            RunStatus::Succeeded => cx.toasts.success(trf("Task \"{}\" succeeded", &[&run.task_name])),
            RunStatus::Failed => cx.toasts.error(trf("Task \"{}\" failed", &[&run.task_name]), run.error.clone()),
            RunStatus::Cancelled => cx.toasts.info(trf("Task \"{}\" was cancelled", &[&run.task_name])),
            RunStatus::Skipped => cx.toasts.warning(trf("Task \"{}\" was skipped", &[&run.task_name]), run.messages.last().map(|m| m.text.clone())),
            RunStatus::Running => {}
        }
    }

    fn apply_event(&mut self, ev: TaskEvent, cx: &mut AppContext) {
        match ev {
            TaskEvent::Started { run } => {
                let mut l = run.clone();
                l.messages.clear();
                self.latest.insert(run.task_id.clone(), l);
                self.upsert_run(run);
            }
            TaskEvent::Message { run_id, message, .. } => {
                if let Some(r) = self.find_run_mut(&run_id) {
                    r.push_message(message);
                }
            }
            TaskEvent::Progress { run_id, progress, .. } => {
                self.progress.insert(run_id, progress);
            }
            TaskEvent::Finished { run } => {
                self.progress.remove(&run.run_id);
                if self.my_runs.remove(&run.run_id) {
                    Self::notify(cx, &run);
                } else if !self.recent_finished.iter().any(|r| r.run_id == run.run_id) {
                    self.recent_finished.push(run.clone());
                    if self.recent_finished.len() > 50 {
                        self.recent_finished.remove(0);
                    }
                }
                let mut l = run.clone();
                l.messages.clear();
                self.latest.insert(run.task_id.clone(), l);
                self.upsert_run(run);
            }
        }
    }

    fn drain_events(&mut self, cx: &mut AppContext) {
        let mut got = Vec::new();
        if let Some(rx) = self.events.as_mut() {
            loop {
                match rx.try_recv() {
                    Ok(ev) => got.push(ev),
                    Err(TryRecvError::Lagged(_)) => {
                        self.reload_runs = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Closed) => {
                        self.events = None;
                        break;
                    }
                }
            }
        }
        for ev in got {
            if self.runs_job.is_some() {
                // replayed on top of the run log snapshot being read
                self.buffered.push(ev.clone());
            }
            self.apply_event(ev, cx);
        }
    }

    fn poll(&mut self, cx: &mut AppContext) {
        if !self.initialized {
            self.initialized = true;
            self.store_rx = cx.services.tasks.as_ref().map(|s| s.subscribe());
            self.refresh_tasks(cx);
        }
        if self.store_rx.as_mut().is_some_and(|rx| rx.has_changed().unwrap_or(false)) {
            if let Some(rx) = self.store_rx.as_mut() {
                rx.borrow_and_update();
            }
            self.refresh_tasks(cx);
        }
        self.ensure_subscribed(cx);
        self.drain_events(cx);
        if self.focus != self.history_for {
            self.history_for = self.focus.clone();
            self.history.clear();
            self.children.clear();
            self.selected_run = None;
            self.reload_runs = true;
        }
        if let Some(r) = self.runs_job.as_mut().and_then(|j| j.poll()) {
            self.runs_job = None;
            match r {
                Ok(snap) => {
                    self.runs_error = None;
                    self.latest = snap.latest;
                    if snap.focus == self.history_for {
                        self.history = snap.history;
                        self.children = snap.children;
                    } else {
                        self.reload_runs = true;
                    }
                }
                Err(e) => self.runs_error = Some(e.full_text()),
            }
            for ev in std::mem::take(&mut self.buffered) {
                self.apply_event(ev, cx);
            }
        }
        let external = self.tasks.iter().any(|t| t.os_scheduler);
        if external && self.last_runs_load.is_some_and(|t| t.elapsed() >= EXTERNAL_REFRESH) {
            self.reload_runs = true;
        }
        if self.reload_runs {
            self.start_runs_job(cx);
        }
        self.poll_jobs(cx);
    }

    fn poll_jobs(&mut self, cx: &mut AppContext) {
        let mut finished = Vec::new();
        self.jobs.retain_mut(|(kind, job)| match job.poll() {
            Some(r) => {
                finished.push((*kind, r));
                false
            }
            None => true,
        });
        for (kind, r) in finished {
            match r {
                Ok(done) => self.store_done(cx, done),
                Err(e) if kind == JobKind::Editor => {
                    if let Some(ed) = self.editor.as_mut() {
                        ed.saving = false;
                        ed.error = Some(e.message);
                    }
                }
                Err(e) => cx.toasts.error(tr("The task change failed"), Some(e.full_text())),
            }
        }
        let mut started = Vec::new();
        self.run_jobs.retain_mut(|(name, job)| match job.poll() {
            Some(r) => {
                started.push((name.clone(), r));
                false
            }
            None => true,
        });
        for (name, r) in started {
            match r {
                Ok(run_id) => {
                    if let Some(i) = self.recent_finished.iter().position(|r| r.run_id == run_id) {
                        let run = self.recent_finished.remove(i);
                        Self::notify(cx, &run);
                    } else {
                        self.my_runs.insert(run_id);
                    }
                }
                Err(e) => cx.toasts.error(trf("Task \"{}\" could not start", &[&name]), Some(e.message)),
            }
        }
        let mut os = Vec::new();
        self.os_jobs.retain_mut(|job| match job.poll() {
            Some(r) => {
                os.push(r);
                false
            }
            None => true,
        });
        for r in os {
            match r {
                OsDone::Synced(task, Synced::Installed(_)) => cx.toasts.success(trf("\"{}\" is registered with {}", &[&task.name, &scheduler_name()])),
                OsDone::Synced(task, Synced::Removed(true)) => cx.toasts.info(trf("\"{}\" was removed from {}", &[&task.name, &scheduler_name()])),
                OsDone::Removed(name, true) => cx.toasts.info(trf("\"{}\" was removed from {}", &[&name, &scheduler_name()])),
                OsDone::Synced(_, Synced::Removed(false)) | OsDone::Removed(_, false) => {}
                OsDone::Failed { task, error } => {
                    cx.toasts.error(trf("The {} entry could not be changed", &[&scheduler_name()]), Some(error));
                    // keep the flag in line with reality
                    if let (Some(t), Some(store)) = (task.filter(|t| t.os_scheduler && system::wanted(t)), cx.services.tasks.clone()) {
                        let id = t.id.clone();
                        self.jobs.push((
                            JobKind::Other,
                            spawn_blocking(cx.backend, move || {
                                if let Some(mut t) = store.get(&id) {
                                    t.os_scheduler = false;
                                    store.update(t)?;
                                }
                                Ok(StoreDone::Reloaded)
                            }),
                        ));
                    }
                }
            }
        }
    }

    fn store_done(&mut self, cx: &mut AppContext, done: StoreDone) {
        let imported = matches!(done, StoreDone::Imported(_));
        match done {
            StoreDone::Saved { task, previous } => {
                self.editor = None;
                self.selected = vec![task.id.clone()];
                self.focus = Some(task.id.clone());
                if task.os_scheduler || previous.as_ref().is_some_and(|p| p.os_scheduler) {
                    self.sync_os(cx, (*task).clone());
                }
                cx.toasts.success(trf("Task \"{}\" saved", &[&task.name]));
            }
            StoreDone::Deleted(tasks) => {
                for t in tasks.iter().filter(|t| t.os_scheduler) {
                    let (id, name) = (t.id.clone(), t.name.clone());
                    self.os_jobs.push(cx.backend.spawn(async move {
                        match system::remove(id).await {
                            Ok(r) => OsDone::Removed(name, r),
                            Err(e) => OsDone::Failed { task: None, error: e.full_text() },
                        }
                    }));
                }
                self.selected.clear();
                cx.toasts.info(trn(tasks.len(), tr("1 task deleted"), tr("{} tasks deleted")));
            }
            StoreDone::Duplicated(tasks) | StoreDone::Imported(tasks) => {
                self.selected = tasks.iter().map(|t| t.id.clone()).collect();
                self.focus = tasks.first().map(|t| t.id.clone());
                cx.toasts.success(trn(tasks.len(), tr("1 task added"), tr("{} tasks added")));
                // Imported scheduled tasks come in disabled: they run on their own only once enabled.
                let disabled = if imported { tasks.iter().filter(|t| t.schedule.is_some() && !t.enabled).count() } else { 0 };
                if disabled > 0 {
                    let title = trn(disabled, tr("1 imported scheduled task is disabled"), tr("{} imported scheduled tasks are disabled"));
                    cx.toasts.warning(title, Some(tr("Review them, then enable them to run on their schedule.").to_string()));
                }
            }
            StoreDone::Toggled(tasks) => {
                for t in tasks.into_iter().filter(|t| t.os_scheduler) {
                    self.sync_os(cx, t);
                }
            }
            StoreDone::Exported { path, count } => {
                let what = trn(count, tr("1 task"), tr("{} tasks"));
                cx.toasts.success(trf("{} exported to {}", &[&what, &path.display()]));
            }
            StoreDone::Moved(n) => cx.toasts.info(trn(n, tr("1 task moved"), tr("{} tasks moved"))),
            StoreDone::Folder | StoreDone::Reloaded => {}
        }
        self.refresh_tasks(cx);
    }

    // ------------------------------------------------------------ actions

    fn save_task(&mut self, cx: &mut AppContext, task: TaskDef) {
        let Some(store) = cx.services.tasks.clone() else { return };
        let previous = self.editor.as_ref().and_then(|e| e.draft.original.clone());
        if let Some(e) = self.editor.as_mut() {
            e.saving = true;
            e.error = None;
        }
        self.jobs.push((
            JobKind::Editor,
            spawn_blocking(cx.backend, move || {
                let task = if previous.is_some() { store.update(task)? } else { store.create(task)? };
                Ok(StoreDone::Saved { task: Box::new(task), previous: previous.map(Box::new) })
            }),
        ));
    }

    /// Bring the operating system entry of `task` in line with its settings.
    fn sync_os(&mut self, cx: &AppContext, task: TaskDef) {
        self.os_jobs.push(cx.backend.spawn(async move {
            match system::sync(task.clone()).await {
                Ok(s) => OsDone::Synced(task, s),
                Err(e) => OsDone::Failed { task: Some(task), error: e.full_text() },
            }
        }));
    }

    fn run_tasks(&mut self, cx: &mut AppContext, ids: Vec<String>) {
        let Some(s) = cx.services.scheduler.clone() else {
            cx.toasts.error(tr("Tasks cannot be run: the task scheduler is not available."), None);
            return;
        };
        for id in ids {
            let name = self.task(&id).map(|t| t.name.clone()).unwrap_or_default();
            let s = s.clone();
            self.run_jobs.push((name, cx.backend.spawn(async move { s.run_now(&id, Trigger::Manual) })));
        }
    }

    fn cancel_runs(&mut self, cx: &mut AppContext, ids: &[String]) {
        let Some(s) = cx.services.scheduler.clone() else { return };
        for r in s.running().into_iter().filter(|r| ids.contains(&r.task_id)) {
            s.cancel(&r.run_id);
        }
    }

    fn set_enabled(&mut self, cx: &mut AppContext, ids: Vec<String>, on: bool) {
        let Some(store) = cx.services.tasks.clone() else { return };
        self.jobs.push((
            JobKind::Other,
            spawn_blocking(cx.backend, move || {
                let mut out = Vec::new();
                for id in &ids {
                    store.set_enabled(id, on)?;
                    out.extend(store.get(id));
                }
                Ok(StoreDone::Toggled(out))
            }),
        ));
    }

    fn duplicate(&mut self, cx: &mut AppContext, ids: Vec<String>) {
        let Some(store) = cx.services.tasks.clone() else { return };
        self.jobs.push((JobKind::Other, spawn_blocking(cx.backend, move || ids.iter().map(|id| store.duplicate(id)).collect::<DbResult<Vec<_>>>().map(StoreDone::Duplicated))));
    }

    fn delete(&mut self, cx: &mut AppContext, ids: Vec<String>) {
        let Some(store) = cx.services.tasks.clone() else { return };
        self.cancel_runs(cx, &ids);
        self.jobs.push((
            JobKind::Other,
            spawn_blocking(cx.backend, move || {
                let mut deleted = Vec::new();
                for id in &ids {
                    if let Some(t) = store.get(id)
                        && store.delete(id)?
                    {
                        deleted.push(t);
                    }
                }
                Ok(StoreDone::Deleted(deleted))
            }),
        ));
    }

    fn move_to(&mut self, cx: &mut AppContext, ids: Vec<String>, folder: Option<String>) {
        let Some(store) = cx.services.tasks.clone() else { return };
        self.jobs.push((
            JobKind::Other,
            spawn_blocking(cx.backend, move || {
                for id in &ids {
                    store.move_task(id, folder.as_deref())?;
                }
                Ok(StoreDone::Moved(ids.len()))
            }),
        ));
    }

    fn export(&mut self, cx: &mut AppContext, ids: Vec<String>) {
        let Some(store) = cx.services.tasks.clone() else { return };
        let Some(path) = rfd::FileDialog::new().set_file_name("ternarygrid-tasks.json").add_filter("JSON", &["json"]).save_file() else { return };
        self.jobs.push((
            JobKind::Other,
            spawn_blocking(cx.backend, move || {
                let json = store.export_json(&ids)?;
                let count = serde_json::from_str::<serde_json::Value>(&json).ok().and_then(|v| v["tasks"].as_array().map(|a| a.len())).unwrap_or(ids.len());
                std::fs::write(&path, json).map_err(|e| ternarygrid_core::DbError::io(format!("{}: {e}", path.display())))?;
                Ok(StoreDone::Exported { path, count })
            }),
        ));
    }

    fn import(&mut self, cx: &mut AppContext) {
        let Some(store) = cx.services.tasks.clone() else { return };
        let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() else { return };
        self.jobs.push((
            JobKind::Other,
            spawn_blocking(cx.backend, move || {
                let text = std::fs::read_to_string(&path).map_err(|e| ternarygrid_core::DbError::io(format!("{}: {e}", path.display())))?;
                store.import_json(&text).map(StoreDone::Imported)
            }),
        ));
    }

    fn folder_op(&mut self, cx: &mut AppContext, op: impl FnOnce(&ternarygrid_tasks::TaskStore) -> DbResult<()> + Send + 'static) {
        let Some(store) = cx.services.tasks.clone() else { return };
        self.jobs.push((JobKind::Other, spawn_blocking(cx.backend, move || op(&store).map(|_| StoreDone::Folder))));
    }

    fn reload_all(&mut self, cx: &mut AppContext) {
        self.reload_runs = true;
        if let Some(store) = cx.services.tasks.clone() {
            self.jobs.push((JobKind::Other, spawn_blocking(cx.backend, move || store.reload().map(|_| StoreDone::Reloaded))));
        }
    }

    // ------------------------------------------------------------ UI

    fn banner(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if cx.settings.tasks.scheduler_enabled {
            return;
        }
        let mut turn_on = false;
        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 0 }).show(ui, |ui| {
            look::banner(ui, Tone::Warning, tr("Scheduled runs are off — tasks run only when you start them."), |ui| {
                let b = kit::button_ex(ui, Kind::Secondary, None, tr("Turn on"), true);
                turn_on = b.on_hover_text(tr("Run scheduled tasks while TernaryGrid is open (Settings › General)")).clicked();
            });
        });
        if turn_on {
            cx.settings.tasks.scheduler_enabled = true;
            cx.push(AppCommand::SettingsChanged);
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let running: HashSet<String> = cx.services.scheduler.as_ref().map(|s| s.running().into_iter().map(|r| r.task_id).collect()).unwrap_or_default();
        let sel = self.selected.clone();
        let one = sel.len() == 1;
        let any = !sel.is_empty();
        let can_run = cx.services.scheduler.is_some();
        ternarygrid_ui::parts::toolbar(ui, |ui| {
            let new = kit::button_ex(ui, Kind::Primary, Some(icon::PLUS), tr("New task"), true);
            egui::Popup::menu(&new).show(|ui| {
                kit::menu_look(ui, 220.0);
                for k in NewTaskKind::ALL {
                    if kit::menu_item(ui, Some(k.icon()), k.label(), "").clicked() {
                        self.open_new_task(cx, k);
                        ui.close();
                    }
                }
                kit::menu_separator(ui);
                for (glyph, label) in [(icon::EXPORT, tr("Data export")), (icon::DOWNLOAD_SIMPLE, tr("Data import")), (icon::ARROWS_LEFT_RIGHT, tr("Data migration"))] {
                    ui.add_enabled_ui(false, |ui| kit::menu_item(ui, Some(glyph), label, ""))
                        .inner
                        .on_disabled_hover_text(tr("Created with \"Save as task\" in the transfer wizards"));
                }
            });
            let ghost = |ui: &mut Ui, enabled: bool, glyph: Option<&str>, label: &str| ui.add_enabled_ui(enabled, |ui| kit::button_ex(ui, Kind::Ghost, glyph, label, true)).inner;
            if ghost(ui, one, Some(icon::PENCIL_SIMPLE), tr("Edit")).clicked() {
                self.edit_task(&sel[0]);
            }
            let all_enabled = any && sel.iter().all(|id| self.task(id).is_some_and(|t| t.enabled));
            let (glyph, label) = if all_enabled { (icon::TOGGLE_LEFT, tr("Disable")) } else { (icon::TOGGLE_RIGHT, tr("Enable")) };
            if ghost(ui, any, Some(glyph), label).clicked() {
                self.set_enabled(cx, sel.clone(), !all_enabled);
            }
            if ghost(ui, any, Some(icon::TRASH), tr("Delete…")).clicked() {
                self.confirm_delete = Some(sel.clone());
            }
            kit::vdivider(ui, 18.0);
            let r = ui.add_enabled_ui(any && can_run, |ui| kit::button_ex(ui, Kind::Secondary, Some(icon::PLAY), tr("Run now"), true)).inner;
            if !can_run {
                r.clone().on_disabled_hover_text(tr("Tasks cannot be run: the task scheduler is not available."));
            }
            if r.clicked() {
                self.run_tasks(cx, sel.clone());
            }
            let any_running = sel.iter().any(|id| running.contains(id));
            if ghost(ui, any_running, Some(icon::STOP_CIRCLE), tr("Stop")).on_hover_text(tr("Stop the runs of the selected tasks")).clicked() {
                self.cancel_runs(cx, &sel);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The less used actions in ⋯, then Refresh and the filter (as tall as the buttons).
                let more = kit::icon_button(ui, icon::DOTS_THREE, tr("More actions"));
                egui::Popup::menu(&more).show(|ui| {
                    kit::menu_look(ui, 220.0);
                    if ui.add_enabled_ui(any, |ui| kit::menu_item(ui, Some(icon::COPY), tr("Duplicate"), "")).inner.clicked() {
                        self.duplicate(cx, sel.clone());
                        ui.close();
                    }
                    kit::menu_separator(ui);
                    let export_label = if any { tr("Export selected tasks…") } else { tr("Export all tasks…") };
                    if ui.add_enabled_ui(!self.tasks.is_empty(), |ui| kit::menu_item(ui, Some(icon::EXPORT), export_label, "")).inner.clicked() {
                        ui.close();
                        self.export(cx, sel.clone());
                    }
                    if kit::menu_item(ui, Some(icon::DOWNLOAD_SIMPLE), tr("Import tasks…"), "").clicked() {
                        ui.close();
                        self.import(cx);
                    }
                });
                if kit::icon_button(ui, icon::ARROW_CLOCKWISE, tr("Refresh")).clicked() {
                    self.reload_all(cx);
                }
                let w = (ui.available_width() - 8.0).clamp(90.0, 200.0);
                look::search_field_h(ui, egui::Id::new("tasks_filter"), &mut self.filter, tr("Filter…"), w, ternarygrid_ui::style::SMALL_H);
            });
        });
    }

    fn folder_visible(&self, f: &str) -> bool {
        let mut acc = String::new();
        let parts: Vec<&str> = f.split('/').collect();
        for p in &parts[..parts.len().saturating_sub(1)] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(p);
            if self.collapsed.contains(&acc) {
                return false;
            }
        }
        true
    }

    fn folders_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            look::small_heading(ui, tr("Folders"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(10.0);
                if kit::icon_button_sized(ui, icon::PLUS, tr("New folder"), false, 22.0, 12.0).clicked() {
                    self.folder_prompt = Some(FolderPrompt { rename: None, parent: self.folder.clone(), name: String::new() });
                }
            });
        });
        let folders = self.folders.clone();
        egui::ScrollArea::vertical().id_salt("tasks_folders").auto_shrink([false, false]).show(ui, |ui| {
            // Rows of 26 points edge to edge.
            ui.spacing_mut().item_spacing.y = 0.0;
            let total = self.tasks.len().to_string();
            if kit::tree_row(ui, 0, None, icon::LIST_CHECKS, tr("All tasks"), Some(&total), self.folder.is_none(), false).resp.clicked() {
                self.folder = None;
            }
            for (i, f) in folders.iter().enumerate() {
                if !self.folder_visible(f) {
                    continue;
                }
                let depth = f.matches('/').count();
                let name = f.rsplit('/').next().unwrap_or(f).to_string();
                let has_children = folders.get(i + 1).is_some_and(|n| n.starts_with(&format!("{f}/")));
                let count = self.tasks.iter().filter(|t| in_folder(t, f)).count().to_string();
                let open = self.folder.as_deref() == Some(f.as_str());
                let collapsed = self.collapsed.contains(f);
                let caret = has_children.then_some(!collapsed);
                let row = kit::tree_row(ui, depth, caret, if open { icon::FOLDER_OPEN } else { icon::FOLDER }, &name, Some(&count), open, false);
                if row.caret {
                    if collapsed {
                        self.collapsed.remove(f);
                    } else {
                        self.collapsed.insert(f.clone());
                    }
                } else if row.resp.clicked() {
                    self.folder = Some(f.clone());
                }
                row.resp.context_menu(|ui| {
                    kit::menu_look(ui, 200.0);
                    if kit::menu_item(ui, Some(icon::FOLDER_PLUS), tr("New sub-folder…"), "").clicked() {
                        self.folder_prompt = Some(FolderPrompt { rename: None, parent: Some(f.clone()), name: String::new() });
                        ui.close();
                    }
                    if kit::menu_item(ui, Some(icon::PENCIL_SIMPLE), tr("Rename…"), "").clicked() {
                        self.folder_prompt = Some(FolderPrompt { rename: Some(f.clone()), parent: None, name: name.clone() });
                        ui.close();
                    }
                    kit::menu_separator(ui);
                    if kit::menu_item_danger(ui, Some(icon::TRASH), tr("Delete folder"), "").on_hover_text(tr("Its tasks move to the parent folder")).clicked() {
                        let path = f.clone();
                        self.folder_op(cx, move |s| s.delete_folder(&path));
                        ui.close();
                    }
                });
            }
        });
    }

    fn list_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let visible: Vec<TaskDef> = self.visible().into_iter().cloned().collect();
        if let Some(e) = &self.runs_error {
            look::error(ui, &trf("Cannot read the run history: {}", &[e]));
        }
        if visible.is_empty() {
            let title = if self.tasks.is_empty() { tr("No tasks yet.") } else { tr("No task matches.") };
            kit::empty_state(
                ui,
                icon::LIST_CHECKS,
                title,
                tr("Tasks save SQL scripts, backups, schema compares and shell commands so that they can run on demand or on a schedule."),
                |_| {},
            );
            return;
        }
        let running: HashMap<String, RunningInfo> = cx.services.scheduler.as_ref().map(|s| s.running().into_iter().map(|r| (r.task_id.clone(), r)).collect()).unwrap_or_default();
        let next_runs: BTreeMap<String, DateTime<Local>> = cx.services.scheduler.as_ref().map(|s| s.next_runs()).unwrap_or_default();
        let scheduling = cx.settings.tasks.scheduler_enabled;
        let show_folder = self.folder.is_none();
        let pal = ternarygrid_ui::pal(ui);
        let folders = self.folders.clone();
        let modifiers = ui.input(|i| i.modifiers);
        let mut actions: Vec<RowAction> = Vec::new();
        let now = Local::now();
        // The next run of each task: planned by the scheduler, or expected from its schedule
        // (`true`: not planned yet, shown faint).
        let next: Vec<Option<(DateTime<Local>, bool)>> = visible
            .iter()
            .map(|t| {
                if !t.enabled {
                    None
                } else if let Some(n) = next_runs.get(&t.id) {
                    Some((*n, false))
                } else {
                    t.schedule.as_ref().and_then(|s| {
                        let anchor = self.latest.get(&t.id).map(|r| r.started.with_timezone(&Local)).unwrap_or_else(|| t.updated.with_timezone(&Local));
                        s.next_after_skipping(&anchor.min(now), &now).map(|n| (n, true))
                    })
                }
            })
            .collect();
        let schedules: Vec<String> = visible.iter().map(|t| t.schedule.as_ref().map_or_else(|| tr("Manual").to_string(), |s| s.describe())).collect();
        // Type, Schedule and Next run as wide as their content; Name and Last run share the
        // rest (inside the table's hairline). A disabled task: its name faint, "Disabled" as
        // its next run (Enable / Disable in the toolbar and the row's menu).
        let os = if visible.iter().any(|t| t.os_scheduler) { 20.0 } else { 0.0 };
        let type_w = look::fit(ui, visible.iter().map(|t| kind_label(&t.kind)).chain([tr("Type")]), 0.0, 90.0, 170.0);
        let sched_w = look::fit(ui, schedules.iter().map(String::as_str).chain([tr("Schedule")]), os, 90.0, 220.0);
        let next_texts: Vec<String> = next.iter().flatten().map(|(n, _)| when(*n, now)).collect();
        let next_w = look::fit(ui, next_texts.iter().map(String::as_str).chain([tr("Next run"), tr("Disabled")]), 0.0, 90.0, 160.0);
        let fr = ((ui.available_width() - 2.0 - (type_w + sched_w + next_w)) / 2.0).max(120.0);
        let cols = [
            look::Col::new(tr("Name"), look::Width::Initial(fr.floor())),
            look::Col::new(tr("Type"), look::Width::Fit(type_w)),
            look::Col::new(tr("Schedule"), look::Width::Fit(sched_w)),
            look::Col::new(tr("Next run"), look::Width::Fit(next_w)),
            look::Col::new(tr("Last run"), look::Width::Rest(160.0)),
        ];
        look::table(ui, "tasks_table", &cols, visible.len(), |row, i| {
            let t = &visible[i];
            row.set_selected(self.selected.iter().any(|s| s == &t.id));
            look::cell(row, false, |ui| {
                // The design's 14 point icon, 8 from the name.
                let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, kind_icon(&t.kind), ternarygrid_ui::style::icon_font(14.0), pal.text_muted);
                ui.add_space(8.0 - ui.spacing().item_spacing.x);
                let name = if show_folder && t.folder.is_some() { t.path() } else { t.name.clone() };
                let r = look::cell_text(ui, RichText::new(name).color(if t.enabled { pal.text } else { pal.text_faint }));
                if let Some(d) = &t.description {
                    r.on_hover_text(d);
                }
            });
            look::cell(row, false, |ui| {
                look::cell_text(ui, RichText::new(kind_label(&t.kind)).color(pal.text_muted));
            });
            look::cell(row, false, |ui| {
                look::cell_text(ui, RichText::new(&schedules[i]).color(pal.text_muted));
                if t.os_scheduler {
                    ui.label(RichText::new(icon::DESKTOP).color(pal.text_faint)).on_hover_text(trf("Also registered with {}", &[&scheduler_name()]));
                }
            });
            look::cell(row, false, |ui| match next[i] {
                Some((n, false)) => {
                    look::cell_text(ui, RichText::new(when(n, now)).color(pal.text_muted)).on_hover_text(full_time(n));
                }
                Some((n, true)) => {
                    let hint = if scheduling { tr("Expected; the scheduler has not planned it yet") } else { tr("Scheduled runs are off") };
                    look::cell_text(ui, RichText::new(when(n, now)).color(pal.text_faint)).on_hover_text(format!("{}\n{hint}", full_time(n)));
                }
                None if !t.enabled => {
                    look::cell_text(ui, RichText::new(tr("Disabled")).color(pal.text_faint)).on_hover_text(tr("Enable the task to run it on its schedule"));
                }
                None => {
                    look::cell_text(ui, RichText::new("—").color(pal.text_faint));
                }
            });
            look::cell(row, false, |ui| {
                if let Some(r) = running.get(&t.id) {
                    let elapsed = (chrono::Utc::now() - r.started).to_std().unwrap_or_default();
                    let mut text = format!("{} {}", tr("Running"), short_duration(elapsed));
                    if let Some(p) = self.progress.get(&r.run_id) {
                        match p.total {
                            Some(total) if total > 0 => text.push_str(&format!(" · {}/{}", p.done, total)),
                            _ if p.done > 0 => text.push_str(&format!(" · {}", p.done)),
                            _ => {}
                        }
                    }
                    look::cell_text(ui, RichText::new(text).color(status_color(RunStatus::Running, &pal)));
                } else if let Some(l) = self.latest.get(&t.id) {
                    let started = l.started.with_timezone(&Local);
                    let text = format!("{} · {} · {}", status_label(l.status), when(started, now), run_duration(l));
                    let r = look::cell_text(ui, RichText::new(text).color(status_color(l.status, &pal)));
                    let tip = match &l.error {
                        Some(e) => format!("{}\n{e}", full_time(started)),
                        None => full_time(started),
                    };
                    r.on_hover_text(tip);
                } else {
                    look::cell_text(ui, RichText::new(tr("Never")).color(pal.text_faint));
                }
            });
            let resp = row.response();
            if resp.clicked() {
                actions.push(RowAction::Click(i, t.id.clone()));
            }
            if resp.double_clicked() {
                actions.push(RowAction::Edit(t.id.clone()));
            }
            let targets = if self.selected.iter().any(|s| s == &t.id) { self.selected.clone() } else { vec![t.id.clone()] };
            resp.context_menu(|ui| {
                kit::menu_look(ui, 200.0);
                if kit::menu_item(ui, Some(icon::PENCIL_SIMPLE), tr("Edit…"), "").clicked() {
                    actions.push(RowAction::Edit(t.id.clone()));
                    ui.close();
                }
                if kit::menu_item(ui, Some(icon::PLAY), tr("Run now"), "").clicked() {
                    actions.push(RowAction::Run(targets.clone()));
                    ui.close();
                }
                if running.contains_key(&t.id) && kit::menu_item(ui, Some(icon::STOP_CIRCLE), tr("Stop run"), "").clicked() {
                    actions.push(RowAction::Cancel(targets.clone()));
                    ui.close();
                }
                if kit::menu_item(ui, Some(icon::COPY), tr("Duplicate"), "").clicked() {
                    actions.push(RowAction::Duplicate(targets.clone()));
                    ui.close();
                }
                let label = if t.enabled { tr("Disable") } else { tr("Enable") };
                if kit::menu_item(ui, Some(if t.enabled { icon::TOGGLE_LEFT } else { icon::TOGGLE_RIGHT }), label, "").clicked() {
                    actions.push(RowAction::Enable(targets.clone(), !t.enabled));
                    ui.close();
                }
                kit::submenu(ui, Some(icon::FOLDER), tr("Move to"), 200.0, &mut |ui| {
                    if kit::menu_item(ui, Some(icon::LIST_CHECKS), tr("Top level"), "").clicked() {
                        actions.push(RowAction::Move(targets.clone(), None));
                        ui.close();
                    }
                    for f in &folders {
                        if kit::menu_item(ui, Some(icon::FOLDER), f, "").clicked() {
                            actions.push(RowAction::Move(targets.clone(), Some(f.clone())));
                            ui.close();
                        }
                    }
                });
                if kit::menu_item(ui, Some(icon::EXPORT), tr("Export…"), "").clicked() {
                    actions.push(RowAction::Export(targets.clone()));
                    ui.close();
                }
                kit::menu_separator(ui);
                if kit::menu_item_danger(ui, Some(icon::TRASH), tr("Delete…"), "").clicked() {
                    actions.push(RowAction::Delete(targets.clone()));
                    ui.close();
                }
            });
        });
        for a in actions {
            match a {
                RowAction::Click(i, id) => {
                    if modifiers.shift {
                        let from = self.anchor.unwrap_or(i).min(visible.len().saturating_sub(1));
                        let (lo, hi) = if from <= i { (from, i) } else { (i, from) };
                        self.selected = visible[lo..=hi].iter().map(|t| t.id.clone()).collect();
                    } else if modifiers.command {
                        if let Some(p) = self.selected.iter().position(|s| *s == id) {
                            self.selected.remove(p);
                        } else {
                            self.selected.push(id.clone());
                        }
                        self.anchor = Some(i);
                    } else {
                        self.selected = vec![id.clone()];
                        self.anchor = Some(i);
                    }
                    self.focus = Some(id);
                }
                RowAction::Edit(id) => self.edit_task(&id),
                RowAction::Enable(ids, on) => self.set_enabled(cx, ids, on),
                RowAction::Run(ids) => self.run_tasks(cx, ids),
                RowAction::Cancel(ids) => self.cancel_runs(cx, &ids),
                RowAction::Duplicate(ids) => self.duplicate(cx, ids),
                RowAction::Delete(ids) => self.confirm_delete = Some(ids),
                RowAction::Move(ids, f) => self.move_to(cx, ids, f),
                RowAction::Export(ids) => self.export(cx, ids),
            }
        }
        if !running.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
    }

    fn history_ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let pal = ternarygrid_ui::pal(ui);
        let Some(focus) = self.focus.clone() else {
            ui.add_space(12.0);
            ui.label(RichText::new(tr("Select a task to see its runs.")).color(pal.text_muted));
            return;
        };
        let name = self.task(&focus).map(|t| t.path()).unwrap_or_default();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            kit::semibold_label(ui, &trf("Run history — {}", &[&name]), 13.0, pal.text);
            if self.runs_job.is_some() {
                ui.add(egui::Spinner::new().size(14.0).color(pal.accent_text));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::icon_button_ex(ui, icon::ARROW_CLOCKWISE, tr("Refresh"), false, 24.0).clicked() {
                    self.reload_runs = true;
                }
            });
        });
        ui.add_space(8.0);
        if self.history.is_empty() {
            ui.label(RichText::new(if self.runs_job.is_some() { tr("Loading…") } else { tr("The task has not run yet.") }).color(pal.text_muted));
            return;
        }
        let shown = self.selected_run.clone().filter(|id| self.history.iter().any(|r| &r.run_id == id)).unwrap_or_else(|| self.history[0].run_id.clone());
        let mut clicked = None;
        let width = ui.available_width();
        // The runs and the selected run's details share the row 1.3 : 1, 16 points apart.
        // (Resizable without a line, as the design shows none; the split starts over when the
        // width changes.)
        let runs_frame = egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 8, top: 0, bottom: 10 });
        egui::Panel::left(egui::Id::new(("tasks_history_runs", width as i32 / 8)))
            .resizable(true)
            .show_separator_line(false)
            .default_size(((width - 16.0) * 1.3 / 2.3).floor() + 8.0)
            .frame(runs_frame)
            .show(ui, |ui| {
                // The run's message is in its details at the right (and on the row's hover).
                let cols = [
                    look::Col::new(tr("Status"), look::Width::Initial(100.0)),
                    look::Col::new(tr("Trigger"), look::Width::Initial(120.0)),
                    look::Col::new(tr("Started"), look::Width::Initial(170.0)),
                    look::Col::new(tr("Duration"), look::Width::Rest(80.0)),
                ];
                look::table(ui, "tasks_history_table", &cols, self.history.len(), |row, i| {
                    let r = &self.history[i];
                    row.set_selected(r.run_id == shown);
                    look::cell(row, false, |ui| {
                        look::cell_text(ui, RichText::new(status_label(r.status)).color(status_color(r.status, &pal)));
                    });
                    look::cell(row, false, |ui| {
                        look::cell_text(ui, trigger_label(r.trigger));
                    });
                    look::cell(row, false, |ui| {
                        look::cell_text(ui, look::mono(ui, &full_time(r.started.with_timezone(&Local)), pal.text_muted));
                    });
                    look::cell(row, false, |ui| {
                        look::cell_text(ui, run_duration(r));
                    });
                    let resp = row.response();
                    let msg = r.error.clone().or_else(|| r.messages.last().map(|m| m.text.clone())).unwrap_or_default();
                    let resp = if msg.is_empty() { resp } else { resp.on_hover_text(msg.lines().take(6).collect::<Vec<_>>().join("\n")) };
                    if resp.clicked() {
                        clicked = Some(r.run_id.clone());
                    }
                });
            });
        if let Some(id) = clicked {
            self.selected_run = Some(id);
        }
        egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 0, top: 0, bottom: 10 })).show(ui, |ui| {
            if let Some(run) = self.history.iter().find(|r| r.run_id == shown).cloned() {
                self.run_details_ui(ui, cx, &run);
            }
        });
    }

    fn run_details_ui(&mut self, ui: &mut Ui, cx: &mut AppContext, run: &TaskRun) {
        let pal = ternarygrid_ui::pal(ui);
        ui.spacing_mut().item_spacing.y = 8.0;
        let color = status_color(run.status, &pal);
        // A line as tall as its text, as designed.
        ui.spacing_mut().interact_size.y = 16.0;
        ui.horizontal_wrapped(|ui| {
            kit::semibold_label(ui, status_label(run.status), 13.0, color);
            let mut facts = format!("{} · {}", full_time(run.started.with_timezone(&Local)), run_duration(run));
            if let Some(rows) = run.rows {
                facts.push_str(&format!(" · {}", trn(rows as usize, tr("1 row"), tr("{} rows"))));
            }
            ui.label(RichText::new(facts).size(12.0).color(pal.text_muted));
            if run.status == RunStatus::Running {
                if let Some(p) = self.progress.get(&run.run_id) {
                    match p.total {
                        Some(total) if total > 0 => {
                            ternarygrid_ui::parts::progress(ui, (p.done as f32 / total as f32).clamp(0.0, 1.0), 120.0);
                            ui.label(RichText::new(format!("{} / {}", p.done, total)).size(12.0).color(pal.text_muted));
                        }
                        _ => {
                            ui.add(egui::Spinner::new().size(14.0).color(pal.accent_text));
                        }
                    }
                }
                if kit::ghost(ui, Some(icon::STOP_CIRCLE), tr("Stop run")).clicked()
                    && let Some(s) = cx.services.scheduler.as_ref()
                {
                    s.cancel(&run.run_id);
                }
            }
        });
        if let Some(e) = &run.error {
            look::error(ui, e);
        }
        if !run.output_files.is_empty() {
            look::small_heading(ui, tr("Output files"));
            for f in &run.output_files {
                ui.horizontal(|ui| {
                    ui.label(look::mono(ui, &f.display().to_string(), pal.text));
                    if kit::icon_button_ex(ui, icon::ARROW_SQUARE_OUT, tr("Open"), false, 22.0).clicked() {
                        let p = f.clone();
                        cx.backend.spawn_detached(async move {
                            let _ = open::that_detached(p);
                        });
                    }
                    if let Some(dir) = f.parent().map(|d| d.to_path_buf())
                        && kit::icon_button_ex(ui, icon::FOLDER_OPEN, tr("Open folder"), false, 22.0).clicked()
                    {
                        cx.backend.spawn_detached(async move {
                            let _ = open::that_detached(dir);
                        });
                    }
                });
            }
        }
        if let Some(steps) = self.children.get(&run.run_id).filter(|s| !s.is_empty()) {
            look::small_heading(ui, tr("Steps"));
            for s in steps {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(status_label(s.status)).color(status_color(s.status, &pal)));
                    ui.label(&s.task_name);
                    ui.label(RichText::new(run_duration(s)).color(pal.text_faint));
                    if let Some(e) = &s.error {
                        ui.add(egui::Label::new(RichText::new(e).color(pal.err)).truncate());
                    }
                });
            }
        }
        look::small_heading(ui, tr("Messages"));
        if run.dropped_messages > 0 {
            let text = trn(run.dropped_messages, tr("1 message was dropped from the middle of the log"), tr("{} messages were dropped from the middle of the log"));
            ui.label(RichText::new(text).size(12.0).color(pal.text_muted));
        }
        egui::ScrollArea::vertical().id_salt(("task_run_messages", &run.run_id)).auto_shrink([false, false]).stick_to_bottom(run.status == RunStatus::Running).show(ui, |ui| {
            // 8 apart on the design's lines of 15 (its monospace's line height).
            ui.spacing_mut().item_spacing.y = 9.5;
            if run.messages.is_empty() {
                ui.label(RichText::new("—").color(pal.text_faint));
            }
            for m in &run.messages {
                let c = match m.level {
                    MessageLevel::Info => pal.text_muted,
                    MessageLevel::Warning => pal.warn,
                    MessageLevel::Error => pal.err,
                };
                let line = format!("{} {}", m.time.with_timezone(&Local).format("%H:%M:%S"), m.text);
                let r = ui.add(egui::Label::new(look::mono(ui, &line, c).size(11.5)).truncate());
                if r.hovered() && ui.fonts_mut(|f| f.layout_no_wrap(line.clone(), egui::FontId::monospace(11.5), c).size().x) > r.rect.width() + 1.0 {
                    r.on_hover_text(&line);
                }
            }
        });
    }

    fn dialogs(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        let ctx = ui.ctx().clone();
        if let Some(mut editor) = self.editor.take() {
            let tasks = self.tasks.clone();
            let folders = self.folders.clone();
            let action = editor.show(&ctx, cx, &tasks, &folders);
            self.editor = Some(editor);
            match action {
                EditorAction::None => {}
                EditorAction::Cancel => self.editor = None,
                EditorAction::Save(task) => self.save_task(cx, *task),
            }
        }
        if let Some(ids) = self.confirm_delete.clone() {
            let names: Vec<String> = ids.iter().filter_map(|id| self.task(id).map(|t| t.path())).collect();
            let os = ids.iter().any(|id| self.task(id).is_some_and(|t| t.os_scheduler));
            let (delete, cancel) = (std::cell::Cell::new(false), std::cell::Cell::new(false));
            let n = names.len();
            let title = if n == 1 { tr("Delete task") } else { tr("Delete tasks") };
            let action = trn(n, tr("Delete task"), tr("Delete {} tasks"));
            let dialog = ternarygrid_ui::parts::Dialog::new(title).tone(Tone::Error).icon(icon::TRASH).width(480.0);
            let (resp, ()) = dialog.show_modal(
                &ctx,
                egui::Id::new("tasks_confirm_delete"),
                |ui| {
                    let pal = ternarygrid_ui::pal(ui);
                    let size = ternarygrid_ui::style::body_size(ui.ctx()) + 0.5;
                    let question = trn(n, tr("Delete 1 task?"), tr("Delete {} tasks?"));
                    ui.label(RichText::new(question).font(ternarygrid_ui::style::medium(ui.ctx(), size)).color(pal.text));
                    for n in names.iter().take(10) {
                        ui.label(format!("• {n}"));
                    }
                    if names.len() > 10 {
                        ui.label(RichText::new(trf("… {} more", &[&(names.len() - 10)])).color(pal.text_muted));
                    }
                    ui.add(egui::Label::new(RichText::new(tr("Deleted tasks are also removed from composite tasks. The run history is kept.")).color(pal.text_muted)).wrap());
                    if os {
                        ui.add(egui::Label::new(RichText::new(trf("Their {} entries are removed too.", &[&scheduler_name()])).color(pal.text_muted)).wrap());
                    }
                },
                |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if kit::button_ex(ui, Kind::DangerFilled, Some(icon::TRASH), &action, false).clicked() {
                            delete.set(true);
                        }
                        if kit::button_ex(ui, Kind::Ghost, None, tr("Cancel"), false).clicked() {
                            cancel.set(true);
                        }
                    });
                },
            );
            if delete.get() {
                self.delete(cx, ids.clone());
            }
            if delete.get() || cancel.get() || resp.close {
                self.confirm_delete = None;
            }
        }
        if let Some(mut p) = self.folder_prompt.take() {
            let renaming = p.rename.is_some();
            let title = if renaming { tr("Rename folder") } else { tr("New folder") };
            let name = p.name.trim().trim_matches('/').to_string();
            let ok_enabled = !name.is_empty();
            let (ok, cancel, enter) = (std::cell::Cell::new(false), std::cell::Cell::new(false), std::cell::Cell::new(false));
            let parent = p.parent.clone();
            let dialog = ternarygrid_ui::parts::Dialog::new(title).icon(icon::FOLDER).width(440.0);
            let (resp, ()) = dialog.show_modal(
                &ctx,
                egui::Id::new("tasks_folder_prompt"),
                |ui| {
                    let pal = ternarygrid_ui::pal(ui);
                    if let Some(parent) = &parent {
                        ui.label(RichText::new(trf("Inside {}", &[parent])).color(pal.text_muted));
                    }
                    let r = look::text_field(ui, &mut p.name, ui.available_width(), tr("Folder name"), false);
                    let pressed = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if pressed {
                        enter.set(true);
                    } else {
                        r.request_focus();
                    }
                },
                |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (glyph, label) = if renaming { (None, tr("Rename")) } else { (Some(icon::PLUS), tr("Create")) };
                        if ui.add_enabled_ui(ok_enabled, |ui| kit::button_ex(ui, Kind::Primary, glyph, label, false)).inner.clicked() {
                            ok.set(true);
                        }
                        if kit::button_ex(ui, Kind::Ghost, None, tr("Cancel"), false).clicked() {
                            cancel.set(true);
                        }
                    });
                },
            );
            let name = p.name.trim().trim_matches('/').to_string();
            let mut keep = !(cancel.get() || resp.close);
            if (ok.get() || enter.get()) && !name.is_empty() {
                match p.rename.clone() {
                    Some(old) => {
                        let new = match old.rsplit_once('/') {
                            Some((parent, _)) => format!("{parent}/{name}"),
                            None => name.clone(),
                        };
                        if self.folder.as_deref() == Some(old.as_str()) {
                            self.folder = Some(new.clone());
                        }
                        self.folder_op(cx, move |s| s.rename_folder(&old, &new).map(|_| ()));
                    }
                    None => {
                        let path = match &p.parent {
                            Some(parent) => format!("{parent}/{name}"),
                            None => name.clone(),
                        };
                        self.folder_op(cx, move |s| s.create_folder(&path));
                    }
                }
                keep = false;
            }
            if keep {
                self.folder_prompt = Some(p);
            }
        }
    }
}

impl TabView for TasksTab {
    fn title(&self) -> String {
        tr("Tasks").to_string()
    }
    fn icon(&self) -> &'static str {
        icon::LIST_CHECKS
    }
    fn dedupe_key(&self) -> Option<String> {
        Some("tasks".into())
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut AppContext) {
        if cx.services.tasks.is_none() {
            look::error(ui, tr("The task store is not available (see the warnings shown at start-up)."));
            return;
        }
        self.poll(cx);
        ui.spacing_mut().item_spacing.y = 0.0;
        self.banner(ui, cx);
        if cx.services.scheduler.is_none() {
            egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 0 }).show(ui, |ui| {
                look::note(ui, Tone::Info, tr("Tasks cannot be run: the task scheduler is not available."));
            });
        }
        self.toolbar(ui, cx);
        let p = ternarygrid_ui::pal(ui);
        let plain = egui::Frame::new().fill(p.canvas);
        egui::Panel::left(egui::Id::new("tasks_folder_panel")).resizable(true).default_size(210.0).frame(plain).show(ui, |ui| self.folders_ui(ui, cx));
        let history = egui::Frame::new().fill(p.canvas).inner_margin(egui::Margin { left: 16, right: 16, top: 2, bottom: 0 });
        egui::Panel::bottom(egui::Id::new("tasks_history_panel")).resizable(true).default_size(300.0).size_range(100.0..=f32::INFINITY).frame(history).show(ui, |ui| {
            // The panel keeps its height (egui's panels shrink to their contents).
            ui.set_min_height(ui.max_rect().height());
            self.history_ui(ui, cx)
        });
        let list = egui::Frame::new().fill(p.canvas).inner_margin(egui::Margin::symmetric(16, 12));
        egui::CentralPanel::default().frame(list).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            self.list_ui(ui, cx)
        });
        self.dialogs(ui, cx);
        if self.is_busy() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        } else if self.tasks.iter().any(|t| t.os_scheduler) {
            ui.ctx().request_repaint_after(EXTERNAL_REFRESH);
        }
    }

    fn on_action(&mut self, action: crate::keys::Action, cx: &mut AppContext) -> bool {
        match action {
            crate::keys::Action::Refresh => {
                self.reload_all(cx);
                true
            }
            _ => false,
        }
    }

    crate::impl_tab_any!();
}

impl Drop for TasksTab {
    fn drop(&mut self) {
        if let Some(t) = self.forwarder.take() {
            t.cancel();
        }
    }
}
