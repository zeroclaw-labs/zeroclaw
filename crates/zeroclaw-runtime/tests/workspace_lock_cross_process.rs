//! Cross-process proof that a workspace admits exactly one owner.
//!
//! The unit tests take both locks from a single process, which `flock`
//! semantics make an easier case: some advisory-lock implementations grant a
//! second lock to the same process that already holds one. The invariant this
//! module exists for is about two *processes*, so it has to be proven with two
//! processes.
//!
//! The helper binary is this test binary re-executed with an env var, which
//! avoids adding a fixture crate just to hold twenty lines of main().

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use zeroclaw_runtime::workspace_lock::WorkspaceLock;

/// Set on the child to make it take the lock and hold it, instead of running
/// the test suite.
const HOLD_ENV: &str = "ZC_TEST_HOLD_WORKSPACE";

/// Spawn a child process that acquires the lock and holds it until killed.
fn spawn_holder(dir: &Path) -> std::process::Child {
    let exe = std::env::current_exe().expect("test binary path");
    Command::new(exe)
        .env(HOLD_ENV, dir)
        // Run the same test by name. The child takes the env-var branch at the
        // top of the body and parks there; it never reaches the parent logic.
        // A filter matching nothing would exit before the branch ever ran.
        .args(["--exact", "a_second_process_cannot_claim_a_held_workspace"])
        .spawn()
        .expect("spawning the lock holder")
}

#[test]
fn a_second_process_cannot_claim_a_held_workspace() {
    // Child branch: hold the lock, then park until the parent kills us.
    if let Ok(dir) = std::env::var(HOLD_ENV) {
        let _lock = WorkspaceLock::acquire(Path::new(&dir)).expect("child acquires the lock");
        std::thread::sleep(Duration::from_secs(30));
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let mut holder = spawn_holder(dir.path());

    // Wait for the child to actually take the lock. Polling beats a fixed
    // sleep: the assertion below is only meaningful once the lock is held, and
    // a too-short sleep would make this pass for the wrong reason.
    let mut held = false;
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(100));
        if WorkspaceLock::acquire(dir.path()).is_err() {
            held = true;
            break;
        }
    }

    let outcome = if held {
        Ok(())
    } else {
        Err("the child never took the lock, so the refusal was never tested")
    };

    let _ = holder.kill();
    let _ = holder.wait();

    outcome.unwrap();

    // The kernel releases the lock when the holder dies — including on SIGKILL,
    // which runs no cleanup code. This is the property a shell wrapper around
    // flock(1) could not provide: there, killing the visible process left the
    // real holder orphaned.
    let mut reclaimed = false;
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(100));
        if WorkspaceLock::acquire(dir.path()).is_ok() {
            reclaimed = true;
            break;
        }
    }
    assert!(
        reclaimed,
        "the workspace must become claimable once the holding process is gone"
    );
}
