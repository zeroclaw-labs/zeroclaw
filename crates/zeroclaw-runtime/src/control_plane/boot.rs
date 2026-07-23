//! Boot wiring for the control-plane — minted once per daemon run.
//!
//! [`ControlPlaneHandle`] bundles the durable [`TaskRegistry`], the goal task
//! extension store, and the run's `boot_id` (the authority key that distinguishes
//! this daemon's live tasks from prior-boot orphans). `DaemonRegistry` owns the
//! spawned reaper task's lifetime via its cancel.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::{fs::OpenOptions, io::Write};

use anyhow::{Context, Result};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::GoalRestartRecovery;

use super::goal_task::GoalTaskRegistry;
use super::reaper;
use super::task_registry::TaskRegistry;
use super::task_store_sqlite::SqliteTaskStore;

/// The live control-plane, shared (cheaply, via `Arc`/clone) across producers and
/// the reaper.
#[derive(Clone)]
pub struct ControlPlaneHandle {
    /// Generic task registry. Owns canonical lifecycle, route, principal, and
    /// ownership state for every durable task kind.
    pub store: Arc<dyn TaskRegistry>,
    /// Goal extension registry. Owns only goal-specific rows and continuation
    /// context keyed by the canonical task id.
    pub goal_store: Arc<dyn GoalTaskRegistry>,
    /// Current daemon owner id used by recovery/reaper authority checks.
    pub boot_id: String,
    /// Goal ids recovered during this boot that need channel continuation after
    /// channel handles are available.
    pub(crate) recovered_goal_ids: Arc<Mutex<Vec<String>>>,
    /// Process-wide data-dir ownership guard.
    ///
    /// This is not control-plane state. It keeps the OS advisory lock on
    /// `<data_dir>/control_plane.lock` alive so restart recovery cannot run in
    /// two daemon processes over the same durable store.
    pub(crate) data_dir_lock: Option<Arc<ControlPlaneDataDirLock>>,
}

impl ControlPlaneHandle {
    /// Open the durable store at `<data_dir>/control_plane.db`, mint a fresh
    /// `boot_id`, and run the one-shot crash-recovery sweep. Prior-boot non-goal
    /// `Running` tasks become `Lost`; prior-boot goals follow the configured
    /// restart recovery policy. Additive and fail-safe: a fresh install gets an
    /// empty DB.
    ///
    /// Single-writer invariant: recovery treats a different `boot_id` as proof
    /// the prior owner is gone, so startup first acquires the data-dir lock
    /// kept in [`ControlPlaneHandle::data_dir_lock`]. Without that lock two
    /// daemon processes could both recover/reap the same durable task table.
    pub async fn start(
        data_dir: &Path,
        goal_restart_recovery: GoalRestartRecovery,
    ) -> Result<Self> {
        let run_id = uuid::Uuid::new_v4().to_string();
        Self::start_with_boot_id(data_dir, run_id, goal_restart_recovery).await
    }

    /// As [`Self::start`] but with a caller-supplied `boot_id` — lets `DaemonRegistry`
    /// reuse a process-stable run-id across reloads instead of a fresh UUID.
    pub async fn start_with_boot_id(
        data_dir: &Path,
        boot_id: String,
        goal_restart_recovery: GoalRestartRecovery,
    ) -> Result<Self> {
        let data_dir_lock = Arc::new(ControlPlaneDataDirLock::acquire(data_dir)?);
        let sqlite_store = Arc::new(SqliteTaskStore::new(data_dir)?);
        let store: Arc<dyn TaskRegistry> = sqlite_store.clone();
        let goal_store: Arc<dyn GoalTaskRegistry> = sqlite_store;
        let recovery = reaper::recovery_pass(
            store.as_ref(),
            goal_store.as_ref(),
            &boot_id,
            goal_restart_recovery,
        )
        .await?;
        if recovery.recovered > 0 {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "recovered": recovery.recovered,
                        "restart_goal_count": recovery.restart_goal_ids.len(),
                        "boot_id": boot_id,
                    })),
                "control-plane: recovered prior-boot tasks at startup"
            );
        }
        Ok(Self {
            store,
            goal_store,
            boot_id,
            recovered_goal_ids: Arc::new(Mutex::new(recovery.restart_goal_ids)),
            data_dir_lock: Some(data_dir_lock),
        })
    }

    /// Drain goal IDs recovered by this boot's `last_state` policy.
    ///
    /// This is an in-memory startup work queue, not canonical lifecycle state.
    /// If the process crashes before the channel loop consumes it, the next boot
    /// will recover the goal again under its new `boot_id`.
    pub fn take_recovered_goal_ids(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .recovered_goal_ids
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }

    /// Spawn the periodic reaper as a detached task whose lifetime `DaemonRegistry`
    /// owns via `cancel`. Errors inside the loop are logged, never propagated.
    ///
    /// Uses `zeroclaw_spawn::spawn!` (NOT raw `tokio::spawn`, which `clippy.toml`
    /// bans workspace-wide) so the reaper task inherits the caller's tracing span.
    pub fn spawn_reaper(
        &self,
        max_runtime_secs: i64,
        goal_restart_recovery: GoalRestartRecovery,
        cancel: CancellationToken,
    ) -> JoinHandle<()> {
        debug_assert!(
            self.data_dir_lock.is_some() || cfg!(test),
            "production control-plane handles must hold the data-dir lock"
        );
        // Hoist owned clones to locals so the spawn! future captures them by value
        // (not `&self`, which the macro would otherwise hold across the 'static boundary).
        let store = Arc::clone(&self.store);
        let goal_store = Arc::clone(&self.goal_store);
        let boot_id = self.boot_id.clone();
        zeroclaw_spawn::spawn!(reaper::reaper_loop(
            store,
            goal_store,
            boot_id,
            max_runtime_secs,
            goal_restart_recovery,
            cancel
        ))
    }
}

