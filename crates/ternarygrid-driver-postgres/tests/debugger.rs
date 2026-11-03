//! Debugger integration test (pldbgapi), kept in its own test binary so it never
//! runs concurrently with DDL heavy tests: pldebugger aborts its socket waits on
//! signals such as the barrier every backend receives on DROP DATABASE.
//!
//! Runs against `TERNARYGRID_TEST_PGDBG` (a server with pldebugger installed), else
//! `TERNARYGRID_TEST_PG` where it is skipped when the extension is missing. Without
//! the variables the test returns immediately (and passes).

#![allow(clippy::result_large_err)]

use std::time::Duration;
use ternarygrid_core::meta::{ObjectKind, ObjectRef};
use ternarygrid_core::{ErrorKind, Value};

mod common;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn debugger_steps_through_plpgsql() {
    let Some(url) = dbg_url().or_else(url) else { return };
    let params = params_from_url(&url);
    with_schema(&params, |params, schema| async move {
        let mut s = connect(&params).await;
        exec(
            &mut s,
            &format!(
                "CREATE FUNCTION {schema}.dbg_add(a int, b int) RETURNS int LANGUAGE plpgsql AS $$
declare
  s int := 0;
begin
  s := a + b;
  raise notice 'sum is %', s;
  s := s * 2;
  return s;
end;
$$"
            ),
        )
        .await;
        let f = ObjectRef::new(ObjectKind::Function, Some(schema.clone()), "dbg_add").with_signature("(integer, integer)");
        let mut dbg = match s.debug_routine(&f, &["2".into(), "3".into()]).await {
            Ok(d) => d,
            Err(e) if e.kind == ErrorKind::Unsupported => {
                eprintln!("debugger not available on this server: {}", e.message);
                s.close().await.ok();
                return;
            }
            Err(e) => panic!("{}", e.full_text()),
        };
        assert!(dbg.source().contains("s := a + b;"));
        let st = dbg.wait().await.unwrap();
        assert!(!st.finished);
        assert_eq!(st.line, Some(5), "first statement: {st:?}");
        let vars = dbg.variables().await.unwrap();
        assert!(vars.iter().any(|v| v.name == "a" && v.value == "2" && v.is_argument));
        assert!(vars.iter().any(|v| v.name == "s" && v.value == "0" && !v.is_argument && v.data_type == "integer"));
        let stack = dbg.stack().await.unwrap();
        assert_eq!(stack[0].line, 5);
        assert!(stack[0].routine.contains("dbg_add"), "{stack:?}");
        let st = dbg.step_over().await.unwrap();
        assert_eq!(st.line, Some(6));
        let st = dbg.step_over().await.unwrap();
        assert_eq!(st.line, Some(7));
        assert!(st.messages.iter().any(|m| m.contains("sum is 5")), "{st:?}");
        dbg.set_variable("s", "100").await.unwrap();
        dbg.set_breakpoint(8).await.unwrap();
        let st = dbg.resume().await.unwrap();
        assert_eq!(st.line, Some(8));
        assert!(dbg.variables().await.unwrap().iter().any(|v| v.name == "s" && v.value == "200"));
        dbg.clear_breakpoint(8).await.unwrap();
        let st = dbg.resume().await.unwrap();
        assert!(st.finished, "{st:?}");
        let result = st.result.expect("result");
        assert_eq!(result.rows, vec![vec![Value::text("200")]]);
        dbg.abort().await.unwrap();

        // aborting a stopped routine
        let mut dbg = s.debug_routine(&f, &["1".into(), "1".into()]).await.unwrap();
        let st = dbg.wait().await.unwrap();
        assert_eq!(st.line, Some(5));
        dbg.abort().await.unwrap();
        // a routine failing before the first breakpoint ends the session cleanly
        let mut dbg = s.debug_routine(&f, &["'x'".into(), "1".into()]).await.unwrap();
        let st = tokio::time::timeout(Duration::from_secs(20), dbg.wait()).await.expect("no hang").unwrap();
        assert!(st.finished);
        assert!(st.messages.iter().any(|m| m.starts_with("ERROR")), "{st:?}");
        dbg.abort().await.unwrap();

        // on a read-only connection the debugged routine cannot write either
        exec(&mut s, &format!("CREATE TABLE {schema}.dbg_t (a int)")).await;
        exec(&mut s, &format!("CREATE FUNCTION {schema}.dbg_ins() RETURNS int LANGUAGE plpgsql AS $$\nbegin\n  insert into {schema}.dbg_t values (1);\n  return 1;\nend;\n$$"))
            .await;
        let mut ro_params = params.clone();
        ro_params.read_only = true;
        let mut ro = connect(&ro_params).await;
        let g = ObjectRef::new(ObjectKind::Function, Some(schema.clone()), "dbg_ins").with_signature("()");
        let mut dbg = ro.debug_routine(&g, &[]).await.unwrap();
        let st = tokio::time::timeout(Duration::from_secs(20), dbg.wait()).await.expect("no hang").unwrap();
        if !st.finished {
            let st = tokio::time::timeout(Duration::from_secs(20), dbg.resume()).await.expect("no hang").unwrap();
            assert!(st.finished, "{st:?}");
        }
        dbg.abort().await.unwrap();
        assert_eq!(scalar(&mut s, &format!("SELECT count(*) FROM {schema}.dbg_t")).await, Value::Int(0));
        ro.close().await.ok();
        s.close().await.ok();
    })
    .await;
}
