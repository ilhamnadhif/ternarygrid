//! Running local shell commands: "shell command before connect", "after
//! disconnect" and "password from command" ([`ternarygrid_core::config::AuthMethod::Command`]).
//!
//! Commands run through `sh -c` on Unix and `cmd /C` on Windows, with stdin
//! closed. On timeout or cancellation (the future is dropped, e.g. a
//! cancelled task) the whole process group (Unix) / process tree (Windows)
//! is killed so helper processes spawned by the command do not linger.
//! Helpers a command leaves running after it *finished* (`ssh -fN …`) are
//! left alone.

use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ternarygrid_core::{DbError, DbResult, ErrorKind, Secret};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use zeroize::Zeroizing;

/// Maximum number of bytes kept from stdout / stderr; the rest is drained and discarded.
const MAX_CAPTURE: usize = 1024 * 1024;
/// Maximum number of stderr characters quoted in error messages (not for
/// password commands, whose stderr is never shown).
const MAX_STDERR_IN_ERROR: usize = 2000;

struct CommandOutput {
    status: ExitStatus,
    stdout: Zeroizing<Vec<u8>>,
    stderr: Vec<u8>,
}

/// Runs `command` in the platform shell and returns its standard output with
/// surrounding whitespace trimmed.
///
/// Errors:
/// * [`ErrorKind::Config`] for an empty command,
/// * [`ErrorKind::Io`] when the shell cannot be started or the command exits
///   with a non-zero status (the message includes the exit code and stderr),
/// * [`ErrorKind::Timeout`] when it runs longer than `timeout` (the process
///   and its process group are killed).
pub async fn run_shell_command(command: &str, timeout: Duration) -> DbResult<String> {
    let out = run_capture(command, timeout).await?;
    check_status(&out, true)?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Runs a "password command" and returns the first line of its standard
/// output as a [`Secret`] (trailing `\r`/`\n` removed, other whitespace kept).
///
/// The output is never logged or included in error messages, stderr
/// included (a helper may print the password there, e.g. with `set -x` or
/// in a usage message): a failure reports the exit code only. Captured
/// buffers are wiped after use. An empty first line is an [`ErrorKind::Auth`] error.
pub async fn password_from_command(command: &str, timeout: Duration) -> DbResult<Secret> {
    let out = run_capture(command, timeout).await?;
    check_status(&out, false)?;
    let text = std::str::from_utf8(&out.stdout)
        .map_err(|_| DbError::auth("The password command printed non UTF-8 output"))?;
    let first = text
        .lines()
        .next()
        .unwrap_or("")
        .trim_end_matches(['\r', '\n']);
    if first.is_empty() {
        return Err(DbError::auth(
            "The password command printed an empty password",
        ));
    }
    Ok(Secret::new(first))
}

fn check_status(out: &CommandOutput, show_stderr: bool) -> DbResult<()> {
    if out.status.success() {
        return Ok(());
    }
    let code = match out.status.code() {
        Some(c) => format!("exit code {c}"),
        None => "a signal".to_string(),
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr = stderr.trim();
    let mut msg = format!("Shell command failed with {code}");
    if !show_stderr {
        msg.push_str(
            " (its error output is not shown because it may contain the password; run the command in a terminal to see it)",
        );
    } else if !stderr.is_empty() {
        let shown: String = stderr.chars().take(MAX_STDERR_IN_ERROR).collect();
        msg.push_str(": ");
        msg.push_str(&shown);
    }
    Err(DbError::io(msg))
}

/// Kills the command's process group / tree when dropped while armed. A
/// cancelled caller drops the future while the command runs, and
/// `kill_on_drop` alone only kills the shell, not what it started. Declared
/// after the child so it runs first, while the child is not yet reaped.
struct TreeKill {
    pid: Option<u32>,
    armed: bool,
}

impl Drop for TreeKill {
    fn drop(&mut self) {
        if self.armed {
            kill_process_group(self.pid);
        }
    }
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C");
        // Pass the command line verbatim; cmd.exe does its own parsing.
        cmd.raw_arg(command);
        // CREATE_NO_WINDOW: do not flash a console window from a GUI app.
        cmd.creation_flags(0x0800_0000);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

/// Read `reader` to its end into `out` (what exceeds [`MAX_CAPTURE`] is read and dropped).
async fn read_capped<R: AsyncRead + Unpin>(reader: Option<R>, out: Arc<Mutex<Zeroizing<Vec<u8>>>>) {
    let Some(mut reader) = reader else { return };
    let mut buf = Zeroizing::new([0u8; 8192]);
    loop {
        match reader.read(&mut buf[..]).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
                let room = MAX_CAPTURE.saturating_sub(out.len());
                out.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
}

async fn run_capture(command: &str, timeout: Duration) -> DbResult<CommandOutput> {
    if command.trim().is_empty() {
        return Err(DbError::config("The shell command is empty"));
    }
    let mut cmd = shell_command(command);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| DbError::io(format!("Cannot start the shell command: {e}")))?;
    let pid = child.id();
    let mut tree = TreeKill { pid, armed: true };
    let stdout_buf = Arc::new(Mutex::new(Zeroizing::new(Vec::new())));
    let stderr_buf = Arc::new(Mutex::new(Zeroizing::new(Vec::new())));
    // Read in tasks of their own that outlive this call: a helper the command
    // started in the background (`vpn-up &`, `ssh -fN …`) keeps the pipes open,
    // and would die of SIGPIPE writing to them once nobody reads.
    let mut out_task = tokio::spawn(read_capped(child.stdout.take(), stdout_buf.clone()));
    let mut err_task = tokio::spawn(read_capped(child.stderr.take(), stderr_buf.clone()));

    let result = tokio::time::timeout(timeout, async {
        // Done when the command exits, with what it printed until shortly after.
        let status = child.wait().await;
        // Helpers it left running in the background are intentional.
        tree.armed = false;
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            let _ = (&mut out_task).await;
            let _ = (&mut err_task).await;
        })
        .await;
        status
    })
    .await;
    let take = |b: &Arc<Mutex<Zeroizing<Vec<u8>>>>| {
        std::mem::take(&mut *b.lock().unwrap_or_else(|e| e.into_inner()))
    };

    match result {
        Ok(Ok(status)) => Ok(CommandOutput {
            status,
            stdout: take(&stdout_buf),
            stderr: take(&stderr_buf).to_vec(),
        }),
        Ok(Err(e)) => Err(DbError::io(format!(
            "Waiting for the shell command failed: {e}"
        ))),
        Err(_) => {
            tree.armed = false;
            kill_process_group(pid);
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            Err(DbError::new(
                ErrorKind::Timeout,
                format!(
                    "The shell command did not finish within {}s and was killed",
                    timeout.as_secs_f32()
                ),
            ))
        }
    }
}

#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        // SAFETY: killpg only sends a signal; the process group was created
        // for this child with `process_group(0)` so its id equals the pid.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
}

