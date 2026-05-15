//! Channel infrastructure: session backends, debouncing, and stall watchdog.
//!
//! These are cross-cutting utilities used by multiple channel implementations.

pub mod debounce;
pub mod session_backend;
pub mod session_sqlite;
pub mod session_store;
pub mod stall_watchdog;

use std::path::Path;
use std::sync::Arc;

use crate::session_backend::SessionBackend;

/// Construct the configured session-persistence backend.
///
/// `backend` is the value of `[channels].session_backend` from config:
/// `"sqlite"` (default) opens `{workspace}/sessions/sessions.db`, `"jsonl"`
/// opens `{workspace}/sessions/*.jsonl`. Unknown values fall back to
/// SQLite with a warning so a typo in config never silently disables
/// persistence. The `Arc<dyn SessionBackend>` return type keeps every
/// call site (channel orchestrator, runtime tools) reading from the
/// same store.
///
/// Errors propagate from the underlying backend constructor (typically
/// filesystem permissions on the sessions directory).
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
            tracing::warn!(
                "Unknown session_backend '{other}'; falling back to sqlite. \
                 Valid values: 'sqlite' (default), 'jsonl'."
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
        Ok(n) => tracing::info!(
            "session_backend=sqlite: imported {n} legacy JSONL session(s) from \
             {}/sessions; renamed to *.jsonl.migrated.",
            workspace_dir.display(),
        ),
        Err(e) => tracing::warn!(
            "session_backend=sqlite: JSONL import skipped: {e}. Existing JSONL \
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
    use zeroclaw_api::provider::ChatMessage;

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
