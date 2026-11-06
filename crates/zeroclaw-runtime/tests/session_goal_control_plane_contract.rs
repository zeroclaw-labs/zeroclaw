//! File-backed contract coverage for session-bound Goal control-plane state.

use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, params};
use zeroclaw_runtime::control_plane::task_registry::TerminalSettlementIntent;
use zeroclaw_runtime::control_plane::{
    GoalAccountingState, GoalPauseReason, GoalPauseState, GoalTaskRecord, GoalTaskRegistry,
    GoalTransitionResult, SqliteTaskStore, TaskKind, TaskProgress, TaskRecord, TaskRegistry,
    TaskStatus,
};

#[test]
fn runtime_task_paths_reexport_the_canonical_api_types() {
    fn accepts_api_kind(_: zeroclaw_api::task::TaskKind) {}
    fn accepts_runtime_kind(_: TaskKind) {}
    fn accepts_api_status(_: zeroclaw_api::task::TaskStatus) {}
    fn accepts_runtime_status(_: TaskStatus) {}
    fn accepts_api_record(_: &zeroclaw_api::task::TaskRecord) {}
    fn accepts_runtime_record(_: &TaskRecord) {}

    let kind = TaskKind::Goal;
    accepts_api_kind(kind);
    accepts_runtime_kind(kind);

    let status = TaskStatus::Paused;
    accepts_api_status(status);
    accepts_runtime_status(status);

    let record = session_goal_task("goal-1", "session-1");
    accepts_api_record(&record);
    accepts_runtime_record(&record);
}

fn insert_current_goal(
    connection: &Connection,
    task_id: &str,
    session_id: &str,
) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES (?1, 'goal', 'main', 'running', 1, 'boot-a', ?2, 1, 'now')",
        params![task_id, session_id],
    )
}

fn session_goal_task(task_id: &str, session_id: &str) -> TaskRecord {
    TaskRecord {
        id: task_id.into(),
        kind: TaskKind::Goal,
        agent: "main".into(),
        status: TaskStatus::Running,
        // Recovery tests must model a dead owner. A real PID 1 is live on
        // supported hosts and must never be reclaimed merely for another boot.
        owner_pid: 999_999,
        owner_boot_id: "boot-a".into(),
        heartbeat_at: None,
        depth: 0,
        parent_id: None,
        originator_route: Some("matrix:!room:example.test".into()),
        originator_chain: Vec::new(),
        delivered: false,
        idem_key: None,
        principal_id: Some("matrix:@operator:example.test".into()),
        session_id: Some(session_id.into()),
        execution_epoch: 0,
        started_at: "2026-08-30T00:00:00Z".into(),
        finished_at: None,
    }
}

fn session_goal_extension(task_id: &str) -> GoalTaskRecord {
    GoalTaskRecord {
        task_id: task_id.into(),
        objective: "produce a verified result".into(),
        ..GoalTaskRecord::default()
    }
}

fn deletion_fixture_rows(connection: &Connection, query: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = connection
        .prepare(query)
        .expect("prepare deletion snapshot");
    let column_count = statement.column_count();
    statement
        .query_map([], |row| {
            (0..column_count).map(|column| row.get(column)).collect()
        })
        .expect("query deletion snapshot")
        .collect::<Result<_, _>>()
        .expect("read deletion snapshot")
}

async fn create_deletion_delegate(store: &SqliteTaskStore, task_id: &str, agent: &str) {
    let mut task = session_goal_task(task_id, "unused-session");
    task.kind = TaskKind::Delegate;
    task.agent = agent.into();
    task.session_id = None;
    task.principal_id = None;
    store.create(task).await.expect("create ordinary delegate");
}

#[tokio::test]
async fn agent_deletion_retains_terminal_goal_history_and_deletes_ordinary_rows() {
    let directory = tempfile::tempdir().expect("create deletion fixture directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize deletion fixture");
    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("goal-delete-history", "session-delete-history"),
                session_goal_extension("goal-delete-history"),
            )
            .await
            .expect("create retained Goal"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .finish_session_goal(
                "goal-delete-history",
                "session-delete-history",
                1,
                TaskStatus::Cancelled,
                Some("policy_revoked".into()),
            )
            .await
            .expect("cancel retained Goal"),
        GoalTransitionResult::Applied
    );
    create_deletion_delegate(&store, "delegate-delete-one", "main").await;
    create_deletion_delegate(&store, "delegate-delete-two", "main").await;
    create_deletion_delegate(&store, "delegate-other-agent", "other").await;
    let connection = Connection::open(directory.path().join("control_plane.db"))
        .expect("open deletion verification connection");
    connection
        .execute(
            "INSERT INTO terminal_settlement_intents
             (task_id, owner_pid, owner_boot_id, desired_status, artifact_path,
              artifact_ref, artifact_sha256, terminal_error)
             VALUES ('goal-delete-history', 999999, 'boot-a', 'cancelled',
                     'fixture-artifact.json', NULL, ?1, 'policy_revoked')",
            params!["00".repeat(32)],
        )
        .expect("seed retained historical settlement intent");
    let queries = [
        "SELECT * FROM tasks WHERE id IN ('goal-delete-history', 'delegate-other-agent') ORDER BY id",
        "SELECT * FROM goal_tasks ORDER BY task_id",
        "SELECT * FROM terminal_settlement_intents ORDER BY task_id",
    ];
    let before = queries.map(|query| deletion_fixture_rows(&connection, query));

    assert_eq!(
        store.delete_by_agent("main").expect("delete ordinary rows"),
        2
    );
    assert_eq!(
        store.count_by_agent("main").expect("count retained Goal"),
        1
    );
    assert_eq!(store.count_by_agent("other").expect("count other agent"), 1);
    assert_eq!(
        queries.map(|query| deletion_fixture_rows(&connection, query)),
        before,
        "task bytes, extension, epoch, error, finished_at and settlement intent are retained"
    );
    assert_eq!(store.delete_by_agent("main").expect("repeat deletion"), 0);
    drop(connection);
    drop(store);
    let reopened = SqliteTaskStore::new(directory.path()).expect("reopen deletion fixture");
    assert_eq!(
        reopened
            .count_by_agent("main")
            .expect("read retained history"),
        1
    );
}

