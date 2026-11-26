use super::*;
use crate::control_plane::goal_task::{
    GoalAccountingState, GoalPauseReason, GoalPauseState, GoalTaskRecord, GoalTaskRegistry,
    GoalTransitionResult,
};
use crate::control_plane::task_registry::{TaskKind, TaskRegistry};
use zeroclaw_commands::goal::GoalBudgetSelection;
use zeroclaw_goal::task::{GoalControlIntent, GoalControlIntentKind, GoalControlIntentSettlement};

const SESSION: &str = "rpc_disposition-fixture";
const BOOT: &str = "disposition-fixture-boot";

fn task(id: &str, session_id: &str, boot_id: &str) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        kind: TaskKind::Goal,
        agent: "main".into(),
        status: TaskStatus::Running,
        owner_pid: std::process::id(),
        owner_boot_id: boot_id.into(),
        heartbeat_at: None,
        depth: 0,
        parent_id: None,
        originator_route: None,
        originator_chain: Vec::new(),
        delivered: false,
        idem_key: None,
        principal_id: None,
        session_id: Some(session_id.into()),
        execution_epoch: 1,
        started_at: "2026-06-18T00:00:00Z".into(),
        finished_at: None,
    }
}

fn extension(id: &str) -> GoalTaskRecord {
    GoalTaskRecord {
        task_id: id.into(),
        objective: "fixture objective".into(),
        effective_token_limit: Some(100),
        ..GoalTaskRecord::default()
    }
}

