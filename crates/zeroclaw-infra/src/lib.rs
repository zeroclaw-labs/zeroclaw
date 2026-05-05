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
        "sqlite" => {
            let store = session_sqlite::SqliteSessionBackend::new(workspace_dir)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(Arc::new(store))
        }
        other => {
            tracing::warn!(
                "Unknown session_backend '{other}'; falling back to sqlite. \
                 Valid values: 'sqlite' (default), 'jsonl'."
            );
            let store = session_sqlite::SqliteSessionBackend::new(workspace_dir)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(Arc::new(store))
        }
    }
}