#[tokio::test]
async fn agent_deletion_preserves_ordinary_only_behavior() {
    let directory = tempfile::tempdir().expect("create ordinary deletion directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize ordinary deletion");
    create_deletion_delegate(&store, "delegate-delete-one", "main").await;
    create_deletion_delegate(&store, "delegate-delete-two", "main").await;
    create_deletion_delegate(&store, "delegate-other-agent", "other").await;
    assert_eq!(
        store.delete_by_agent("main").expect("delete ordinary rows"),
        2
    );
    assert_eq!(
        store.count_by_agent("main").expect("count removed agent"),
        0
    );
    assert_eq!(store.count_by_agent("other").expect("count other agent"), 1);
}

#[tokio::test]
async fn agent_deletion_rejects_running_and_paused_goals_without_partial_deletion() {
    for paused in [false, true] {
        let directory = tempfile::tempdir().expect("create live deletion directory");
        let store = SqliteTaskStore::new(directory.path()).expect("initialize live deletion");
        assert_eq!(
            store
                .create_or_replace_session_goal(
                    session_goal_task("goal-delete-live", "session-delete-live"),
                    session_goal_extension("goal-delete-live"),
                )
                .await
                .expect("create live Goal"),
            GoalTransitionResult::Applied
        );
        if paused {
            assert_eq!(
                store
                    .pause_session_goal(
                        "goal-delete-live",
                        "session-delete-live",
                        1,
                        GoalPauseState {
                            reason: GoalPauseReason::OperatorPaused,
                            description: Some("operator pause".into()),
                            blockers: Vec::new(),
                        },
                    )
                    .await
                    .expect("pause live Goal"),
                GoalTransitionResult::Applied
            );
        }
        create_deletion_delegate(&store, "delegate-delete-live", "main").await;
        let connection = Connection::open(directory.path().join("control_plane.db"))
            .expect("open live deletion verification connection");
        let queries = [
            "SELECT * FROM tasks ORDER BY id",
            "SELECT * FROM goal_tasks ORDER BY task_id",
            "SELECT * FROM terminal_settlement_intents ORDER BY task_id",
        ];
        let before = queries.map(|query| deletion_fixture_rows(&connection, query));
        let error = store
            .delete_by_agent("main")
            .expect_err("reject live Goal deletion");
        assert!(format!("{error:#}").contains("session-bound Goal"));
        assert_eq!(
            queries.map(|query| deletion_fixture_rows(&connection, query)),
            before
        );
    }
}

#[test]
fn migration_converges_upstream_v8_without_losing_terminal_settlement_schema() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let database = directory.path().join("control_plane.db");
    let connection = Connection::open(&database).expect("open upstream-v8 fixture database");
    connection
        .execute_batch(
            "CREATE TABLE tasks (
                 id TEXT PRIMARY KEY, kind TEXT NOT NULL, agent TEXT NOT NULL,
                 status TEXT NOT NULL, owner_pid INTEGER NOT NULL DEFAULT 0,
                 owner_boot_id TEXT NOT NULL DEFAULT '', heartbeat_at TEXT,
                 depth INTEGER NOT NULL DEFAULT 0, parent_id TEXT,
                 originator_route TEXT, delivered INTEGER NOT NULL DEFAULT 0,
                 idem_key TEXT, principal_id TEXT, started_at TEXT NOT NULL,
                 finished_at TEXT, output TEXT, error TEXT
             );
             CREATE TABLE terminal_settlement_intents (
                 task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
                 owner_pid INTEGER NOT NULL, owner_boot_id TEXT NOT NULL,
                 desired_status TEXT NOT NULL, artifact_path TEXT NOT NULL,
                 artifact_ref TEXT, artifact_sha256 TEXT NOT NULL, terminal_error TEXT
             );
             CREATE INDEX idx_terminal_settlement_intents_owner
                 ON terminal_settlement_intents(owner_pid, owner_boot_id);
             PRAGMA user_version = 8;",
        )
        .expect("write upstream-v8 fixture schema");
    drop(connection);

    SqliteTaskStore::new(directory.path()).expect("migrate upstream-v8 fixture");
    let verify = Connection::open(&database).expect("open migrated fixture database");
    assert_eq!(
        verify
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("read schema version"),
        13
    );
    let columns: Vec<String> = verify
        .prepare("PRAGMA table_info(tasks)")
        .expect("inspect migrated task columns")
        .query_map([], |row| row.get(1))
        .expect("query migrated task columns")
        .collect::<Result<_, _>>()
        .expect("read migrated task columns");
    assert!(columns.contains(&"session_id".to_string()));
    assert!(columns.contains(&"execution_epoch".to_string()));
    let terminal_table: String = verify
        .query_row(
            "SELECT name FROM sqlite_master
              WHERE type = 'table' AND name = 'terminal_settlement_intents'",
            [],
            |row| row.get(0),
        )
        .expect("retain terminal settlement table");
    assert_eq!(terminal_table, "terminal_settlement_intents");
}

#[test]
fn migration_replaces_v11_epoch_guards_with_integer_domain_checks() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize current schema");
    let database = directory.path().join("control_plane.db");
    drop(store);

    let connection = Connection::open(&database).expect("open v11 fixture database");
    connection
        .execute_batch(
            "DROP TRIGGER trg_goal_tasks_require_epoch_insert;
             DROP TRIGGER trg_goal_tasks_require_epoch_update;
             DROP TRIGGER trg_goal_tasks_epoch_monotonic;
             CREATE TRIGGER trg_goal_tasks_require_epoch_insert
                 BEFORE INSERT ON tasks FOR EACH ROW
                 WHEN NEW.kind = 'goal' AND NEW.execution_epoch < 1
                 BEGIN SELECT RAISE(ABORT, 'goal tasks require execution_epoch >= 1'); END;
             CREATE TRIGGER trg_goal_tasks_require_epoch_update
                 BEFORE UPDATE OF execution_epoch ON tasks FOR EACH ROW
                 WHEN NEW.kind = 'goal' AND NEW.execution_epoch < 1
                 BEGIN SELECT RAISE(ABORT, 'goal tasks require execution_epoch >= 1'); END;
             CREATE TRIGGER trg_goal_tasks_epoch_monotonic
                 BEFORE UPDATE OF execution_epoch ON tasks FOR EACH ROW
                 WHEN OLD.kind = 'goal' AND NEW.execution_epoch < OLD.execution_epoch
                 BEGIN SELECT RAISE(ABORT, 'goal task execution_epoch is monotonic'); END;
             INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('malformed-v11-goal', 'goal', 'main', 'running', 1, 'boot-old',
                       'session-malformed-v11', 'not-an-epoch', 'now');
             INSERT INTO goal_tasks (task_id, objective)
             VALUES ('malformed-v11-goal', 'legacy malformed epoch');
             PRAGMA user_version = 11;",
        )
        .expect("write v11 epoch guard fixture");
    drop(connection);

    SqliteTaskStore::new(directory.path()).expect("migrate v11 epoch guards");
    let verify = Connection::open(&database).expect("open migrated v12 database");
    let migrated: (String, String, i64) = verify
        .query_row(
            "SELECT status, error, execution_epoch FROM tasks WHERE id = 'malformed-v11-goal'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read fail-closed malformed v11 Goal");
    assert_eq!(migrated.0, "failed");
    assert_eq!(migrated.1, "goal_invalid_execution_epoch");
    assert_eq!(migrated.2, 1);
    let error = verify
        .execute(
            "INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('goal-text-epoch', 'goal', 'main', 'running', 1, 'boot-a',
                       'session-text-epoch', 'not-an-epoch', 'now')",
            [],
        )
        .expect_err("v12 must replace v11 epoch guards");
    assert_eq!(
        error.sqlite_error_code(),
        Some(ErrorCode::ConstraintViolation)
    );
}

