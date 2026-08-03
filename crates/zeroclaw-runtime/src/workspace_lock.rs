//! Exclusive ownership of a workspace, enforced by the kernel.
//!
//! Two processes serving one `data_dir` corrupt each other in ways that are
//! invisible until they are expensive. The control plane's crash recovery
//! treats a different `boot_id` as proof the previous owner is gone, so a
//! second daemon reaps the first one's live tasks. Worse, on a paired
//! messaging channel the session store is a SQLite file holding device
//! credentials: two processes opening it concurrently made WhatsApp revoke
//! this deployment's device three times on 2026-08-01.
//!
//! `control_plane::boot` documented this requirement and deferred it, so the
//! invariant was enforced outside the binary by a shell wrapper running
//! `flock(1)`. That works only for invocations that go through the wrapper,
//! and it leaks: `flock(1)` does not forward signals to the command it wraps,
//! so killing the visible process leaves the real one orphaned, still holding
//! the inherited descriptor. Recovering from that means finding a child nobody
//! knew existed.
//!
//! Holding the lock in-process fixes both. Every entry point is covered, not
//! just the wrapped ones, and the kernel releases the lock when the process
//! dies — including on SIGKILL, where no cleanup handler would ever run.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Name of the lock file created inside the workspace's `data_dir`.
const LOCK_FILE_NAME: &str = ".zeroclaw-workspace.lock";

/// An exclusive claim on one workspace, released when this value is dropped
/// or when the process exits — whichever happens first.
///
/// The claim lives in the open file descriptor, not in the file's contents or
/// its existence. A stale lock file left behind by a crash blocks nobody: the
/// next process opens it, finds no holder, and takes the lock. This is why the
/// implementation never deletes the file to "clean up" — deleting it while
/// another process holds the lock would let a third process create a fresh
/// file and lock that instead, and both would believe they were alone.
#[derive(Debug)]
pub struct WorkspaceLock {
    /// Kept alive solely to hold the descriptor open. Dropping it closes the
    /// fd, which is what releases the advisory lock.
    _file: File,
    path: PathBuf,
}

impl WorkspaceLock {
    /// Take exclusive ownership of `data_dir`, or fail with the identity of
    /// the process that already owns it.
    ///
    /// Fails rather than waits. A second daemon is a deployment mistake, not a
    /// queueing problem: blocking would hide it until something timed out.
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating workspace dir {}", data_dir.display()))?;
        let path = data_dir.join(LOCK_FILE_NAME);

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening workspace lock {}", path.display()))?;

        // SAFETY: `flock` takes a valid descriptor and a flag constant, and
        // returns a status code. `file` owns the descriptor and outlives the
        // call. LOCK_NB makes this non-blocking, so it cannot deadlock.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                let owner = read_owner(&path);
                bail!(
                    "another ZeroClaw process is already using this workspace{owner}\n  \
                     workspace: {}\n  \
                     Two processes sharing one workspace corrupt each other's task state and \
                     can get a paired messaging device revoked.\n  \
                     Stop the running instance first (systemctl --user stop zeroclaw).",
                    data_dir.display()
                );
            }
            return Err(err).with_context(|| format!("locking workspace {}", data_dir.display()));
        }

        // Record who holds it, for the error message the *next* process prints.
        // Best-effort: the lock is already ours, and a workspace that cannot be
        // written to has bigger problems than an unhelpful diagnostic.
        let mut file = file;
        let _ = file.set_len(0);
        let _ = write!(file, "{}", std::process::id());
        let _ = file.flush();

        Ok(Self { _file: file, path })
    }

    /// Path of the lock file backing this claim.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Describe the current holder for an error message, if it can be determined.
///
/// Returns an empty string rather than failing: this runs on the error path,
/// where a missing detail must never mask the error it was meant to explain.
fn read_owner(path: &Path) -> String {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let pid = contents.trim();
    if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_digit()) {
        return String::new();
    }
    format!(" (pid {pid})")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant the whole module exists for.
    #[test]
    fn second_acquire_is_refused_while_the_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let _first = WorkspaceLock::acquire(dir.path()).expect("first claim should succeed");

        let second = WorkspaceLock::acquire(dir.path());
        assert!(
            second.is_err(),
            "a second process must not be able to claim a workspace that is already owned"
        );
    }

    /// Releasing must actually release. If the descriptor outlived the value,
    /// a restart would be locked out by its own dead predecessor.
    #[test]
    fn dropping_the_claim_frees_the_workspace() {
        let dir = tempfile::tempdir().unwrap();

        let first = WorkspaceLock::acquire(dir.path()).unwrap();
        drop(first);

        WorkspaceLock::acquire(dir.path())
            .expect("workspace must be claimable again once the holder is gone");
    }

    /// A crash leaves the lock file behind. That file is not the lock — the
    /// descriptor is — so its presence must not keep the next process out.
    #[test]
    fn a_leftover_lock_file_does_not_block_a_fresh_claim() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join(LOCK_FILE_NAME);
        std::fs::write(&stale, "999999").unwrap();

        WorkspaceLock::acquire(dir.path())
            .expect("a stale lock file left by a crashed process must not block startup");
    }

    /// Different workspaces are independent; one deployment must not lock out
    /// another on the same host.
    #[test]
    fn separate_workspaces_do_not_contend() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();

        let _lock_a = WorkspaceLock::acquire(a.path()).unwrap();
        WorkspaceLock::acquire(b.path())
            .expect("a different workspace must be independently lockable");
    }

    /// The refusal has to say who is holding it, or the operator is left
    /// guessing which process to stop.
    #[test]
    fn refusal_names_the_holding_process() {
        let dir = tempfile::tempdir().unwrap();
        let _held = WorkspaceLock::acquire(dir.path()).unwrap();

        let err = WorkspaceLock::acquire(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains(&std::process::id().to_string()),
            "the error must identify the holder, got: {err}"
        );
    }

    /// The workspace directory may not exist yet on a first run.
    #[test]
    fn acquire_creates_a_missing_workspace_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("does").join("not").join("exist");

        let lock =
            WorkspaceLock::acquire(&nested).expect("first run must not require a pre-made dir");
        assert!(lock.path().exists());
    }
}
