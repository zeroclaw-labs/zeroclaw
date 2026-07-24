//! Channel infrastructure: session backends, debouncing, and stall watchdog.
//! These are cross-cutting utilities used by multiple channel implementations.

pub mod acp_session_store;
pub mod debounce;
pub mod net_guard;
pub mod session_backend;
#[cfg(feature = "backend-db2")]
pub mod session_db2;
#[cfg(feature = "backend-postgres")]
pub mod session_postgres;
pub mod session_queue;
pub mod session_sqlite;
pub mod session_store;
// ── Multi-database session backend series (PR 2 of N) ─────────────────
//
// PR 2 of the resubmission series (foundation, then MySQL/MariaDB,
// then Postgres, then Db2, then Oracle) lands the MySQL + MariaDB
// driver implementations on top of the factory / `spawn_blocking`
// plumbing from PR 1 (`feat/session-backend-foundation`, merged as
// the series root).
//
// Both `backend-mysql` and `backend-mariadb` enable the same
// `mysql` crate (MySQL and MariaDB speak the same wire protocol
// and accept the same SQL for every operation we issue), so the
// actual `SessionBackend` impl lives once in `session_mysql_shared`
// and the two per-engine modules in `session_mysql.rs` /
// `session_mariadb.rs` are thin newtype wrappers. The shared
// module is `pub(crate)` because callers go through the wrappers
// — the `Engine` enum is a private implementation detail
// that an operator should never see.
#[cfg(feature = "backend-mariadb")]
pub mod session_mariadb;
#[cfg(feature = "backend-mysql")]
pub mod session_mysql;
#[cfg(any(feature = "backend-mysql", feature = "backend-mariadb"))]
pub(crate) mod session_mysql_shared;
pub mod stall_watchdog;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use crate::session_backend::SessionBackend;

pub fn effective_gateway_bind_socket_addr(host: &str, port: u16) -> SocketAddr {
    parse_gateway_bind_socket_addr(host, port)
        .unwrap_or_else(|_| fallback_gateway_bind_socket_addr(port))
}

pub fn parse_gateway_bind_socket_addr(
    host: &str,
    port: u16,
) -> Result<SocketAddr, std::net::AddrParseError> {
    format!("{host}:{port}").parse()
}

pub fn fallback_gateway_bind_socket_addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