#[test]
fn current_schema_open_does_not_wait_for_an_unrelated_writer() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let database = directory.path().join("control_plane.db");
    SqliteTaskStore::new(directory.path()).expect("initialize current control-plane schema");

    let writer = Connection::open(&database).expect("open competing writer");
    writer
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold competing write reservation");

    let path = directory.path().to_path_buf();
    let (sender, receiver) = mpsc::channel();
    let opener = std::thread::spawn(move || sender.send(SqliteTaskStore::new(&path).map(|_| ())));

    let result = receiver.recv_timeout(Duration::from_millis(250));
    writer
        .execute_batch("ROLLBACK")
        .expect("release competing write reservation");
    opener
        .join()
        .expect("join current-schema opener")
        .expect("send current-schema open result");

    assert!(
        matches!(result, Ok(Ok(()))),
        "opening a current schema must not acquire a migration write lock: {result:?}"
    );
}

#[test]
fn independent_connections_admit_exactly_one_current_goal_for_a_session() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    SqliteTaskStore::new(directory.path()).expect("initialize control-plane schema");

    let barrier = Arc::new(Barrier::new(2));
    let outcomes: Vec<_> = ["left", "right"]
        .into_iter()
        .map(|task_id| {
            let database = directory.path().join("control_plane.db");
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let connection = Connection::open(database).expect("open independent connection");
                connection
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .expect("configure independent connection busy timeout");
                barrier.wait();
                insert_current_goal(&connection, task_id, "shared-session")
            })
        })
        .collect();

    let successes = outcomes
        .into_iter()
        .map(|thread| thread.join().expect("join concurrent writer"))
        .filter(|outcome| outcome.as_ref().is_ok_and(|rows| *rows == 1))
        .count();

    assert_eq!(successes, 1, "a session has one current Goal row");

    let connection = Connection::open(directory.path().join("control_plane.db"))
        .expect("open verification connection");
    let current_goals: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM tasks
             WHERE kind = 'goal' AND session_id = 'shared-session'",
            [],
            |row| row.get(0),
        )
        .expect("count current session Goal rows");
    assert_eq!(current_goals, 1);

    let duplicate_error = connection
        .execute(
            "INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('third', 'goal', 'main', 'running', 1, 'boot-a', 'shared-session', 1, 'now')",
            [],
        )
        .expect_err("a second current Goal for one session must be rejected");
    assert_eq!(
        duplicate_error.sqlite_error_code(),
        Some(ErrorCode::ConstraintViolation)
    );
}

#[test]
fn objective_is_immutable_even_to_raw_sql() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    SqliteTaskStore::new(directory.path()).expect("initialize control-plane schema");

    let connection = Connection::open(directory.path().join("control_plane.db"))
        .expect("open verification connection");
    let success_criteria_columns: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('goal_tasks') WHERE name = 'success_criteria'",
            [],
            |row| row.get(0),
        )
        .expect("inspect fresh Goal schema");
    assert_eq!(
        success_criteria_columns, 0,
        "fresh schemas retain only the canonical objective"
    );
    insert_current_goal(&connection, "goal-one", "session-one")
        .expect("insert session-bound Goal task");
    connection
        .execute(
            "INSERT INTO goal_tasks (task_id, objective) VALUES (?1, ?2)",
            params!["goal-one", "finish the existing task"],
        )
        .expect("insert Goal control state");

    let error = connection
        .execute(
            "UPDATE goal_tasks SET objective = ?1 WHERE task_id = ?2",
            params!["a different stopping criterion", "goal-one"],
        )
        .expect_err("direct SQLite cannot rewrite an admitted Goal objective");
    assert_eq!(
        error.sqlite_error_code(),
        Some(ErrorCode::ConstraintViolation)
    );

    let objective: String = connection
        .query_row(
            "SELECT objective FROM goal_tasks WHERE task_id = ?1",
            params!["goal-one"],
            |row| row.get(0),
        )
        .expect("read persisted objective");
    assert_eq!(objective, "finish the existing task");
}

#[tokio::test]
async fn provisional_v8_success_criteria_is_ignored() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize control-plane schema");
    let database = directory.path().join("control_plane.db");
    let connection = Connection::open(&database).expect("open provisional v8 database");
    connection
        .execute_batch(
            "ALTER TABLE goal_tasks
                 ADD COLUMN success_criteria TEXT NOT NULL DEFAULT '';
             UPDATE goal_tasks
                SET success_criteria = 'ignored legacy criterion';",
        )
        .expect("add provisional local v8 column");
    drop(connection);

    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("provisional", "provisional-session"),
                session_goal_extension("provisional"),
            )
            .await
            .expect("create Goal through corrected storage model"),
        GoalTransitionResult::Applied
    );
    drop(store);

    let reopened = SqliteTaskStore::new(directory.path()).expect("reopen provisional v8 database");
    let goal = reopened
        .get_goal_task("provisional")
        .await
        .expect("read provisional v8 Goal");
    assert_eq!(
        goal.expect("Goal control state remains readable").objective,
        "produce a verified result",
        "the old column is ignored rather than becoming a second model field"
    );
}

