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
use serde_json::json;
use zeroclaw_config::schema::GoalRestartRecovery;

use super::authority::is_authoritative;
use super::goal_task::{
    GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState, GoalTaskRegistry,
};
use super::task_registry::{TaskKind, TaskRecord, TaskRegistry, TaskStatus};

/// How often the periodic sweep runs.
pub const REAP_INTERVAL: Duration = Duration::from_secs(60);
/// Default grace before a same-boot task with a parseable stale heartbeat is timed out.
pub const DEFAULT_MAX_RUNTIME_SECS: i64 = 3600;

/// Summary of the one-shot boot recovery sweep.
///
/// `restart_goal_ids` is a delivery queue for goals recovered under
/// `[goal].restart_recovery = "last_state"`; the task table remains the source
/// of truth for which goals are running or paused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryPassReport {
    /// Number of prior-boot records whose persisted status/pause state changed.
    pub recovered: usize,
    /// Goal ids that should receive a synthetic continuation after channels are
    /// ready.
    pub restart_goal_ids: Vec<String>,
}

/// Age in seconds of an RFC3339 instant, or `None` if it cannot be parsed. We NEVER
/// reap on a timestamp we could not read — a corrupt `heartbeat_at` must not kill a
/// task.
fn age_secs(ts: &str, now: DateTime<Utc>) -> Option<i64> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| (now - t.with_timezone(&Utc)).num_seconds())
}

/// One-shot crash-recovery sweep.
///
/// Prior-boot non-goal `Running` records are reclaimed as `Lost`. Prior-boot
/// `Goal` records recover according to trusted `[goal].restart_recovery`
/// policy. Same-boot records are not yet present during normal startup, and the
/// authority guard protects against split-brain cases.
pub async fn recovery_pass(
    store: &dyn TaskRegistry,
    goal_store: &dyn GoalTaskRegistry,
    boot_id: &str,
    goal_restart_recovery: GoalRestartRecovery,
) -> anyhow::Result<RecoveryPassReport> {
    let mut report = RecoveryPassReport::default();
    for rec in store.list_running().await? {
        if rec.owner_boot_id != boot_id
            && recover_prior_boot_running(store, goal_store, &rec, boot_id, goal_restart_recovery)
                .await?
        {
            report.recovered += 1;
            if rec.kind == TaskKind::Goal && goal_restart_recovery == GoalRestartRecovery::LastState
            {
                report.restart_goal_ids.push(rec.id);
            }
        }
    }
    Ok(report)
}

