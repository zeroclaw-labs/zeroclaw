//! The supervision reaper — moves abandoned `Running` tasks to a terminal state
//! from OUTSIDE the task body, which the flat-file design could never do.
//!
//! Two entry points, both modelled on the ACP idle-reaper
//! (`zeroclaw_channels::orchestrator::acp_server` — `interval(60s)` + lock-aware
//! skip):
//!   * [`recovery_pass`] — a one-shot sweep at boot that reclaims prior-boot orphans.
//!   * [`reaper_loop`] — the periodic sweep that also times out the daemon's own
//!     hung tasks.
//!
//! Safety: reclamation goes through [`TaskRegistry::reconcile_lost`], which itself
//! enforces [`super::authority::is_authoritative`] — a live same-boot owner's
//! heart-beating task is never reclaimed (the split-brain guard).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::task_registry::{TaskRegistry, TaskStatus};

/// How often the periodic sweep runs.
pub const REAP_INTERVAL: Duration = Duration::from_secs(60);
/// Default grace before a same-boot task with a stale/absent heartbeat is timed out.
pub const DEFAULT_MAX_RUNTIME_SECS: i64 = 3600;

/// Age in seconds of an RFC3339 instant, or `None` if it cannot be parsed. We NEVER
/// reap on a timestamp we could not read — a corrupt `heartbeat_at` must not kill a
/// task (review finding #9).
fn age_secs(ts: &str, now: DateTime<Utc>) -> Option<i64> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| (now - t.with_timezone(&Utc)).num_seconds())
}

/// One-shot crash-recovery sweep: reclaim every `Running` record left behind by a
/// PRIOR boot. Safe to run at every startup — same-boot records are not yet present
/// (this runs before the reaper spawns) and the authority guard protects any that
/// are. Returns the number of records reclaimed.
pub async fn recovery_pass(store: &dyn TaskRegistry, boot_id: &str) -> anyhow::Result<usize> {
    let mut reclaimed = 0usize;
    for rec in store.list_running().await? {
        if rec.owner_boot_id != boot_id && store.reconcile_lost(&rec.id, boot_id).await? {
            reclaimed += 1;
        }
    }
    Ok(reclaimed)
}

pub async fn reaper_loop(
    store: Arc<dyn TaskRegistry>,
    boot_id: String,
    max_runtime_secs: i64,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut tick = tokio::time::interval(REAP_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tick.tick() => {
                if let Err(e) = sweep(store.as_ref(), &boot_id, max_runtime_secs).await {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({ "error": format!("{e}") })),
                        "control-plane reaper sweep failed"
                    );
                }
            }
        }
    }
}

/// A single sweep — separated for direct unit testing.
pub async fn sweep(
    store: &dyn TaskRegistry,
    boot_id: &str,
    max_runtime_secs: i64,
) -> anyhow::Result<()> {
    let now = Utc::now();
    for rec in store.list_running().await? {
        if rec.owner_boot_id != boot_id {
            // Prior-boot orphan — reclaim (authority-guarded inside reconcile_lost).
            let _ = store.reconcile_lost(&rec.id, boot_id).await?;
        } else {
            if let Some(beat) = rec.heartbeat_at.as_deref()
                && age_secs(beat, now).is_some_and(|age| age > max_runtime_secs)
            {
                store
                    .update_status(
                        &rec.id,
                        TaskStatus::TimedOut,
                        None,
                        Some("heartbeat timeout".into()),
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::task_registry::{TaskKind, TaskRecord};
    use crate::control_plane::task_store_sqlite::SqliteTaskStore;

    fn rec(id: &str, boot: &str, pid: u32, beat_secs_ago: Option<i64>) -> TaskRecord {
        let beat = beat_secs_ago.map(|s| (Utc::now() - chrono::Duration::seconds(s)).to_rfc3339());
        TaskRecord {
            id: id.into(),
            kind: TaskKind::Delegate,
            agent: "main".into(),
            status: TaskStatus::Running,
            owner_pid: pid,
            owner_boot_id: boot.into(),
            heartbeat_at: beat,
            depth: 0,
            parent_id: None,
            originator_route: None,
            delivered: false,
            idem_key: None,
            principal_id: None,
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
        }
    }

    #[tokio::test]
    async fn recovery_reclaims_prior_boot_orphans() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("orphan", "boot-OLD", 999_999, None))
            .await
            .unwrap();
        s.create(rec("mine", "boot-NEW", std::process::id(), Some(0)))
            .await
            .unwrap();
        let n = recovery_pass(&s, "boot-NEW").await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            s.get("orphan").await.unwrap().unwrap().status,
            TaskStatus::Lost
        );
        assert_eq!(
            s.get("mine").await.unwrap().unwrap().status,
            TaskStatus::Running
        );
    }

    #[tokio::test]
    async fn sweep_times_out_own_stale_task_but_not_fresh() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let me = std::process::id();
        s.create(rec("stale", "boot-NEW", me, Some(99_999)))
            .await
            .unwrap(); // very old beat
        s.create(rec("fresh", "boot-NEW", me, Some(1)))
            .await
            .unwrap(); // just beat
        sweep(&s, "boot-NEW", 600).await.unwrap();
        assert_eq!(
            s.get("stale").await.unwrap().unwrap().status,
            TaskStatus::TimedOut
        );
        assert_eq!(
            s.get("fresh").await.unwrap().unwrap().status,
            TaskStatus::Running
        );
    }
}