#[tokio::test]
async fn migration_normalizes_a_session_bound_goal_epoch_before_operation_admission() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize control-plane schema");
    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("provisional-epoch", "provisional-epoch-session"),
                session_goal_extension("provisional-epoch"),
            )
            .await
            .expect("create session Goal"),
        GoalTransitionResult::Applied
    );
    let database = directory.path().join("control_plane.db");
    Connection::open(&database)
        .expect("open provisional migration fixture")
        .execute_batch(
            "DROP TRIGGER trg_goal_tasks_require_epoch_update;
             DROP TRIGGER trg_goal_tasks_epoch_monotonic;
             UPDATE tasks SET execution_epoch = 0 WHERE id = 'provisional-epoch';
             PRAGMA user_version = 9;",
        )
        .expect("write provisional epoch-zero fixture");
    drop(store);

    let migrated = SqliteTaskStore::new(directory.path()).expect("migrate provisional fixture");
    assert_eq!(
        migrated
            .current_goal_for_session("provisional-epoch-session")
            .await
            .expect("read migrated Goal")
            .expect("Goal remains current")
            .execution_epoch,
        1
    );
    assert_eq!(
        migrated
            .admit_pending_operation(
                "provisional-epoch",
                "provisional-epoch-session",
                1,
                "provisional-epoch-operation",
            )
            .await
            .expect("admit operation at normalized epoch"),
        GoalTransitionResult::Applied
    );
}

#[test]
fn independent_goal_registries_admit_exactly_one_current_goal_for_a_session() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let left = SqliteTaskStore::new(directory.path()).expect("open left registry");
    let right = SqliteTaskStore::new(directory.path()).expect("open right registry");
    let barrier = Arc::new(Barrier::new(2));

    let outcomes: Vec<_> = [("left", left), ("right", right)]
        .into_iter()
        .map(|(task_id, store)| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build thread-local runtime")
                    .block_on(async move {
                        store
                            .create_or_replace_session_goal(
                                session_goal_task(task_id, "shared-session"),
                                session_goal_extension(task_id),
                            )
                            .await
                    })
            })
        })
        .collect();

    let outcomes: Vec<_> = outcomes
        .into_iter()
        .map(|thread| thread.join().expect("join concurrent registry"))
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Ok(GoalTransitionResult::Applied)))
            .count(),
        1,
        "exactly one guarded admission succeeds"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Ok(GoalTransitionResult::Stale)))
            .count(),
        1,
        "the concurrent loser observes the current nonterminal Goal"
    );
}

#[tokio::test]
async fn guarded_transitions_fence_stale_epochs_and_terminal_replacement() {
    let store = SqliteTaskStore::new_in_memory().expect("initialize store");
    let task = session_goal_task("goal-one", "session-one");
    let extension = session_goal_extension("goal-one");

    assert_eq!(
        store
            .create_or_replace_session_goal(task, extension)
            .await
            .expect("create session Goal"),
        GoalTransitionResult::Applied
    );
    let current = store
        .current_goal_for_session("session-one")
        .await
        .expect("read current Goal")
        .expect("Goal exists");
    assert_eq!(current.id, "goal-one");
    assert_eq!(current.execution_epoch, 1);

    assert_eq!(
        store
            .pause_session_goal(
                "goal-one",
                "session-one",
                1,
                GoalPauseState {
                    reason: GoalPauseReason::OperatorPaused,
                    description: None,
                    blockers: Vec::new(),
                },
            )
            .await
            .expect("pause exact epoch"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .finish_session_goal("goal-one", "session-one", 1, TaskStatus::Completed, None)
            .await
            .expect("classify stale finish"),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .resume_session_goal("goal-one", "session-one", 2, 2, "boot-b")
            .await
            .expect("resume exact epoch"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .update_session_goal_limits("goal-one", "session-one", 2, Some(11), None)
            .await
            .expect("fence stale limit update"),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .update_session_goal_limits("goal-one", "session-one", 3, Some(11), Some(1.25))
            .await
            .expect("update limits at exact epoch"),
        GoalTransitionResult::Applied
    );
    let limits = store
        .get_goal_task("goal-one")
        .await
        .expect("read updated Goal limits")
        .expect("Goal extension exists");
    assert_eq!(limits.effective_token_limit, Some(11));
    assert_eq!(limits.effective_cost_limit_usd, Some(1.25));
    assert_eq!(
        store
            .admit_pending_operation("goal-one", "session-one", 3, "operation-one")
            .await
            .expect("admit operation"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .pause_session_goal(
                "goal-one",
                "session-one",
                3,
                GoalPauseState {
                    reason: GoalPauseReason::OperatorPaused,
                    description: None,
                    blockers: Vec::new(),
                },
            )
            .await
            .expect("fence admitted operation"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .admit_pending_operation("goal-one", "session-one", 4, "operation-after-pause")
            .await
            .expect("reject post-pause operation admission"),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .resume_session_goal("goal-one", "session-one", 4, 2, "boot-new")
            .await
            .expect("reject resume with pending operation"),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .settle_pending_operation(
                "goal-one",
                "session-one",
                3,
                "operation-one",
                GoalAccountingState::Complete,
            )
            .await
            .expect("settle matching admitted operation"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .finish_session_goal(
                "goal-one",
                "session-one",
                4,
                TaskStatus::Completed,
                Some("verified completion".into()),
            )
            .await
            .expect("finish fenced Goal"),
        GoalTransitionResult::Applied
    );
    let terminal = store
        .current_goal_for_session("session-one")
        .await
        .expect("read terminal Goal")
        .expect("terminal Goal stays current until replacement");
    assert_eq!(terminal.status, TaskStatus::Completed);
    assert_eq!(terminal.execution_epoch, 5);
    assert_eq!(
        store
            .terminal_reason_for_session_goal("goal-one", "session-one")
            .await
            .expect("read terminal reason")
            .as_deref(),
        Some("verified completion")
    );

    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("goal-two", "session-one"),
                session_goal_extension("goal-two"),
            )
            .await
            .expect("replace settled terminal Goal"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .finish_session_goal("goal-one", "session-one", 5, TaskStatus::Completed, None)
            .await
            .expect("classify removed predecessor"),
        GoalTransitionResult::Missing
    );
}

#[tokio::test]
async fn replacement_rejects_an_existing_successor_id_without_deleting_terminal_goal() {
    let store = SqliteTaskStore::new_in_memory().expect("initialize store");
    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("terminal-goal", "collision-session"),
                session_goal_extension("terminal-goal"),
            )
            .await
            .expect("create session Goal"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .finish_session_goal(
                "terminal-goal",
                "collision-session",
                1,
                TaskStatus::Completed,
                None,
            )
            .await
            .expect("finish session Goal"),
        GoalTransitionResult::Applied
    );

    let colliding_task = TaskRecord {
        id: "colliding-goal".into(),
        kind: TaskKind::Delegate,
        agent: "main".into(),
        status: TaskStatus::Running,
        owner_pid: 1,
        owner_boot_id: "boot-a".into(),
        heartbeat_at: None,
        depth: 0,
        parent_id: None,
        originator_route: None,
        originator_chain: Vec::new(),
        delivered: false,
        idem_key: None,
        principal_id: None,
        session_id: None,
        execution_epoch: 0,
        started_at: "2026-08-30T00:00:00Z".into(),
        finished_at: None,
    };
    store
        .create(colliding_task)
        .await
        .expect("insert collision");

    let error = store
        .create_or_replace_session_goal(
            session_goal_task("colliding-goal", "collision-session"),
            session_goal_extension("colliding-goal"),
        )
        .await
        .expect_err("existing successor identity must be rejected");
    assert!(error.to_string().contains("already exists"));

    let current = store
        .current_goal_for_session("collision-session")
        .await
        .expect("read terminal Goal")
        .expect("terminal Goal remains current after rejected replacement");
    assert_eq!(current.id, "terminal-goal");
    assert_eq!(current.status, TaskStatus::Completed);
}

#[tokio::test]
async fn pausing_a_goal_is_resumable_when_no_operation_is_pending() {
    let store = SqliteTaskStore::new_in_memory().expect("initialize store");
    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("tool-pause", "tool-pause-session"),
                session_goal_extension("tool-pause"),
            )
            .await
            .expect("create session Goal"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .pause_session_goal(
                "tool-pause",
                "tool-pause-session",
                1,
                GoalPauseState {
                    reason: GoalPauseReason::OperatorPaused,
                    description: None,
                    blockers: Vec::new(),
                },
            )
            .await
            .expect("pause Goal"),
        GoalTransitionResult::Applied
    );
    let task = store
        .current_goal_for_session("tool-pause-session")
        .await
        .expect("read fenced Goal")
        .expect("Goal remains auditable");
    assert_eq!(task.status, TaskStatus::Paused);
    assert_eq!(task.execution_epoch, 2);
    assert_eq!(
        store
            .resume_session_goal("tool-pause", "tool-pause-session", 2, 2, "boot-new")
            .await
            .expect("resume paused Goal"),
        GoalTransitionResult::Applied
    );
}

#[tokio::test]
async fn pause_rejects_a_goal_missing_its_control_extension() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize control-plane schema");
    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("missing-extension", "missing-extension-session"),
                session_goal_extension("missing-extension"),
            )
            .await
            .expect("create session Goal"),
        GoalTransitionResult::Applied
    );
    Connection::open(directory.path().join("control_plane.db"))
        .expect("open independent corruption connection")
        .execute(
            "DELETE FROM goal_tasks WHERE task_id = 'missing-extension'",
            [],
        )
        .expect("remove corrupt control extension");

    assert_eq!(
        store
            .pause_session_goal(
                "missing-extension",
                "missing-extension-session",
                1,
                GoalPauseState {
                    reason: GoalPauseReason::OperatorPaused,
                    description: None,
                    blockers: Vec::new(),
                },
            )
            .await
            .expect("classify corrupt Goal pause"),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        store
            .current_goal_for_session("missing-extension-session")
            .await
            .expect("read corrupt Goal")
            .expect("Goal remains auditable")
            .status,
        TaskStatus::Running
    );
}

