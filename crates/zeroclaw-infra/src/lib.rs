//! Channel infrastructure: session backends, debouncing, and stall watchdog.
//!
//! These are cross-cutting utilities used by multiple channel implementations.

pub mod acp_session_store;
pub mod debounce;
pub mod session_backend;
#[cfg(feature = "backend-db2")]
pub mod session_db2;
#[cfg(feature = "backend-mysql")]
pub mod session_mysql;
#[cfg(feature = "backend-oracle")]
pub mod session_oracle;
#[cfg(feature = "backend-postgres")]
pub mod session_postgres;
pub mod session_sqlite;
pub mod session_store;
pub mod stall_watchdog;

use std::path::Path;
use std::sync::Arc;

use crate::session_backend::SessionBackend;

/// Construct the configured session-persistence backend from `[channels]` config.
///
/// Selects the backend named by `channels.session_backend`:
/// - `"sqlite"` (default) — file-backed, single-host.
/// - `"jsonl"` — legacy one-file-per-session format.
/// - `"postgres"` — shared PostgreSQL store; requires `backend-postgres` feature
///   and `channels.postgres_url`.
/// - `"oracle"` — Oracle 23ai store; requires `backend-oracle` feature and
///   `channels.oracle_user` / `oracle_password` / `oracle_dsn`.
/// - `"db2"` — IBM Db2 store; requires `backend-db2` feature and
///   `channels.db2_conn_str`.
/// - `"mysql"` — MySQL 9.0+ store; requires `backend-mysql` feature and
///   `channels.mysql_url`.
///
/// Unknown values fall back to SQLite with a warning so a typo in config
/// never silently disables persistence.
pub fn make_session_backend(
    workspace_dir: &Path,
    channels: &zeroclaw_config::schema::ChannelsConfig,
) -> std::io::Result<Arc<dyn SessionBackend>> {
    match channels.session_backend.as_str() {
        "jsonl" => {
            let store = session_store::SessionStore::new(workspace_dir)?;
            Ok(Arc::new(store))
        }
        "sqlite" => Ok(Arc::new(open_sqlite_with_jsonl_import(workspace_dir)?)),

        #[cfg(feature = "backend-postgres")]
        "postgres" => {
            let url = channels.postgres_url.as_deref().ok_or_else(|| {
                std::io::Error::other("session_backend=postgres requires postgres_url in [channels]")
            })?;
            let store = session_postgres::PostgresSessionBackend::new(url, channels.pool_size)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(Arc::new(store))
        }

        #[cfg(feature = "backend-db2")]
        "db2" => {
            let conn_str = channels.db2_conn_str.as_deref().ok_or_else(|| {
                std::io::Error::other(
                    "session_backend=db2 requires db2_conn_str in [channels]",
                )
            })?;
            let store = session_db2::Db2SessionBackend::new(conn_str, channels.pool_size)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(Arc::new(store))
        }

        #[cfg(feature = "backend-oracle")]
        "oracle" => {
            let user = channels.oracle_user.as_deref().ok_or_else(|| {
                std::io::Error::other("session_backend=oracle requires oracle_user in [channels]")
            })?;
            let password = channels.oracle_password.as_deref().ok_or_else(|| {
                std::io::Error::other(
                    "session_backend=oracle requires oracle_password in [channels]",
                )
            })?;
            let dsn = channels.oracle_dsn.as_deref().ok_or_else(|| {
                std::io::Error::other("session_backend=oracle requires oracle_dsn in [channels]")
            })?;
            let store =
                session_oracle::OracleSessionBackend::new(user, password, dsn, channels.pool_size)
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(Arc::new(store))
        }

        #[cfg(feature = "backend-mysql")]
        "mysql" => {
            let url = channels.mysql_url.as_deref().ok_or_else(|| {
                std::io::Error::other("session_backend=mysql requires mysql_url in [channels]")
            })?;
            let store =
                session_mysql::MysqlSessionBackend::new(url, channels.pool_size as usize)
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(Arc::new(store))
        }

        other => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"other": other})),
                "Unknown session_backend ''; falling back to sqlite. \
                 Valid values: 'sqlite' (default), 'jsonl', 'postgres', 'oracle', 'db2', 'mysql'."
            );
            Ok(Arc::new(open_sqlite_with_jsonl_import(workspace_dir)?))
        }
    }
}

/// Open the SQLite backend and, on first open, import any pre-existing
/// `sessions/*.jsonl` files left over from the legacy JSONL store. Renames
/// the imported files to `*.jsonl.migrated` so re-runs are no-ops; preserves
/// them on disk so an operator can roll back without data loss. Errors from
/// the import path are logged and skipped — the SQLite backend itself still
/// opens, since blocking startup on a best-effort migration would be worse
/// than a partial migration.
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

    fn channels_with_backend(backend: &str) -> zeroclaw_config::schema::ChannelsConfig {
        zeroclaw_config::schema::ChannelsConfig {
            session_backend: backend.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn make_session_backend_jsonl_round_trips_through_session_store() {
        let tmp = TempDir::new().unwrap();
        let backend = make_session_backend(tmp.path(), &channels_with_backend("jsonl")).unwrap();
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
        let backend = make_session_backend(tmp.path(), &channels_with_backend("sqlite")).unwrap();
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
        let backend =
            make_session_backend(tmp.path(), &channels_with_backend("totally-not-a-backend"))
                .unwrap();
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
            let jsonl =
                make_session_backend(tmp.path(), &channels_with_backend("jsonl")).unwrap();
            jsonl.append("legacy", &user_msg("from-jsonl")).unwrap();
        }
        let sqlite = make_session_backend(tmp.path(), &channels_with_backend("sqlite")).unwrap();
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
}