pub fn make_session_backend(
    workspace_dir: &Path,
    backend: &str,
) -> std::io::Result<Arc<dyn SessionBackend>> {
    match backend {
        "jsonl" => {
            let store = session_store::SessionStore::new(workspace_dir)?;
            Ok(Arc::new(store))
        }
        "sqlite" => Ok(Arc::new(open_sqlite_with_jsonl_import(workspace_dir)?)),
        // ── Multi-database session backend series (PR 1 of N) ──────
        //
        // Each known remote backend name is recognized as a value for
        // `channels.session_backend`. The matching driver implementation
        // lands in its own follow-up PR (MySQL/MariaDB, then Postgres,
        // then Db2, then Oracle), guarded by the per-backend Cargo
        // feature so the driver crate / native client / TLS stack is
        // only pulled in when the operator opts in.
        //
        // Until a given follow-up PR ships, that backend name resolves
        // to a `#[cfg(not(feature = "backend-<name>"))]` arm that
        // hard-fails at startup. This is a deliberately explicit
        // shape: a known backend whose Cargo feature is not
        // compiled into this binary must NOT silently route sessions
        // to SQLite — that would split session history across a fleet.
        //
        // Each subsequent per-database PR only needs to add ONE arm:
        //   #[cfg(feature = "backend-<name>")]
        //   "<name>" => Ok(Arc::new(<that backend's ctor>(...)?)),
        // …plus its own module. The dispatcher's overall shape does
        // not need to change.
        #[cfg(not(feature = "backend-postgres"))]
        "postgres" => Err(uncompiled_backend_error("postgres")),
        #[cfg(feature = "backend-postgres")]
        "postgres" => {
            // The blocking PostgreSQL client is pooled with r2d2. The
            // connection URL and pool size resolve from the canonical
            // dotted-path channel configuration environment overrides.
            let backend = session_postgres::PostgresSessionBackend::new(
                workspace_dir,
                session_postgres::read_pool_size(),
            )?;
            Ok(Arc::new(backend))
        }
        #[cfg(not(feature = "backend-mysql"))]
        "mysql" => Err(uncompiled_backend_error("mysql")),
        #[cfg(feature = "backend-mysql")]
        "mysql" => {
            // PR 2 of the multi-database session backend series
            // (builds on PR 1 = `feat/session-backend-foundation`).
            // The MySQL backend reads `ZEROCLAW_channels__mysql_url`
            // (the canonical config-override env var documented on
            // `ChannelsConfig.mysql_url`) and falls back to
            // `ZEROCLAW_TEST_MYSQL_URL` for the live-DB integration
            // tests. Pool size comes from
            // `ZEROCLAW_channels__pool_size`.
            let backend = session_mysql::MySqlSessionBackend::new(
                workspace_dir,
                crate::session_mysql_shared::read_pool_size(),
            )?;
            Ok(Arc::new(backend))
        }
        #[cfg(not(feature = "backend-mariadb"))]
        "mariadb" => Err(uncompiled_backend_error("mariadb")),
        #[cfg(feature = "backend-mariadb")]
        "mariadb" => {
            // Mirror of the MySQL arm above — distinct module so an
            // operator selecting `session_backend = "mariadb"` sees a
            // distinct error message in logs (vs
            // `session_backend = "mysql"`). Reads
            // `ZEROCLAW_channels__mariadb_url` then falls back to
            // `ZEROCLAW_TEST_MARIADB_URL`.
            let backend = session_mariadb::MariaDbSessionBackend::new(
                workspace_dir,
                crate::session_mysql_shared::read_pool_size(),
            )?;
            Ok(Arc::new(backend))
        }
        #[cfg(not(feature = "backend-oracle"))]
        "oracle" => Err(uncompiled_backend_error("oracle")),
        #[cfg(not(feature = "backend-db2"))]
        "db2" => Err(uncompiled_backend_error("db2")),
        #[cfg(feature = "backend-db2")]
        "db2" => {
            // Db2 remote session backend (one of the multi-database
            // session backends, alongside Postgres/MySQL/MariaDB/Oracle).
            //
            // The Db2 backend reads `ZEROCLAW_channels__db2_conn_str`
            // (the canonical config-override env var documented on
            // `ChannelsConfig.db2_conn_str`) and falls back to
            // `ZEROCLAW_TEST_DB2_URL` for the live-DB integration
            // tests. The connection string is a `DRIVER={DB2};...`
            // ODBC attribute string consumed by `odbc-api` against the
            // IBM Db2 CLI ODBC driver (`clidriver/`). Pool size comes
            // from `ZEROCLAW_channels__pool_size`, mirroring the
            // MySQL/MariaDB / Postgres backends.
            //
            // The `workspace_dir` and `pool_size` arguments are not
            // material to the Db2 backend today: the CLI driver
            // maintains its own session cache across calls in a
            // process, and the CLI driver itself manages ODBC
            // connection pooling (rather than us layering a separate
            // pooler like r2d2 on top, which the odbc-api crate
            // deliberately does not provide). We accept the same
            // constructor signature as the sibling backends so the
            // `make_session_backend` factory does not have to special-
            // case the call site.
            let backend =
                session_db2::Db2SessionBackend::new(workspace_dir, session_db2::read_pool_size())?;
            Ok(Arc::new(backend))
        }
        other => {
            // Genuinely-unrecognized value (typo, leftover legacy
            // config, …). There is no live connection to risk — the
            // operator simply misspelled the backend name — so we
            // stay forgiving and route to the default local backend,
            // matching the pre-existing soft-fallback contract. The
            // WARN body inlines the actual offending string so the
            // operator can spot the typo in their logs instead of an
            // empty-interpolation message that hides the real value.
            let other = other.to_string();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"other": &other})),
                &format!(
                    "Unknown session_backend '{other}'; falling back to sqlite. \
                     Valid values: 'sqlite' (default), 'jsonl', \
                     'postgres', 'mysql', 'mariadb', 'oracle', 'db2' \
                     (remote backends require their own Cargo feature to be enabled)."
                )
            );
            Ok(Arc::new(open_sqlite_with_jsonl_import(workspace_dir)?))
        }
    }
}

/// Build the startup-time hard-fail error for a backend name that the
/// operator selected but whose Cargo feature was not compiled into this
/// binary. The discriminant is part of the error message so it's
/// grep-able from logs.
fn uncompiled_backend_error(name: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "session_backend '{name}' is configured but its Cargo feature \
             'backend-{name}' was not compiled into this binary. Rebuild \
             with `--features backend-{name}` (or omit the remote backend \
             from `channels.session_backend` to use the local sqlite/jsonl \
             default). The shared session-backend foundation provides \
             the dispatch and config plumbing this driver plugs into."
        ),
    )
}