#[tokio::test]
async fn different_sessions_do_not_share_goal_admission_or_disposal_state() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize control-plane schema");
    for (task_id, session_id) in [
        ("goal-left", "session-left"),
        ("goal-right", "session-right"),
    ] {
        assert_eq!(
            store
                .create_or_replace_session_goal(
                    session_goal_task(task_id, session_id),
                    session_goal_extension(task_id),
                )
                .await
                .expect("create independent Goal"),
            GoalTransitionResult::Applied
        );
    }
    assert_eq!(
        store
            .finish_session_goal("goal-left", "session-left", 1, TaskStatus::Cancelled, None)
            .await
            .expect("cancel left Goal"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .delete_session_goal("goal-left", "session-left", 2)
            .await
            .expect("dispose left control state"),
        GoalTransitionResult::Applied
    );
    assert!(
        store
            .current_goal_for_session("session-left")
            .await
            .expect("read deleted session")
            .is_none()
    );
    let removed_control_rows: i64 = Connection::open(directory.path().join("control_plane.db"))
        .expect("open independent verification connection")
        .query_row(
            "SELECT COUNT(*) FROM goal_tasks WHERE task_id = 'goal-left'",
            [],
            |row| row.get(0),
        )
        .expect("query disposed Goal control rows");
    assert_eq!(
        removed_control_rows, 0,
        "disposal cascades Goal control-state removal"
    );
    assert_eq!(
        store
            .current_goal_for_session("session-right")
            .await
            .expect("read unaffected session")
            .expect("right Goal survives left disposal")
            .id,
        "goal-right"
    );
}

#[tokio::test]
async fn generic_task_registration_cannot_create_a_goal() {
    let store = SqliteTaskStore::new_in_memory().expect("initialize store");
    let error = store
        .create(session_goal_task("goal-direct", "session-direct"))
        .await
        .expect_err("Goal creation must use the guarded session API");
    assert!(format!("{error:#}").contains("Goal tasks require"));
}

