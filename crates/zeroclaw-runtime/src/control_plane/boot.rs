//! Boot wiring for the control-plane — minted once per daemon run.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::reaper;
use super::task_registry::TaskRegistry;
use super::task_store_sqlite::SqliteTaskStore;

/// The live control-plane, shared (cheaply, via `Arc`/clone) across producers and
/// the reaper.
#[derive(Clone)]
pub struct ControlPlaneHandle {
    pub store: Arc<dyn TaskRegistry>,
    pub boot_id: String,
}

impl ControlPlaneHandle {
    /// Open the durable task store for producers and observers without gaining
    /// startup-recovery or reaper authority.
    pub fn open(data_dir: &Path) -> Result<Self> {
        Ok(Self {
            store: Arc::new(SqliteTaskStore::new(data_dir)?),
            boot_id: process_identity().to_string(),
        })
    }
}

/// Daemon-only capability for startup recovery and the periodic reaper.
///
/// Producers receive [`ControlPlaneHandle`] instead, so opening the shared store
/// cannot accidentally reclaim work or start another supervisor.
pub(crate) struct ControlPlaneRecoveryOwner {
    handle: ControlPlaneHandle,
}

impl ControlPlaneRecoveryOwner {
    pub(crate) async fn start(data_dir: &Path) -> Result<Self> {
        Self::start_with_boot_id(data_dir, process_identity().to_string()).await
    }

    /// As [`Self::start`] but with a caller-supplied `boot_id` — lets `DaemonRegistry`
    /// reuse a process-stable run-id across reloads instead of a fresh UUID.
    pub(crate) async fn start_with_boot_id(data_dir: &Path, boot_id: String) -> Result<Self> {
        let store: Arc<dyn TaskRegistry> = Arc::new(SqliteTaskStore::new(data_dir)?);
        let reclaimed = reaper::recovery_pass(store.as_ref(), &boot_id).await?;
        if reclaimed > 0 {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({ "reclaimed": reclaimed, "boot_id": boot_id })
                    ),
                "control-plane: reclaimed prior-boot orphan tasks at startup"
            );
        }
        Ok(Self {
            handle: ControlPlaneHandle { store, boot_id },
        })
    }

    pub(crate) fn handle(&self) -> &ControlPlaneHandle {
        &self.handle
    }

    pub(crate) fn spawn_reaper(
        &self,
        max_runtime_secs: i64,
        cancel: CancellationToken,
    ) -> JoinHandle<()> {
        // Hoist owned clones to locals so the spawn! future captures them by value
        // (not `&self`, which the macro would otherwise hold across the 'static boundary).
        let store = Arc::clone(&self.handle.store);
        let boot_id = self.handle.boot_id.clone();
        zeroclaw_spawn::spawn!(reaper::reaper_loop(
            store,
            boot_id,
            max_runtime_secs,
            cancel
        ))
    }
}

fn process_identity() -> &'static str {
    static IDENTITY: OnceLock<String> = OnceLock::new();
    IDENTITY.get_or_init(|| {
        let pid = std::process::id();
        let started_at = current_process_started_at()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".into());
        format!("zc-process-v1:{pid}:{started_at}:{}", uuid::Uuid::new_v4())
    })
}