async fn create(store: &SqliteTaskStore, id: &str, session_id: &str, boot_id: &str) {
    assert_eq!(
        store
            .create_or_replace_session_goal(task(id, session_id, boot_id), extension(id))
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
}

async fn pause(store: &SqliteTaskStore, id: &str, session_id: &str) -> TaskRecord {
    assert_eq!(
        store
            .pause_session_goal(
                id,
                session_id,
                1,
                GoalPauseState {
                    reason: GoalPauseReason::NeedsUserInput,
                    description: Some("fixture clarification".into()),
                    blockers: Vec::new(),
                }
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    store
        .current_goal_for_session(session_id)
        .await
        .unwrap()
        .unwrap()
}

fn live(task: &TaskRecord) -> SessionDisposition {
    SessionDisposition::new(
        SESSION.into(),
        task,
        SessionDispositionKind::Replace,
        SessionDispositionOrigin::Live,
        BOOT.into(),
        Some(7),
    )
}

fn durable(task: &TaskRecord) -> SessionDisposition {
    SessionDisposition::new(
        SESSION.into(),
        task,
        SessionDispositionKind::Delete,
        SessionDispositionOrigin::Durable {
            backend_key: "disposition-fixture".into(),
            identity: SessionDispositionIdentity {
                backend: SessionDispositionBackend::ChatSqlite,
                instance_id: "fixture-db-instance".into(),
            },
        },
        BOOT.into(),
        None,
    )
}

fn resume_intent(task: &TaskRecord) -> GoalControlIntent {
    GoalControlIntent {
        intent_id: uuid::Uuid::new_v4().to_string(),
        session_id: task.session_id.clone().unwrap(),
        task_id: Some(task.id.clone()),
        expected_epoch: Some(task.execution_epoch),
        predecessor_task_id: None,
        kind: GoalControlIntentKind::Resume {
            has_response: false,
        },
        owner_pid: task.owner_pid,
        owner_boot_id: task.owner_boot_id.clone(),
        created_at: task.started_at.clone(),
    }
}

#[tokio::test]
async fn additive_v14_migration_preserves_populated_rows_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(dir.path()).unwrap();
    create(&store, "goal", SESSION, BOOT).await;
    let paused = pause(&store, "goal", SESSION).await;
    store
        .record_control_intent(&resume_intent(&paused))
        .await
        .unwrap();
    let mut ordinary = task("ordinary", SESSION, BOOT);
    ordinary.kind = TaskKind::Delegate;
    ordinary.session_id = None;
    ordinary.execution_epoch = 0;
    store.create(ordinary).await.unwrap();
    // The v15 migration adds only this table. Remove it to build the
    // previous on-disk schema without rewriting its populated rows.
    store
        .conn
        .lock()
        .execute_batch(
            "DROP TABLE goal_session_dispositions;
         PRAGMA user_version = 14;",
        )
        .unwrap();
    let snapshot = |sql: &str, store: &SqliteTaskStore| -> Vec<Vec<rusqlite::types::Value>> {
        let conn = store.conn.lock();
        let mut stmt = conn.prepare(sql).unwrap();
        let columns = stmt.column_count();
        stmt.query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let tasks = snapshot("SELECT * FROM tasks ORDER BY id", &store);
    let goals = snapshot("SELECT * FROM goal_tasks ORDER BY task_id", &store);
    let controls = snapshot(
        "SELECT * FROM goal_control_intents ORDER BY intent_id",
        &store,
    );
    drop(store);

    let store = SqliteTaskStore::new(dir.path()).unwrap();
    let version: i64 = store
        .conn
        .lock()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 15);
    assert_eq!(snapshot("SELECT * FROM tasks ORDER BY id", &store), tasks);
    assert_eq!(
        snapshot("SELECT * FROM goal_tasks ORDER BY task_id", &store),
        goals
    );
    assert_eq!(
        snapshot(
            "SELECT * FROM goal_control_intents ORDER BY intent_id",
            &store
        ),
        controls
    );
    assert!(store.list_session_dispositions().unwrap().is_empty());
    let intent = live(&paused);
    assert_eq!(
        store.prepare_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );
    drop(store);
    let reopened = SqliteTaskStore::new(dir.path()).unwrap();
    assert_eq!(reopened.list_session_dispositions().unwrap(), vec![intent]);
}

#[tokio::test]
async fn live_rejection_and_failed_commit_preserve_the_paused_goal() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(dir.path()).unwrap();
    create(&store, "goal", SESSION, BOOT).await;
    let paused = pause(&store, "goal", SESSION).await;
    let intent = live(&paused);
    assert_eq!(
        store.prepare_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );
    let mut stale = intent.clone();
    stale.execution_epoch += 1;
    assert_eq!(
        store.commit_live_session_disposition(&stale).unwrap(),
        DispositionResult::Conflict
    );
    assert_eq!(
        store.session_disposition(SESSION).unwrap(),
        Some(intent.clone())
    );
    store
        .conn
        .lock()
        .execute_batch(
            "CREATE TABLE injected_commit_parent (id TEXT PRIMARY KEY);
         CREATE TABLE injected_commit_child (
             id TEXT REFERENCES injected_commit_parent(id) DEFERRABLE INITIALLY DEFERRED
         );
         CREATE TRIGGER injected_commit_failure AFTER DELETE ON tasks
         BEGIN INSERT INTO injected_commit_child VALUES ('missing parent'); END;",
        )
        .unwrap();
    let error = store.commit_live_session_disposition(&intent).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("commit live Goal session disposition")
    );
    assert_eq!(
        store.session_disposition(SESSION).unwrap(),
        Some(intent.clone())
    );
    let before_reopen = store
        .current_goal_for_session(SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before_reopen.id, paused.id);
    assert_eq!(before_reopen.status, TaskStatus::Paused);
    assert_eq!(before_reopen.execution_epoch, paused.execution_epoch);
    let before_reopen_goal = store.get_goal_task("goal").await.unwrap().unwrap();
    assert_eq!(
        before_reopen_goal.pause_reason,
        Some(GoalPauseReason::NeedsUserInput)
    );
    assert_eq!(
        before_reopen_goal.pause_description.as_deref(),
        Some("fixture clarification")
    );
    drop(store);

    let store = SqliteTaskStore::new(dir.path()).unwrap();
    assert_eq!(
        store.session_disposition(SESSION).unwrap(),
        Some(intent.clone())
    );
    assert_eq!(
        store
            .abort_session_disposition(&intent, intent.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::Applied
    );
    assert_eq!(
        store
            .abort_session_disposition(&intent, intent.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::AlreadySettled
    );
    let goal = store
        .current_goal_for_session(SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.id, paused.id);
    assert_eq!(goal.status, TaskStatus::Paused);
    assert_eq!(goal.execution_epoch, paused.execution_epoch);
    let goal = store.get_goal_task("goal").await.unwrap().unwrap();
    assert_eq!(goal.pause_reason, Some(GoalPauseReason::NeedsUserInput));
    assert_eq!(
        goal.pause_description.as_deref(),
        Some("fixture clarification")
    );
}

#[tokio::test]
async fn live_commit_busy_writer_rolls_back_then_succeeds_after_release() {
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(SqliteTaskStore::new(dir.path()).unwrap());
    create(&store, "goal", SESSION, BOOT).await;
    let paused = pause(&store, "goal", SESSION).await;
    let intent = live(&paused);
    assert_eq!(
        store.prepare_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );

    let blocker = rusqlite::Connection::open(dir.path().join("control_plane.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let blocked_store = std::sync::Arc::clone(&store);
    let blocked_intent = intent.clone();
    let blocked =
        std::thread::spawn(move || blocked_store.commit_live_session_disposition(&blocked_intent));
    let result = blocked.join().unwrap();
    assert!(result.is_err());
    assert_eq!(
        store.session_disposition(SESSION).unwrap(),
        Some(intent.clone())
    );
    let goal = store
        .current_goal_for_session(SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.status, TaskStatus::Paused);
    assert_eq!(goal.execution_epoch, paused.execution_epoch);

    blocker.execute_batch("ROLLBACK;").unwrap();
    assert_eq!(
        store.commit_live_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );
    assert!(store.session_disposition(SESSION).unwrap().is_none());
}

#[tokio::test]
async fn committed_cleanup_failure_rolls_back_goal_changes_then_retries_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(dir.path()).unwrap();
    create(&store, "goal", SESSION, BOOT).await;
    let paused = pause(&store, "goal", SESSION).await;
    let mut intent = durable(&paused);
    assert_eq!(
        store.prepare_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );
    assert_eq!(
        store
            .finalize_session_disposition(&intent, intent.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::Conflict
    );
    assert_eq!(
        store
            .confirm_session_disposition(&intent, intent.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::Applied
    );
    intent.phase = SessionDispositionPhase::SessionCommitted;
    assert_eq!(
        store
            .abort_session_disposition(&intent, intent.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::Conflict
    );
    store
        .conn
        .lock()
        .execute_batch(
            "CREATE TRIGGER injected_cleanup_failure BEFORE DELETE ON goal_session_dispositions
         BEGIN SELECT RAISE(ABORT, 'fixture cleanup failure'); END;",
        )
        .unwrap();
    assert!(
        store
            .finalize_session_disposition(&intent, intent.owner_pid, BOOT)
            .is_err()
    );
    drop(store);

    let store = SqliteTaskStore::new(dir.path()).unwrap();
    assert_eq!(
        store.session_disposition(SESSION).unwrap(),
        Some(intent.clone())
    );
    let goal = store
        .current_goal_for_session(SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.status, TaskStatus::Paused);
    assert_eq!(goal.execution_epoch, paused.execution_epoch);
    assert!(store.get_goal_task("goal").await.unwrap().is_some());
    store
        .conn
        .lock()
        .execute_batch("DROP TRIGGER injected_cleanup_failure;")
        .unwrap();
    assert_eq!(
        store
            .finalize_session_disposition(&intent, intent.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::Applied
    );
    assert!(store.session_disposition(SESSION).unwrap().is_none());
    assert!(store.get("goal").await.unwrap().is_none());
    assert!(store.get_goal_task("goal").await.unwrap().is_none());
    assert_eq!(
        store
            .finalize_session_disposition(&intent, intent.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::AlreadySettled
    );
    create(&store, "successor", SESSION, BOOT).await;
    assert_eq!(
        store
            .finalize_session_disposition(&intent, intent.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::Conflict
    );
    assert_eq!(
        store
            .current_goal_for_session(SESSION)
            .await
            .unwrap()
            .unwrap()
            .id,
        "successor"
    );
}

#[tokio::test]
async fn unresolved_intent_guards_controls_and_preserves_already_admitted_settlement() {
    let store = SqliteTaskStore::new_in_memory().unwrap();
    create(&store, "goal", SESSION, BOOT).await;
    let paused = pause(&store, "goal", SESSION).await;
    let intent = durable(&paused);
    let control = resume_intent(&paused);
    store.record_control_intent(&control).await.unwrap();
    assert_eq!(
        store.prepare_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );
    assert_eq!(
        store.prepare_session_disposition(&intent).unwrap(),
        DispositionResult::AlreadySettled
    );
    assert_eq!(
        store
            .prepare_session_disposition(&durable(&paused))
            .unwrap(),
        DispositionResult::Conflict
    );
    assert_eq!(
        store
            .abort_session_disposition(&intent, intent.owner_pid, "foreign boot")
            .unwrap(),
        DispositionResult::Conflict
    );
    assert_eq!(
        store
            .claim_control_intent(&control.intent_id, BOOT)
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert!(
        store
            .record_control_intent(&GoalControlIntent {
                intent_id: uuid::Uuid::new_v4().to_string(),
                ..control
            })
            .await
            .is_err()
    );
    assert_eq!(
        store
            .resume_session_goal(
                "goal",
                SESSION,
                paused.execution_epoch,
                intent.owner_pid,
                BOOT
            )
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .update_session_goal_limits("goal", SESSION, paused.execution_epoch, Some(200), None)
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .finish_session_goal(
                "goal",
                SESSION,
                paused.execution_epoch,
                TaskStatus::Cancelled,
                None
            )
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );

    // An unexpected pending marker is not cleanup authority, even if
    // the exact original operation can still settle through its own CAS.
    store
        .conn
        .lock()
        .execute(
            "UPDATE goal_tasks SET pending_call_id = 'admitted', pending_call_epoch = ?1
          WHERE task_id = 'goal'",
            params![paused.execution_epoch],
        )
        .unwrap();
    assert_eq!(
        store
            .confirm_session_disposition(&intent, intent.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::Conflict
    );
    assert_eq!(
        store
            .settle_pending_operation(
                "goal",
                SESSION,
                paused.execution_epoch,
                "admitted",
                GoalAccountingState::Complete
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .confirm_session_disposition(&intent, intent.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::Applied
    );
}

#[tokio::test]
async fn prepared_terminal_target_fences_replacement_and_live_effect_runs_once() {
    let store = SqliteTaskStore::new_in_memory().unwrap();
    create(&store, "goal", SESSION, BOOT).await;
    assert_eq!(
        store
            .finish_session_goal("goal", SESSION, 1, TaskStatus::Completed, None)
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    let completed = store
        .current_goal_for_session(SESSION)
        .await
        .unwrap()
        .unwrap();
    let intent = live(&completed);
    let settled_start = GoalControlIntent {
        intent_id: uuid::Uuid::new_v4().to_string(),
        session_id: SESSION.into(),
        task_id: None,
        expected_epoch: None,
        predecessor_task_id: None,
        kind: GoalControlIntentKind::Start {
            budget: GoalBudgetSelection::Defaults,
        },
        owner_pid: std::process::id(),
        owner_boot_id: BOOT.into(),
        created_at: "2026-01-01T00:00:00Z".into(),
    };
    store.record_control_intent(&settled_start).await.unwrap();
    assert_eq!(
        store
            .claim_control_intent(&settled_start.intent_id, BOOT)
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .settle_control_intent(
                &settled_start.intent_id,
                GoalControlIntentSettlement::Applied
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .conn
            .lock()
            .query_row(
                "SELECT count(*) FROM goal_control_intents
                  WHERE session_id = ?1 AND intent_id = ?2 AND state = 'applied'",
                rusqlite::params![SESSION, &settled_start.intent_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        store.prepare_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );
    assert_eq!(
        store
            .create_or_replace_session_goal(
                task("successor", SESSION, BOOT),
                extension("successor")
            )
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .delete_session_goal("goal", SESSION, completed.execution_epoch)
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    let mut stale = intent.clone();
    stale.execution_epoch += 1;
    assert_eq!(
        store.commit_live_session_disposition(&stale).unwrap(),
        DispositionResult::Conflict
    );
    assert_eq!(
        store.commit_live_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );
    assert_eq!(
        store.commit_live_session_disposition(&intent).unwrap(),
        DispositionResult::Conflict
    );
    assert!(store.session_disposition(SESSION).unwrap().is_none());
    assert!(store.get("goal").await.unwrap().is_none());
    assert!(store.get_goal_task("goal").await.unwrap().is_none());
    assert_eq!(
        store
            .conn
            .lock()
            .query_row(
                "SELECT count(*) FROM goal_control_intents
                  WHERE session_id = ?1 AND intent_id = ?2",
                rusqlite::params![SESSION, &settled_start.intent_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    create(&store, "successor", SESSION, BOOT).await;
}

#[tokio::test]
async fn live_commit_removes_paused_goal_before_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(dir.path()).unwrap();
    create(&store, "goal", SESSION, BOOT).await;
    let paused = pause(&store, "goal", SESSION).await;
    let intent = live(&paused);
    assert_eq!(
        store.prepare_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );
    assert_eq!(
        store.commit_live_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );
    assert!(store.session_disposition(SESSION).unwrap().is_none());
    assert!(
        store
            .current_goal_for_session(SESSION)
            .await
            .unwrap()
            .is_none()
    );
    drop(store);

    let reopened = SqliteTaskStore::new(dir.path()).unwrap();
    assert!(reopened.session_disposition(SESSION).unwrap().is_none());
    assert!(
        reopened
            .current_goal_for_session(SESSION)
            .await
            .unwrap()
            .is_none()
    );
    assert!(reopened.get("goal").await.unwrap().is_none());
    assert!(reopened.get_goal_task("goal").await.unwrap().is_none());
}

#[tokio::test]
async fn legacy_session_committed_live_disposition_finalizes() {
    let store = SqliteTaskStore::new_in_memory().unwrap();
    create(&store, "goal", SESSION, BOOT).await;
    let paused = pause(&store, "goal", SESSION).await;
    let intent = live(&paused);
    assert_eq!(
        store.prepare_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );
    store
        .conn
        .lock()
        .execute(
            "UPDATE goal_session_dispositions
                SET phase = 'session_committed'
              WHERE session_id = ?1 AND operation_id = ?2",
            rusqlite::params![intent.session_id, intent.operation_id],
        )
        .unwrap();
    let mut committed = intent;
    committed.phase = SessionDispositionPhase::SessionCommitted;
    assert_eq!(
        store
            .finalize_session_disposition(&committed, committed.owner_pid, BOOT)
            .unwrap(),
        DispositionResult::Applied
    );
    assert!(store.session_disposition(SESSION).unwrap().is_none());
    assert!(store.get("goal").await.unwrap().is_none());
    assert!(store.get_goal_task("goal").await.unwrap().is_none());
}

#[tokio::test]
async fn running_goal_is_paused_only_after_pending_accounting_settles() {
    let store = SqliteTaskStore::new_in_memory().unwrap();
    create(&store, "goal", SESSION, BOOT).await;
    let running = store
        .current_goal_for_session(SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.prepare_session_disposition(&live(&running)).unwrap(),
        DispositionResult::Conflict
    );
    assert_eq!(
        store
            .admit_pending_operation("goal", SESSION, 1, "admitted")
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .pause_running_goal_for_session_interruption("goal", SESSION, 1)
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .settle_pending_operation(
                "goal",
                SESSION,
                1,
                "admitted",
                GoalAccountingState::Complete
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .pause_running_goal_for_session_interruption("goal", SESSION, 1)
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    let paused = store
        .current_goal_for_session(SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(paused.status, TaskStatus::Paused);
    assert_eq!(paused.execution_epoch, 2);
    assert_eq!(
        store
            .get_goal_task("goal")
            .await
            .unwrap()
            .unwrap()
            .pause_reason,
        None
    );
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[tokio::test]
async fn boot_reconciliation_does_not_rewrite_an_outstanding_exact_target() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(dir.path()).unwrap();
    let previous = format!("zc-process-v1:{}:1:fixture", std::process::id());
    assert!(is_authoritative_owner(std::process::id(), &previous));
    create(&store, "goal", SESSION, &previous).await;
    let paused = pause(&store, "goal", SESSION).await;
    let intent = durable(&paused);
    assert_eq!(
        store.prepare_session_disposition(&intent).unwrap(),
        DispositionResult::Applied
    );

    // An unresolved marker is not normally reachable once prepared. If
    // present on recovery, do not advance the stored witness's exact epoch.
    store
        .conn
        .lock()
        .execute(
            "UPDATE goal_tasks SET pending_call_id = 'interrupted', pending_call_epoch = ?1
          WHERE task_id = 'goal'",
            params![paused.execution_epoch],
        )
        .unwrap();
    const OTHER_SESSION: &str = "rpc_other-fixture";
    create(&store, "unrelated", OTHER_SESSION, &previous).await;
    drop(store);

    let store = SqliteTaskStore::new(dir.path()).unwrap();
    assert_eq!(store.reconcile_goal_boot_state(BOOT).unwrap(), 1);
    let unchanged = store
        .current_goal_for_session(SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.execution_epoch, paused.execution_epoch);
    assert_eq!(
        store
            .get_goal_task("goal")
            .await
            .unwrap()
            .unwrap()
            .pending_call_id
            .as_deref(),
        Some("interrupted")
    );
    assert_eq!(store.session_disposition(SESSION).unwrap(), Some(intent));
    assert_eq!(
        store
            .current_goal_for_session(OTHER_SESSION)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Paused
    );
}
