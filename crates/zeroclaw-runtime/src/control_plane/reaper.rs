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
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::authority::{is_authoritative, is_authoritative_owner};
use super::task_registry::{TaskRegistry, TaskStatus, TerminalSettlementIntent};

/// How often the periodic sweep runs.
pub const REAP_INTERVAL: Duration = Duration::from_secs(60);
/// Default grace before a same-boot task with a stale/absent heartbeat is timed out.
pub const DEFAULT_MAX_RUNTIME_SECS: i64 = 3600;

async fn artifact_validation_error(intent: &TerminalSettlementIntent) -> Option<String> {
    let expected_filename = format!("{}.json", intent.task_id);
    let expected_artifact_ref = format!("artifact:{expected_filename}");
    if intent.desired_status == TaskStatus::Completed
        && intent.artifact_ref.as_deref() != Some(expected_artifact_ref.as_str())
    {
        return Some(format!(
            "artifact reference does not match completed task {}",
            intent.task_id,
        ));
    }
    let artifact_path = std::path::Path::new(&intent.artifact_path);
    if !artifact_path.is_absolute()
        || artifact_path.file_name().and_then(|name| name.to_str())
            != Some(expected_filename.as_str())
    {
        return Some(format!(
            "artifact path '{}' does not identify task {}",
            intent.artifact_path, intent.task_id
        ));
    }
    let bytes = match tokio::fs::read(&intent.artifact_path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return Some(format!(
                "artifact '{}' cannot be read: {error}",
                intent.artifact_path
            ));
        }
    };
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != intent.artifact_sha256 {
        return Some(format!(
            "artifact '{}' SHA-256 mismatch: expected {}, got {}",
            intent.artifact_path, intent.artifact_sha256, actual
        ));
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct DelegateOutputIdentity {
        task_id: String,
        output: serde_json::Value,
    }

    match serde_json::from_slice::<DelegateOutputIdentity>(&bytes) {
        Ok(output)
            if output.task_id == intent.task_id
                && (output.output.is_null() || output.output.is_string()) =>
        {
            None
        }
        Ok(output) => Some(format!(
            "artifact '{}' has invalid output identity or payload for task '{}' (embedded task '{}')",
            intent.artifact_path, intent.task_id, output.task_id
        )),
        Err(error) => Some(format!(
            "artifact '{}' is not a valid delegate output: {error}",
            intent.artifact_path
        )),
    }
}

