# DBeaver parity matrix

Every row of the PRD §4 inventory, with where it is implemented and the
automated test that covers it, or the recorded reason for an exception
(PRD §13, phase 3 gate). Test names are `crate::file::test` (integration
tests in `crates/<crate>/tests/`). Tests marked *(env)* need the Docker
servers from `.github/workflows/ci.yml` (`TERNARYGRID_TEST_PG`, `TERNARYGRID_TEST_MYSQL`,
`TERNARYGRID_TEST_MARIADB`, `TERNARYGRID_TEST_CLICKHOUSE`, `TERNARYGRID_TEST_MSSQL`).

Legend: ✅ implemented and tested · 🟡 implemented, partially tested ·
⛔ exception (reason given).

## Connections

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Connection wizard (all driver properties, URL paste, test) | ✅ | `ternarygrid-app/src/dialogs/connection.rs` | `ternarygrid-app::tabs_smoke`, `ternarygrid-config::import::url` unit tests |
| Connection types dev/test/prod, colours, confirmations, auto-commit | ✅ | `ternarygrid-config::connections`, settings → Connection types | `ternarygrid-app::editor_e2e::editor_executes_…` (confirmations) |
| SSH tunnel (password, keys, agent; host key policies) | ✅ | `ternarygrid-net::tunnel`, `ssh` | `ternarygrid-net::tunnel` (17 end-to-end tests against an in-process SSH server) |
| SSL with truststore / client certificates | ✅ | drivers (`ConnectParams.ssl`) | `ternarygrid-driver-postgres::integration` TLS modes *(env)*, `ternarygrid-driver-mysql::connect` *(env)* |
| Proxy (SOCKS5, HTTP CONNECT) | ✅ | `ternarygrid-net::socks`, `http_proxy` | `ternarygrid-net::tunnel` proxy → SSH chains |
| Network profiles | ✅ | `ConnectionStore::network_profiles`, settings → Network profiles, connection dialog selector | `ternarygrid-config::connections` unit tests |
| Init SQL and shell commands before/after connect | ✅ | `ternarygrid-app::conn::connect_task`, `ternarygrid-net::process` | `ternarygrid-net::process` unit tests |
| Auto-commit / manual mode, pending transaction, transaction log | ✅ | `SessionHandle::tx_state`, SQL editor toolbar | `ternarygrid-app::data_edit_e2e` (save inside transactions) *(env)* |
| Separate connection per editor | ✅ | `InitConfig::separate_editor_connection`, `SqlEditorTab::session` | `ternarygrid-app::editor_e2e` |
| Driver manager | ⛔ | Drivers are compiled in (no JDBC jars to manage); *Help → Database drivers* lists them with capabilities | — |
| Import connections (DBeaver, pgpass, URL) | ✅ | `ternarygrid-config::import`, `dialogs/import.rs` | `ternarygrid-config::tests::dbeaver_import` (no lost fields), url/pgpass unit tests |

## Authentication and security

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| User / password | ✅ | all drivers | every integration suite *(env)* |
| pgpass | ✅ | `AuthMethod::Pgpass`, `import::pgpass::lookup` | `ternarygrid-config::import::pgpass` unit tests |
| Password from shell command | ✅ | `AuthMethod::Command`, `ternarygrid-net::password_from_command` | `ternarygrid-net::process` unit tests |
| Kerberos | ⛔ | No maintained pure-Rust GSSAPI implementation; would need the system Kerberos C libraries, which the PRD's "full Rust" rule excludes for network clients | — |
| MySQL 2FA (multi-factor) | ⛔ | Not supported by `mysql_async`; tracked for when the protocol support lands upstream | — |
| Secret storage with master password | ✅ | `ternarygrid-config::secrets` (Keychain, Argon2id + XChaCha20-Poly1305 vault) | `ternarygrid-config::secrets` unit tests (vault, wrong password, migration) |
| Read-only connections | ✅ | client-side classification (`ternarygrid-sql::classify`) | `ternarygrid-app::editor_e2e::read_only_connections_block_writes` |
| Change database password | ✅ | `tabs/dbtools.rs` (`ChangePasswordTab`) | see *Administration* |

