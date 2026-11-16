//! Running vendor command line tools (pg_dump, mysqldump, sqlcmd ...) for
//! backup, restore and "execute in native client" (FR-A2).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use ternarygrid_core::admin::{BackupFormat, BackupOptions, CommandSpec};
use ternarygrid_core::{DbError, DbResult};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

/// Run `spec` to completion. Standard error (and standard output when it is
/// not redirected to a file) is passed line by line to `on_line`; secrets in
/// the environment are never logged. Cancelling kills the process.
pub async fn run_command(spec: CommandSpec, cancel: CancellationToken, mut on_line: impl FnMut(String) + Send) -> DbResult<()> {
    let mut cmd = tokio::process::Command::new(&spec.program);
    cmd.args(&spec.args).kill_on_drop(true).stderr(Stdio::piped());
    for (k, v) in &spec.env {
        cmd.env(k, v.expose());
    }
    match &spec.stdin_file {
        Some(p) => {
            let f = std::fs::File::open(p).map_err(|e| DbError::io(format!("{}: {e}", p.display())))?;
            cmd.stdin(Stdio::from(f));
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }
    let piped_stdout = match &spec.stdout_file {
        Some(p) => {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let f = std::fs::File::create(p).map_err(|e| DbError::io(format!("{}: {e}", p.display())))?;
            cmd.stdout(Stdio::from(f));
            false
        }
        None => {
            cmd.stdout(Stdio::piped());
            true
        }
    };
    let mut child = cmd.spawn().map_err(|e| DbError::io(format!("Cannot start {}: {e}. Install the client tools or set the tool path in the dialog.", spec.program)))?;
    let stderr = child.stderr.take();
    let stdout = if piped_stdout { child.stdout.take() } else { None };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    if let Some(e) = stderr {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(e).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let _ = tx.send(l);
            }
        });
    }
    if let Some(o) = stdout {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(o).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let _ = tx.send(l);
            }
        });
    }
    drop(tx);
    let mut last_lines: Vec<String> = Vec::new();
    let status = loop {
        tokio::select! {
            line = rx.recv() => match line {
                Some(l) => {
                    last_lines.push(l.clone());
                    if last_lines.len() > 20 {
                        last_lines.remove(0);
                    }
                    on_line(l);
                }
                None => break child.wait().await,
            },
            s = child.wait() => {
                // The readers may still hold the last lines (often the error itself).
                while let Ok(Some(l)) = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                    last_lines.push(l.clone());
                    if last_lines.len() > 20 {
                        last_lines.remove(0);
                    }
                    on_line(l);
                }
                break s;
            }
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(DbError::cancelled());
            }
        }
    };
    let status = status.map_err(DbError::from)?;
    if status.success() { Ok(()) } else { Err(DbError::io(format!("{} exited with {status}", spec.program)).with_detail(last_lines.join("\n"))) }
}

/// A backup goes to a file next to its own (`<file>.partial`), moved over it once the tool
/// succeeded: written in place, a failed or cancelled run (a dropped connection, Stop) left
/// a cut file where the last good backup was. `None` for a directory backup (pg_dump -Fd).
pub fn backup_staging(options: &BackupOptions) -> Option<(BackupOptions, PathBuf)> {
    if options.format == BackupFormat::Directory {
        return None;
    }
    let name = options.output.file_name()?.to_string_lossy().into_owned();
    let mut staged = options.clone();
    staged.output = options.output.with_file_name(format!("{name}.partial"));
    Some((staged, options.output.clone()))
}

/// End a staged backup: moved into place when it succeeded, removed otherwise.
pub fn finish_backup(staged: &Path, output: &Path, succeeded: bool) -> DbResult<()> {
    if succeeded {
        std::fs::rename(staged, output).map_err(|e| DbError::io(format!("Cannot move the backup to {}: {e}", output.display())))
    } else {
        let _ = std::fs::remove_file(staged);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_replaces_its_file_only_when_done() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("nightly.sql");
        std::fs::write(&out, "last good backup").unwrap();
        let (staged, target) = backup_staging(&BackupOptions { output: out.clone(), ..Default::default() }).unwrap();
        assert_eq!(staged.output, dir.path().join("nightly.sql.partial"));
        std::fs::write(&staged.output, "cut").unwrap();
        finish_backup(&staged.output, &target, false).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "last good backup");
        assert!(!staged.output.exists());
        std::fs::write(&staged.output, "new backup").unwrap();
        finish_backup(&staged.output, &target, true).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "new backup");
        assert!(backup_staging(&BackupOptions { output: out, format: BackupFormat::Directory, ..Default::default() }).is_none());
    }
}