#[tokio::test]
async fn generic_lifecycle_mutators_cannot_bypass_session_bound_goal_fencing() {
    let store = SqliteTaskStore::new_in_memory().expect("initialize store");
    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("goal-generic-fence", "session-generic-fence"),
                session_goal_extension("goal-generic-fence"),
            )
            .await
            .expect("create session Goal"),
        GoalTransitionResult::Applied
    );

    for result in [
        TaskRegistry::heartbeat(&store, "goal-generic-fence", "boot-a").await,
        TaskRegistry::claim_owner(&store, "goal-generic-fence", 2, "boot-b").await,
        TaskRegistry::update_status(
            &store,
            "goal-generic-fence",
            TaskStatus::Completed,
            None,
            None,
        )
        .await,
    ] {
        let error = result.expect_err("generic lifecycle mutation must reject a session Goal");
        assert!(format!("{error:#}").contains("session-bound Goal"));
    }
    let goal = store
        .get("goal-generic-fence")
        .await
        .expect("load Goal before progress mutation")
        .expect("Goal remains after refused generic mutations");
    assert_eq!(goal.status, TaskStatus::Running);
    let error = TaskRegistry::record_progress(
        &store,
        "goal-generic-fence",
        &goal.owner_boot_id,
        &TaskProgress::default(),
    )
    .await
    .expect_err("generic progress mutation must reject a session Goal");
    assert!(format!("{error:#}").contains("session-bound Goal"));
    let snapshot = store
        .get_snapshot("goal-generic-fence")
        .await
        .expect("load Goal snapshot after refused progress mutation")
        .expect("Goal snapshot remains");
    assert!(snapshot.progress.is_none());
    assert_eq!(snapshot.task.heartbeat_at, goal.heartbeat_at);
    let before_delete = serde_json::to_value(
        store
            .get_goal_task("goal-generic-fence")
            .await
            .expect("load session Goal before deletion")
            .expect("session Goal exists"),
    )
    .expect("serialize session Goal before deletion");
    let error = store
        .delete_by_agent("main")
        .expect_err("generic agent deletion must reject a session Goal");
    assert!(format!("{error:#}").contains("session-bound Goal"));
    assert_eq!(
        serde_json::to_value(
            store
                .get_goal_task("goal-generic-fence")
                .await
                .expect("load session Goal after refused deletion")
                .expect("session Goal remains"),
        )
        .expect("serialize session Goal after refused deletion"),
        before_delete
    );

    let error = store
        .persist_terminal_settlement_intent(TerminalSettlementIntent {
            task_id: "goal-generic-fence".into(),
            owner_pid: 1,
            owner_boot_id: "boot-a".into(),
            desired_status: TaskStatus::Completed,
            artifact_path: "/tmp/goal.json".into(),
            artifact_ref: Some("artifact:goal.json".into()),
            artifact_sha256: "00".repeat(32),
            terminal_error: None,
        })
        .await
        .expect_err("generic settlement intent must reject a session Goal");
    assert!(format!("{error:#}").contains("session-bound Goal"));

    let settlement = TerminalSettlementIntent {
        task_id: "goal-generic-fence".into(),
        owner_pid: 1,
        owner_boot_id: "boot-a".into(),
        desired_status: TaskStatus::Completed,
        artifact_path: "/tmp/goal.json".into(),
        artifact_ref: Some("artifact:goal.json".into()),
        artifact_sha256: "00".repeat(32),
        terminal_error: None,
    };
    for result in [
        TaskRegistry::transition_terminal(
            &store,
            "goal-generic-fence",
            TaskStatus::Completed,
            None,
            None,
        )
        .await,
        TaskRegistry::transition_terminal_if_owner(
            &store,
            "goal-generic-fence",
            1,
            "boot-a",
            TaskStatus::Completed,
            None,
            None,
        )
        .await,
        store
            .promote_terminal_settlement(&settlement, TaskStatus::Completed, None, None)
            .await,
        store.discard_terminal_settlement_intent(&settlement).await,
        TaskRegistry::reconcile_timed_out(&store, "goal-generic-fence", 1, "boot-a", "heartbeat-a")
            .await,
    ] {
        let error = result.expect_err("generic lifecycle mutation must reject a session Goal");
        assert!(format!("{error:#}").contains("session-bound Goal"));
    }
}

#[tokio::test]
async fn sqlite_guards_session_binding_pending_pairing_and_goal_state_domains() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize control-plane schema");
    for (task_id, session_id) in [
        ("goal-raw-one", "session-raw-one"),
        ("goal-raw-two", "session-raw-two"),
    ] {
        assert_eq!(
            store
                .create_or_replace_session_goal(
                    session_goal_task(task_id, session_id),
                    session_goal_extension(task_id),
                )
                .await
                .expect("create session Goal"),
            GoalTransitionResult::Applied
        );
    }
    let connection = Connection::open(directory.path().join("control_plane.db"))
        .expect("open independent verification connection");
    let assert_constraint = |result: rusqlite::Result<usize>, scenario: &str| {
        let error = result.expect_err(scenario);
        assert_eq!(
            error.sqlite_error_code(),
            Some(ErrorCode::ConstraintViolation)
        );
    };

    assert_constraint(
        connection.execute(
            "INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('goal-null-session', 'goal', 'main', 'running', 1, 'boot-a', NULL, 1, 'now')",
            [],
        ),
        "raw SQL must reject a null Goal session",
    );
    assert_constraint(
        connection.execute(
            "INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('goal-blank-session', 'goal', 'main', 'running', 1, 'boot-a', '  ', 1, 'now')",
            [],
        ),
        "raw SQL must reject a blank Goal session",
    );
    assert_constraint(
        connection.execute(
            "INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('goal-text-epoch', 'goal', 'main', 'running', 1, 'boot-a',
                       'text-epoch-session', 'not-an-epoch', 'now')",
            [],
        ),
        "raw SQL must reject a noninteger Goal execution epoch on insert",
    );
    assert_constraint(
        connection.execute(
            "UPDATE tasks SET session_id = 'other-session' WHERE id = 'goal-raw-one'",
            [],
        ),
        "raw SQL must not rebind a Goal session",
    );
    assert_constraint(
        connection.execute(
            "UPDATE tasks SET kind = 'delegate' WHERE id = 'goal-raw-one'",
            [],
        ),
        "raw SQL must not demote a Goal before unbinding it",
    );
    assert_constraint(
        connection.execute(
            "UPDATE tasks SET execution_epoch = 0 WHERE id = 'goal-raw-one'",
            [],
        ),
        "raw SQL must not lower a session Goal execution epoch below one",
    );
    connection
        .execute(
            "UPDATE tasks SET execution_epoch = 2 WHERE id = 'goal-raw-one'",
            [],
        )
        .expect("raw SQL may advance a session Goal execution epoch");
    assert_constraint(
        connection.execute(
            "UPDATE tasks SET execution_epoch = 1 WHERE id = 'goal-raw-one'",
            [],
        ),
        "raw SQL must not rewind a session Goal execution epoch",
    );
    assert_constraint(
        connection.execute(
            "UPDATE tasks SET execution_epoch = 'not-an-epoch' WHERE id = 'goal-raw-one'",
            [],
        ),
        "raw SQL must reject a noninteger Goal execution epoch on update",
    );
    connection
        .execute(
            "INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('delegate-promote', 'delegate', 'main', 'running', 1, 'boot-a',
                       'promotion-session', 0, 'now')",
            [],
        )
        .expect("insert non-Goal row for promotion guard");
    assert_constraint(
        connection.execute(
            "UPDATE tasks SET kind = 'goal' WHERE id = 'delegate-promote'",
            [],
        ),
        "raw SQL must not promote a non-Goal into a session-bound Goal",
    );
    assert_constraint(
        connection.execute(
            "UPDATE goal_tasks SET pending_call_id = 'call-one' WHERE task_id = 'goal-raw-one'",
            [],
        ),
        "raw SQL must require a pending operation epoch",
    );
    connection
        .execute(
            "UPDATE goal_tasks SET pending_call_id = 'call-one', pending_call_epoch = 1
             WHERE task_id = 'goal-raw-one'",
            [],
        )
        .expect("write one valid pending operation");
    assert_constraint(
        connection.execute(
            "UPDATE goal_tasks SET pending_call_id = 'call-one', pending_call_epoch = 1
             WHERE task_id = 'goal-raw-two'",
            [],
        ),
        "raw SQL must reject a reused pending operation id",
    );
    assert_constraint(
        connection.execute(
            "UPDATE goal_tasks SET accounting_state = 'not-a-state'
             WHERE task_id = 'goal-raw-two'",
            [],
        ),
        "raw SQL must reject an unknown accounting state",
    );
}