## Navigator

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Connection / object tree, lazy and cached | ✅ | `nav.rs`, `conn/meta.rs` | `ternarygrid-app::app_e2e::execute_fetch_count_and_metadata` *(env)* |
| Simple and advanced views | ✅ | `NavigatorView` per connection | `ternarygrid-app::tabs_smoke` |
| Object filters | ✅ | `ObjectFilters`, connection dialog → Filters | `ternarygrid-core` filter unit tests |
| Bookmarks | ✅ | `Project::add_bookmark`, Project panel | `ternarygrid-config::project` unit tests |
| Projects and project explorer (scripts, diagrams) | ✅ | `ternarygrid-config::project`, `project_view.rs` | `ternarygrid-config::project` unit tests |
| Metadata search | ✅ | `Session::search_metadata`, `tabs/search.rs` | driver suites *(env)* |
| Full-text / data search | ✅ | `tabs/search.rs` (data search over text columns) | `ternarygrid-app::tabs_smoke` |
| Properties editor | ✅ | object editor → Properties (+ virtual model) | `ternarygrid-app::tabs_smoke` |

## SQL editor

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Many scripts per connection saved as files, restored on start | ✅ | `SqlEditorTab::save`, `ProjectManager::{load,save}_session` | `ternarygrid-config::project` unit tests |
| Execute statement / script / selection, new result tab, cancel | ✅ | `sqlprep.rs`, `exec.rs` | `ternarygrid-app::editor_e2e`, `ternarygrid-app::app_e2e::errors_stop_the_script_and_cancel_works` *(env)* |
| Results in several tabs or one | ✅ | result tabs, *Open each query in a new tab* | `ternarygrid-app::editor_e2e` |
| Autocomplete and semantic analysis (tables, columns, joins, aliases, CTEs) | ✅ | `ternarygrid-sql::complete`, `sqllang.rs` | `ternarygrid-sql::complete` 42 scenarios on the standard schema |
| Templates | ✅ | `TemplateLibrary`, offered in autocomplete | `ternarygrid-app::sqllang::tests` |
| Formatter | ✅ | `ternarygrid-sql::format` | formatter corpus (PostgreSQL, MySQL, SQLite, T-SQL × 5 option sets, idempotent) |
| Variables / parameters panel | ✅ | parameter prompt + *Variables* result tab | `ternarygrid-app::sqlprep::tests`, `editor_e2e` |
| Object hyperlinks | ✅ | `EditorLanguage::link_at` | `ternarygrid-editor` headless tests |
| Outline | ✅ | `ternarygrid-sql::outline`, editor outline panel | `ternarygrid-sql::outline` tests |
| Error indicators | ✅ | server error position → editor diagnostic; unknown table/column diagnostics | `ternarygrid-app::editor_e2e` |
| Spell checker | ✅ | `ternarygrid-sql::spell` (English dictionary + user words) | `ternarygrid-sql::spell` tests |
| SQL generation (SELECT/INSERT/UPDATE/DELETE/UPSERT/DDL) | ✅ | `ternarygrid-sql::generate`, navigator → Generate SQL | `ternarygrid-sql::generate` tests |
| SQL terminal | ✅ | editor *Terminal mode*, SQL console tabs | `ternarygrid-app::tabs_smoke` |
| Client commands `@set`, `@include`, `@export`, `@echo` | ✅ | `ternarygrid-sql::commands`, `sqlprep.rs` | `ternarygrid-app::sqlprep::tests`, `editor_e2e` |
| Query manager (history) | ✅ | `ternarygrid-config::history`, `tabs/history.rs` | `ternarygrid-config::history` tests |
| Switch active connection / schema | ✅ | editor toolbar, ⌘9 / ⌘0 | `ternarygrid-app::keys::tests` |
| Execute with the native client | ✅ | `Driver::client_command` + `tools::run_command` | driver command tests |

