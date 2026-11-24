//! Text transformations of the SQL editor (DBeaver's "Morph to delimited
//! list", "Trim spaces", "Search in web").

/// The non-empty lines of `text` as a comma separated list: numbers as they
/// are, everything else as quoted strings (`'` doubled).
pub fn morph_to_delimited(text: &str) -> String {
    let items: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| l.trim_end_matches(',')).collect();
    let numeric = !items.is_empty() && items.iter().all(|i| i.parse::<f64>().is_ok_and(f64::is_finite));
    items.iter().map(|i| if numeric { i.to_string() } else { format!("'{}'", i.trim_matches('\'').replace('\'', "''")) }).collect::<Vec<_>>().join(", ")
}

/// `text` without spaces and tabs at the end of its lines.
pub fn trim_trailing(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let (body, cr) = match line.strip_suffix('\r') {
            Some(b) => (b, true),
            None => (line, false),
        };
        out.push_str(body.trim_end_matches([' ', '\t']));
        if cr {
            out.push('\r');
        }
    }
    out
}

/// A web search URL for `query`.
pub fn search_url(query: &str) -> String {
    let mut q = String::new();
    for b in query.trim().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => q.push(b as char),
            b' ' => q.push('+'),
            _ => q.push_str(&format!("%{b:02X}")),
        }
    }
    format!("https://www.google.com/search?q={q}")
}

/// Run an external formatter: `command` (through the shell) gets `sql` on
/// stdin and must print the formatted SQL; at most 10 seconds.
pub fn run_formatter(command: &str, sql: &str) -> Result<String, String> {
    run_formatter_timeout(command, sql, std::time::Duration::from_secs(10))
}

/// Kill the formatter shell and every descendant process it spawned, then reap
/// the shell so no zombie or open pipe outlives a timed-out / failed run.
fn kill_and_reap(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        if pid > 0 {
            // SAFETY: `child` was spawned in its own process group (`process_group(0)`),
            // so `pid` is the PGID of the shell and all of its descendants.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let pid = child.id();
        if pid > 0 {
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                // CREATE_NO_WINDOW
                .creation_flags(0x0800_0000)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn run_formatter_timeout(command: &str, sql: &str, timeout: std::time::Duration) -> Result<String, String> {
    use std::io::Write;
    use std::process::Command;
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn_piped().map_err(|e| e.to_string())?;
    let Some(mut stdin) = child.stdin.take() else {
        kill_and_reap(&mut child);
        return Err("no stdin".into());
    };
    let input = sql.to_string();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    // Read while it runs: a formatter blocks once its output fills the pipe.
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() >= deadline => {
                kill_and_reap(&mut child);
                let _ = writer.join();
                let _ = stdout.join();
                let _ = stderr.join();
                return Err(format!("the formatter did not finish within {} seconds", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => {
                kill_and_reap(&mut child);
                let _ = writer.join();
                let _ = stdout.join();
                let _ = stderr.join();
                return Err(e.to_string());
            }
        }
    };
    let _ = writer.join();
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    if !status.success() {
        let err = String::from_utf8_lossy(&err).trim().to_string();
        return Err(if err.is_empty() { format!("exit status {status}") } else { err });
    }
    String::from_utf8(out).map_err(|_| "the formatter printed invalid UTF-8".to_string())
}

trait SpawnPiped {
    fn spawn_piped(&mut self) -> std::io::Result<std::process::Child>;
}

impl SpawnPiped for std::process::Command {
    fn spawn_piped(&mut self) -> std::io::Result<std::process::Child> {
        use std::process::Stdio;
        self.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
    }
}

/// `stmt` wrapped to count its rows.
pub fn row_count_sql(stmt: &str) -> String {
    let body = stmt.trim().trim_end_matches(';').trim_end();
    format!("SELECT COUNT(*) AS row_count FROM (\n{body}\n) ternarygrid_count")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn morph() {
        assert_eq!(morph_to_delimited("Ani\nBudi\n\n  O'Neil  \n"), "'Ani', 'Budi', 'O''Neil'");
        assert_eq!(morph_to_delimited("1\n2,\n 3.5"), "1, 2, 3.5");
        assert_eq!(morph_to_delimited("'a'\n'b'"), "'a', 'b'");
        assert_eq!(morph_to_delimited(""), "");
        // Non-finite float tokens must be quoted as SQL strings, never emitted bare.
        assert_eq!(morph_to_delimited("NaN\n1"), "'NaN', '1'");
        assert_eq!(morph_to_delimited("inf\n-inf\nInfinity\n-Infinity"), "'inf', '-inf', 'Infinity', '-Infinity'");
    }

    #[test]
    fn trim() {
        assert_eq!(trim_trailing("select 1;  \n\tfrom t\t\r\nx "), "select 1;\n\tfrom t\r\nx");
    }

    #[cfg(unix)]
    #[test]
    fn external_formatter() {
        assert_eq!(run_formatter("tr a-z A-Z", "select 1 from t").unwrap(), "SELECT 1 FROM T");
        assert!(run_formatter("exit 3", "x").is_err());
        assert!(run_formatter("echo bad >&2; exit 1", "x").unwrap_err().contains("bad"));
    }

    #[cfg(unix)]
    #[test]
    fn external_formatter_timeout_kills_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let cmd = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        let started = std::time::Instant::now();
        let err = run_formatter_timeout(&cmd, "select 1", std::time::Duration::from_millis(300)).unwrap_err();
        assert!(err.contains("did not finish"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "reader threads should not hang on leaked child pipes");
        let bg_pid: libc::pid_t = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        let dead = (0..50).any(|_| {
            // SAFETY: signal 0 only checks whether `bg_pid` still exists.
            if unsafe { libc::kill(bg_pid, 0) } != 0 {
                true
            } else {
                std::thread::sleep(std::time::Duration::from_millis(20));
                false
            }
        });
        assert!(dead, "background descendant pid {bg_pid} was leaked after formatter timeout");
    }

    #[test]
    fn urls_and_count() {
        assert_eq!(search_url("pg_stat_activity wait_event"), "https://www.google.com/search?q=pg_stat_activity+wait_event");
        assert_eq!(search_url("a&b=c"), "https://www.google.com/search?q=a%26b%3Dc");
        assert_eq!(row_count_sql("select * from t;\n"), "SELECT COUNT(*) AS row_count FROM (\nselect * from t\n) ternarygrid_count");
    }
}
