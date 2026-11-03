//! Integration tests against a live Redis.
//!
//! Set `TERNARYGRID_TEST_REDIS=redis://[user:password@]host:port` (for example
//! `redis://127.0.0.1:56379` with the `redis:7` image) to run them; without
//! it every test returns immediately. The tests use database 9 and flush it.

use ternarygrid_core::config::{ConnectionConfig, SslMode};
use ternarygrid_core::driver::{execute_update, query_all, query_scalar};
use ternarygrid_core::meta::*;
use ternarygrid_core::{
    ConnectParams, DbKind, Driver, ExecOptions, ResultHeader, Secret, Session, Value,
};
use ternarygrid_driver_redis::RedisDriver;

fn params() -> Option<ConnectParams> {
    let url = std::env::var("TERNARYGRID_TEST_REDIS")
        .ok()
        .filter(|u| !u.trim().is_empty())?;
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(&url);
    let (auth, hostport) = match rest.rsplit_once('@') {
        Some((a, h)) => (Some(a), h),
        None => (None, rest),
    };
    let hostport = hostport.split('/').next().unwrap_or(hostport);
    let (host, port) = hostport
        .rsplit_once(':')
        .map(|(h, p)| (h.to_string(), p.parse().ok()))
        .unwrap_or((hostport.to_string(), None));
    let mut cfg = ConnectionConfig::new(DbKind::Redis, "test");
    cfg.host = host;
    cfg.port = port.or(Some(6379));
    cfg.database = Some("9".into());
    cfg.ssl.mode = SslMode::Disable;
    let mut password = None;
    if let Some((u, p)) = auth.and_then(|a| a.split_once(':')) {
        cfg.user = Some(u.to_string()).filter(|u| !u.is_empty());
        password = Some(Secret::new(p));
    }
    Some(ConnectParams::from_config(&cfg, password))
}

async fn connect() -> Option<Box<dyn Session>> {
    Some(
        RedisDriver::new()
            .connect(&params()?)
            .await
            .expect("connect"),
    )
}

async fn run(s: &mut dyn Session, cmd: &str) {
    execute_update(s, cmd, &[])
        .await
        .unwrap_or_else(|e| panic!("{cmd}: {}", e.full_text()));
}

/// All rows of a command, fetched in pages of `page`.
async fn rows(s: &mut dyn Session, cmd: &str, page: usize) -> (Vec<String>, Vec<Vec<Value>>) {
    let mut c = s
        .execute(cmd, &[], &ExecOptions::default())
        .await
        .unwrap_or_else(|e| panic!("{cmd}: {}", e.full_text()));
    let Some(ResultHeader::Rows(cols)) = c.next_result().await.unwrap() else {
        panic!("{cmd}: no rows")
    };
    let mut out = Vec::new();
    loop {
        let batch = c.fetch(page).await.unwrap();
        if batch.is_empty() {
            break;
        }
        assert!(batch.len() <= page);
        out.extend(batch.into_iter().map(|r| r.to_vec()));
    }
    (cols.into_iter().map(|c| c.name).collect(), out)
}