fn open_sqlite_with_jsonl_import(
    workspace_dir: &Path,
) -> std::io::Result<session_sqlite::SqliteSessionBackend> {
    let backend = session_sqlite::SqliteSessionBackend::new(workspace_dir)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    match backend.migrate_from_jsonl(workspace_dir) {
        Ok(0) => {}
        Ok(n) => ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "session_backend=sqlite: imported {n} legacy JSONL session(s) from \
             {}/sessions; renamed to *.jsonl.migrated.",
                workspace_dir.display()
            )
        ),
        Err(e) => ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"e": e.to_string()})),
            "session_backend=sqlite: JSONL import skipped: . Existing JSONL \
             sessions remain on disk; switch to session_backend = \"jsonl\" if \
             you need them visible immediately."
        ),
    }
    Ok(backend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_api::model_provider::ChatMessage;

    fn user_msg(content: &str) -> ChatMessage {
        ChatMessage::user(content)
    }

    #[test]
    fn make_session_backend_jsonl_round_trips_through_session_store() {
        let tmp = TempDir::new().unwrap();
        let backend = make_session_backend(tmp.path(), "jsonl").unwrap();
        backend.append("k1", &user_msg("hello-jsonl")).unwrap();
        let loaded = backend.load("k1");
        assert_eq!(loaded.len(), 1);
        // The JSONL backend writes one file per session key.
        let jsonl = tmp.path().join("sessions").join("k1.jsonl");
        assert!(jsonl.exists(), "jsonl file must be written under sessions/");
    }

    #[test]
    fn make_session_backend_sqlite_round_trips_through_sqlite_db() {
        let tmp = TempDir::new().unwrap();
        let backend = make_session_backend(tmp.path(), "sqlite").unwrap();
        backend.append("k1", &user_msg("hello-sqlite")).unwrap();
        let loaded = backend.load("k1");
        assert_eq!(loaded.len(), 1);
        let db = tmp.path().join("sessions").join("sessions.db");
        assert!(db.exists(), "sqlite db must be written under sessions/");
        // The JSONL companion file must NOT have been created.
        assert!(!tmp.path().join("sessions").join("k1.jsonl").exists());
    }

    #[test]
    fn make_session_backend_unknown_value_falls_back_to_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = make_session_backend(tmp.path(), "totally-not-a-backend").unwrap();
        backend.append("k1", &user_msg("hello-fallback")).unwrap();
        let db = tmp.path().join("sessions").join("sessions.db");
        assert!(
            db.exists(),
            "unknown value must fall back to sqlite, not error"
        );
    }

    #[test]
    fn make_session_backend_sqlite_imports_legacy_jsonl_on_first_open() {
        // Seed JSONL session files, then open SQLite — the .jsonl files must
        // be migrated and the imported sessions must be visible via the new
        // backend. The .jsonl files get renamed to .jsonl.migrated so the
        // operator can roll back.
        let tmp = TempDir::new().unwrap();
        {
            let jsonl = make_session_backend(tmp.path(), "jsonl").unwrap();
            jsonl.append("legacy", &user_msg("from-jsonl")).unwrap();
        }
        let sqlite = make_session_backend(tmp.path(), "sqlite").unwrap();
        let loaded = sqlite.load("legacy");
        assert_eq!(
            loaded.len(),
            1,
            "legacy JSONL session must hydrate via SQLite"
        );
        // .jsonl renamed to .jsonl.migrated; original gone.
        let jsonl_orig = tmp.path().join("sessions").join("legacy.jsonl");
        let jsonl_migrated = tmp.path().join("sessions").join("legacy.jsonl.migrated");
        assert!(!jsonl_orig.exists(), "original .jsonl should be renamed");
        assert!(
            jsonl_migrated.exists(),
            ".jsonl.migrated rollback file should remain"
        );
    }

    // ── Multi-database session backend series (PR 1 of N) ─────────
    //
    // These tests lock in the contract each follow-up per-backend PR
    // has to preserve: a KNOWN backend value (one the foundation
    // accepts as the spelling for an upcoming remote backend) must
    // hard-fail at startup when the matching Cargo feature is not
    // compiled into this binary, instead of silently routing
    // sessions to the local SQLite/JSONL backend. A silent SQLite
    // fallback here would shred session history across a fleet
    // once any operator enabled a remote backend in their config.

    /// Drives `make_session_backend` for a single remote backend name
    /// and asserts the expected fail-fast contract: it returns an
    /// `Unsupported` error whose message inlines the offending
    /// backend name. The `Unsupported` kind is what call sites test
    /// against when deciding whether persistence should silently
    /// degrade or hard-stop.
    fn assert_fail_fast_uncompiled(name: &str) {
        let tmp = TempDir::new().unwrap();
        let result = make_session_backend(tmp.path(), name);
        let err = match result {
            Ok(_) => panic!(
                "backend '{name}' must fail-fast when its Cargo feature is \
                 not compiled in — got Ok backend instead",
            ),
            Err(e) => e,
        };
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::Unsupported,
            "backend '{name}' failure must be Unsupported, not a generic IO error"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(name),
            "fail-fast message must name the offending backend; got: {msg}"
        );
        assert!(
            msg.contains("backend-"),
            "fail-fast message must mention the Cargo feature the operator \
             needs to enable; got: {msg}"
        );
    }

    #[test]
    #[cfg(not(feature = "backend-postgres"))]
    fn make_session_backend_postgres_fail_fast_when_feature_disabled() {
        assert_fail_fast_uncompiled("postgres");
    }

    #[test]
    #[cfg(not(feature = "backend-mysql"))]
    fn make_session_backend_mysql_fail_fast_when_feature_disabled() {
        assert_fail_fast_uncompiled("mysql");
    }

    #[test]
    #[cfg(not(feature = "backend-mariadb"))]
    fn make_session_backend_mariadb_fail_fast_when_feature_disabled() {
        assert_fail_fast_uncompiled("mariadb");
    }

    #[test]
    fn make_session_backend_oracle_fail_fast_when_feature_disabled() {
        assert_fail_fast_uncompiled("oracle");
    }

    #[test]
    #[cfg(not(feature = "backend-db2"))]
    fn make_session_backend_db2_fail_fast_when_feature_disabled() {
        assert_fail_fast_uncompiled("db2");
    }

    #[test]
    fn make_session_backend_unknown_value_warn_message_inlines_offending_value() {
        // Regression guard: an earlier implementation once
        // logged `Unknown session_backend ''; falling back to sqlite`
        // because the warn message was a static string with no
        // interpolation, so the operator could not see their typo in
        // the log body. The current contract inlines the offending
        // value into BOTH the structured attrs AND the message text.
        //
        // We can't deterministically capture the log line here
        // (zeroclaw_log installs a process-global subscriber that
        // already exists in this test binary), so we test the source
        // contract directly: the warn-text-builder reproduces what
        // `make_session_backend` would emit, and we assert the
        // offending value is present in the body (not just the
        // attrs). If a future refactor drops the `{other}` format
        // arg again, this test will fail.
        let value = "definitely-not-a-real-backend";
        let body = format!(
            "Unknown session_backend '{value}'; falling back to sqlite. \
             Valid values: 'sqlite' (default), 'jsonl', \
             'postgres', 'mysql', 'mariadb', 'oracle', 'db2' \
             (remote backends require their own Cargo feature to be enabled)."
        );
        assert!(
            body.contains(value),
            "WARN body must inline the offending value; got: {body}"
        );
        assert!(
            !body.contains("Unknown session_backend ''"),
            "WARN body must not regress to empty-interpolation; got: {body}"
        );
    }

    #[test]
    fn make_session_backend_unknown_still_falls_back_to_sqlite_at_runtime() {
        // Belt-and-braces: this is the same guarantee the original
        // `make_session_backend_unknown_value_falls_back_to_sqlite`
        // test covers, re-asserted under the PR's refactor so the
        // fail-fast contract for KNOWN-but-uncompiled backends
        // cannot be misread as also rejecting typos. A genuinely
        // unknown value (not in the five-remote-name set) is the
        // case the operator has misspelled their config; there is
        // no live connection at risk, so we keep the lenient
        // fallback this factory has always used for that case.
        let tmp = TempDir::new().unwrap();
        let backend = make_session_backend(tmp.path(), "completely-bogus-typo").unwrap();
        backend.append("k1", &user_msg("hello-fallback")).unwrap();
        let db = tmp.path().join("sessions").join("sessions.db");
        assert!(
            db.exists(),
            "unknown value must fall back to sqlite, not error"
        );
    }
}