/// The periodic supervision loop. Runs until `cancel` is triggered. Each tick:
///   * prior-boot non-goal `Running` records → `Lost`;
///   * prior-boot goal `Running` records → configured restart recovery policy;
///   * same-boot `Running` records whose heartbeat is older than `max_runtime_secs`
///     → `TimedOut` (the daemon's own hung task — we own it, so we may time it out);
///   * fresh same-boot records → skipped.
///
/// Errors are logged, never propagated: a reaper panic must not take down the daemon
/// (mirrors the ACP idle-reaper's detached-task discipline).
pub async fn reaper_loop(
    store: Arc<dyn TaskRegistry>,
    goal_store: Arc<dyn GoalTaskRegistry>,
    boot_id: String,
    max_runtime_secs: i64,
    goal_restart_recovery: GoalRestartRecovery,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut tick = tokio::time::interval(REAP_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tick.tick() => {
                if let Err(e) = sweep(
                    store.as_ref(),
                    goal_store.as_ref(),
                    &boot_id,
                    max_runtime_secs,
                    goal_restart_recovery,
                ).await {
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
    goal_store: &dyn GoalTaskRegistry,
    boot_id: &str,
    max_runtime_secs: i64,
    goal_restart_recovery: GoalRestartRecovery,
) -> anyhow::Result<()> {
    let now = Utc::now();
    for rec in store.list_running().await? {
        if rec.owner_boot_id != boot_id {
            let _ =
                recover_prior_boot_running(store, goal_store, &rec, boot_id, goal_restart_recovery)
                    .await?;
        } else {
            // Our own boot: the owning daemon (this process) is alive, so the only
            // legitimate reason to terminate is a task that USES heartbeats and has gone
            // silent past the grace window. A task with NO heartbeat is NOT timed out on
            // `started_at` — a legitimately long-running task must not be killed merely
            // for running a while. An unparseable heartbeat is never grounds
            // for reaping the task.
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

async fn recover_prior_boot_running(
    store: &dyn TaskRegistry,
    goal_store: &dyn GoalTaskRegistry,
    rec: &TaskRecord,
    boot_id: &str,
    goal_restart_recovery: GoalRestartRecovery,
) -> anyhow::Result<bool> {
    if rec.kind != TaskKind::Goal {
        return store.reconcile_lost(&rec.id, boot_id).await;
    }

    if !is_authoritative(rec, boot_id) {
        return Ok(false);
    }

    if goal_store.get_goal_task(&rec.id).await?.is_none() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "task_id": rec.id })),
            "control-plane: goal task missing extension during restart recovery"
        );
        return store.reconcile_lost(&rec.id, boot_id).await;
    }

    match goal_restart_recovery {
        GoalRestartRecovery::LastState => {
            store
                .claim_owner(&rec.id, std::process::id(), boot_id)
                .await?;
            Ok(matches!(
                store.get(&rec.id).await?,
                Some(task)
                    if task.status == TaskStatus::Running
                        && task.owner_boot_id == boot_id
            ))
        }
        GoalRestartRecovery::Paused => {
            goal_store
                .pause_goal_task_if_status(
                    &rec.id,
                    TaskStatus::Running,
                    daemon_restart_pause(rec, boot_id),
                )
                .await
        }
    }
}

fn daemon_restart_pause(rec: &TaskRecord, boot_id: &str) -> GoalPauseState {
    let description =
        crate::i18n::get_required_cli_string("goal-command-restart-recovery-paused-description");
    let message = crate::i18n::get_required_cli_string("goal-command-daemon-restarted-blocker");
    GoalPauseState {
        reason: GoalPauseReason::DaemonRestart,
        description: Some(description),
        blockers: vec![GoalBlocker {
            kind: GoalBlockerKind::RestartRecovery,
            message,
            payload: Some(json!({
                "previous_boot_id": rec.owner_boot_id,
                "recovered_by_boot_id": boot_id,
            })),
        }],
    }
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
        let report = recovery_pass(&s, &s, "boot-NEW", GoalRestartRecovery::default())
            .await
            .unwrap();
        assert_eq!(report.recovered, 1);
        assert!(report.restart_goal_ids.is_empty());
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
    async fn recovery_keeps_prior_boot_running_goals_in_last_state_by_default() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut goal = rec("goal", "boot-OLD", 999_999, None);
        goal.kind = TaskKind::Goal;
        s.create_goal(
            goal,
            crate::control_plane::goal_task::GoalTaskRecord {
                task_id: "goal".into(),
                objective: "keep working".into(),
                effective_token_limit: None,
                effective_cost_limit_usd: None,
                pause_reason: None,
                pause_description: None,
                blockers: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();

        let report = recovery_pass(&s, &s, "boot-NEW", GoalRestartRecovery::default())
            .await
            .unwrap();

        assert_eq!(report.recovered, 1);
        assert_eq!(report.restart_goal_ids, vec!["goal"]);
        let recovered = s.get("goal").await.unwrap().unwrap();
        assert_eq!(recovered.status, TaskStatus::Running);
        assert_eq!(recovered.owner_boot_id, "boot-NEW");
        assert_eq!(recovered.owner_pid, std::process::id());
        let goal = s.get_goal_task("goal").await.unwrap().unwrap();
        assert!(goal.pause_reason.is_none());
        assert!(goal.blockers.is_empty());
    }

    #[tokio::test]
    async fn recovery_pauses_prior_boot_goals_when_policy_requires_it() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut goal = rec("goal", "boot-OLD", 999_999, None);
        goal.kind = TaskKind::Goal;
        s.create_goal(
            goal,
            crate::control_plane::goal_task::GoalTaskRecord {
                task_id: "goal".into(),
                objective: "keep working".into(),
                effective_token_limit: None,
                effective_cost_limit_usd: None,
                pause_reason: None,
                pause_description: None,
                blockers: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();

        let report = recovery_pass(&s, &s, "boot-NEW", GoalRestartRecovery::Paused)
            .await
            .unwrap();

        assert_eq!(report.recovered, 1);
        assert!(report.restart_goal_ids.is_empty());
        assert_eq!(
            s.get("goal").await.unwrap().unwrap().status,
            TaskStatus::Paused
        );
        let recovered = s.get_goal_task("goal").await.unwrap().unwrap();
        assert_eq!(recovered.pause_reason, Some(GoalPauseReason::DaemonRestart));
        assert_eq!(recovered.blockers.len(), 1);
        assert_eq!(recovered.blockers[0].kind, GoalBlockerKind::RestartRecovery);
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
        sweep(&s, &s, "boot-NEW", 600, GoalRestartRecovery::default())
            .await
            .unwrap();
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