## Query plan and builder

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Execution plan (text, tree, diagram; EXPLAIN ANALYZE) | ✅ | drivers' `explain`, `ternarygrid-visual::plan` | `editor_e2e` (explain), driver explain tests *(env)*, `ternarygrid-visual` plan tests |
| Visual query builder | ✅ | `ternarygrid-visual::builder`, `tabs/query_builder.rs` | `ternarygrid-visual` builder tests (SQL generation) |

## Data editor

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Grid, text, record, JSON, XML views; spatial | ✅ | `ternarygrid-grid`, `ternarygrid-visual::spatial` | `ternarygrid-grid` UI tests, spatial parser tests |
| Server- or client-side filter and sort | ✅ | `ResultPanel::requery`, grid client views | `ternarygrid-grid::view` tests |
| Inline editing, add / duplicate / delete rows, save / cancel | ✅ | `ResultModel`, `SessionHandle::apply_changes` | `ternarygrid-app::data_edit_e2e::edit_save_and_cancel` (PostgreSQL, MySQL, MariaDB, SQLite) |
| Value, references, metadata, grouping, calc panels | ✅ | `ternarygrid-grid::panels` | `ternarygrid-grid` UI tests |
| Advanced copy (TSV, CSV, Markdown, JSON, SQL, HTML, XML) | ✅ | `ternarygrid-grid::copy` | `ternarygrid-grid::copy` tests |
| Foreign key navigation with history | ✅ | `ResultPanel::navigate` | `ternarygrid-app::tabs_smoke` |
| Refresh and auto-refresh, fetch size, total count | ✅ | `ResultSource` | `ternarygrid-app::app_e2e::execute_fetch_count_and_metadata` |
| Virtual columns and virtual keys | ✅ | object editor → Virtual model, `ResultPanel` | `ternarygrid-app::data_edit_e2e::virtual_model_dictionary_and_virtual_columns` |
| Data dictionary | ✅ | dictionary description columns | same test |
| Data formats | ✅ | settings → Result sets → Value formats | `ternarygrid-core::value` tests |

## Charts and special data

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Charts from results (line, bar, area, scatter, pie) | ✅ | `ternarygrid-visual::chart`, result panel *Chart* | `ternarygrid-visual` chart tests |
| Dashboards (server health) | ✅ | `tabs/admin.rs` (`DashboardTab`), `ternarygrid-visual::dashboard` | `ternarygrid-visual` dashboard tests |
| Mock data generation | ✅ | `ternarygrid-transfer::mock`, `tabs/transfer` | `ternarygrid-transfer::mock` tests |
| GIS / spatial data | ✅ | `ternarygrid-visual::spatial` (WKT, WKB, EWKB, MySQL internal) | spatial tests |
| XML / JSON data | ✅ | grid JSON/XML views and value panel | `ternarygrid-grid` view tests |

## ER diagrams

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Automatic diagrams from a schema or objects | ✅ | `ternarygrid-visual::er`, `tabs/er.rs` | layout tests; 200-table benchmark (≤ 1.7 ms/frame) |
| Custom saved diagrams, image export | ✅ | project diagrams, SVG/PNG export | `ternarygrid-visual::er` export tests |
| Edit mode (change the schema from the diagram) | ✅ | edit events → reviewed DDL | `ternarygrid-visual` UI tests |

## Data transfer

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Export: CSV, DBUnit, JSON, Markdown, PHP, SQL, TXT, XML, XLSX, HTML (+ Parquet, JSON Lines, TSV) | ✅ | `ternarygrid-transfer::export`, `tabs/transfer` | `ternarygrid-transfer::formats`, `roundtrip` |
| Import: CSV, XLSX, XML (+ JSON) | ✅ | `ternarygrid-transfer::import` | `ternarygrid-transfer::import`, `roundtrip` |
| Import and replace | ✅ | `ImportMapping::mode` | `ternarygrid-transfer::import` |
| Migration between databases | ✅ | `ternarygrid-transfer::migration` | `ternarygrid-transfer::migration` |
| Save as task | ✅ | `TaskKind::{Export, Import, Migration}`, `transferexec.rs` | `ternarygrid-app::transfer_e2e` *(env)* |
| Round trip CSV / JSON / SQL produces identical data (PRD gate) | ✅ | — | `ternarygrid-transfer::roundtrip` |

