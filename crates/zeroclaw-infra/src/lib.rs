//! Channel infrastructure: session backends, debouncing, and stall watchdog.
//! These are cross-cutting utilities used by multiple channel implementations.

pub mod acp_session_store;
pub mod debounce;
pub mod net_guard;
pub mod session_backend;
pub mod session_queue;
pub mod session_sqlite;
pub mod session_store;
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
        other => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"other": other})),
                "Unknown session_backend ''; falling back to sqlite. \
                 Valid values: 'sqlite' (default), 'jsonl'."
            );
            Ok(Arc::new(open_sqlite_with_jsonl_import(workspace_dir)?))
        }
    }
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
}
