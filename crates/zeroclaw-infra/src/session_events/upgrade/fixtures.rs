//! Explicit old-schema test fixtures. Production migration uses the read-only
//! `readers` module; these APIs are retained only for compatibility tests.

pub mod session_backend;
pub mod session_store;
pub mod sqlite;

use crate::session_events;
use session_backend::SessionBackend;
use std::path::Path;
use std::sync::Arc;

/// Constructs pre-upgrade fixtures only. Production opens the installation's native event store.
pub fn make_session_backend(
    workspace_dir: &Path,
    backend: &str,
) -> std::io::Result<Arc<dyn SessionBackend>> {
    let guard =
        session_events::installation::InstallationGuard::acquire(&workspace_dir.join("sessions"))
            .map_err(std::io::Error::other)?;
    if let Err(error) = guard.require_legacy() {
        if error.kind() != std::io::ErrorKind::PermissionDenied {
            return Err(error);
        }
        // Compatibility-only consumers cannot recreate an old SQLite file.
        // The directory reader shares the durable native writer fence; native
        // entrances and tools read through their authorized event query port.
        return Ok(Arc::new(session_store::SessionStore::new(workspace_dir)?));
    }
    match backend {
        "jsonl" => {
            let store = session_store::SessionStore::new(workspace_dir)?;
            Ok(Arc::new(store))
        }
        "sqlite" => Ok(Arc::new(
            sqlite::SqliteSessionBackend::open_locked(&guard)
                .map_err(|error| std::io::Error::other(format!("{error:#}")))?,
        )),
        other => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"other": other})),
                "Unknown session_backend ''; falling back to sqlite. \
                 Valid values: 'sqlite' (default), 'jsonl'."
            );
            Ok(Arc::new(
                sqlite::SqliteSessionBackend::open_locked(&guard)
                    .map_err(|error| std::io::Error::other(format!("{error:#}")))?,
            ))
        }
    }
}
