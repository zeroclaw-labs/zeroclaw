//! Exclusive ownership of gateway turn state in the existing session store.
//!
//! Keep the guard alive for every detached turn, not just the listener. The
//! empty lock file is never removed: unlinking it would create two lock domains.

use std::fs::{File, TryLockError};
use std::io;
use std::path::Path;

use crate::session_sqlite::SqliteSessionBackend;

#[derive(Debug)]
#[must_use]
pub struct GatewaySessionOwner {
    _lock: File,
    recovered_turns: usize,
}

impl GatewaySessionOwner {
    /// Refuse a competing gateway before reading or settling its session state.
    /// Acquisition and recovery must precede admission of any gateway work.
    pub fn acquire(data_dir: &Path) -> io::Result<Self> {
        let directory = data_dir.join("sessions");
        std::fs::create_dir_all(&directory)?;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("gateway.lock"))?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "another gateway still owns this data directory; connect to it or wait for its turns to stop",
            ),
            TryLockError::Error(error) => error,
        })?;
        let recovered_turns = SqliteSessionBackend::settle_abandoned_gateway_turns(data_dir)?;
        Ok(Self {
            _lock: lock,
            recovered_turns,
        })
    }

    pub fn recovered_turns(&self) -> usize {
        self.recovered_turns
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_backend::SessionBackend;
    use zeroclaw_api::model_provider::ChatMessage;

    #[test]
    fn competing_owner_cannot_settle_live_turns_and_recovery_preserves_history() {
        let dir = tempfile::tempdir().unwrap();
        let first = GatewaySessionOwner::acquire(dir.path()).unwrap();
        assert_eq!(first.recovered_turns(), 0);
        assert!(!SqliteSessionBackend::database_path(dir.path()).exists());
        let backend = SqliteSessionBackend::new(dir.path()).unwrap();
        for key in ["gw_chat", "channel_chat", "gw_idle", "gw_failed"] {
            backend
                .append(key, &ChatMessage::user("keep history"))
                .unwrap();
        }
        backend
            .set_session_state("gw_chat", "running", Some("turn-1"))
            .unwrap();
        backend
            .set_session_state("channel_chat", "running", Some("other-turn"))
            .unwrap();
        backend
            .set_session_state("gw_failed", "error", Some("failed-turn"))
            .unwrap();
        backend
            .record_request("gw_chat", "request-1", "accepted")
            .unwrap();
        let error = GatewaySessionOwner::acquire(dir.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            backend.get_session_state("gw_chat").unwrap().unwrap().state,
            "running"
        );
        drop(first);
        let restarted = GatewaySessionOwner::acquire(dir.path()).unwrap();
        assert_eq!(restarted.recovered_turns(), 1);
        let state = backend.get_session_state("gw_chat").unwrap().unwrap();
        assert_eq!(state.state, "error");
        assert_eq!(state.turn_id.as_deref(), Some("turn-1"));
        assert!(state.turn_started_at.is_none());
        assert_eq!(backend.load("gw_chat")[0].content, "keep history");
        assert_eq!(
            backend
                .get_session_state("channel_chat")
                .unwrap()
                .unwrap()
                .state,
            "running"
        );
        assert_eq!(
            backend.get_session_state("gw_idle").unwrap().unwrap().state,
            "idle"
        );
        assert_eq!(
            backend
                .get_session_state("gw_failed")
                .unwrap()
                .unwrap()
                .turn_id
                .as_deref(),
            Some("failed-turn")
        );
        assert!(
            backend
                .record_request("gw_chat", "request-1", "accepted")
                .unwrap()
                .is_some(),
            "restart must not erase dedup receipts or authorize effect replay"
        );
        drop(restarted);
        assert_eq!(
            GatewaySessionOwner::acquire(dir.path())
                .unwrap()
                .recovered_turns(),
            0
        );
    }

    #[test]
    fn recovery_failure_refuses_startup_without_losing_the_lock_forever() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        let database = SqliteSessionBackend::database_path(dir.path());
        std::fs::write(&database, b"not a database").unwrap();
        assert!(GatewaySessionOwner::acquire(dir.path()).is_err());
        assert_eq!(std::fs::read(&database).unwrap(), b"not a database");
        std::fs::remove_file(&database).unwrap();
        assert!(GatewaySessionOwner::acquire(dir.path()).is_ok());
    }
    // Invoked only in a child test process with a synthetic temporary directory.
    #[test]
    #[ignore = "subprocess helper"]
    fn owner_process_fixture() {
        use std::io::Write;
        let Some(path) = std::env::var_os("ZEROCLAW_TEST_OWNER_DIR") else {
            return;
        };
        let _owner = GatewaySessionOwner::acquire(Path::new(&path)).unwrap();
        let backend = SqliteSessionBackend::new(Path::new(&path)).unwrap();
        backend
            .append("gw_killed", &ChatMessage::user("survives kill"))
            .unwrap();
        backend
            .set_session_state("gw_killed", "running", Some("killed-turn"))
            .unwrap();
        println!("OWNER_READY");
        std::io::stdout().flush().unwrap();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
    }

    #[test]
    fn process_death_releases_ownership_and_next_owner_settles_turn() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "gateway_session_owner::tests::owner_process_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("ZEROCLAW_TEST_OWNER_DIR", dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(output).lines() {
                if line.unwrap().contains("OWNER_READY") {
                    let _ = ready_tx.send(());
                    break;
                }
            }
        });
        let ready = ready_rx.recv_timeout(std::time::Duration::from_secs(10));
        let competing = ready
            .is_ok()
            .then(|| GatewaySessionOwner::acquire(dir.path()));
        child.kill().unwrap();
        child.wait().unwrap();
        reader.join().unwrap();
        ready.expect("child must acquire ownership and persist its running turn");
        assert_eq!(
            competing.unwrap().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let restarted = GatewaySessionOwner::acquire(dir.path()).unwrap();
        assert_eq!(restarted.recovered_turns(), 1);
        let backend = SqliteSessionBackend::new(dir.path()).unwrap();
        assert_eq!(
            backend
                .get_session_state("gw_killed")
                .unwrap()
                .unwrap()
                .state,
            "error"
        );
        assert_eq!(backend.load("gw_killed")[0].content, "survives kill");
    }
}
