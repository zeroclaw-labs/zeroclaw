//! Boot wiring for the control-plane — minted once per daemon run.
//!
//! [`ControlPlaneHandle`] bundles the durable [`TaskRegistry`] and the run's `boot_id`
//! (the authority key that distinguishes this daemon's live tasks from prior-boot
//! orphans). `DaemonRegistry` owns the spawned reaper task's lifetime via its cancel.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::GoalRestartRecovery;

use super::reaper;
use super::task_registry::TaskRegistry;
use super::task_store_sqlite::SqliteTaskStore;

/// The live control-plane, shared (cheaply, via `Arc`/clone) across producers and
/// the reaper.
#[derive(Clone)]
pub struct ControlPlaneHandle {
    pub store: Arc<dyn TaskRegistry>,
    pub boot_id: String,
    pub(crate) recovered_goal_ids: Arc<Mutex<Vec<String>>>,
}

impl ControlPlaneHandle {
    /// Open the durable store at `<data_dir>/control_plane.db`, mint a fresh
    /// `boot_id`, and run the one-shot crash-recovery sweep. Prior-boot non-goal
    /// `Running` tasks become `Lost`; prior-boot goals follow the configured
    /// restart recovery policy. Additive and fail-safe: a fresh install gets an
    /// empty DB.
    ///
    /// SINGLE-WRITER ASSUMPTION (review finding #8): recovery treats a *different*
    /// `boot_id` as proof the prior owner is gone. That holds under the deployment
    /// invariant of one daemon per `data_dir`. The engine-coordinated wiring that mounts
    /// this into `DaemonRegistry` MUST enforce that invariant with an OS advisory lock
    /// (`flock`/`O_EXCL` lockfile on `data_dir`) so two concurrent daemons can never both
    /// run recovery and reap each other's live tasks. Until that lock lands, do not run
    /// two daemons on one workspace.
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
        let store: Arc<dyn TaskRegistry> = Arc::new(SqliteTaskStore::new(data_dir)?);
        let recovery =
            reaper::recovery_pass(store.as_ref(), &boot_id, goal_restart_recovery).await?;
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
            boot_id,
            recovered_goal_ids: Arc::new(Mutex::new(recovery.restart_goal_ids)),
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
        // Hoist owned clones to locals so the spawn! future captures them by value
        // (not `&self`, which the macro would otherwise hold across the 'static boundary).
        let store = Arc::clone(&self.store);
        let boot_id = self.boot_id.clone();
        zeroclaw_spawn::spawn!(reaper::reaper_loop(
            store,
            boot_id,
            max_runtime_secs,
            goal_restart_recovery,
            cancel
        ))
    }
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