## Comparison

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Schema compare → DDL script | ✅ | `ternarygrid-compare`, `tabs/compare.rs` | `ternarygrid-app::app_e2e::schema_compare_gate` (DDL makes schemas identical; PostgreSQL, MySQL, MariaDB) *(env)* |
| HTML report, diagram diff, Liquibase changelog | ✅ | `html_report`, *Diff diagram*, `liquibase_changelog` | `ternarygrid-compare` tests |
| Data compare with sync script | ✅ | `run_data_compare`, `tabs/data_compare.rs` | `ternarygrid-compare` data tests, `ternarygrid-app::tabs_smoke` |

## Tasks

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Task management, SQL script tasks, composite tasks | ✅ | `ternarygrid-tasks`, `tabs/tasks.rs`, `taskexec.rs` | `ternarygrid-tasks` tests, `ternarygrid-app::headless_task`, `ternarygrid-tasks` tests |
| Scheduler (in app, and launchd / cron / schtasks when closed) | ✅ | `Scheduler`, `ternarygrid_tasks::os`, `ternarygrid --run-task` | `ternarygrid-tasks` scheduler tests (DST, catch-up, cancellation) |

## Administration

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Backup and restore with the vendor tools | ✅ | `Driver::backup_command`/`restore_command`, `tools.rs`, `tabs/dbtools.rs` | `ternarygrid-app::backup_e2e` (PostgreSQL custom + plain; MySQL / MariaDB when the client tools are installed) *(env)* |
| Session manager, lock manager | ✅ | `tabs/admin.rs` | driver admin tests *(env)* |
| Maintenance tools | ✅ | `Dialect::maintenance_ops`, `tabs/dbtools.rs` | `ternarygrid-app::tabs_smoke` |
| PostgreSQL debugger | ✅ | `PgSession::debug_routine` (pldebugger), `DebuggerTab` | `ternarygrid-driver-postgres` debugger test *(env, pldebugger)* |
| Server health dashboard | ✅ | `DashboardTab` | `ternarygrid-app::tabs_smoke` |

## Schema and DDL

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Create tables, columns, indexes, constraints, foreign keys, triggers | ✅ | `tabs/table_designer.rs`, `DdlGenerator` | `ternarygrid-app::tabs_smoke`, driver DDL round trips |
| Partitions | 🟡 | shown in the object editor; created via SQL (the designer does not edit partition schemes) | driver metadata tests |
| DDL generation | ✅ | `Session::object_ddl`, `DdlGenerator` | driver DDL round-trip tests |
| Sample database | ✅ | *Help → Create sample database* (`sample.rs`) | `ternarygrid-app::sample::tests` |

## Interface

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Themes (light, dark, system), high contrast | ✅ | `theme.rs` | `ternarygrid-app::tabs_smoke` |
| Keyboard shortcuts (customisable, TernaryGrid / DBeaver presets) | ✅ | `ternarygrid-config::keymap`, `keys.rs` | `ternarygrid-config::keymap` tests, `ternarygrid-app::keys::tests` |
| Toolbar customisation | ✅ | settings → Toolbar | `ternarygrid-config::settings` tests |
| Accessibility | 🟡 | AccessKit tree (egui), editor text runs, keyboard navigation everywhere; screen reader support per grid cell is not implemented (one node per grid) | `ternarygrid-editor` AccessKit test |

## AI assistant