/// Recover durable terminal settlements before ordinary Lost reconciliation.
/// A live or uncertain owner is left alone; only an absent or reused recorded
/// owner may drive recovery.
async fn recover_terminal_settlements(store: &dyn TaskRegistry) -> anyhow::Result<usize> {
    let mut recovered = 0usize;
    for intent in store.list_terminal_settlement_intents().await? {
        let Some(task) = store.get(&intent.task_id).await? else {
            if is_authoritative_owner(intent.owner_pid, &intent.owner_boot_id) {
                let _ = store.discard_terminal_settlement_intent(&intent).await?;
            }
            continue;
        };

        // A terminal winner is authoritative. The outbox row can only be stale.
        if task.status.is_terminal() {
            if is_authoritative_owner(intent.owner_pid, &intent.owner_boot_id) {
                let _ = store.discard_terminal_settlement_intent(&intent).await?;
            }
            continue;
        }

        if task.owner_pid != intent.owner_pid || task.owner_boot_id != intent.owner_boot_id {
            // A resumed task has a new owner. Remove the old owner's intent only
            // after proving that old owner is absent or its PID was reused.
            if is_authoritative_owner(intent.owner_pid, &intent.owner_boot_id) {
                let _ = store.discard_terminal_settlement_intent(&intent).await?;
            }
            continue;
        }
        if !is_authoritative(&task) {
            continue;
        }

        let (resolved_status, output, error) =
            if let Some(artifact_error) = artifact_validation_error(&intent).await {
                (
                    TaskStatus::Failed,
                    None,
                    Some(format!(
                        "terminal settlement recovery failed for task {}: {artifact_error}",
                        intent.task_id
                    )),
                )
            } else {
                (
                    intent.desired_status,
                    if intent.desired_status == TaskStatus::Completed {
                        intent.artifact_ref.clone()
                    } else {
                        None
                    },
                    intent.terminal_error.clone(),
                )
            };

        if store
            .promote_terminal_settlement(&intent, resolved_status, output, error)
            .await?
        {
            recovered += 1;
        }
    }
    Ok(recovered)
}

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
/// are. Returns the number of records reconciled.
pub async fn recovery_pass(store: &dyn TaskRegistry, boot_id: &str) -> anyhow::Result<usize> {
    let mut reclaimed = recover_terminal_settlements(store).await?;
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
    let _ = recover_terminal_settlements(store).await?;
    let now = Utc::now();
    for rec in store.list_running().await? {
        if rec.owner_boot_id != boot_id {
            // Prior-boot orphan — reclaim (authority-guarded inside reconcile_lost).
            let _ = store.reconcile_lost(&rec.id, boot_id).await?;
        } else {
            if let Some(beat) = rec.heartbeat_at.as_deref()
                && age_secs(beat, now).is_some_and(|age| age > max_runtime_secs)
            {
                let _ = store
                    .reconcile_timed_out(&rec.id, rec.owner_pid, &rec.owner_boot_id, beat)
                    .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::task_registry::{
        TaskKind, TaskRecord, TaskStatus, TerminalSettlementIntent,
    };
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

    fn intent(
        task_id: &str,
        artifact_path: &std::path::Path,
        desired_status: TaskStatus,
        bytes: &[u8],
    ) -> TerminalSettlementIntent {
        TerminalSettlementIntent {
            task_id: task_id.into(),
            owner_pid: 999_999,
            owner_boot_id: "boot-OLD".into(),
            desired_status,
            artifact_path: artifact_path.display().to_string(),
            artifact_ref: Some(format!("artifact:{task_id}.json")),
            artifact_sha256: hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes)),
            terminal_error: None,
        }
    }

    #[tokio::test]
    async fn recovery_promotes_a_valid_published_intent_and_keeps_output_readable() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let task_id = "settlement-valid";
        let path = dir.path().join(format!("{task_id}.json"));
        let bytes = br#"{"task_id":"settlement-valid","output":"done"}"#;
        tokio::fs::write(&path, bytes).await.unwrap();
        s.create(rec(task_id, "boot-OLD", 999_999, None))
            .await
            .unwrap();
        let intent = intent(task_id, &path, TaskStatus::Completed, bytes);
        assert!(s.persist_terminal_settlement_intent(intent).await.unwrap());

        assert_eq!(recovery_pass(&s, "boot-NEW").await.unwrap(), 1);
        let snapshot = s.get_snapshot(task_id).await.unwrap().unwrap();
        assert_eq!(snapshot.task.status, TaskStatus::Completed);
        assert_eq!(
            snapshot.output.as_deref(),
            Some("artifact:settlement-valid.json")
        );
        assert_eq!(tokio::fs::read(&path).await.unwrap(), &bytes[..]);
        assert!(
            s.list_terminal_settlement_intents()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn recovery_never_completes_an_intent_before_artifact_publication() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let task_id = "settlement-missing";
        let path = dir.path().join(format!("{task_id}.json"));
        s.create(rec(task_id, "boot-OLD", 999_999, None))
            .await
            .unwrap();
        let intent = intent(task_id, &path, TaskStatus::Completed, b"published later");
        assert!(s.persist_terminal_settlement_intent(intent).await.unwrap());

        assert_eq!(recovery_pass(&s, "boot-NEW").await.unwrap(), 1);
        let snapshot = s.get_snapshot(task_id).await.unwrap().unwrap();
        assert_eq!(snapshot.task.status, TaskStatus::Failed);
        assert_ne!(snapshot.task.status, TaskStatus::Completed);
        assert!(
            snapshot
                .error
                .as_deref()
                .is_some_and(|error| error.contains("cannot be read"))
        );
    }

    #[tokio::test]
    async fn recovery_rejects_a_digest_mismatched_artifact() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let task_id = "settlement-corrupt";
        let path = dir.path().join(format!("{task_id}.json"));
        tokio::fs::write(&path, b"corrupt bytes").await.unwrap();
        s.create(rec(task_id, "boot-OLD", 999_999, None))
            .await
            .unwrap();
        let intent = intent(task_id, &path, TaskStatus::Completed, b"original bytes");
        assert!(s.persist_terminal_settlement_intent(intent).await.unwrap());

        assert_eq!(recovery_pass(&s, "boot-NEW").await.unwrap(), 1);
        let snapshot = s.get_snapshot(task_id).await.unwrap().unwrap();
        assert_eq!(snapshot.task.status, TaskStatus::Failed);
        assert!(
            snapshot
                .error
                .as_deref()
                .is_some_and(|error| error.contains("SHA-256 mismatch"))
        );
    }

    #[tokio::test]
    async fn recovery_rejects_digest_matched_output_for_another_task() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let task_id = "settlement-mismatched";
        let path = dir.path().join(format!("{task_id}.json"));
        let bytes = br#"{"task_id":"another-task","output":"done"}"#;
        tokio::fs::write(&path, bytes).await.unwrap();
        s.create(rec(task_id, "boot-OLD", 999_999, None))
            .await
            .unwrap();
        let intent = intent(task_id, &path, TaskStatus::Completed, bytes);
        assert!(s.persist_terminal_settlement_intent(intent).await.unwrap());

        assert_eq!(recovery_pass(&s, "boot-NEW").await.unwrap(), 1);
        let snapshot = s.get_snapshot(task_id).await.unwrap().unwrap();
        assert_eq!(snapshot.task.status, TaskStatus::Failed);
        assert!(
            snapshot
                .error
                .as_deref()
                .is_some_and(|error| error.contains("embedded task 'another-task'"))
        );
    }

    #[tokio::test]
    async fn competing_terminal_winner_is_preserved_and_stale_intent_removed() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let task_id = "settlement-cancelled";
        let path = dir.path().join(format!("{task_id}.json"));
        let bytes = b"late output";
        tokio::fs::write(&path, bytes).await.unwrap();
        s.create(rec(task_id, "boot-OLD", 999_999, None))
            .await
            .unwrap();
        let intent = intent(task_id, &path, TaskStatus::Completed, bytes);
        assert!(s.persist_terminal_settlement_intent(intent).await.unwrap());
        assert!(
            s.transition_terminal(
                task_id,
                TaskStatus::Cancelled,
                None,
                Some("cancelled by user request".into()),
            )
            .await
            .unwrap()
        );

        recovery_pass(&s, "boot-NEW").await.unwrap();
        let snapshot = s.get_snapshot(task_id).await.unwrap().unwrap();
        assert_eq!(snapshot.task.status, TaskStatus::Cancelled);
        assert_eq!(snapshot.error.as_deref(), Some("cancelled by user request"));
        assert!(
            s.list_terminal_settlement_intents()
                .await
                .unwrap()
                .is_empty()
        );
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
