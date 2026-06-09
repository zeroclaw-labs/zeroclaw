use super::{
    Task, TaskActivity, TaskActor, TaskError, TaskOrigin, TaskOutcome, TaskState, Transition,
};
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use uuid::Uuid;

fn db_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("tasks").join("tasks.db")
}

fn connect(workspace_dir: &Path) -> Result<Connection> {
    let path = db_path(workspace_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create tasks directory: {}", parent.display())
        })?;
    }
    let conn = Connection::open(&path)
        .with_context(|| format!("Failed to open tasks.db: {}", path.display()))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA temp_store = MEMORY;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                 id                 TEXT PRIMARY KEY,
                 title              TEXT NOT NULL,
                 origin             TEXT NOT NULL,
                 state              TEXT NOT NULL DEFAULT 'open',
                 priority           INTEGER NOT NULL DEFAULT 2,
                 created_at         TEXT NOT NULL,
                 updated_at         TEXT NOT NULL,
                 claimed_by_channel TEXT,
                 claimed_by_id      TEXT,
                 blocked_reason     TEXT,
                 outcome            TEXT,
                 parent_task_id     TEXT REFERENCES tasks(id)
             );
             CREATE INDEX IF NOT EXISTS idx_tasks_state ON tasks(state);
             CREATE INDEX IF NOT EXISTS idx_tasks_priority ON tasks(priority);
             CREATE INDEX IF NOT EXISTS idx_tasks_origin ON tasks(origin);
             CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_task_id);
             CREATE INDEX IF NOT EXISTS idx_tasks_updated ON tasks(updated_at);

             CREATE TABLE IF NOT EXISTS task_activity (
                 id            INTEGER PRIMARY KEY AUTOINCREMENT,
                 task_id       TEXT NOT NULL REFERENCES tasks(id),
                 old_state     TEXT,
                 new_state     TEXT NOT NULL,
                 actor_channel TEXT NOT NULL,
                 actor_id      TEXT,
                 reason        TEXT,
                 timestamp     TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_activity_task ON task_activity(task_id);
             CREATE INDEX IF NOT EXISTS idx_activity_ts ON task_activity(timestamp);

             PRAGMA user_version = 1;",
        )
        .context("Failed to run tasks.db migration v1")?;
    }

    Ok(())
}

pub fn create_task(
    workspace_dir: &Path,
    title: &str,
    origin: TaskOrigin,
    priority: u8,
    parent_task_id: Option<&str>,
    actor: &TaskActor,
) -> Result<Task, TaskError> {
    if priority > 4 {
        return Err(TaskError::InvalidPriority(priority));
    }

    let conn = connect(workspace_dir)?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO tasks (id, title, origin, state, priority, created_at, updated_at, parent_task_id)
         VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6, ?7)",
        params![id, title, origin.as_str(), priority as i64, now, now, parent_task_id],
    )
    .context("Failed to insert task")?;

    record_activity(&conn, &id, None, TaskState::Open, actor, None)?;

    get_task_inner(&conn, &id)
}

pub fn get_task(workspace_dir: &Path, task_id: &str) -> Result<Task, TaskError> {
    let conn = connect(workspace_dir)?;
    get_task_inner(&conn, task_id)
}

fn get_task_inner(conn: &Connection, task_id: &str) -> Result<Task, TaskError> {
    conn.query_row(
        "SELECT id, title, origin, state, priority, created_at, updated_at,
                claimed_by_channel, claimed_by_id, blocked_reason, outcome, parent_task_id
         FROM tasks WHERE id = ?1",
        params![task_id],
        |row| {
            Ok(Task {
                id: row.get(0)?,
                title: row.get(1)?,
                origin: TaskOrigin::from_str(&row.get::<_, String>(2)?)
                    .unwrap_or(TaskOrigin::Manual),
                state: TaskState::from_str(&row.get::<_, String>(3)?)
                    .unwrap_or(TaskState::Open),
                priority: row.get::<_, i64>(4)? as u8,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                claimed_by_channel: row.get(7)?,
                claimed_by_id: row.get(8)?,
                blocked_reason: row.get(9)?,
                outcome: row.get::<_, Option<String>>(10)?
                    .and_then(|s| TaskOutcome::from_str(&s)),
                parent_task_id: row.get(11)?,
            })
        },
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => TaskError::NotFound(task_id.to_string()),
        other => TaskError::Db(other.into()),
    })
}

