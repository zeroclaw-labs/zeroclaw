//! File-backed regressions for deferred-control audit records.

use super::*;
use crate::control_plane::TaskProgress;
use crate::control_plane::task_registry::TaskRegistry;
use crate::control_plane::task_store_sqlite::CONTROL_PLANE_SCHEMA_VERSION;
use zeroclaw_commands::goal::GoalBudgetSelection;
use zeroclaw_goal::task::{GoalControlIntent, GoalControlIntentKind, GoalControlIntentSettlement};

fn start_intent(boot: &str) -> GoalControlIntent {
    GoalControlIntent {
        intent_id: uuid::Uuid::new_v4().to_string(),
        session_id: "test-session".into(),
        task_id: None,
        expected_epoch: None,
        predecessor_task_id: None,
        kind: GoalControlIntentKind::Start {
            budget: GoalBudgetSelection::Defaults,
        },
        owner_pid: std::process::id(),
        owner_boot_id: boot.into(),
        created_at: "2026-01-01T00:00:00Z".into(),
    }
}

fn state(store: &SqliteTaskStore, id: &str) -> (String, Option<String>) {
    store
        .conn
        .lock()
        .query_row(
            "SELECT state, outcome_reason FROM goal_control_intents WHERE intent_id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

async fn create_goal(store: &SqliteTaskStore) -> TaskRecord {
    let task = TaskRecord {
        id: "test-goal".into(),
        kind: TaskKind::Goal,
        agent: "test-agent".into(),
        status: TaskStatus::Running,
        owner_pid: std::process::id(),
        owner_boot_id: "current-boot".into(),
        heartbeat_at: None,
        depth: 0,
        parent_id: None,
        originator_route: None,
        originator_chain: Vec::new(),
        delivered: false,
        idem_key: None,
        principal_id: None,
        session_id: Some("test-session".into()),
        execution_epoch: 1,
        started_at: "2026-01-01T00:00:00Z".into(),
        finished_at: None,
    };
    assert_eq!(
        store
            .create_or_replace_session_goal(
                task.clone(),
                GoalTaskRecord {
                    task_id: task.id.clone(),
                    objective: "Complete the bounded test".into(),
                    ..GoalTaskRecord::default()
                },
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    task
}

fn existing_rows(store: &SqliteTaskStore) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let conn = store.conn.lock();
    [
        "tasks",
        "goal_tasks",
        "task_continuation_contexts",
        "terminal_settlement_intents",
    ]
    .into_iter()
    .map(|table| {
        let mut query = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
            .unwrap();
        let columns = query.column_count();
        query
            .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    })
    .collect()
}

#[tokio::test]
async fn control_intent_v12_migration_preserves_existing_rows_and_reopens() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(directory.path()).unwrap();
    let task = create_goal(&store).await;
    let mut ordinary = task.clone();
    ordinary.id = "ordinary-task".into();
    ordinary.kind = TaskKind::Delegate;
    ordinary.session_id = None;
    ordinary.execution_epoch = 0;
    store.create(ordinary.clone()).await.unwrap();
    store
        .insert_malformed_terminal_settlement_intent_for_test(&ordinary.id)
        .unwrap();
    let continuation = TaskContinuationContext {
        channel: "matrix".into(),
        channel_alias: Some("test".into()),
        reply_target: "test-target".into(),
        sender: "test-sender".into(),
        thread_ts: None,
        interruption_scope_id: None,
        conversation_scope: zeroclaw_goal::task::TaskContinuationConversationScope::Sender,
    };
    store
        .conn
        .lock()
        .execute(
            "INSERT INTO task_continuation_contexts (task_id, context_json) VALUES (?1, ?2)",
            params![task.id, serde_json::to_string(&continuation).unwrap()],
        )
        .unwrap();
    let before = existing_rows(&store);
    assert!(
        before.iter().all(|rows| !rows.is_empty()),
        "every legacy table must have retained rows"
    );
    store
        .conn
        .lock()
        .execute_batch("DROP TABLE IF EXISTS goal_control_intents; PRAGMA user_version = 12;")
        .unwrap();
    drop(store);
    let store = SqliteTaskStore::new(directory.path()).unwrap();
    assert_eq!(existing_rows(&store), before);
    let version: i64 = store
        .conn
        .lock()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 14);
    let intent = start_intent("current-boot");
    store.record_control_intent(&intent).await.unwrap();
    drop(store);
    let reopened = SqliteTaskStore::new(directory.path()).unwrap();
    assert_eq!(existing_rows(&reopened), before);
    assert_eq!(
        state(&reopened, &intent.intent_id),
        ("pending".into(), None)
    );
}

// Version 13 was written by candidate builds before ordinary progress landed.
async fn progress_migration_fixture(
    directory: &std::path::Path,
) -> (SqliteTaskStore, TaskRecord, TaskRecord, GoalControlIntent) {
    let store = SqliteTaskStore::new(directory).unwrap();
    let goal = create_goal(&store).await;
    let mut delegate = goal.clone();
    delegate.id = "progress-delegate".into();
    delegate.kind = TaskKind::Delegate;
    delegate.session_id = None;
    delegate.execution_epoch = 0;
    store.create(delegate.clone()).await.unwrap();
    let intent = start_intent("current-boot");
    store.record_control_intent(&intent).await.unwrap();
    let continuation = TaskContinuationContext {
        channel: "matrix".into(),
        channel_alias: Some("test".into()),
        reply_target: "test-target".into(),
        sender: "test-sender".into(),
        thread_ts: None,
        interruption_scope_id: None,
        conversation_scope: zeroclaw_goal::task::TaskContinuationConversationScope::Sender,
    };
    store
        .conn
        .lock()
        .execute(
            "INSERT INTO task_continuation_contexts (task_id, context_json) VALUES (?1, ?2)",
            params![goal.id, serde_json::to_string(&continuation).unwrap()],
        )
        .unwrap();
    (store, goal, delegate, intent)
}

#[tokio::test]
async fn control_intent_v13_progress_migration_preserves_rows_and_reopens() {
    let directory = tempfile::tempdir().unwrap();
    let (store, goal, delegate, intent) = progress_migration_fixture(directory.path()).await;
    let before = existing_rows(&store);
    let audit_before = table_rows(&store.conn.lock(), "goal_control_intents");
    store
        .conn
        .lock()
        .execute_batch("ALTER TABLE tasks DROP COLUMN progress; PRAGMA user_version = 13;")
        .unwrap();
    drop(store);

    let store = SqliteTaskStore::new(directory.path()).unwrap();
    let version: i64 = store
        .conn
        .lock()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, CONTROL_PLANE_SCHEMA_VERSION);
    let progress: Option<String> = store
        .conn
        .lock()
        .query_row(
            "SELECT progress FROM tasks WHERE id = ?1",
            params![delegate.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(progress, None);
    assert_eq!(existing_rows(&store), before);
    assert_eq!(
        table_rows(&store.conn.lock(), "goal_control_intents"),
        audit_before
    );
    assert_eq!(state(&store, &intent.intent_id), ("pending".into(), None));
    let progress = TaskProgress::default();
    assert!(
        store
            .record_progress(&delegate.id, &delegate.owner_boot_id, &progress)
            .await
            .unwrap()
    );
    let goal_before = store.get_snapshot(&goal.id).await.unwrap().unwrap();
    let error = store
        .record_progress(&goal.id, &goal.owner_boot_id, &progress)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("session-bound Goal"));
    let goal_after = store.get_snapshot(&goal.id).await.unwrap().unwrap();
    assert_eq!(goal_after.progress, None);
    assert_eq!(goal_after.task.heartbeat_at, goal_before.task.heartbeat_at);
    let settled = existing_rows(&store);
    drop(store);
    let reopened = SqliteTaskStore::new(directory.path()).unwrap();
    assert_eq!(existing_rows(&reopened), settled);
    assert_eq!(
        table_rows(&reopened.conn.lock(), "goal_control_intents"),
        audit_before
    );
    assert_eq!(
        reopened
            .get_snapshot(&delegate.id)
            .await
            .unwrap()
            .unwrap()
            .progress,
        Some(progress)
    );
}

// Compare raw values rather than decoded records so migration cannot silently
// normalize unrelated data or alter pending audit state.
fn table_rows(conn: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
    query_rows(conn, &format!("SELECT * FROM \"{table}\" ORDER BY 1"))
}

fn query_rows(conn: &Connection, sql: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut query = conn.prepare(sql).unwrap();
    let columns = query.column_count();
    query
        .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

// Include schema, column order, version and every table's raw rows so rollback
// is proved after the progress step, not merely inferred from the error.
fn migration_snapshot(conn: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let mut snapshot = vec![
        query_rows(conn, "PRAGMA user_version"),
        query_rows(conn, "PRAGMA table_info(tasks)"),
        query_rows(
            conn,
            "SELECT type, name, sql FROM sqlite_master ORDER BY type, name",
        ),
    ];
    let mut query = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    let tables = query
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for table in tables {
        snapshot.push(table_rows(conn, &table));
    }
    snapshot
}

#[tokio::test]
async fn control_intent_v13_progress_migration_failure_rolls_back_everything() {
    let directory = tempfile::tempdir().unwrap();
    let (store, goal, _, _) = progress_migration_fixture(directory.path()).await;
    {
        let conn = store.conn.lock();
        conn.execute_batch("DROP INDEX idx_goal_tasks_current_session;")
            .unwrap();
        for id in ["duplicate-goal-one", "duplicate-goal-two"] {
            let mut duplicate = goal.clone();
            duplicate.id = id.into();
            duplicate.status = TaskStatus::Completed;
            duplicate.session_id = Some("duplicate-session".into());
            duplicate.finished_at = Some("2026-01-01T00:00:01Z".into());
            insert_task_record(&conn, duplicate).unwrap();
            conn.execute(
                "INSERT INTO goal_tasks (task_id, objective) VALUES (?1, ?2)",
                params![id, "Retained completed fixture"],
            )
            .unwrap();
        }
        conn.execute_batch("ALTER TABLE tasks DROP COLUMN progress; PRAGMA user_version = 13;")
            .unwrap();
    }
    let before = migration_snapshot(&store.conn.lock());
    drop(store);
    let error = match SqliteTaskStore::new(directory.path()) {
        Ok(_) => panic!("duplicate session bindings must reject schema convergence"),
        Err(error) => error,
    };
    let detail = format!("{error:#}");
    assert!(detail.contains("migrate control-plane schema"), "{detail}");
    assert!(
        detail.contains("UNIQUE constraint failed: tasks.session_id"),
        "{detail}"
    );
    let raw = Connection::open(directory.path().join("control_plane.db")).unwrap();
    assert_eq!(migration_snapshot(&raw), before);
    let version: i64 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 13);
    let progress_columns: i64 = raw
        .query_row(
            "SELECT count(*) FROM pragma_table_info('tasks') WHERE name = 'progress'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(progress_columns, 0);
}

#[tokio::test]
async fn control_intent_settled_rows_never_change_and_missing_rows_are_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(directory.path()).unwrap();
    let settlements = [
        GoalControlIntentSettlement::Superseded {
            reason: "state_changed".into(),
        },
        GoalControlIntentSettlement::Abandoned {
            reason: "delivery_failed".into(),
        },
    ];
    for claimed in [false, true] {
        for settlement in &settlements {
            let intent = start_intent("boot");
            store.record_control_intent(&intent).await.unwrap();
            if claimed {
                assert_eq!(
                    store
                        .claim_control_intent(&intent.intent_id, "boot")
                        .await
                        .unwrap(),
                    GoalTransitionResult::Applied
                );
            }
            assert_eq!(
                store
                    .settle_control_intent(&intent.intent_id, settlement.clone())
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
            let before = state(&store, &intent.intent_id);
            for next in settlements
                .iter()
                .cloned()
                .chain([GoalControlIntentSettlement::Applied])
            {
                assert_eq!(
                    store
                        .settle_control_intent(&intent.intent_id, next)
                        .await
                        .unwrap(),
                    GoalTransitionResult::Stale
                );
                assert_eq!(state(&store, &intent.intent_id), before);
            }
        }
    }
    assert_eq!(
        store.claim_control_intent("missing", "boot").await.unwrap(),
        GoalTransitionResult::Missing
    );
    assert_eq!(
        store
            .settle_control_intent("missing", GoalControlIntentSettlement::Applied)
            .await
            .unwrap(),
        GoalTransitionResult::Missing
    );
}

#[tokio::test]
async fn control_intent_parameters_are_bounded_and_contain_no_user_text() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(directory.path()).unwrap();
    let task = create_goal(&store).await;
    for (kind, expected) in [
        (
            GoalControlIntentKind::Start {
                budget: GoalBudgetSelection::Defaults,
            },
            serde_json::json!({"budget":{"mode":"defaults"}}),
        ),
        (
            GoalControlIntentKind::Resume { has_response: true },
            serde_json::json!({"has_response":true}),
        ),
        (GoalControlIntentKind::Pause, serde_json::json!({})),
        (
            GoalControlIntentKind::SetBudget {
                budget: GoalBudgetSelection::Unlimited,
            },
            serde_json::json!({"budget":{"mode":"unlimited"}}),
        ),
    ] {
        let mut intent = start_intent("boot");
        if !matches!(kind, GoalControlIntentKind::Start { .. }) {
            intent.task_id = Some(task.id.clone());
            intent.expected_epoch = Some(task.execution_epoch);
        }
        intent.kind = kind;
        store.record_control_intent(&intent).await.unwrap();
        let json: String = store
            .conn
            .lock()
            .query_row(
                "SELECT parameters_json FROM goal_control_intents WHERE intent_id = ?1",
                params![intent.intent_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(json.len() <= 512);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn control_intent_cleanup_failure_rolls_back_task_cascade() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(directory.path()).unwrap();
    let task = create_goal(&store).await;
    let intent = start_intent("boot");
    store.record_control_intent(&intent).await.unwrap();
    store
        .settle_control_intent(
            &intent.intent_id,
            GoalControlIntentSettlement::Abandoned {
                reason: "cancelled".into(),
            },
        )
        .await
        .unwrap();
    store
        .finish_session_goal(&task.id, "test-session", 1, TaskStatus::Cancelled, None)
        .await
        .unwrap();
    let before = existing_rows(&store);
    store
        .conn
        .lock()
        .execute_batch(
            "CREATE TRIGGER refuse_intent_cleanup BEFORE DELETE ON goal_control_intents
         BEGIN SELECT RAISE(ABORT, 'test cleanup failure'); END;",
        )
        .unwrap();
    assert!(
        store
            .delete_session_goal(&task.id, "test-session", 2)
            .await
            .is_err()
    );
    assert_eq!(existing_rows(&store), before);
    assert_eq!(state(&store, &intent.intent_id).0, "abandoned");
}

#[tokio::test]
async fn control_intent_sql_constraints_reject_invalid_bindings() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(directory.path()).unwrap();
    create_goal(&store).await;
    let conn = store.conn.lock();
    for (id, session, kind, task, epoch, state) in [
        (
            "id",
            "test-session",
            "start",
            Some("test-goal"),
            Some(1),
            "pending",
        ),
        ("id", "test-session", "pause", None, None, "pending"),
        ("id", "test-session", "unknown", None, None, "pending"),
        ("id", "test-session", "start", None, None, "unknown"),
        (" ", "test-session", "start", None, None, "pending"),
        ("id", " ", "start", None, None, "pending"),
        (
            "id",
            "test-session",
            "pause",
            Some("test-goal"),
            None,
            "pending",
        ),
    ] {
        let error = conn
            .execute(
                "INSERT INTO goal_control_intents
                (intent_id, session_id, kind, task_id, expected_epoch, state,
                 owner_pid, owner_boot_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 'boot', 'timestamp')",
                params![id, session, kind, task, epoch, state],
            )
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        );
    }
}

#[tokio::test]
async fn control_intent_task_cascade_and_session_cleanup_preserve_agent_delete_policy() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(directory.path()).unwrap();
    let task = create_goal(&store).await;
    let start = start_intent("current-boot");
    store.record_control_intent(&start).await.unwrap();
    store
        .settle_control_intent(
            &start.intent_id,
            GoalControlIntentSettlement::Abandoned {
                reason: "cancelled".into(),
            },
        )
        .await
        .unwrap();
    let mut bound = start_intent("current-boot");
    bound.task_id = Some(task.id.clone());
    bound.expected_epoch = Some(1);
    bound.kind = GoalControlIntentKind::Pause;
    store.record_control_intent(&bound).await.unwrap();
    assert!(store.delete_by_agent("test-agent").is_err());
    assert_eq!(
        store
            .finish_session_goal(&task.id, "test-session", 1, TaskStatus::Cancelled, None)
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(store.delete_by_agent("test-agent").unwrap(), 0);
    assert_eq!(state(&store, &bound.intent_id), ("pending".into(), None));
    assert_eq!(
        store
            .delete_session_goal(&task.id, "test-session", 2)
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    let count: i64 = store
        .conn
        .lock()
        .query_row("SELECT count(*) FROM goal_control_intents", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
    assert!(store.get(&task.id).await.unwrap().is_none());
}

#[tokio::test]
async fn control_intent_claim_and_settlement_are_guarded() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(directory.path()).unwrap();
    let intent = start_intent("current-boot");
    store.record_control_intent(&intent).await.unwrap();
    assert_eq!(state(&store, &intent.intent_id), ("pending".into(), None));
    assert_eq!(
        store
            .claim_control_intent(&intent.intent_id, "other-boot")
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .settle_control_intent(&intent.intent_id, GoalControlIntentSettlement::Applied)
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .claim_control_intent(&intent.intent_id, "current-boot")
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .claim_control_intent(&intent.intent_id, "current-boot")
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .settle_control_intent(&intent.intent_id, GoalControlIntentSettlement::Applied)
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .settle_control_intent(
                &intent.intent_id,
                GoalControlIntentSettlement::Abandoned {
                    reason: "cancelled".into(),
                },
            )
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(state(&store, &intent.intent_id), ("applied".into(), None));
}

#[tokio::test]
async fn control_intent_restart_abandons_only_predecessor_open_intents() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::new(directory.path()).unwrap();
    let pending = start_intent("previous-boot");
    let claimed = start_intent("previous-boot");
    let applied = start_intent("previous-boot");
    let current = start_intent("current-boot");
    for intent in [&pending, &claimed, &applied, &current] {
        store.record_control_intent(intent).await.unwrap();
    }
    for intent in [&claimed, &applied] {
        assert_eq!(
            store
                .claim_control_intent(&intent.intent_id, "previous-boot")
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
    }
    store
        .settle_control_intent(&applied.intent_id, GoalControlIntentSettlement::Applied)
        .await
        .unwrap();
    store.reconcile_goal_boot_state("current-boot").unwrap();
    for intent in [&pending, &claimed] {
        assert_eq!(
            state(&store, &intent.intent_id),
            ("abandoned".into(), Some("restart".into()))
        );
    }
    assert_eq!(state(&store, &applied.intent_id), ("applied".into(), None));
    assert_eq!(state(&store, &current.intent_id), ("pending".into(), None));
    drop(store);
    let reopened = SqliteTaskStore::new(directory.path()).unwrap();
    assert_eq!(
        state(&reopened, &current.intent_id),
        ("pending".into(), None)
    );
}