#[tokio::test]
async fn migration_fails_nonterminal_legacy_goals_but_keeps_terminal_audit_rows() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize current schema");
    let database = directory.path().join("control_plane.db");
    drop(store);

    let connection = Connection::open(&database).expect("open fixture database");
    connection
        .execute_batch(
            "DROP TRIGGER trg_goal_tasks_require_session_insert;
             DROP TRIGGER trg_goal_tasks_require_session_update;
             DROP TRIGGER trg_goal_tasks_require_epoch_insert;
             INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES
                 ('legacy-running', 'goal', 'main', 'running', 1, 'boot-old', NULL, 0, 'now'),
                 ('legacy-completed', 'goal', 'main', 'completed', 1, 'boot-old', NULL, 0, 'now'),
                 ('legacy-blank-objective', 'goal', 'main', 'running', 1, 'boot-old',
                  'legacy-blank-session', 0, 'now');
             INSERT INTO goal_tasks (task_id, objective) VALUES
                 ('legacy-running', 'old goal'),
                 ('legacy-completed', 'old terminal goal'),
                 ('legacy-blank-objective', '');
             PRAGMA user_version = 7;",
        )
        .expect("write pre-v8 Goal fixtures");
    drop(connection);

    let migrated = SqliteTaskStore::new(directory.path()).expect("migrate fixture database");
    let verify = Connection::open(&database).expect("open migrated database");
    let schema_version: i64 = verify
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read migrated schema version");
    assert_eq!(
        schema_version, 13,
        "migration records the final control-plane schema"
    );
    let running: (String, Option<String>, Option<String>) = verify
        .query_row(
            "SELECT status, error, session_id FROM tasks WHERE id = 'legacy-running'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read migrated nonterminal Goal");
    assert_eq!(running.0, "failed");
    assert_eq!(running.1.as_deref(), Some("legacy_goal_missing_session"));
    assert!(running.2.is_none());
    let blank_objective: (String, Option<String>) = verify
        .query_row(
            "SELECT status, error FROM tasks WHERE id = 'legacy-blank-objective'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read migrated blank-objective Goal");
    assert_eq!(blank_objective.0, "failed");
    assert_eq!(
        blank_objective.1.as_deref(),
        Some("legacy_goal_missing_objective")
    );
    let terminal_status: String = verify
        .query_row(
            "SELECT status FROM tasks WHERE id = 'legacy-completed'",
            [],
            |row| row.get(0),
        )
        .expect("read terminal legacy Goal");
    assert_eq!(terminal_status, "completed");
    assert_eq!(
        migrated
            .finish_session_goal(
                "legacy-completed",
                "session-never-bound",
                0,
                TaskStatus::Completed,
                None,
            )
            .await
            .expect("classify legacy Goal transition"),
        GoalTransitionResult::Stale,
        "legacy null-session rows must return a typed nonmatching-binding result"
    );
    assert!(
        migrated
            .current_goal_for_session("legacy-session")
            .await
            .expect("current Goal query")
            .is_none()
    );
}