pub fn list_tasks(
    workspace_dir: &Path,
    state_filter: Option<TaskState>,
    limit: usize,
) -> Result<Vec<Task>> {
    let conn = connect(workspace_dir)?;
    let lim = i64::try_from(limit.max(1)).unwrap_or(100);

    let mut tasks = Vec::new();

    if let Some(st) = state_filter {
        let mut stmt = conn.prepare(
            "SELECT id, title, origin, state, priority, created_at, updated_at,
                    claimed_by_channel, claimed_by_id, blocked_reason, outcome, parent_task_id
             FROM tasks WHERE state = ?1
             ORDER BY priority DESC, updated_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![st.as_str(), lim], row_to_task)?;
        for row in rows {
            tasks.push(row?);
        }
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, title, origin, state, priority, created_at, updated_at,
                    claimed_by_channel, claimed_by_id, blocked_reason, outcome, parent_task_id
             FROM tasks
             ORDER BY priority DESC, updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![lim], row_to_task)?;
        for row in rows {
            tasks.push(row?);
        }
    }

    Ok(tasks)
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        origin: TaskOrigin::from_str(&row.get::<_, String>(2)?).unwrap_or(TaskOrigin::Manual),
        state: TaskState::from_str(&row.get::<_, String>(3)?).unwrap_or(TaskState::Open),
        priority: row.get::<_, i64>(4)? as u8,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        claimed_by_channel: row.get(7)?,
        claimed_by_id: row.get(8)?,
        blocked_reason: row.get(9)?,
        outcome: row.get::<_, Option<String>>(10)?.and_then(|s| TaskOutcome::from_str(&s)),
        parent_task_id: row.get(11)?,
    })
}

/// Claim a task. Uses CAS: the caller provides the expected current `updated_at`
/// timestamp. If the task was modified since that read, the claim fails with
/// `ClaimConflict`.
pub fn claim_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    expected_updated_at: &str,
) -> Result<Task, TaskError> {
    transition(
        workspace_dir,
        task_id,
        Transition::Claim,
        actor,
        expected_updated_at,
        None,
        None,
    )
}

pub fn block_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    reason: &str,
) -> Result<Task, TaskError> {
    let task = get_task(workspace_dir, task_id)?;
    transition(
        workspace_dir,
        task_id,
        Transition::Block,
        actor,
        &task.updated_at,
        Some(reason),
        None,
    )
}

pub fn unblock_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
) -> Result<Task, TaskError> {
    let task = get_task(workspace_dir, task_id)?;
    transition(
        workspace_dir,
        task_id,
        Transition::Unblock,
        actor,
        &task.updated_at,
        None,
        None,
    )
}

pub fn complete_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    outcome: TaskOutcome,
) -> Result<Task, TaskError> {
    let task = get_task(workspace_dir, task_id)?;

    // No-self-complete invariant: the actor completing must NOT be the same
    // as the actor who claimed it, unless it was manually created.
    if task.origin != TaskOrigin::Manual {
        if let (Some(claim_ch), Some(claim_id)) =
            (&task.claimed_by_channel, &task.claimed_by_id)
        {
            if claim_ch == &actor.channel && actor.id.as_deref() == Some(claim_id.as_str()) {
                return Err(TaskError::InvalidTransition {
                    current_state: task.state,
                    transition: "complete (no-self-complete: claimer cannot complete their own task)"
                        .to_string(),
                });
            }
        }
    }

    transition(
        workspace_dir,
        task_id,
        Transition::Complete,
        actor,
        &task.updated_at,
        None,
        Some(outcome),
    )
}