| Feature | Status | Where | Tests |
| --- | --- | --- | --- |
| Chat, query suggestions, explanations, error fixes, object descriptions | ✅ | `ternarygrid-ai`, `tabs/ai.rs` | `ternarygrid-ai` tests (mock HTTP server), `ternarygrid-app::tabs_smoke` (prompt assembly) |
| Several model providers | ✅ | Anthropic, OpenAI-compatible servers | `ternarygrid-ai` tests |

## Databases (PRD §7)

| Database | Status | Tests |
| --- | --- | --- |
| PostgreSQL | ✅ | `ternarygrid-driver-postgres` (66 unit, 23 integration) *(env)* |
| MySQL, MariaDB | ✅ | `ternarygrid-driver-mysql` (67 unit, 34 integration × 2 servers) *(env)* |
| SQLite | ✅ | `ternarygrid-driver-sqlite` (87) |
| SQL Server | ✅ | `ternarygrid-driver-mssql` (39 unit, 6 integration) *(env)* |
| ClickHouse | ✅ | `ternarygrid-driver-clickhouse` (54 unit, 5 integration) *(env)* |
| Oracle | ✅ | `ternarygrid-driver-oracle` on Oracle's pure Rust thin driver (`oracledb`, beta): SQL and PL/SQL with DBMS_OUTPUT and compile errors, catalog, DBMS_METADATA DDL, EXPLAIN PLAN, sessions *(env `TERNARYGRID_TEST_ORACLE`)* |
| Cassandra, ScyllaDB | ✅ | `ternarygrid-driver-cassandra` on the `scylla` driver: CQL with paging, keyspaces, tables, indexes, materialized views, server DESCRIBE *(env `TERNARYGRID_TEST_CASSANDRA`)* |
| CockroachDB | ✅ | PostgreSQL driver with CockroachDB catalog adjustments; `ternarygrid-app` end-to-end suites (execution, cancel, schema compare gate, data editing, editor) run against CockroachDB *(env `TERNARYGRID_TEST_COCKROACH`)* |
| Redis, Valkey, Dragonfly | ✅ | `ternarygrid-driver-redis` on the `redis` crate: commands, keys by type (SCAN + pipelined TYPE), paged HSCAN/SSCAN/ZSCAN/LRANGE/ZRANGE/XRANGE, INFO and CLIENT LIST as tables, server dashboard, client list *(env `TERNARYGRID_TEST_REDIS`)* |
| MongoDB, FerretDB, DocumentDB | ✅ | `ternarygrid-driver-mongodb` on the official driver: a `mongosh` subset, streamed cursors, collections, sampled fields, indexes, explain, currentOp, serverStatus *(env `TERNARYGRID_TEST_MONGO`)* |
| Elasticsearch, OpenSearch | ✅ | `ternarygrid-driver-elasticsearch` over HTTP: SQL with cursor paging, REST requests, indices, aliases, mappings, tasks, cluster metrics *(env `TERNARYGRID_TEST_ELASTIC`)* |
| Neo4j | ⛔ | `neo4rs` requires the `ring` crypto provider beside rustls' aws-lc (a second native crypto library) |
| DuckDB | ⛔ | Removed: its engine is C++ (the full-Rust rule, PRD §7) |
| Db2, Teradata, Informix | ⛔ | No usable pure-Rust network driver yet (PRD §7: "only if a viable Rust driver exists") |

## Performance targets (PRD §2)

Measured with `ternarygrid --bench all` on the release build (macOS, Apple M1 Pro):

| Target | Result |
| --- | --- |
| Binary < 50 MB | 44.0 MB (MiB, with Oracle and Cassandra) |
| Cold start < 1 s | 105 ms to the first frame |
| Idle memory < 120 MB | 69 MB |
| 3 connections + 5 result tabs < 250 MB | 83 MB (20 000 rows fetched per tab) |
| 1 M rows scroll, < 16 ms/frame | p95 0.5 ms (grid benchmark), 1 M × 100 columns: avg < 3 ms, worst 11.7 ms |
| Editor typing at 50 000 lines | p95 0.4 ms |