/// Windows: `taskkill /T` ends the shell and its descendants (the shell is
/// still running and its handle open, so the id is not reused).
#[cfg(windows)]
fn kill_process_group(pid: Option<u32>) {
    use std::os::windows::process::CommandExt;
    if let Some(pid) = pid {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // CREATE_NO_WINDOW
            .creation_flags(0x0800_0000)
            .status();
    }
}

#[cfg(not(any(unix, windows)))]
fn kill_process_group(_pid: Option<u32>) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn captures_trimmed_stdout() {
        let out = run_shell_command("printf '  hello world \\n\\n'", Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(out, "hello world");
    }

    #[tokio::test]
    async fn non_zero_exit_reports_stderr() {
        let err = run_shell_command("echo boom >&2; exit 3", Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Io);
        assert!(err.message.contains("exit code 3"), "{}", err.message);
        assert!(err.message.contains("boom"), "{}", err.message);
    }

    #[tokio::test]
    async fn empty_command_is_config_error() {
        let err = run_shell_command("   ", Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Config);
    }

    #[tokio::test]
    async fn timeout_kills_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        // The background grandchild would create the marker after 2s if it was not killed.
        let cmd = format!("(sleep 2; touch '{}') & sleep 30", marker.display());
        let started = Instant::now();
        let err = run_shell_command(&cmd, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Timeout);
        assert!(started.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "grandchild process was not killed");
    }

    #[tokio::test]
    async fn cancellation_kills_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        // A cancelled task drops the future while the command runs.
        let cmd = format!("(sleep 2; touch '{}') & sleep 30", marker.display());
        let run = run_shell_command(&cmd, Duration::from_secs(60));
        assert!(
            tokio::time::timeout(Duration::from_millis(300), run)
                .await
                .is_err()
        );
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "background child survived cancellation");
    }

    #[tokio::test]
    async fn helpers_left_by_a_finished_command_keep_running() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("helper-ran");
        let cmd = format!(
            "(sleep 1; touch '{}') >/dev/null 2>&1 & echo started",
            marker.display()
        );
        let out = run_shell_command(&cmd, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(out, "started");
        tokio::time::sleep(Duration::from_millis(1800)).await;
        assert!(
            marker.exists(),
            "an intentional background helper was killed"
        );
    }

    #[tokio::test]
    async fn password_command_stderr_is_not_shown() {
        let err = password_from_command(
            "echo 'usage: helper --password hunter2' >&2; exit 2",
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Io);
        assert!(err.message.contains("exit code 2"), "{}", err.message);
        assert!(!err.full_text().contains("hunter2"), "{}", err.full_text());
    }

    #[tokio::test]
    async fn password_command_takes_first_line() {
        let s = password_from_command(
            "printf 's3cr:et \\nsecond line\\n'",
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert_eq!(s.expose(), "s3cr:et ");
        let err = password_from_command("printf ''", Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Auth);
        assert!(!format!("{err:?}").contains("s3cr"));
    }

    #[tokio::test]
    async fn password_never_in_error() {
        // Non-zero exit: stdout (which could be a password) must not appear in the error.
        let err = password_from_command("echo topsecret; exit 1", Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(!err.full_text().contains("topsecret"));
    }
}