pub fn abandon_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    reason: &str,
) -> Result<Task, TaskError> {
    if reason.trim().is_empty() {
        return Err(TaskError::AbandonRequiresReason);
    }
    let task = get_task(workspace_dir, task_id)?;
    transition(
        workspace_dir,
        task_id,
        Transition::Abandon,
        actor,
        &task.updated_at,
        Some(reason),
        None,
    )
}

pub fn get_task_activity(
    workspace_dir: &Path,
    task_id: &str,
    limit: usize,
) -> Result<Vec<TaskActivity>> {
    let conn = connect(workspace_dir)?;
    let lim = i64::try_from(limit.max(1)).unwrap_or(50);
    let mut stmt = conn.prepare(
        "SELECT id, task_id, old_state, new_state, actor_channel, actor_id, reason, timestamp
         FROM task_activity WHERE task_id = ?1
         ORDER BY id ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![task_id, lim], |row| {
        Ok(TaskActivity {
            id: row.get(0)?,
            task_id: row.get(1)?,
            old_state: row.get::<_, Option<String>>(2)?
                .and_then(|s| TaskState::from_str(&s)),
            new_state: TaskState::from_str(&row.get::<_, String>(3)?)
                .unwrap_or(TaskState::Open),
            actor_channel: row.get(4)?,
            actor_id: row.get(5)?,
            reason: row.get(6)?,
            timestamp: row.get(7)?,
        })
    })?;

    let mut activity = Vec::new();
    for row in rows {
        activity.push(row?);
    }
    Ok(activity)
}

fn transition(
    workspace_dir: &Path,
    task_id: &str,
    trans: Transition,
    actor: &TaskActor,
    expected_updated_at: &str,
    reason: Option<&str>,
    outcome: Option<TaskOutcome>,
) -> Result<Task, TaskError> {
    if trans == Transition::Abandon && reason.map_or(true, |r| r.trim().is_empty()) {
        return Err(TaskError::AbandonRequiresReason);
    }
    if trans == Transition::Complete && outcome.is_none() {
        return Err(TaskError::CompleteRequiresOutcome);
    }

    let conn = connect(workspace_dir)?;
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin transaction")?;

    let (current_state_str, actual_updated_at): (String, String) = tx
        .query_row(
            "SELECT state, updated_at FROM tasks WHERE id = ?1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskError::NotFound(task_id.to_string()),
            other => TaskError::Db(other.into()),
        })?;

    // CAS check
    if actual_updated_at != expected_updated_at {
        return Err(TaskError::ClaimConflict {
            expected: expected_updated_at.to_string(),
            found: actual_updated_at,
        });
    }

    let current_state = TaskState::from_str(&current_state_str).unwrap_or(TaskState::Open);

    if !trans.valid_from().contains(&current_state) {
        return Err(TaskError::InvalidTransition {
            current_state,
            transition: format!("{:?}", trans),
        });
    }

    let new_state = trans.target_state();
    let now = Utc::now().to_rfc3339();

    match trans {
        Transition::Claim => {
            tx.execute(
                "UPDATE tasks SET state = ?1, claimed_by_channel = ?2, claimed_by_id = ?3,
                 updated_at = ?4 WHERE id = ?5",
                params![new_state.as_str(), actor.channel, actor.id, now, task_id],
            )
            .context("Failed to claim task")?;
        }
        Transition::Block => {
            tx.execute(
                "UPDATE tasks SET state = ?1, blocked_reason = ?2, updated_at = ?3 WHERE id = ?4",
                params![new_state.as_str(), reason, now, task_id],
            )
            .context("Failed to block task")?;
        }
        Transition::Unblock => {
            tx.execute(
                "UPDATE tasks SET state = ?1, blocked_reason = NULL, updated_at = ?2 WHERE id = ?3",
                params![new_state.as_str(), now, task_id],
            )
            .context("Failed to unblock task")?;
        }
        Transition::Complete => {
            tx.execute(
                "UPDATE tasks SET state = ?1, outcome = ?2, updated_at = ?3 WHERE id = ?4",
                params![new_state.as_str(), outcome.map(|o| o.as_str()), now, task_id],
            )
            .context("Failed to complete task")?;
        }
        Transition::Abandon => {
            tx.execute(
                "UPDATE tasks SET state = ?1, outcome = 'cancelled', updated_at = ?2 WHERE id = ?3",
                params![new_state.as_str(), now, task_id],
            )
            .context("Failed to abandon task")?;
        }
    }

    record_activity_tx(&tx, task_id, Some(current_state), new_state, actor, reason)?;

    tx.commit().context("Failed to commit transition")?;

    get_task_inner(&conn, task_id)
}

