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

/// Data directories whose startup recovery this process has already claimed,
/// by resolved location, so two spellings of one directory are one claim.
static STARTUP_RECOVERY_CLAIMED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
> = std::sync::LazyLock::new(Default::default);

/// Settle the session turns a previous process left marked "running" under
/// `data_dir`, and return how many there were.
///
/// The caller must hold process ownership of `data_dir` and must not have
/// admitted a turn yet. The first time a process owns a data directory is
/// the one moment it knows no turn is running there: whoever marked them is
/// gone. Each such turn is recorded as "error" with its history kept, and
/// nothing is restarted.
///
/// It acts once per process for a directory. Every later call returns
/// `Ok(0)` without looking, because by then a running turn may be this
/// process's own. That holds after a failed attempt too: the error is
/// returned once, and the turns stay as they are until the next process
/// start.
///
/// Run state lives only in the SQLite store, so that store is settled
/// whenever it exists, whichever backend is configured now: a later reload
/// may switch to it. It is neither created nor migrated for this.
pub fn recover_abandoned_session_turns(data_dir: &Path) -> std::io::Result<usize> {
    let identity = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    if !STARTUP_RECOVERY_CLAIMED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(identity)
    {
        return Ok(0);
    }
    let recovered = session_sqlite::SqliteSessionBackend::recover_abandoned_turns_at(data_dir)?;
    if recovered > 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "sessions": recovered })),
            "marked session turns left running by a previous process as failed"
        );
    }
    Ok(recovered)
}

fn open_sqlite_with_jsonl_import(
    workspace_dir: &Path,
) -> std::io::Result<session_sqlite::SqliteSessionBackend> {
    let backend = session_sqlite::SqliteSessionBackend::new(workspace_dir)
        .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
    match backend
        .migrate_from_jsonl(workspace_dir)
        .map_err(|e| std::io::Error::other(format!("{e:#}")))?
    {
        0 => {}
        n => ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "session_backend=sqlite: completed {n} legacy JSONL session migration \
             handoff(s) from {}/sessions to *.jsonl.migrated.",
                workspace_dir.display()
            )
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
    fn make_session_backend_can_reload_from_empty_sqlite_to_jsonl() {
        let tmp = TempDir::new().unwrap();
        let sqlite = make_session_backend(tmp.path(), "sqlite").unwrap();
        drop(sqlite);

        let jsonl = make_session_backend(tmp.path(), "jsonl").unwrap();
        jsonl
            .append("reload_user", &user_msg("hello after reload"))
            .unwrap();
        assert_eq!(jsonl.load("reload_user").len(), 1);
    }

    fn store_with_running_turn(data_dir: &Path) -> Arc<dyn SessionBackend> {
        let backend = make_session_backend(data_dir, "sqlite").unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend
            .set_session_state("s1", "running", Some("turn-1"))
            .unwrap();
        backend
    }

    #[test]
    fn recovery_settles_what_a_killed_process_left_and_acts_once() {
        let tmp = TempDir::new().unwrap();
        drop(store_with_running_turn(tmp.path()));

        assert_eq!(recover_abandoned_session_turns(tmp.path()).unwrap(), 1);

        let restarted = make_session_backend(tmp.path(), "sqlite").unwrap();
        assert!(restarted.list_running_sessions().is_empty());
        let state = restarted.get_session_state("s1").unwrap().unwrap();
        assert_eq!(state.state, "error");
        assert_eq!(state.turn_id.as_deref(), Some("turn-1"));
        assert_eq!(restarted.load("s1").len(), 1);

        // From here on a running turn is this process's own.
        restarted
            .set_session_state("s1", "running", Some("turn-2"))
            .unwrap();
        assert_eq!(recover_abandoned_session_turns(tmp.path()).unwrap(), 0);
        assert_eq!(restarted.list_running_sessions().len(), 1);
    }

    #[test]
    fn recovery_creates_no_store() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(recover_abandoned_session_turns(tmp.path()).unwrap(), 0);
        assert!(!tmp.path().join("sessions").exists());
    }

    /// One directory reached through two spellings is one claim: the second
    /// spelling must not settle turns started since the first.
    #[cfg(unix)]
    #[test]
    fn recovery_claim_follows_the_directory_not_its_spelling() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("data");
        std::fs::create_dir_all(&real).unwrap();
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        assert_eq!(recover_abandoned_session_turns(&real).unwrap(), 0);
        let store = store_with_running_turn(&real);

        assert_eq!(recover_abandoned_session_turns(&alias).unwrap(), 0);
        assert_eq!(
            recover_abandoned_session_turns(&real.join("..").join("data")).unwrap(),
            0
        );
        assert_eq!(store.list_running_sessions().len(), 1);
    }

    /// A failed attempt is reported once and never repeated: by the time the
    /// store works again, the process may be running turns in it.
    #[test]
    fn failed_recovery_is_reported_and_not_retried() {
        let tmp = TempDir::new().unwrap();
        let db_path = session_sqlite::SqliteSessionBackend::db_path(tmp.path());
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(&db_path, b"not a database").unwrap();

        assert!(recover_abandoned_session_turns(tmp.path()).is_err());

        std::fs::remove_file(&db_path).unwrap();
        let store = store_with_running_turn(tmp.path());
        assert_eq!(recover_abandoned_session_turns(tmp.path()).unwrap(), 0);
        assert_eq!(store.list_running_sessions().len(), 1);
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

    #[test]
    fn make_session_backend_sqlite_fails_closed_on_import_collision() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("legacy.jsonl"),
            "{\"role\":\"user\",\"content\":\"from-jsonl\"}\n",
        )
        .unwrap();
        std::fs::write(
            sessions_dir.join("legacy.jsonl.migrated"),
            "existing archive",
        )
        .unwrap();

        let err = make_session_backend(tmp.path(), "sqlite")
            .err()
            .expect("migration collision must prevent SQLite startup");
        assert!(err.to_string().contains("Refusing to replace"));
    }

    #[test]
    fn make_session_backend_preserves_initialization_error_chain() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("sessions/sessions.db")).unwrap();

        let err = make_session_backend(tmp.path(), "sqlite")
            .err()
            .expect("a directory cannot be opened as the SQLite database");
        let message = err.to_string();
        assert!(message.contains("Failed to open session DB"));
        assert!(message.contains("unable to open database file"));
    }
}