fn current_process_started_at() -> Option<u64> {
    let pid = sysinfo::get_current_pid().ok()?;
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    system
        .process(pid)
        .map(sysinfo::Process::start_time)
        .filter(|started_at| *started_at > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_in_tempdir_and_reap_handle() {
        let dir = tempfile::tempdir().unwrap();
        let owner = ControlPlaneRecoveryOwner::start(dir.path()).await.unwrap();
        assert!(!owner.handle().boot_id.is_empty());
        // a reaper spawns and stops cleanly on cancel
        let cancel = CancellationToken::new();
        let jh = owner.spawn_reaper(600, cancel.clone());
        cancel.cancel();
        jh.await.unwrap();
    }

    #[tokio::test]
    async fn boot_id_distinguishes_runs_over_the_same_db() {
        use crate::control_plane::task_registry::{TaskKind, TaskRecord, TaskStatus};
        let dir = tempfile::tempdir().unwrap();
        // First "boot" registers a running task, then the daemon "dies".
        let h1 = ControlPlaneRecoveryOwner::start_with_boot_id(dir.path(), "boot-1".into())
            .await
            .unwrap();
        h1.handle()
            .store
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
        // Second boot recovers the orphan at startup.
        let h2 = ControlPlaneRecoveryOwner::start_with_boot_id(dir.path(), "boot-2".into())
            .await
            .unwrap();
        assert_eq!(
            h2.handle().store.get("t").await.unwrap().unwrap().status,
            TaskStatus::Lost
        );
    }

    #[tokio::test]
    async fn producer_open_does_not_run_recovery() {
        use crate::control_plane::task_registry::{TaskKind, TaskRecord, TaskStatus};
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteTaskStore::new(dir.path()).unwrap();
        store
            .create(TaskRecord {
                id: "producer-observer".into(),
                kind: TaskKind::Delegate,
                agent: "main".into(),
                status: TaskStatus::Running,
                owner_pid: 999_999,
                owner_boot_id: "legacy-dead-owner".into(),
                heartbeat_at: None,
                depth: 0,
                parent_id: None,
                originator_route: Some("main".into()),
                delivered: false,
                idem_key: None,
                principal_id: None,
                started_at: "2026-06-18T00:00:00Z".into(),
                finished_at: None,
            })
            .await
            .unwrap();
        drop(store);

        let observer = ControlPlaneHandle::open(dir.path()).unwrap();
        assert_eq!(
            observer
                .store
                .get("producer-observer")
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Running
        );
    }

    #[tokio::test]
    async fn recovery_does_not_reclaim_a_live_observer_owner() {
        use crate::control_plane::task_registry::{TaskKind, TaskRecord, TaskStatus};
        let dir = tempfile::tempdir().unwrap();
        let observer = ControlPlaneHandle::open(dir.path()).unwrap();
        observer
            .store
            .create(TaskRecord {
                id: "live-observer".into(),
                kind: TaskKind::Delegate,
                agent: "main".into(),
                status: TaskStatus::Running,
                owner_pid: std::process::id(),
                owner_boot_id: observer.boot_id.clone(),
                heartbeat_at: None,
                depth: 0,
                parent_id: None,
                originator_route: Some("main".into()),
                delivered: false,
                idem_key: None,
                principal_id: None,
                started_at: "2026-06-18T00:00:00Z".into(),
                finished_at: None,
            })
            .await
            .unwrap();

        let recovery =
            ControlPlaneRecoveryOwner::start_with_boot_id(dir.path(), "daemon-boot".into())
                .await
                .unwrap();
        assert_eq!(
            recovery
                .handle()
                .store
                .get("live-observer")
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Running
        );
    }

    #[test]
    fn producer_open_reuses_process_identity() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first = ControlPlaneHandle::open(first.path()).unwrap();
        let second = ControlPlaneHandle::open(second.path()).unwrap();
        assert_eq!(first.boot_id, second.boot_id);
        assert!(first.boot_id.starts_with("zc-process-v1:"));
    }

    #[test]
    #[ignore]
    fn live_owner_process_helper() {
        if std::env::var_os("ZEROCLAW_LIVE_OWNER_TEST_HELPER").is_some() {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }

    #[tokio::test]
    async fn recovery_retains_live_foreign_process_then_reclaims_after_exit() {
        use crate::control_plane::task_registry::{TaskKind, TaskRecord, TaskStatus};

        struct ChildGuard(std::process::Child);

        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let current_exe = std::env::current_exe().unwrap();
        let mut child = ChildGuard(
            std::process::Command::new(current_exe)
                .args([
                    "--ignored",
                    "--exact",
                    "control_plane::boot::tests::live_owner_process_helper",
                ])
                .env("ZEROCLAW_LIVE_OWNER_TEST_HELPER", "1")
                .spawn()
                .unwrap(),
        );
        let child_pid = child.0.id();

        let pid = sysinfo::Pid::from_u32(child_pid);
        let mut started_at = None;
        for _ in 0..50 {
            let mut system = sysinfo::System::new();
            system.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                true,
                sysinfo::ProcessRefreshKind::nothing(),
            );
            started_at = system
                .process(pid)
                .map(sysinfo::Process::start_time)
                .filter(|value| *value > 0);
            if started_at.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let started_at = started_at.expect("child process should become visible to sysinfo");

        let dir = tempfile::tempdir().unwrap();
        let observer = ControlPlaneHandle::open(dir.path()).unwrap();
        observer
            .store
            .create(TaskRecord {
                id: "foreign-process-owner".into(),
                kind: TaskKind::Delegate,
                agent: "main".into(),
                status: TaskStatus::Running,
                owner_pid: child_pid,
                owner_boot_id: format!("zc-process-v1:{child_pid}:{started_at}:test-owner"),
                heartbeat_at: None,
                depth: 0,
                parent_id: None,
                originator_route: Some("main".into()),
                delivered: false,
                idem_key: None,
                principal_id: None,
                started_at: "2026-06-18T00:00:00Z".into(),
                finished_at: None,
            })
            .await
            .unwrap();

        let recovery =
            ControlPlaneRecoveryOwner::start_with_boot_id(dir.path(), "daemon-boot".into())
                .await
                .unwrap();
        assert_eq!(
            recovery
                .handle()
                .store
                .get("foreign-process-owner")
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Running
        );

        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(
            recovery
                .handle()
                .store
                .reconcile_lost("foreign-process-owner", "daemon-boot")
                .await
                .unwrap()
        );
        assert_eq!(
            recovery
                .handle()
                .store
                .get("foreign-process-owner")
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Lost
        );
    }
}