fn record_activity(
    conn: &Connection,
    task_id: &str,
    old_state: Option<TaskState>,
    new_state: TaskState,
    actor: &TaskActor,
    reason: Option<&str>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO task_activity (task_id, old_state, new_state, actor_channel, actor_id, reason, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            task_id,
            old_state.map(|s| s.as_str()),
            new_state.as_str(),
            actor.channel,
            actor.id,
            reason,
            now,
        ],
    )
    .context("Failed to record task activity")?;
    Ok(())
}

fn record_activity_tx(
    tx: &rusqlite::Transaction<'_>,
    task_id: &str,
    old_state: Option<TaskState>,
    new_state: TaskState,
    actor: &TaskActor,
    reason: Option<&str>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO task_activity (task_id, old_state, new_state, actor_channel, actor_id, reason, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            task_id,
            old_state.map(|s| s.as_str()),
            new_state.as_str(),
            actor.channel,
            actor.id,
            reason,
            now,
        ],
    )
    .context("Failed to record task activity")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cli_actor() -> TaskActor {
        TaskActor {
            channel: "cli".to_string(),
            id: Some("richard".to_string()),
        }
    }

    fn other_actor() -> TaskActor {
        TaskActor {
            channel: "telegram".to_string(),
            id: Some("bot-42".to_string()),
        }
    }

    #[test]
    fn create_and_get_task() {
        let tmp = TempDir::new().unwrap();
        let task = create_task(
            tmp.path(),
            "Fix the widget",
            TaskOrigin::Manual,
            2,
            None,
            &cli_actor(),
        )
        .unwrap();

        assert_eq!(task.title, "Fix the widget");
        assert_eq!(task.state, TaskState::Open);
        assert_eq!(task.priority, 2);
        assert_eq!(task.origin, TaskOrigin::Manual);
        assert!(task.claimed_by_channel.is_none());

        let fetched = get_task(tmp.path(), &task.id).unwrap();
        assert_eq!(fetched.id, task.id);
    }

    #[test]
    fn create_task_records_activity() {
        let tmp = TempDir::new().unwrap();
        let task = create_task(
            tmp.path(),
            "Test activity",
            TaskOrigin::Manual,
            1,
            None,
            &cli_actor(),
        )
        .unwrap();

        let activity = get_task_activity(tmp.path(), &task.id, 10).unwrap();
        assert_eq!(activity.len(), 1);
        assert!(activity[0].old_state.is_none());
        assert_eq!(activity[0].new_state, TaskState::Open);
        assert_eq!(activity[0].actor_channel, "cli");
    }

    #[test]
    fn invalid_priority_rejected() {
        let tmp = TempDir::new().unwrap();
        let err = create_task(
            tmp.path(),
            "Bad priority",
            TaskOrigin::Manual,
            5,
            None,
            &cli_actor(),
        )
        .unwrap_err();
        assert!(matches!(err, TaskError::InvalidPriority(5)));
    }

    #[test]
    fn claim_task_with_cas() {
        let tmp = TempDir::new().unwrap();
        let task = create_task(
            tmp.path(),
            "Claim me",
            TaskOrigin::Manual,
            2,
            None,
            &cli_actor(),
        )
        .unwrap();

        let claimed = claim_task(
            tmp.path(),
            &task.id,
            &cli_actor(),
            &task.updated_at,
        )
        .unwrap();

        assert_eq!(claimed.state, TaskState::Claimed);
        assert_eq!(claimed.claimed_by_channel.as_deref(), Some("cli"));
        assert_eq!(claimed.claimed_by_id.as_deref(), Some("richard"));
    }

    #[test]
    fn claim_conflict_on_stale_version() {
        let tmp = TempDir::new().unwrap();
        let task = create_task(
            tmp.path(),
            "Race me",
            TaskOrigin::Manual,
            2,
            None,
            &cli_actor(),
        )
        .unwrap();

        claim_task(tmp.path(), &task.id, &cli_actor(), &task.updated_at).unwrap();

        let err = claim_task(
            tmp.path(),
            &task.id,
            &other_actor(),
            &task.updated_at,
        )
        .unwrap_err();
        assert!(matches!(err, TaskError::ClaimConflict { .. }));
    }

    #[test]
    fn full_lifecycle_open_claim_done() {
        let tmp = TempDir::new().unwrap();
        let actor_a = cli_actor();
        let actor_b = other_actor();

        let task = create_task(
            tmp.path(),
            "Lifecycle test",
            TaskOrigin::Heartbeat,
            3,
            None,
            &actor_a,
        )
        .unwrap();

        let claimed = claim_task(
            tmp.path(),
            &task.id,
            &actor_a,
            &task.updated_at,
        )
        .unwrap();
        assert_eq!(claimed.state, TaskState::Claimed);

        let done = complete_task(
            tmp.path(),
            &claimed.id,
            &actor_b,
            TaskOutcome::Succeeded,
        )
        .unwrap();
        assert_eq!(done.state, TaskState::Done);
        assert_eq!(done.outcome, Some(TaskOutcome::Succeeded));

        let activity = get_task_activity(tmp.path(), &task.id, 50).unwrap();
        assert_eq!(activity.len(), 3);
    }

    #[test]
    fn no_self_complete_enforced() {
        let tmp = TempDir::new().unwrap();
        let actor = cli_actor();

        let task = create_task(
            tmp.path(),
            "Self-complete test",
            TaskOrigin::Heartbeat,
            2,
            None,
            &actor,
        )
        .unwrap();

        let claimed = claim_task(
            tmp.path(),
            &task.id,
            &actor,
            &task.updated_at,
        )
        .unwrap();

        let err = complete_task(
            tmp.path(),
            &claimed.id,
            &actor,
            TaskOutcome::Succeeded,
        )
        .unwrap_err();
        assert!(
            matches!(err, TaskError::InvalidTransition { .. }),
            "expected InvalidTransition, got: {err:?}"
        );
    }

    #[test]
    fn self_complete_allowed_for_manual_origin() {
        let tmp = TempDir::new().unwrap();
        let actor = cli_actor();

        let task = create_task(
            tmp.path(),
            "Manual self-complete",
            TaskOrigin::Manual,
            2,
            None,
            &actor,
        )
        .unwrap();

        let claimed = claim_task(
            tmp.path(),
            &task.id,
            &actor,
            &task.updated_at,
        )
        .unwrap();

        let done = complete_task(
            tmp.path(),
            &claimed.id,
            &actor,
            TaskOutcome::Succeeded,
        )
        .unwrap();
        assert_eq!(done.state, TaskState::Done);
    }

    #[test]
    fn block_and_unblock() {
        let tmp = TempDir::new().unwrap();
        let actor = cli_actor();

        let task = create_task(
            tmp.path(),
            "Block test",
            TaskOrigin::Channel,
            2,
            None,
            &actor,
        )
        .unwrap();

        let claimed = claim_task(
            tmp.path(),
            &task.id,
            &actor,
            &task.updated_at,
        )
        .unwrap();

        let blocked = block_task(
            tmp.path(),
            &claimed.id,
            &actor,
            "waiting on external API",
        )
        .unwrap();
        assert_eq!(blocked.state, TaskState::Blocked);
        assert_eq!(
            blocked.blocked_reason.as_deref(),
            Some("waiting on external API")
        );

        let unblocked = unblock_task(tmp.path(), &blocked.id, &actor).unwrap();
        assert_eq!(unblocked.state, TaskState::Claimed);
        assert!(unblocked.blocked_reason.is_none());
    }

    #[test]
    fn abandon_requires_reason() {
        let tmp = TempDir::new().unwrap();
        let actor = cli_actor();

        let task = create_task(
            tmp.path(),
            "Abandon test",
            TaskOrigin::Manual,
            1,
            None,
            &actor,
        )
        .unwrap();

        let err = abandon_task(tmp.path(), &task.id, &actor, "").unwrap_err();
        assert!(matches!(err, TaskError::AbandonRequiresReason));

        let abandoned =
            abandon_task(tmp.path(), &task.id, &actor, "no longer needed").unwrap();
        assert_eq!(abandoned.state, TaskState::Abandoned);
    }

    #[test]
    fn cannot_transition_from_terminal() {
        let tmp = TempDir::new().unwrap();
        let actor_a = cli_actor();
        let actor_b = other_actor();

        let task = create_task(
            tmp.path(),
            "Terminal test",
            TaskOrigin::Heartbeat,
            2,
            None,
            &actor_a,
        )
        .unwrap();

        let claimed = claim_task(
            tmp.path(),
            &task.id,
            &actor_a,
            &task.updated_at,
        )
        .unwrap();

        let done = complete_task(
            tmp.path(),
            &claimed.id,
            &actor_b,
            TaskOutcome::Succeeded,
        )
        .unwrap();

        let err = claim_task(
            tmp.path(),
            &done.id,
            &actor_a,
            &done.updated_at,
        )
        .unwrap_err();
        assert!(matches!(err, TaskError::InvalidTransition { .. }));
    }

    #[test]
    fn list_tasks_with_filter() {
        let tmp = TempDir::new().unwrap();
        let actor = cli_actor();

        create_task(tmp.path(), "Task A", TaskOrigin::Manual, 1, None, &actor).unwrap();
        let b =
            create_task(tmp.path(), "Task B", TaskOrigin::Manual, 3, None, &actor).unwrap();
        create_task(tmp.path(), "Task C", TaskOrigin::Manual, 2, None, &actor).unwrap();

        claim_task(tmp.path(), &b.id, &actor, &b.updated_at).unwrap();

        let all = list_tasks(tmp.path(), None, 100).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].title, "Task B");

        let open_only = list_tasks(tmp.path(), Some(TaskState::Open), 100).unwrap();
        assert_eq!(open_only.len(), 2);

        let claimed_only = list_tasks(tmp.path(), Some(TaskState::Claimed), 100).unwrap();
        assert_eq!(claimed_only.len(), 1);
        assert_eq!(claimed_only[0].title, "Task B");
    }

    #[test]
    fn parent_task_reference() {
        let tmp = TempDir::new().unwrap();
        let actor = cli_actor();

        let parent = create_task(
            tmp.path(),
            "Parent task",
            TaskOrigin::Manual,
            3,
            None,
            &actor,
        )
        .unwrap();

        let child = create_task(
            tmp.path(),
            "Child task",
            TaskOrigin::Manual,
            2,
            Some(&parent.id),
            &actor,
        )
        .unwrap();

        assert_eq!(child.parent_task_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn task_not_found() {
        let tmp = TempDir::new().unwrap();
        let _ = list_tasks(tmp.path(), None, 1);

        let err = get_task(tmp.path(), "nonexistent-id").unwrap_err();
        assert!(matches!(err, TaskError::NotFound(_)));
    }

    #[test]
    fn schema_migration_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let actor = cli_actor();

        create_task(tmp.path(), "First", TaskOrigin::Manual, 1, None, &actor).unwrap();
        create_task(tmp.path(), "Second", TaskOrigin::Manual, 1, None, &actor).unwrap();

        let tasks = list_tasks(tmp.path(), None, 100).unwrap();
        assert_eq!(tasks.len(), 2);

        let path = db_path(tmp.path());
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }
}