/// Held OS advisory lock for one control-plane data directory.
///
/// The file lock, not this Rust object, is the source of truth for single
/// writer ownership. The object only owns the open file descriptor/handle so
/// the lock remains held for as long as the installed control-plane handle
/// lives.
#[derive(Debug)]
pub(crate) struct ControlPlaneDataDirLock {
    file: std::fs::File,
}

impl ControlPlaneDataDirLock {
    fn acquire(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create control-plane data dir {}", data_dir.display()))?;
        let lock_path = data_dir.join("control_plane.lock");
        let mut file = open_locked_file(&lock_path)?;
        file.set_len(0)
            .with_context(|| format!("truncate control-plane lock {}", lock_path.display()))?;
        writeln!(
            file,
            "pid={}\nstarted_at={}",
            std::process::id(),
            chrono::Utc::now().to_rfc3339()
        )
        .with_context(|| format!("write control-plane lock {}", lock_path.display()))?;
        Ok(Self { file })
    }
}

#[cfg(unix)]
fn open_locked_file(path: &Path) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // Do not truncate until after the advisory lock is held; otherwise a
        // rejected second owner could still rewrite the incumbent's lock file.
        .truncate(false)
        .open(path)
        .with_context(|| format!("open control-plane lock {}", path.display()))?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(file);
    }
    let error = std::io::Error::last_os_error();
    anyhow::bail!(
        "control-plane data dir is already locked at {}: {error}",
        path.display()
    );
}

#[cfg(unix)]
impl Drop for ControlPlaneDataDirLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(windows)]
fn open_locked_file(path: &Path) -> Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
        .with_context(|| {
            format!(
                "open exclusive control-plane lock {}; another daemon may already own this data dir",
                path.display()
            )
        })
}

#[cfg(not(any(unix, windows)))]
fn open_locked_file(path: &Path) -> Result<std::fs::File> {
    OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| {
            format!(
                "create control-plane lock {}; this platform has no advisory lock implementation",
                path.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_in_tempdir_and_reap_handle() {
        let dir = tempfile::tempdir().unwrap();
        let h = ControlPlaneHandle::start(dir.path(), GoalRestartRecovery::default())
            .await
            .unwrap();
        assert!(!h.boot_id.is_empty());
        // a reaper spawns and stops cleanly on cancel
        let cancel = CancellationToken::new();
        let jh = h.spawn_reaper(600, GoalRestartRecovery::default(), cancel.clone());
        cancel.cancel();
        jh.await.unwrap();
    }

    #[tokio::test]
    async fn start_rejects_second_live_owner_for_same_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let _h = ControlPlaneHandle::start(dir.path(), GoalRestartRecovery::default())
            .await
            .unwrap();

        let second = ControlPlaneHandle::start(dir.path(), GoalRestartRecovery::default()).await;
        let err = second.err().expect("second live owner should fail");

        assert!(
            err.to_string()
                .contains("control-plane data dir is already locked")
                || err.to_string().contains("control-plane lock"),
            "unexpected lock error: {err:#}"
        );
    }

    #[tokio::test]
    async fn boot_id_distinguishes_runs_over_the_same_db() {
        use crate::control_plane::task_registry::{TaskKind, TaskRecord, TaskStatus};
        let dir = tempfile::tempdir().unwrap();
        // First "boot" registers a running task, then the daemon "dies".
        let h1 = ControlPlaneHandle::start_with_boot_id(
            dir.path(),
            "boot-1".into(),
            GoalRestartRecovery::default(),
        )
        .await
        .unwrap();
        h1.store
            .create(TaskRecord {
                id: "t".into(),
                kind: TaskKind::Delegate,
                agent: "main".into(),
                status: TaskStatus::Running,
                owner_pid: 999_999,
                owner_boot_id: "boot-1".into(),
                heartbeat_at: None,
                depth: 0,
                parent_id: None,
                originator_route: None,
                delivered: false,
                idem_key: None,
                principal_id: None,
                started_at: "2026-06-18T00:00:00Z".into(),
                finished_at: None,
            })
            .await
            .unwrap();
        drop(h1);
        // Second boot recovers the non-goal orphan at startup.
        let h2 = ControlPlaneHandle::start_with_boot_id(
            dir.path(),
            "boot-2".into(),
            GoalRestartRecovery::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            h2.store.get("t").await.unwrap().unwrap().status,
            TaskStatus::Lost
        );
    }
}
