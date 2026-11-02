# TernaryGrid architecture

TernaryGrid is a single native process. The UI thread (egui/eframe, glow renderer)
only draws state and sends commands; all database I/O, metadata loading and
data transfer runs on a small tokio runtime (2–4 worker threads). The UI polls
finished background jobs once per frame and repaints only when something
changed, so the application uses ~0% CPU while idle.

```
┌──────────────────────────── ternarygrid-app (binary "ternarygrid") ─────────────────────────────┐
│ UI thread (egui)                                                                    │
│  navigator · SQL editor tabs · result grids · object editors · diagrams · admin     │
│  tools · dialogs · command palette · status bar · operations panel                  │
│        │ commands / jobs                                   ▲ results (Job<T>)        │
│        ▼                                                   │                         │
│ Backend (tokio runtime) ── ConnectionManager ── SessionHandle (1 open cursor/session) │
└────────┬───────────────────────────┬────────────────────────────┬──────────────────┘
         │                           │                            │
   ternarygrid-sql (tokenizer,       ternarygrid-core (Value, RowStore,   ternarygrid-net (SSH tunnel,
   splitter, formatter,        Driver/Session traits,         SOCKS/HTTP proxy as a
   autocomplete, analysis)     metadata/DDL/plan model,       local forwarder)
                               Dialect, UI strings, Secret)
         │                           │
   ternarygrid-grid / ternarygrid-editor   drivers: postgres (+CockroachDB), mysql (+MariaDB),
   ternarygrid-visual (egui          sqlite, mssql (tiberius), clickhouse (HTTP),
   widgets)                    oracle (oracledb thin), cassandra (scylla)
         │
   ternarygrid-config (settings, connections, secrets: Keychain / encrypted vault,
   DBeaver / pgpass / URL import, query history, projects & scripts)
   ternarygrid-transfer (export/import/migration/mock data) · ternarygrid-compare (schema &
   data compare, HTML, Liquibase) · ternarygrid-tasks (tasks, scheduler) · ternarygrid-ai
```

## Crates

| Crate | Responsibility |
| --- | --- |
| `ternarygrid-core` | Shared contract: `Value`/`DataKind`/`ColumnMeta`, `Driver`/`Session`/`QueryCursor` traits, metadata, DDL and plan models, `Dialect` + ANSI DDL helpers, `RowStore` (paged, spills to disk), `Secret`, UI strings. |
| `ternarygrid-sql` | Dialect-aware tokenizer, statement splitter (DELIMITER, GO, compound blocks, dollar quotes), classifier (read-only enforcement, dangerous statements), formatter, autocomplete and semantic analysis, parameters, client commands, outline, spell checker, SQL generation, fuzzy matching. |
| `ternarygrid-driver-*` | One crate per product. Each implements `Driver`, `Session`, `QueryCursor`, `Dialect` and `DdlGenerator` and reads metadata from the system catalogs. |
| `ternarygrid-net` | SSH tunnels (russh) and SOCKS5/HTTP CONNECT proxies, all exposed as a local TCP forwarder so every driver works through them unchanged. |
| `ternarygrid-config` | Paths, settings, keymaps, connection store, secret stores, importers, query history (SQLite), projects/scripts/bookmarks/virtual model, SQL templates. |
| `ternarygrid-transfer` | Streaming exporters (CSV, JSON, SQL, XLSX, Markdown, HTML, XML, TXT, DBUnit, PHP, Parquet), importers (CSV, XLSX, XML, JSON), type inference, migration between databases, mock data. |
| `ternarygrid-compare` | Schema diff → dependency-ordered DDL, HTML report, Liquibase changelog, diagram diff; data compare with sync script. |
| `ternarygrid-tasks` | Task definitions, schedules (cron/interval/daily), scheduler, run log, OS scheduler integration (launchd/cron/schtasks). |
| `ternarygrid-ai` | Anthropic Messages API and OpenAI-compatible clients (streaming), prompts, schema context builder. |
| `ternarygrid-grid` | Virtual data grid (custom painted, both axes virtualised), result model with pending edits → DML, record/text/JSON/XML views, value/metadata/grouping/calc/references panels, virtual columns. |
| `ternarygrid-editor` | SQL editor widget (highlighting, autocomplete popup, diagnostics, hyperlinks, find/replace, editing commands). |
| `ternarygrid-visual` | ER diagrams (layered layout, edit mode, SVG/PNG export), plan diagrams, visual query builder, charts, dashboards, spatial viewer. |
| `ternarygrid-app` | The application: windows, tabs, navigator, execution engine, wiring of all crates, CLI modes, benchmarks. |

## Key design decisions

* **Driver contract.** `Session::execute` returns a `'static` cursor; at most one
  cursor is open per session. `SessionHandle` (app) enforces this by closing the
  open cursor before any other operation, so drivers that move the connection
  into the cursor (MySQL, SQLite) never deadlock.
* **Separate connection per editor** (default on, as in DBeaver): long queries in
  an editor never block the navigator. PostgreSQL databases other than the
  connected one get their own sessions on demand.
* **Bounded memory.** Result rows are fetched in pages (fetch size, default 200).
  `RowStore` keeps rows in pages and spills least-recently-used pages to an
  anonymous temp file when a per-store or global budget is exceeded; inactive
  result tabs release their pages. A 1M-row result therefore costs memory only
  for the visible/cached pages.
* **Text protocol for PostgreSQL results.** User queries are prepared (types,
  source table OIDs for editability) and streamed with the simple query
  protocol, so every type — including extension types — displays exactly as the
  server prints it.
* **Tunnels as local forwarders.** SSH and proxies listen on `127.0.0.1:<port>`;
  drivers connect there and verify TLS against the real host name
  (`ConnectParams::tls_server_name`).
* **Secrets** never touch disk in plain text: macOS Keychain by default, or an
  Argon2id + XChaCha20-Poly1305 vault protected by a master password. Config files
  are written atomically with mode 0600. `Secret` has no `Serialize` impl and
  redacts itself in `Debug`/`Display`.
* **UI language.** English only; user visible strings go through `ternarygrid_core::tr`
  (unchanged) and `trf` (placeholders).
* **Privacy.** No telemetry. The only outbound connections are the configured
  database servers, SSH/proxy hosts and — only when enabled — the AI provider.