#[tokio::test]
async fn boot_recovery_pauses_clean_goals_and_fails_unsettled_operations() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize store");
    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("clean", "clean-session"),
                session_goal_extension("clean"),
            )
            .await
            .expect("create clean Goal"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("pending", "pending-session"),
                session_goal_extension("pending"),
            )
            .await
            .expect("create pending Goal"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("missing", "missing-session"),
                session_goal_extension("missing"),
            )
            .await
            .expect("create missing-usage Goal"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .admit_pending_operation("pending", "pending-session", 1, "operation-pending")
            .await
            .expect("admit pending operation"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .admit_pending_operation("missing", "missing-session", 1, "operation-missing")
            .await
            .expect("admit missing-usage operation"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .settle_pending_operation(
                "missing",
                "missing-session",
                1,
                "operation-missing",
                GoalAccountingState::Missing,
            )
            .await
            .expect("record missing usage"),
        GoalTransitionResult::Applied
    );
    drop(store);

    let reopened = SqliteTaskStore::new(directory.path()).expect("reopen store");
    assert_eq!(
        reopened
            .reconcile_goal_boot_state("boot-new")
            .expect("reconcile previous boot"),
        3
    );
    let clean = reopened
        .current_goal_for_session("clean-session")
        .await
        .expect("read clean Goal")
        .expect("clean Goal exists");
    assert_eq!(clean.status, TaskStatus::Paused);
    assert_eq!(clean.execution_epoch, 2);
    assert_eq!(
        reopened
            .resume_session_goal("clean", "clean-session", 2, 2, "boot-new")
            .await
            .expect("resume post-restart Goal"),
        GoalTransitionResult::Applied
    );
    let pending = reopened
        .current_goal_for_session("pending-session")
        .await
        .expect("read pending Goal")
        .expect("pending Goal exists");
    assert_eq!(pending.status, TaskStatus::Failed);
    let pending_extension = reopened
        .get_goal_task("pending")
        .await
        .expect("read pending extension")
        .expect("extension exists");
    assert_eq!(
        pending_extension.accounting_state,
        GoalAccountingState::OutcomeUnknown
    );
    assert!(pending_extension.pending_call_id.is_none());
    assert_eq!(
        reopened
            .terminal_reason_for_session_goal("missing", "missing-session")
            .await
            .expect("read missing-usage terminal reason")
            .as_deref(),
        Some("accounting_missing_or_invalid")
    );
}

#[tokio::test]
async fn boot_recovery_settles_terminal_goal_operation_before_replacement() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize store");
    assert_eq!(
        store
            .create_or_replace_session_goal(
                session_goal_task("cancelled-pending", "terminal-pending-session"),
                session_goal_extension("cancelled-pending"),
            )
            .await
            .expect("create Goal"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .admit_pending_operation(
                "cancelled-pending",
                "terminal-pending-session",
                1,
                "operation-cancelled-pending",
            )
            .await
            .expect("admit pending operation"),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        store
            .finish_session_goal(
                "cancelled-pending",
                "terminal-pending-session",
                1,
                TaskStatus::Cancelled,
                None,
            )
            .await
            .expect("cancel Goal while operation settles"),
        GoalTransitionResult::Applied
    );
    drop(store);

    let reopened = SqliteTaskStore::new(directory.path()).expect("reopen store");
    assert_eq!(
        reopened
            .reconcile_goal_boot_state("boot-new")
            .expect("classify interrupted terminal operation"),
        1
    );
    let cancelled = reopened
        .current_goal_for_session("terminal-pending-session")
        .await
        .expect("read cancelled Goal")
        .expect("cancelled Goal remains auditable");
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    assert_eq!(cancelled.execution_epoch, 2);
    let extension = reopened
        .get_goal_task("cancelled-pending")
        .await
        .expect("read Goal extension")
        .expect("Goal extension remains readable");
    assert_eq!(
        extension.accounting_state,
        GoalAccountingState::OutcomeUnknown
    );
    assert!(extension.pending_call_id.is_none());
    assert!(extension.pending_call_epoch.is_none());

    assert_eq!(
        reopened
            .create_or_replace_session_goal(
                session_goal_task("replacement", "terminal-pending-session"),
                session_goal_extension("replacement"),
            )
            .await
            .expect("replace settled terminal Goal"),
        GoalTransitionResult::Applied,
        "terminal Goal control state must not wedge its session after recovery"
    );
    assert_eq!(
        reopened
            .current_goal_for_session("terminal-pending-session")
            .await
            .expect("read replacement Goal")
            .expect("replacement Goal exists")
            .id,
        "replacement"
    );
}

#[tokio::test]
async fn boot_recovery_does_not_fence_a_live_foreign_goal_owner() {
    let store = SqliteTaskStore::new_in_memory().expect("initialize store");
    let mut task = session_goal_task("live-owner", "live-owner-session");
    task.owner_pid = std::process::id();
    task.owner_boot_id = format!("zc-process-v1:{}:unknown:live", task.owner_pid);
    assert_eq!(
        store
            .create_or_replace_session_goal(task, session_goal_extension("live-owner"))
            .await
            .expect("create live-owner Goal"),
        GoalTransitionResult::Applied
    );

    assert_eq!(
        store
            .reconcile_goal_boot_state("foreign-boot")
            .expect("reconcile foreign boot"),
        0,
        "a different boot id alone is not authority to fence a live owner"
    );
    assert_eq!(
        store
            .current_goal_for_session("live-owner-session")
            .await
            .expect("read live-owner Goal")
            .expect("Goal remains current")
            .status,
        TaskStatus::Running
    );
}

#[tokio::test]
async fn boot_recovery_fails_corrupt_session_goal_without_extension() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize store");
    let database = directory.path().join("control_plane.db");
    drop(store);
    Connection::open(&database)
        .expect("open fixture database")
        .execute(
            "INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('corrupt', 'goal', 'main', 'running', 999999, 'boot-old', 'session-corrupt', 1, 'now')",
            [],
        )
        .expect("insert corrupt Goal fixture");

    let reopened = SqliteTaskStore::new(directory.path()).expect("reopen store");
    assert_eq!(
        reopened
            .reconcile_goal_boot_state("boot-new")
            .expect("reconcile corrupt Goal"),
        1
    );
    assert_eq!(
        reopened
            .get("corrupt")
            .await
            .expect("read corrupt Goal")
            .expect("row remains auditable")
            .status,
        TaskStatus::Failed
    );
    assert_eq!(
        reopened
            .delete_session_goal("corrupt", "session-corrupt", 2)
            .await
            .expect("dispose terminal corrupt Goal"),
        GoalTransitionResult::Applied,
        "a missing extension cannot hold a pending operation or block disposal"
    );
    assert!(
        reopened
            .current_goal_for_session("session-corrupt")
            .await
            .expect("read disposed corrupt Goal")
            .is_none()
    );

    Connection::open(&database)
        .expect("open replacement fixture database")
        .execute(
            "INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('corrupt-replace', 'goal', 'main', 'running', 999999, 'boot-old',
                       'session-replace', 1, 'now')",
            [],
        )
        .expect("insert corrupt replacement fixture");
    let reopened = SqliteTaskStore::new(directory.path()).expect("reopen replacement fixture");
    assert_eq!(
        reopened
            .reconcile_goal_boot_state("boot-newer")
            .expect("fail corrupt predecessor"),
        1
    );
    assert_eq!(
        reopened
            .create_or_replace_session_goal(
                session_goal_task("replacement", "session-replace"),
                session_goal_extension("replacement"),
            )
            .await
            .expect("replace terminal corrupt Goal"),
        GoalTransitionResult::Applied,
        "a missing extension cannot block terminal replacement"
    );
}

#[tokio::test]
async fn boot_recovery_skips_an_unreadable_goal_without_starving_other_recovery() {
    let directory = tempfile::tempdir().expect("create temporary control-plane directory");
    let store = SqliteTaskStore::new(directory.path()).expect("initialize store");
    let database = directory.path().join("control_plane.db");
    drop(store);

    let fixture = Connection::open(&database).expect("open fixture database");
    fixture
        .execute_batch(
            "INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('unreadable', 'goal', 'main', 'running', 'not-a-pid', 'boot-old',
                       'session-unreadable', 1, 'now');
             INSERT INTO tasks (
                 id, kind, agent, status, owner_pid, owner_boot_id, session_id,
                 execution_epoch, started_at
             ) VALUES ('recoverable', 'goal', 'main', 'running', 999999, 'boot-old',
                       'session-recoverable', 1, 'now');",
        )
        .expect("write unreadable and recoverable Goal fixtures");
    drop(fixture);

    let reopened = SqliteTaskStore::new(directory.path()).expect("reopen store");
    assert_eq!(
        reopened
            .reconcile_goal_boot_state("boot-new")
            .expect("reconcile recoverable Goal despite unreadable sibling"),
        1
    );
    assert_eq!(
        reopened
            .get("recoverable")
            .await
            .expect("read recovered Goal")
            .expect("recoverable Goal exists")
            .status,
        TaskStatus::Failed
    );
}
