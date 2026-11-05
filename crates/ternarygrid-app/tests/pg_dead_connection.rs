//! Cursors whose connection died, as the idle watchdog finds them after the computer slept
//! (needs TERNARYGRID_TEST_PG). tokio-postgres repeats the "connection closed" error for ever, at
//! once: reading on used to spin a worker thread at full speed, unstoppable by any timeout,
//! allocating an error each time (a crash report of 0.6.3 stopped in macOS's allocator there).
//!
//! The stress run is not part of the normal test run; it is made for guard malloc, which stops
//! at the first write to freed memory or past an allocation:
//!
//! ```text
//! cargo test -p ternarygrid-app --test pg_dead_connection --no-run
//! TERNARYGRID_TEST_PG=postgres://… DYLD_INSERT_LIBRARIES=/usr/lib/libgmalloc.dylib \
//!     target/debug/deps/pg_dead_connection-… --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;
use ternarygrid_app::conn::session::SessionHandle;
use ternarygrid_core::driver::ConnectParams;
use ternarygrid_core::{DbResult, ExecOptions, Secret, Value};

// The type drivers connect with.
#[allow(clippy::result_large_err)]
async fn open(url: &str) -> DbResult<SessionHandle> {
    let (cfg, password) = ternarygrid_config::parse_connection_url(url).expect("the URL");
    let registry = ternarygrid_app::driver_registry();
    let password = password.map(|p| Secret::new(p.expose().to_string()));
    let s = registry.get(cfg.kind).unwrap().connect(&ConnectParams::from_config(&cfg, password)).await?;
    Ok(SessionHandle::new(s, "stress", cfg.database.clone()))
}

/// A long result: more than the socket buffers, so that the server is still sending.
const LONG: &str = "SELECT g, repeat('x', 200) AS pad, md5(g::text) AS m FROM generate_series(1, 200000) g";

/// A session with a cursor over `sql`, a few rows read: the watchdog's case.
async fn with_open_cursor(url: &str, sql: &str, params: &[Value]) -> (SessionHandle, u64, i64) {
    let s = open(url).await.expect("connected");
    let pid = backend_pid(&s).await;
    let ex = s.execute(sql, params, &ExecOptions::default()).await.expect("the query");
    s.fetch(ex.token, 50).await.expect("rows");
    (s, ex.token, pid)
}

/// The server process of `session`.
async fn backend_pid(session: &SessionHandle) -> i64 {
    let ex = session.execute("SELECT pg_backend_pid()", &[], &ExecOptions::default()).await.expect("the pid");
    let rows = session.fetch(ex.token, 2).await.expect("the pid's row");
    session.close_cursor_token(ex.token).await;
    rows.rows[0][0].to_text().parse().expect("a number")
}

#[test]
fn a_cursor_on_a_dead_connection_closes() {
    let Some(url) = std::env::var("TERNARYGRID_TEST_PG").ok().filter(|u| !u.trim().is_empty()) else { return };
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    rt.block_on(async move {
        let admin = open(&url).await.expect("the admin session");
        let cases: [(&str, &str, Vec<Value>); 3] = [
            ("simple", LONG, Vec::new()),
            ("extended", "SELECT g, repeat('x', 200) AS pad FROM generate_series(1, $1::int) g", vec![Value::Int(200_000)]),
            ("copy", "COPY (SELECT g, repeat('x', 200) FROM generate_series(1, 200000) g) TO STDOUT", Vec::new()),
        ];
        for (name, sql, params) in cases {
            let (s, token, pid) = with_open_cursor(&url, sql, &params).await;
            admin.execute_update(&format!("SELECT pg_terminate_backend({pid})"), &[]).await.ok();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let close = tokio::spawn(async move { s.close_cursor_token(token).await });
            if tokio::time::timeout(Duration::from_secs(10), close).await.is_err() {
                // A worker spins: dropping the runtime would wait for it for ever.
                eprintln!("closing a {name} cursor on a dead connection never ends");
                std::process::exit(101);
            }
        }
    });
}

#[test]
#[ignore = "a stress run for guard malloc; needs TERNARYGRID_TEST_PG"]
fn idle_cursors_close_at_once_after_sleep() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let url: Arc<str> = std::env::var("TERNARYGRID_TEST_PG").expect("TERNARYGRID_TEST_PG").into();
    let rounds: usize = std::env::var("STRESS_ROUNDS").ok().and_then(|r| r.parse().ok()).unwrap_or(40);
    // As the application's runtime: a few worker threads.
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    rt.block_on(async move {
        let admin = open(&url).await.expect("the admin session");
        for round in 0..rounds {
            let mut open_ones = Vec::new();
            for _ in 0..6 {
                open_ones.push(with_open_cursor(&url, LONG, &[]).await);
            }
            // Connections that died while the computer slept.
            for (i, (_, _, pid)) in open_ones.iter().enumerate() {
                if (i + round) % 3 == 0 {
                    admin.execute_update(&format!("SELECT pg_terminate_backend({pid})"), &[]).await.ok();
                }
            }
            // The watchdogs' timers all fire at once.
            let closes: Vec<_> = open_ones.into_iter().map(|(s, token, _)| tokio::spawn(async move { s.close_cursor_token(token).await })).collect();
            for c in closes {
                c.await.expect("no panic");
            }
            if round % 5 == 0 {
                eprintln!("round {round}");
            }
        }
    });
}