#[tokio::test]
async fn commands_values_and_navigator() {
    let Some(mut s) = connect().await else { return };
    assert!(matches!(
        s.server_info().product.as_str(),
        "Redis" | "Valkey" | "Dragonfly"
    ));
    run(s.as_mut(), "FLUSHDB").await;
    run(s.as_mut(), "SET greeting \"hello world\"").await;
    run(s.as_mut(), "HSET user:1 name Ayu city Jakarta").await;
    run(s.as_mut(), "RPUSH queue a b c").await;
    run(s.as_mut(), "SADD tags red green").await;
    run(s.as_mut(), "ZADD board 10 ayu 25 budi").await;
    run(s.as_mut(), "XADD events 1-0 kind login").await;
    run(s.as_mut(), "EXPIRE greeting 1000").await;
    assert_eq!(
        query_scalar(s.as_mut(), "GET greeting", &[]).await.unwrap(),
        Value::Text("hello world".into())
    );
    let (cols, r) = rows(s.as_mut(), "HGETALL user:1", 100).await;
    assert_eq!(cols, ["field", "value"]);
    assert_eq!(r.len(), 2);
    let (cols, r) = rows(s.as_mut(), "ZRANGE board 0 -1 WITHSCORES", 100).await;
    assert_eq!(cols, ["member", "score"]);
    assert_eq!(r[1], vec![Value::Text("budi".into()), Value::Float(25.0)]);
    let (cols, r) = rows(s.as_mut(), "INFO server", 1000).await;
    assert_eq!(cols, ["section", "field", "value"]);
    assert!(
        r.iter()
            .any(|row| row[1] == Value::Text("redis_version".into()))
    );
    // The navigator: databases with keys, keys with their types.
    let dbs = s.list_schemas().await.unwrap();
    assert!(dbs.iter().any(|d| d.name == "db9" && d.is_current));
    let keys = s
        .list_objects(&SchemaRef::schema("db9"), ObjectKind::Table)
        .await
        .unwrap();
    let types: Vec<(String, String)> = keys
        .iter()
        .map(|k| (k.obj.name.clone(), k.detail.clone().unwrap_or_default()))
        .collect();
    assert_eq!(
        types,
        [
            ("board", "zset"),
            ("events", "stream"),
            ("greeting", "string"),
            ("queue", "list"),
            ("tags", "set"),
            ("user:1", "hash")
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    // Each key read through the dialect, as the data tab does, and counted.
    let d = s.dialect();
    for k in &keys {
        let read = d.select_table(&k.obj, None, None, None, None);
        let (_, r) = rows(s.as_mut(), &read, 2).await;
        let count = query_scalar(s.as_mut(), &d.count_query(&read), &[])
            .await
            .unwrap();
        assert_eq!(Value::Int(r.len() as i64), count, "{read}");
    }
    let ddl = s.object_ddl(&keys[5].obj).await.unwrap();
    assert!(ddl.contains("HSET user:1 name Ayu"), "{ddl}");
    let props = s.object_properties(&keys[2].obj).await.unwrap();
    assert!(
        props.iter().any(|p| p.name == "TTL" && p.value != "none"),
        "{props:?}"
    );
    assert!(
        s.server_metrics()
            .await
            .unwrap()
            .iter()
            .any(|m| m.key == "connected_clients")
    );
    assert!(!s.list_sessions().await.unwrap().data.is_empty());
    let e = query_all(s.as_mut(), "NOSUCHCOMMAND x", &[])
        .await
        .unwrap_err();
    assert_eq!(e.kind, ternarygrid_core::ErrorKind::Syntax, "{e:?}");
    run(s.as_mut(), "FLUSHDB").await;
}

#[tokio::test]
async fn big_values_are_paged() {
    let Some(mut s) = connect().await else { return };
    // Database 8: the other test flushes 9.
    run(s.as_mut(), "SELECT 8").await;
    run(s.as_mut(), "DEL big:h big:l big:z big:s").await;
    for chunk in 0..5 {
        let fields: Vec<String> = (0..1000)
            .map(|i| format!("f{} {}", chunk * 1000 + i, i))
            .collect();
        run(s.as_mut(), &format!("HSET big:h {}", fields.join(" "))).await;
        let items: Vec<String> = (0..1000)
            .map(|i| format!("v{}", chunk * 1000 + i))
            .collect();
        run(s.as_mut(), &format!("RPUSH big:l {}", items.join(" "))).await;
        run(s.as_mut(), &format!("SADD big:s {}", items.join(" "))).await;
        let scored: Vec<String> = (0..1000)
            .map(|i| format!("{} m{}", chunk * 1000 + i, chunk * 1000 + i))
            .collect();
        run(s.as_mut(), &format!("ZADD big:z {}", scored.join(" "))).await;
    }
    for (cmd, n) in [
        ("HSCAN big:h 0 COUNT 1000", 5000),
        ("LRANGE big:l 0 -1", 5000),
        ("SSCAN big:s 0", 5000),
        ("ZRANGE big:z 0 -1 WITHSCORES", 5000),
        ("LRANGE big:l 10 2509", 2500),
        ("SCAN 0 MATCH big:*", 4),
    ] {
        let (_, r) = rows(s.as_mut(), cmd, 777).await;
        assert_eq!(r.len(), n, "{cmd}");
    }
    let (_, r) = rows(s.as_mut(), "LRANGE big:l 0 -1", 5000).await;
    assert_eq!(r[4999], vec![Value::Int(4999), Value::Text("v4999".into())]);
    let (_, r) = rows(s.as_mut(), "ZRANGE big:z 0 -1 WITHSCORES", 5000).await;
    assert_eq!(
        r[1234],
        vec![Value::Text("m1234".into()), Value::Float(1234.0)]
    );
    run(s.as_mut(), "DEL big:h big:l big:z big:s").await;
}

#[tokio::test]
async fn counts_match_the_rows() {
    let Some(mut s) = connect().await else { return };
    // Database 7: the other tests use 8 and 9.
    run(s.as_mut(), "SELECT 7").await;
    run(s.as_mut(), "FLUSHDB").await;
    let items: Vec<String> = (0..50).map(|i| format!("v{i}")).collect();
    run(s.as_mut(), &format!("RPUSH l {}", items.join(" "))).await;
    for i in 0..30 {
        run(s.as_mut(), &format!("SET user:{i} x")).await;
    }
    run(s.as_mut(), "SET other y").await;
    run(s.as_mut(), "HSET h f1 1 f2 2 g1 3").await;
    // More stream entries than one page, with ids sharing a millisecond.
    for chunk in 0..3 {
        let entries: Vec<String> = (0..700)
            .map(|i| format!("XADD st 1-{} n {i}", chunk * 700 + i + 1))
            .collect();
        for e in entries {
            run(s.as_mut(), &e).await;
        }
    }
    let d = s.dialect();
    for (cmd, n) in [
        ("SCAN 0 MATCH user:*", 30),
        ("SCAN 0 MATCH user:* COUNT 5", 30),
        ("SCAN 0", 34),
        ("KEYS user:1*", 11),
        ("LRANGE l 10 20", 11),
        ("LRANGE l 45 100", 5),
        ("LRANGE l 0 -1", 50),
        ("HSCAN h 0 MATCH f*", 2),
        ("GET other", 1),
        ("GET missing", 1),
        ("XRANGE st - +", 2100),
        ("XRANGE st 1-1000 +", 1101),
    ] {
        let (_, r) = rows(s.as_mut(), cmd, 333).await;
        assert_eq!(r.len(), n, "{cmd}");
        let count = query_scalar(s.as_mut(), &d.count_query(cmd), &[])
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {}", e.full_text()));
        assert_eq!(count, Value::Int(n as i64), "{cmd}");
    }
    // The stream pages continue after the last id read, without repeats.
    let (_, r) = rows(s.as_mut(), "XRANGE st - +", 1000).await;
    assert_eq!(r[999][0], Value::Text("1-1000".into()));
    assert_eq!(r[1000][0], Value::Text("1-1001".into()));
    // A count never runs a command that writes again.
    let e = query_all(s.as_mut(), &d.count_query("SPOP l 3"), &[])
        .await
        .unwrap_err();
    assert_eq!(e.kind, ternarygrid_core::ErrorKind::Unsupported, "{e:?}");
    // GET's column is the one the key's definition names.
    let (cols, _) = rows(s.as_mut(), "GET other", 10).await;
    let keys = s
        .list_objects(&SchemaRef::schema("db7"), ObjectKind::Table)
        .await
        .unwrap();
    let other = keys.iter().find(|k| k.obj.name == "other").unwrap();
    let t = s.table_definition(&other.obj).await.unwrap();
    assert_eq!(
        cols,
        t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
    );
    run(s.as_mut(), "FLUSHDB").await;
}

#[tokio::test]
async fn read_only_connections_refuse_writes() {
    let Some(mut p) = params() else { return };
    // Database 6: the other tests use 7, 8 and 9.
    p.database = Some("6".into());
    let mut rw = RedisDriver::new().connect(&p).await.unwrap();
    run(rw.as_mut(), "FLUSHDB").await;
    run(rw.as_mut(), "SET k v").await;
    run(rw.as_mut(), "SADD s a b c").await;
    p.read_only = true;
    let mut s = RedisDriver::new().connect(&p).await.unwrap();
    assert_eq!(
        query_scalar(s.as_mut(), "GET k", &[]).await.unwrap(),
        Value::Text("v".into())
    );
    rows(s.as_mut(), "SCAN 0", 10).await;
    rows(s.as_mut(), "INFO keyspace", 100).await;
    let d = s.dialect();
    assert_eq!(
        query_scalar(s.as_mut(), &d.count_query("SSCAN s 0 MATCH a*"), &[])
            .await
            .unwrap(),
        Value::Int(1)
    );
    let keys = s
        .list_objects(&SchemaRef::schema("db6"), ObjectKind::Table)
        .await
        .unwrap();
    assert_eq!(keys.len(), 2);
    s.object_ddl(&keys[0].obj).await.unwrap();
    s.object_properties(&keys[0].obj).await.unwrap();
    for write in [
        "SET k w",
        "DEL k",
        "SPOP s",
        "FLUSHDB",
        "EVAL \"return 1\" 0",
    ] {
        let e = query_all(s.as_mut(), write, &[]).await.unwrap_err();
        assert_eq!(
            e.kind,
            ternarygrid_core::ErrorKind::ReadOnly,
            "{write}: {e:?}"
        );
    }
    assert_eq!(
        query_scalar(rw.as_mut(), "GET k", &[]).await.unwrap(),
        Value::Text("v".into())
    );
    assert_eq!(
        query_scalar(rw.as_mut(), "SCARD s", &[]).await.unwrap(),
        Value::Int(3)
    );
    run(rw.as_mut(), "FLUSHDB").await;
}
