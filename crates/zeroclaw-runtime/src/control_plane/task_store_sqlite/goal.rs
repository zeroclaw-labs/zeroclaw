//! SQLite-backed [`GoalTaskRegistry`] implementation.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::control_plane::goal_task::{
    GoalBlocker, GoalPauseReason, GoalPauseState, GoalTaskRecord, GoalTaskRegistry,
    TaskContinuationContext,
};
use crate::control_plane::task_registry::{TaskKind, TaskRecord, TaskStatus};

use super::{
    SqliteTaskStore, add_column_if_missing, insert_task_record, log_unreadable_task_row,
    row_to_record, status_to_db,
};

pub(super) fn migrate_schema(conn: &Connection, version: i64) -> Result<()> {
    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS goal_tasks (
                 task_id        TEXT PRIMARY KEY
                                REFERENCES tasks(id) ON DELETE CASCADE,
                 objective      TEXT NOT NULL
             );
             PRAGMA user_version = 1;",
        )
        .context("apply control-plane schema v1")?;
    }
    if version < 2 {
        add_column_if_missing(
            conn,
            "goal_tasks",
            "effective_token_limit",
            "ALTER TABLE goal_tasks ADD COLUMN effective_token_limit INTEGER",
        )?;
        add_column_if_missing(
            conn,
            "goal_tasks",
            "effective_cost_limit_usd",
            "ALTER TABLE goal_tasks ADD COLUMN effective_cost_limit_usd REAL",
        )?;
        conn.execute_batch("PRAGMA user_version = 2;")
            .context("mark control-plane schema v2")?;
    }
    if version < 3 {
        add_column_if_missing(
            conn,
            "goal_tasks",
            "pause_reason",
            "ALTER TABLE goal_tasks ADD COLUMN pause_reason TEXT",
        )?;
        add_column_if_missing(
            conn,
            "goal_tasks",
            "pause_description",
            "ALTER TABLE goal_tasks ADD COLUMN pause_description TEXT",
        )?;
        add_column_if_missing(
            conn,
            "goal_tasks",
            "blockers_json",
            "ALTER TABLE goal_tasks ADD COLUMN blockers_json TEXT NOT NULL DEFAULT '[]'",
        )?;
        conn.execute_batch("PRAGMA user_version = 3;")
            .context("mark control-plane schema v3")?;
    }
    if version < 4 {
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_active_goal_context
                ON tasks(
                    agent,
                    COALESCE(originator_route, ''),
                    COALESCE(principal_id, '')
                )
                WHERE kind = 'goal'
                  AND status NOT IN ('completed','failed','cancelled','lost','timed_out');
             PRAGMA user_version = 4;",
        )
        .context("apply control-plane schema v4")?;
    }
    if version < 5 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_continuation_contexts (
                 task_id      TEXT PRIMARY KEY
                              REFERENCES tasks(id) ON DELETE CASCADE,
                 context_json TEXT NOT NULL
             );
             PRAGMA user_version = 5;",
        )
        .context("apply control-plane schema v5")?;
    }
    if version < 6 {
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS trg_goal_tasks_require_goal_kind
                BEFORE INSERT ON goal_tasks
                FOR EACH ROW
                WHEN COALESCE((SELECT kind FROM tasks WHERE id = NEW.task_id), '') != 'goal'
                BEGIN
                    SELECT RAISE(ABORT, 'goal_tasks.task_id must reference TaskKind::Goal');
                END;
             PRAGMA user_version = 6;",
        )
        .context("apply control-plane schema v6")?;
    }
    if version < 7 {
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS trg_goal_tasks_effective_limits_insert
                BEFORE INSERT ON goal_tasks
                FOR EACH ROW
                WHEN (NEW.effective_token_limit IS NOT NULL AND NEW.effective_token_limit < 0)
                  OR (NEW.effective_cost_limit_usd IS NOT NULL
                      AND NOT (NEW.effective_cost_limit_usd >= 0.0
                               AND NEW.effective_cost_limit_usd <= 1.7976931348623157e308))
                BEGIN
                    SELECT RAISE(ABORT, 'goal effective limits must be non-negative finite values');
                END;
             CREATE TRIGGER IF NOT EXISTS trg_goal_tasks_effective_limits_update
                BEFORE UPDATE OF effective_token_limit, effective_cost_limit_usd ON goal_tasks
                FOR EACH ROW
                WHEN (NEW.effective_token_limit IS NOT NULL AND NEW.effective_token_limit < 0)
                  OR (NEW.effective_cost_limit_usd IS NOT NULL
                      AND NOT (NEW.effective_cost_limit_usd >= 0.0
                               AND NEW.effective_cost_limit_usd <= 1.7976931348623157e308))
                BEGIN
                    SELECT RAISE(ABORT, 'goal effective limits must be non-negative finite values');
                END;
             CREATE TRIGGER IF NOT EXISTS trg_task_continuation_contexts_require_goal_insert
                BEFORE INSERT ON task_continuation_contexts
                FOR EACH ROW
                WHEN NOT EXISTS (
                    SELECT 1
                      FROM tasks
                      JOIN goal_tasks ON goal_tasks.task_id = tasks.id
                     WHERE tasks.id = NEW.task_id
                       AND tasks.kind = 'goal'
                )
                BEGIN
                    SELECT RAISE(ABORT, 'task_continuation_contexts.task_id must reference a goal task');
                END;
             CREATE TRIGGER IF NOT EXISTS trg_task_continuation_contexts_require_goal_update
                BEFORE UPDATE OF task_id ON task_continuation_contexts
                FOR EACH ROW
                WHEN NOT EXISTS (
                    SELECT 1
                      FROM tasks
                      JOIN goal_tasks ON goal_tasks.task_id = tasks.id
                     WHERE tasks.id = NEW.task_id
                       AND tasks.kind = 'goal'
                )
                BEGIN
                    SELECT RAISE(ABORT, 'task_continuation_contexts.task_id must reference a goal task');
                END;
             PRAGMA user_version = 7;",
        )
        .context("apply control-plane schema v7")?;
    }
    Ok(())
}

fn ensure_goal_task_identity(task: &TaskRecord, goal: &GoalTaskRecord) -> Result<()> {
    if task.kind != TaskKind::Goal {
        anyhow::bail!("goal task {} must use TaskKind::Goal", task.id);
    }
    if task.id != goal.task_id {
        anyhow::bail!(
            "goal task id mismatch: TaskRecord id {} does not match GoalTaskRecord task_id {}",
            task.id,
            goal.task_id
        );
    }
    Ok(())
}

fn pause_reason_to_db(reason: GoalPauseReason) -> Result<String> {
    let value = serde_json::to_value(reason).context("serialize goal pause reason")?;
    value
        .as_str()
        .map(str::to_owned)
        .context("serialized goal pause reason is not a string")
}

fn pause_reason_from_db(value: Option<String>) -> Result<Option<GoalPauseReason>> {
    value
        .map(|value| {
            serde_json::from_value(serde_json::Value::String(value.clone()))
                .with_context(|| format!("unknown goal pause reason {value:?}"))
        })
        .transpose()
}

fn blockers_to_db(blockers: &[GoalBlocker]) -> Result<String> {
    serde_json::to_string(blockers).context("serialize goal blockers")
}

fn blockers_from_db(value: String) -> rusqlite::Result<Vec<GoalBlocker>> {
    serde_json::from_str(&value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })
}

fn continuation_context_to_db(context: &TaskContinuationContext) -> Result<String> {
    serde_json::to_string(context).context("serialize task continuation context")
}

fn continuation_context_from_db(value: String) -> rusqlite::Result<TaskContinuationContext> {
    serde_json::from_str(&value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })
}

fn token_limit_to_db(value: u64) -> Result<i64> {
    i64::try_from(value)
        .with_context(|| format!("effective token limit {value} exceeds SQLite INTEGER domain"))
}

fn token_limit_from_db(value: Option<i64>) -> rusqlite::Result<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Integer,
                    e.into(),
                )
            })
        })
        .transpose()
}

fn cost_limit_to_db(value: f64) -> Result<f64> {
    if !value.is_finite() || value < 0.0 {
        anyhow::bail!("effective cost limit must be a non-negative finite value");
    }
    Ok(value)
}

fn cost_limit_from_db(value: Option<f64>) -> rusqlite::Result<Option<f64>> {
    match value {
        Some(value) if !value.is_finite() || value < 0.0 => {
            Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Real,
                format!("invalid effective cost limit {value}").into(),
            ))
        }
        other => Ok(other),
    }
}

fn goal_limits_to_db(
    token_limit: Option<u64>,
    cost_limit_usd: Option<f64>,
) -> Result<(Option<i64>, Option<f64>)> {
    Ok((
        token_limit.map(token_limit_to_db).transpose()?,
        cost_limit_usd.map(cost_limit_to_db).transpose()?,
    ))
}

fn row_to_goal_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<GoalTaskRecord> {
    let pause_reason = pause_reason_from_db(row.get("pause_reason")?).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })?;
    Ok(GoalTaskRecord {
        task_id: row.get("task_id")?,
        objective: row.get("objective")?,
        effective_token_limit: token_limit_from_db(row.get("effective_token_limit")?)?,
        effective_cost_limit_usd: cost_limit_from_db(row.get("effective_cost_limit_usd")?)?,
        pause_reason,
        pause_description: row.get("pause_description")?,
        blockers: blockers_from_db(row.get("blockers_json")?)?,
    })
}

fn insert_goal_task_record(conn: &Connection, rec: GoalTaskRecord) -> Result<()> {
    let pause_reason = rec.pause_reason.map(pause_reason_to_db).transpose()?;
    let blockers_json = blockers_to_db(&rec.blockers)?;
    let (effective_token_limit, effective_cost_limit_usd) =
        goal_limits_to_db(rec.effective_token_limit, rec.effective_cost_limit_usd)?;
    conn.execute(
        "INSERT INTO goal_tasks
            (task_id, objective, effective_token_limit, effective_cost_limit_usd,
             pause_reason, pause_description, blockers_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(task_id) DO NOTHING",
        params![
            rec.task_id,
            rec.objective,
            effective_token_limit,
            effective_cost_limit_usd,
            pause_reason,
            rec.pause_description,
            blockers_json,
        ],
    )
    .context("insert goal task record")?;
    Ok(())
}

fn update_goal_pause_record(
    conn: &Connection,
    task_id: &str,
    pause: Option<GoalPauseState>,
) -> Result<usize> {
    let (reason, description, blockers_json) = match pause {
        Some(pause) => (
            Some(pause_reason_to_db(pause.reason)?),
            pause.description,
            blockers_to_db(&pause.blockers)?,
        ),
        None => (None, None, blockers_to_db(&[])?),
    };
    conn.execute(
        "UPDATE goal_tasks
            SET pause_reason = ?1,
                pause_description = ?2,
                blockers_json = ?3
          WHERE task_id = ?4",
        params![reason, description, blockers_json, task_id],
    )
    .context("update goal pause state")
}

fn ensure_goal_task_row(conn: &Connection, task_id: &str) -> Result<()> {
    let exists = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1
                   FROM tasks
                   JOIN goal_tasks ON goal_tasks.task_id = tasks.id
                  WHERE tasks.id = ?1
                    AND tasks.kind = 'goal'
             )",
            params![task_id],
            |row| row.get::<_, i64>(0),
        )
        .context("verify goal task row")?
        != 0;
    if !exists {
        anyhow::bail!("goal task {task_id} has no canonical TaskKind::Goal row and goal extension");
    }
    Ok(())
}

fn upsert_continuation_context(
    conn: &Connection,
    task_id: &str,
    context: &TaskContinuationContext,
) -> Result<()> {
    conn.execute(
        "INSERT INTO task_continuation_contexts (task_id, context_json)
         VALUES (?1, ?2)
         ON CONFLICT(task_id) DO UPDATE
             SET context_json = excluded.context_json",
        params![task_id, continuation_context_to_db(context)?],
    )
    .context("upsert task continuation context")?;
    Ok(())
}

#[async_trait::async_trait]
impl GoalTaskRegistry for SqliteTaskStore {
    async fn create_goal(
        &self,
        task: TaskRecord,
        goal: GoalTaskRecord,
        continuation_context: Option<TaskContinuationContext>,
    ) -> Result<()> {
        ensure_goal_task_identity(&task, &goal)?;
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .context("start create goal transaction")?;
        let task_id = goal.task_id.clone();
        insert_task_record(&tx, task)?;
        insert_goal_task_record(&tx, goal)?;
        if let Some(context) = continuation_context {
            upsert_continuation_context(&tx, &task_id, &context)?;
        }
        tx.commit().context("commit create goal transaction")?;
        Ok(())
    }

    async fn latest_active_goal_for_agent(&self, agent: &str) -> Result<Option<TaskRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT * FROM tasks
              WHERE agent = ?1
                AND kind = 'goal'
                AND status IN ('running','paused')
              ORDER BY started_at DESC, rowid DESC
              LIMIT 1",
            )
            .context("prepare latest active goal by agent")?;
        let rows = stmt
            .query_map(params![agent], row_to_record)
            .context("query latest active goal by agent")?;
        for row in rows {
            match row {
                Ok(task) => return Ok(Some(task)),
                Err(error) => log_unreadable_task_row(error),
            }
        }
        Ok(None)
    }

    async fn latest_active_goal_for_context(
        &self,
        agent: &str,
        originator_route: Option<&str>,
        principal_id: Option<&str>,
    ) -> Result<Option<TaskRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT * FROM tasks
              WHERE agent = ?1
                AND kind = 'goal'
                AND status IN ('running','paused')
                AND (?2 IS NULL OR originator_route = ?2)
                AND (?3 IS NULL OR principal_id = ?3)
              ORDER BY started_at DESC, rowid DESC
              LIMIT 1",
            )
            .context("prepare latest active goal by context")?;
        let rows = stmt
            .query_map(
                params![agent, originator_route, principal_id],
                row_to_record,
            )
            .context("query latest active goal by context")?;
        for row in rows {
            match row {
                Ok(task) => return Ok(Some(task)),
                Err(error) => log_unreadable_task_row(error),
            }
        }
        Ok(None)
    }

    async fn latest_active_goal_id_for_context(
        &self,
        agent: &str,
        originator_route: Option<&str>,
        principal_id: Option<&str>,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT id FROM tasks
                 WHERE agent = ?1
                   AND kind = 'goal'
                   AND status IN ('running','paused')
                   AND (?2 IS NULL OR originator_route = ?2)
                   AND (?3 IS NULL OR principal_id = ?3)
                 ORDER BY started_at DESC, rowid DESC
                 LIMIT 1",
            )
            .context("prepare latest active goal id by context")?;
        stmt.query_row(params![agent, originator_route, principal_id], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .context("query latest active goal id by context")
    }

    async fn rebind_goal_task_identity(
        &self,
        task_id: &str,
        expected_originator_route: &str,
        expected_principal_id: &str,
        originator_route: &str,
        principal_id: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock();
        let updated = conn
            .execute(
                "UPDATE tasks
                    SET originator_route = ?4,
                        principal_id = ?5
                  WHERE id = ?1
                    AND kind = 'goal'
                    AND originator_route = ?2
                    AND principal_id = ?3",
                params![
                    task_id,
                    expected_originator_route,
                    expected_principal_id,
                    originator_route,
                    principal_id,
                ],
            )
            .context("compare-and-set rebind of goal task identity")?;
        Ok(updated == 1)
    }

    async fn get_goal_task(&self, task_id: &str) -> Result<Option<GoalTaskRecord>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT task_id, objective, effective_token_limit, effective_cost_limit_usd,
                    pause_reason, pause_description, blockers_json
             FROM goal_tasks WHERE task_id = ?1",
            params![task_id],
            row_to_goal_task,
        )
        .optional()
        .context("get goal task")
    }

    async fn update_goal_objective(&self, task_id: &str, objective: &str) -> Result<()> {
        let conn = self.conn.lock();
        let updated = conn
            .execute(
                "UPDATE goal_tasks
                    SET objective = ?1
                  WHERE task_id = ?2",
                params![objective, task_id],
            )
            .context("update goal objective")?;
        if updated == 0 {
            anyhow::bail!("goal task {task_id} has no goal extension row");
        }
        Ok(())
    }

    async fn update_goal_limits(
        &self,
        task_id: &str,
        token_limit: Option<u64>,
        cost_limit_usd: Option<f64>,
    ) -> Result<()> {
        let (effective_token_limit, effective_cost_limit_usd) =
            goal_limits_to_db(token_limit, cost_limit_usd)?;
        let conn = self.conn.lock();
        let updated = conn
            .execute(
                "UPDATE goal_tasks
                SET effective_token_limit = ?1,
                    effective_cost_limit_usd = ?2
              WHERE task_id = ?3",
                params![effective_token_limit, effective_cost_limit_usd, task_id],
            )
            .context("update goal effective limits")?;
        if updated == 0 {
            anyhow::bail!("goal task {task_id} has no goal extension row");
        }
        Ok(())
    }

    async fn update_goal_pause(&self, task_id: &str, pause: Option<GoalPauseState>) -> Result<()> {
        let conn = self.conn.lock();
        let updated = update_goal_pause_record(&conn, task_id, pause)?;
        if updated == 0 {
            anyhow::bail!("goal task {task_id} has no goal extension row");
        }
        Ok(())
    }

    async fn pause_goal_task_if_status(
        &self,
        task_id: &str,
        expected_status: TaskStatus,
        pause: GoalPauseState,
    ) -> Result<bool> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .context("start pause goal task transaction")?;
        ensure_goal_task_row(&tx, task_id)?;
        let updated = tx
            .execute(
                "UPDATE tasks SET status = 'paused'
                   WHERE id = ?1 AND kind = 'goal' AND status = ?2",
                params![task_id, status_to_db(expected_status)],
            )
            .context("conditionally pause goal task")?;
        if updated == 0 {
            return Ok(false);
        }
        let updated = update_goal_pause_record(&tx, task_id, Some(pause))?;
        if updated == 0 {
            anyhow::bail!("goal task {task_id} has no goal extension row");
        }
        tx.commit().context("commit pause goal task transaction")?;
        Ok(true)
    }

    async fn cancel_goal_task_if_status(
        &self,
        task_id: &str,
        expected_status: TaskStatus,
        error: String,
    ) -> Result<bool> {
        let conn = self.conn.lock();
        ensure_goal_task_row(&conn, task_id)?;
        let updated = conn
            .execute(
                "UPDATE tasks
                    SET status = 'cancelled', error = COALESCE(?2, error),
                        finished_at = ?3
                  WHERE id = ?1 AND kind = 'goal' AND status = ?4",
                params![
                    task_id,
                    error,
                    chrono::Utc::now().to_rfc3339(),
                    status_to_db(expected_status),
                ],
            )
            .context("conditionally cancel goal task")?;
        Ok(updated == 1)
    }

    async fn complete_running_goal_task(&self, task_id: &str, output: String) -> Result<bool> {
        let conn = self.conn.lock();
        ensure_goal_task_row(&conn, task_id)?;
        let updated = conn
            .execute(
                "UPDATE tasks SET status = 'completed', output = COALESCE(?2, output), finished_at = ?3
             WHERE id = ?1 AND kind = 'goal' AND status = 'running'",
                params![task_id, output, chrono::Utc::now().to_rfc3339()],
            )
            .context("conditionally complete running goal task")?;
        Ok(updated == 1)
    }

    async fn complete_running_goal_task_if_limits(
        &self,
        task_id: &str,
        token_limit: Option<u64>,
        cost_limit_usd: Option<f64>,
        output: String,
    ) -> Result<bool> {
        let conn = self.conn.lock();
        ensure_goal_task_row(&conn, task_id)?;
        let token_limit = token_limit.map(i64::try_from).transpose()?;
        let updated = conn
            .execute(
                "UPDATE tasks SET status = 'completed', output = COALESCE(?4, output), finished_at = ?5
                 WHERE id = ?1 AND kind = 'goal' AND status = 'running'
                   AND EXISTS (SELECT 1 FROM goal_tasks
                       WHERE task_id = ?1
                         AND effective_token_limit IS ?2
                         AND effective_cost_limit_usd IS ?3)",
                params![task_id, token_limit, cost_limit_usd, output, chrono::Utc::now().to_rfc3339()],
            )
            .context("conditionally complete goal task with verified limits")?;
        Ok(updated == 1)
    }
    async fn resume_paused_goal_task(
        &self,
        task_id: &str,
        owner_pid: u32,
        owner_boot_id: &str,
        continuation_context: Option<TaskContinuationContext>,
    ) -> Result<bool> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .context("start resume goal task transaction")?;
        ensure_goal_task_row(&tx, task_id)?;
        let updated = tx
            .execute(
                "UPDATE tasks
                    SET status = 'running', owner_pid = ?2, owner_boot_id = ?3,
                        heartbeat_at = NULL
                  WHERE id = ?1 AND kind = 'goal' AND status = 'paused'",
                params![task_id, owner_pid as i64, owner_boot_id],
            )
            .context("conditionally resume paused goal task")?;
        if updated == 0 {
            return Ok(false);
        }
        if let Some(context) = continuation_context {
            upsert_continuation_context(&tx, task_id, &context)?;
        }
        let updated = update_goal_pause_record(&tx, task_id, None)?;
        if updated == 0 {
            anyhow::bail!("goal task {task_id} has no goal extension row");
        }
        tx.commit().context("commit resume goal task transaction")?;
        Ok(true)
    }

    async fn set_continuation_context(
        &self,
        task_id: &str,
        context: Option<TaskContinuationContext>,
    ) -> Result<()> {
        let conn = self.conn.lock();
        ensure_goal_task_row(&conn, task_id)?;
        match context {
            Some(context) => upsert_continuation_context(&conn, task_id, &context)?,
            None => {
                conn.execute(
                    "DELETE FROM task_continuation_contexts WHERE task_id = ?1",
                    params![task_id],
                )
                .context("delete task continuation context")?;
            }
        }
        Ok(())
    }

    async fn get_continuation_context(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskContinuationContext>> {
        let conn = self.conn.lock();
        ensure_goal_task_row(&conn, task_id)?;
        conn.query_row(
            "SELECT context_json
             FROM task_continuation_contexts WHERE task_id = ?1",
            params![task_id],
            |row| continuation_context_from_db(row.get("context_json")?),
        )
        .optional()
        .context("get task continuation context")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::goal_task::{GoalBlockerKind, GoalTaskRegistry};
    use crate::control_plane::task_registry::{TaskKind, TaskRegistry, TaskStatus};
    use rusqlite::params;

    fn rec(id: &str, agent: &str, owner_pid: u32, boot: &str) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            kind: TaskKind::Delegate,
            agent: agent.into(),
            status: TaskStatus::Running,
            owner_pid,
            owner_boot_id: boot.into(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: None,
            delivered: false,
            idem_key: None,
            principal_id: None,
            started_at: "2026-06-18T00:00:00Z".into(),
            finished_at: None,
        }
    }

    fn goal_record(task_id: &str, objective: &str) -> GoalTaskRecord {
        GoalTaskRecord {
            task_id: task_id.into(),
            objective: objective.into(),
            effective_token_limit: None,
            effective_cost_limit_usd: None,
            pause_reason: None,
            pause_description: None,
            blockers: Vec::new(),
        }
    }

    fn continuation_context() -> TaskContinuationContext {
        TaskContinuationContext {
            channel: "telegram".into(),
            channel_alias: Some("main".into()),
            reply_target: "chat-1".into(),
            sender: "alice".into(),
            thread_ts: None,
            interruption_scope_id: Some("scope-1".into()),
            conversation_scope:
                crate::control_plane::goal_task::TaskContinuationConversationScope::Sender,
        }
    }

    #[tokio::test]
    async fn latest_active_goal_for_agent_resolves_in_sql() {
        let s = SqliteTaskStore::new_in_memory().unwrap();

        let mut old_goal = rec("old-goal", "main", 1, "boot-1");
        old_goal.kind = TaskKind::Goal;
        old_goal.originator_route = Some("route-old".into());
        old_goal.started_at = "2026-06-18T00:00:00Z".into();
        s.create(old_goal).await.unwrap();

        let mut newer_terminal_goal = rec("done-goal", "main", 1, "boot-1");
        newer_terminal_goal.kind = TaskKind::Goal;
        newer_terminal_goal.started_at = "2026-06-20T00:00:00Z".into();
        s.create(newer_terminal_goal).await.unwrap();
        s.update_status("done-goal", TaskStatus::Completed, None, None)
            .await
            .unwrap();

        let mut newest_delegate = rec("newest-delegate", "main", 1, "boot-1");
        newest_delegate.started_at = "2026-06-21T00:00:00Z".into();
        s.create(newest_delegate).await.unwrap();

        let mut latest_active_goal = rec("latest-goal", "main", 1, "boot-1");
        latest_active_goal.kind = TaskKind::Goal;
        latest_active_goal.started_at = "2026-06-19T00:00:00Z".into();
        s.create(latest_active_goal).await.unwrap();

        let got = s
            .latest_active_goal_for_agent("main")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "latest-goal");
        assert!(
            s.latest_active_goal_for_agent("other")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn latest_active_goal_for_context_filters_route_and_principal() {
        let s = SqliteTaskStore::new_in_memory().unwrap();

        let mut other_route_goal = rec("other-route", "main", 1, "boot-1");
        other_route_goal.kind = TaskKind::Goal;
        other_route_goal.originator_route = Some("route-b".into());
        other_route_goal.principal_id = Some("principal-a".into());
        other_route_goal.started_at = "2026-06-20T00:00:00Z".into();
        s.create(other_route_goal).await.unwrap();

        let mut other_principal_goal = rec("other-principal", "main", 1, "boot-1");
        other_principal_goal.kind = TaskKind::Goal;
        other_principal_goal.originator_route = Some("route-a".into());
        other_principal_goal.principal_id = Some("principal-b".into());
        other_principal_goal.started_at = "2026-06-19T00:00:00Z".into();
        s.create(other_principal_goal).await.unwrap();

        let mut wanted_goal = rec("wanted", "main", 1, "boot-1");
        wanted_goal.kind = TaskKind::Goal;
        wanted_goal.originator_route = Some("route-a".into());
        wanted_goal.principal_id = Some("principal-a".into());
        wanted_goal.started_at = "2026-06-18T00:00:00Z".into();
        s.create(wanted_goal).await.unwrap();

        let got = s
            .latest_active_goal_for_context("main", Some("route-a"), Some("principal-a"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "wanted");
        let got_id = s
            .latest_active_goal_id_for_context("main", Some("route-a"), Some("principal-a"))
            .await
            .unwrap();
        assert_eq!(got_id.as_deref(), Some("wanted"));
    }

    #[tokio::test]
    async fn latest_active_goal_breaks_started_at_ties_by_insert_order() {
        let s = SqliteTaskStore::new_in_memory().unwrap();

        let mut first_goal = rec("first-goal", "main", 1, "boot-1");
        first_goal.kind = TaskKind::Goal;
        first_goal.originator_route = Some("route-a".into());
        first_goal.principal_id = Some("principal-a".into());
        first_goal.started_at = "2026-06-19T00:00:00Z".into();
        s.create(first_goal).await.unwrap();

        let mut second_goal = rec("second-goal", "main", 1, "boot-1");
        second_goal.kind = TaskKind::Goal;
        second_goal.originator_route = Some("route-b".into());
        second_goal.principal_id = Some("principal-b".into());
        second_goal.started_at = "2026-06-19T00:00:00Z".into();
        s.create(second_goal).await.unwrap();

        let got = s
            .latest_active_goal_for_agent("main")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "second-goal");

        let got = s
            .latest_active_goal_for_context("main", None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "second-goal");

        let got_id = s
            .latest_active_goal_id_for_context("main", None, None)
            .await
            .unwrap();
        assert_eq!(got_id.as_deref(), Some("second-goal"));
    }

    #[tokio::test]
    async fn latest_active_goal_for_context_ignores_unknown_status_candidate() {
        let s = SqliteTaskStore::new_in_memory().unwrap();

        let mut older_valid_goal = rec("older-valid-goal", "main", 1, "boot-1");
        older_valid_goal.kind = TaskKind::Goal;
        older_valid_goal.originator_route = Some("route-a".into());
        older_valid_goal.principal_id = Some("principal-a".into());
        older_valid_goal.started_at = "2026-06-19T00:00:00Z".into();
        s.create(older_valid_goal).await.unwrap();

        {
            let conn = s.conn.lock();
            conn.execute("DROP INDEX idx_tasks_active_goal_context", [])
                .unwrap();
        }

        let mut unreadable_newer_goal = rec("unreadable-newer-goal", "main", 1, "boot-1");
        unreadable_newer_goal.kind = TaskKind::Goal;
        unreadable_newer_goal.originator_route = Some("route-a".into());
        unreadable_newer_goal.principal_id = Some("principal-a".into());
        unreadable_newer_goal.started_at = "2026-06-20T00:00:00Z".into();
        s.create(unreadable_newer_goal).await.unwrap();
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE tasks SET status = 'future_paused' WHERE id = ?1",
                params!["unreadable-newer-goal"],
            )
            .unwrap();
        }

        let got = s
            .latest_active_goal_for_context("main", Some("route-a"), Some("principal-a"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "older-valid-goal");
        let got_id = s
            .latest_active_goal_id_for_context("main", Some("route-a"), Some("principal-a"))
            .await
            .unwrap();
        assert_eq!(got_id.as_deref(), Some("older-valid-goal"));
    }

    #[tokio::test]
    async fn latest_active_goal_for_agent_ignores_unknown_status_candidate() {
        let s = SqliteTaskStore::new_in_memory().unwrap();

        let mut older_valid_goal = rec("older-valid-goal", "main", 1, "boot-1");
        older_valid_goal.kind = TaskKind::Goal;
        older_valid_goal.originator_route = Some("route-valid".into());
        older_valid_goal.started_at = "2026-06-19T00:00:00Z".into();
        s.create(older_valid_goal).await.unwrap();

        let mut unreadable_newer_goal = rec("unreadable-newer-goal", "main", 1, "boot-1");
        unreadable_newer_goal.kind = TaskKind::Goal;
        unreadable_newer_goal.started_at = "2026-06-20T00:00:00Z".into();
        s.create(unreadable_newer_goal).await.unwrap();
        {
            let conn = s.conn.lock();
            conn.execute(
                "UPDATE tasks SET status = 'future_paused' WHERE id = ?1",
                params!["unreadable-newer-goal"],
            )
            .unwrap();
        }

        let got = s
            .latest_active_goal_for_agent("main")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "older-valid-goal");
    }

    #[tokio::test]
    async fn goal_task_extension_roundtrips_without_duplicating_lifecycle() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-1", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        task.status = TaskStatus::Paused;
        s.create_goal(
            task,
            GoalTaskRecord {
                task_id: "goal-1".into(),
                objective: "ship goal mode".into(),
                effective_token_limit: Some(10_000),
                effective_cost_limit_usd: Some(1.25),
                pause_reason: Some(GoalPauseReason::NeedsUserInput),
                pause_description: Some("waiting for operator".into()),
                blockers: vec![GoalBlocker {
                    kind: GoalBlockerKind::NeedsUserInput,
                    message: "Need operator answer".into(),
                    payload: Some(serde_json::json!({"question": "continue?"})),
                }],
            },
            None,
        )
        .await
        .unwrap();

        let task = s.get("goal-1").await.unwrap().unwrap();
        let goal = s.get_goal_task("goal-1").await.unwrap().unwrap();

        assert_eq!(task.kind, TaskKind::Goal);
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.task_id, task.id);
        assert_eq!(goal.objective, "ship goal mode");
        assert_eq!(goal.effective_token_limit, Some(10_000));
        assert_eq!(goal.effective_cost_limit_usd, Some(1.25));
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::NeedsUserInput));
        assert_eq!(
            goal.pause_description.as_deref(),
            Some("waiting for operator")
        );
        assert_eq!(goal.blockers.len(), 1);

        s.update_goal_limits("goal-1", Some(20_000), None)
            .await
            .unwrap();
        let limited = s.get_goal_task("goal-1").await.unwrap().unwrap();
        assert_eq!(limited.effective_token_limit, Some(20_000));
        assert_eq!(limited.effective_cost_limit_usd, None);
        assert_eq!(
            limited.objective, "ship goal mode",
            "budget updates must not duplicate or rewrite objective state"
        );

        s.update_goal_objective("goal-1", "ship amended goal mode")
            .await
            .unwrap();
        let amended = s.get_goal_task("goal-1").await.unwrap().unwrap();
        assert_eq!(amended.objective, "ship amended goal mode");

        s.update_goal_pause("goal-1", None).await.unwrap();
        let resumed = s.get_goal_task("goal-1").await.unwrap().unwrap();
        assert!(resumed.pause_reason.is_none());
        assert!(resumed.pause_description.is_none());
        assert!(resumed.blockers.is_empty());
    }

    #[tokio::test]
    async fn goal_task_requires_canonical_goal_task_record() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let err = {
            let conn = s.conn.lock();
            insert_goal_task_record(
                &conn,
                GoalTaskRecord {
                    task_id: "missing".into(),
                    objective: "orphan objective".into(),
                    effective_token_limit: None,
                    effective_cost_limit_usd: None,
                    pause_reason: None,
                    pause_description: None,
                    blockers: Vec::new(),
                },
            )
        }
        .unwrap_err();

        assert!(format!("{err:#}").contains("TaskKind::Goal"));
    }

    #[tokio::test]
    async fn goal_task_extension_rejects_non_goal_task_record() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("delegate-1", "main", 1, "boot-1"))
            .await
            .unwrap();

        let err = {
            let conn = s.conn.lock();
            insert_goal_task_record(
                &conn,
                GoalTaskRecord {
                    task_id: "delegate-1".into(),
                    objective: "must not attach to a delegate".into(),
                    effective_token_limit: None,
                    effective_cost_limit_usd: None,
                    pause_reason: None,
                    pause_description: None,
                    blockers: Vec::new(),
                },
            )
        }
        .unwrap_err();

        assert!(format!("{err:#}").contains("TaskKind::Goal"));
    }

    #[tokio::test]
    async fn create_goal_rejects_non_goal_task_record() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let task = rec("delegate-goal", "main", 1, "boot-1");

        let err = s
            .create_goal(
                task,
                GoalTaskRecord {
                    task_id: "delegate-goal".into(),
                    objective: "should fail before insert".into(),
                    effective_token_limit: None,
                    effective_cost_limit_usd: None,
                    pause_reason: None,
                    pause_description: None,
                    blockers: Vec::new(),
                },
                None,
            )
            .await
            .expect_err("non-goal task records must not create goal extensions");

        assert!(format!("{err:#}").contains("TaskKind::Goal"));
        assert!(
            s.get("delegate-goal").await.unwrap().is_none(),
            "pre-validation failure must not create a generic task row"
        );
    }

    #[tokio::test]
    async fn create_goal_rejects_mismatched_task_and_goal_ids() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-task", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;

        let err = s
            .create_goal(
                task,
                GoalTaskRecord {
                    task_id: "missing-extension-parent".into(),
                    objective: "should roll back".into(),
                    effective_token_limit: None,
                    effective_cost_limit_usd: None,
                    pause_reason: None,
                    pause_description: None,
                    blockers: Vec::new(),
                },
                None,
            )
            .await
            .expect_err("goal task id mismatch must be rejected before insert");

        assert!(format!("{err:#}").contains("id mismatch"));
        assert!(
            s.get("goal-task").await.unwrap().is_none(),
            "pre-validation failure must not create a generic task row"
        );
    }

    #[tokio::test]
    async fn update_goal_limits_rejects_invalid_effective_limits() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-limit-validation", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        s.create_goal(
            task,
            goal_record("goal-limit-validation", "validate limits"),
            None,
        )
        .await
        .unwrap();

        let err = s
            .update_goal_limits("goal-limit-validation", Some(i64::MAX as u64 + 1), None)
            .await
            .expect_err("token limit outside SQLite INTEGER domain must fail");
        assert!(format!("{err:#}").contains("SQLite INTEGER domain"));

        for invalid in [-1.0, f64::INFINITY, f64::NAN] {
            let err = s
                .update_goal_limits("goal-limit-validation", None, Some(invalid))
                .await
                .expect_err("invalid cost limit must fail before SQLite bind");
            assert!(format!("{err:#}").contains("non-negative finite"));
        }
    }

    #[tokio::test]
    async fn sqlite_rejects_invalid_effective_limit_rows() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-sqlite-limit-validation", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        s.create_goal(
            task,
            goal_record("goal-sqlite-limit-validation", "validate SQLite limits"),
            None,
        )
        .await
        .unwrap();

        let conn = s.conn.lock();
        let err = conn
            .execute(
                "UPDATE goal_tasks
                    SET effective_token_limit = -1
                  WHERE task_id = ?1",
                params!["goal-sqlite-limit-validation"],
            )
            .expect_err("SQLite must reject negative token limits");
        assert!(format!("{err:#}").contains("non-negative finite"));

        let err = conn
            .execute(
                "UPDATE goal_tasks
                    SET effective_cost_limit_usd = -0.01
                  WHERE task_id = ?1",
                params!["goal-sqlite-limit-validation"],
            )
            .expect_err("SQLite must reject negative cost limits");
        assert!(format!("{err:#}").contains("non-negative finite"));

        let err = conn
            .execute(
                "UPDATE goal_tasks
                    SET effective_cost_limit_usd = 1e999
                  WHERE task_id = ?1",
                params!["goal-sqlite-limit-validation"],
            )
            .expect_err("SQLite must reject non-finite cost limits");
        assert!(format!("{err:#}").contains("non-negative finite"));
    }

    #[tokio::test]
    async fn get_goal_task_rejects_legacy_negative_token_limit() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-corrupt-limit", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        s.create_goal(
            task,
            goal_record("goal-corrupt-limit", "reject legacy corrupt limit"),
            None,
        )
        .await
        .unwrap();

        {
            let conn = s.conn.lock();
            conn.execute("DROP TRIGGER trg_goal_tasks_effective_limits_update", [])
                .unwrap();
            conn.execute(
                "UPDATE goal_tasks
                    SET effective_token_limit = -1
                  WHERE task_id = ?1",
                params!["goal-corrupt-limit"],
            )
            .unwrap();
        }

        let err = s
            .get_goal_task("goal-corrupt-limit")
            .await
            .expect_err("legacy negative token limits must not inflate to u64");
        assert!(format!("{err:#}").contains("get goal task"));
    }

    #[tokio::test]
    async fn goal_extension_updates_reject_missing_goal_rows() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-without-extension", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        s.create(task).await.unwrap();

        let err = s
            .update_goal_limits("goal-without-extension", Some(100), None)
            .await
            .expect_err("limit updates must reject missing goal extension rows");
        assert!(format!("{err:#}").contains("goal extension"));

        let err = s
            .update_goal_pause(
                "goal-without-extension",
                Some(GoalPauseState {
                    reason: GoalPauseReason::NeedsUserInput,
                    description: None,
                    blockers: Vec::new(),
                }),
            )
            .await
            .expect_err("pause updates must reject missing goal extension rows");
        assert!(format!("{err:#}").contains("goal extension"));
    }

    #[tokio::test]
    async fn continuation_context_requires_goal_extension_row() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        s.create(rec("delegate-context", "main", 1, "boot-1"))
            .await
            .unwrap();

        let err = s
            .set_continuation_context("delegate-context", Some(continuation_context()))
            .await
            .expect_err("continuation context must reject non-goal tasks");
        assert!(format!("{err:#}").contains("goal extension"));

        let err = s
            .get_continuation_context("delegate-context")
            .await
            .expect_err("continuation context reads must reject non-goal tasks");
        assert!(format!("{err:#}").contains("goal extension"));

        let conn = s.conn.lock();
        let err = conn
            .execute(
                "INSERT INTO task_continuation_contexts (task_id, context_json)
                    VALUES (?1, ?2)",
                params![
                    "delegate-context",
                    continuation_context_to_db(&continuation_context()).unwrap()
                ],
            )
            .expect_err("SQLite must reject continuation contexts for non-goal tasks");
        assert!(format!("{err:#}").contains("goal task"));
    }

    #[tokio::test]
    async fn pause_goal_task_updates_status_and_pause_atomically() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-paused", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        s.create_goal(
            task,
            GoalTaskRecord {
                task_id: "goal-paused".into(),
                objective: "pause me".into(),
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

        assert!(
            s.pause_goal_task_if_status(
                "goal-paused",
                TaskStatus::Running,
                GoalPauseState {
                    reason: GoalPauseReason::NeedsUserInput,
                    description: Some("waiting".into()),
                    blockers: vec![GoalBlocker {
                        kind: GoalBlockerKind::NeedsUserInput,
                        message: "Need operator answer".into(),
                        payload: None,
                    }],
                },
            )
            .await
            .unwrap()
        );

        let task = s.get("goal-paused").await.unwrap().unwrap();
        let goal = s.get_goal_task("goal-paused").await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::NeedsUserInput));
        assert_eq!(goal.pause_description.as_deref(), Some("waiting"));
        assert_eq!(goal.blockers.len(), 1);
    }

    #[tokio::test]
    async fn pause_goal_task_roundtrips_operator_pause() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-operator-paused", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        s.create_goal(
            task,
            GoalTaskRecord {
                task_id: "goal-operator-paused".into(),
                objective: "pause from operator command".into(),
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

        assert!(
            s.pause_goal_task_if_status(
                "goal-operator-paused",
                TaskStatus::Running,
                GoalPauseState {
                    reason: GoalPauseReason::OperatorPaused,
                    description: Some("maintenance window".into()),
                    blockers: vec![GoalBlocker {
                        kind: GoalBlockerKind::OperatorPause,
                        message: "maintenance window".into(),
                        payload: None,
                    }],
                },
            )
            .await
            .unwrap()
        );

        let task = s.get("goal-operator-paused").await.unwrap().unwrap();
        let goal = s
            .get_goal_task("goal-operator-paused")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::OperatorPaused));
        assert_eq!(
            goal.pause_description.as_deref(),
            Some("maintenance window")
        );
        assert_eq!(goal.blockers[0].kind, GoalBlockerKind::OperatorPause);
        assert_eq!(goal.blockers[0].message, "maintenance window");
    }

    #[tokio::test]
    async fn pause_goal_task_rejects_goal_without_extension_before_status_change() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-missing-extension", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        s.create(task).await.unwrap();

        let err = s
            .pause_goal_task_if_status(
                "goal-missing-extension",
                TaskStatus::Running,
                GoalPauseState {
                    reason: GoalPauseReason::NeedsUserInput,
                    description: None,
                    blockers: Vec::new(),
                },
            )
            .await
            .expect_err("goal pause must require the goal extension row");

        assert!(format!("{err:#}").contains("goal extension"));
        let task = s.get("goal-missing-extension").await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Running);
    }

    #[tokio::test]
    async fn resume_goal_task_clears_pause_claims_owner_and_sets_running() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-resume", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        task.status = TaskStatus::Paused;
        s.create_goal(
            task,
            GoalTaskRecord {
                task_id: "goal-resume".into(),
                objective: "resume me".into(),
                effective_token_limit: None,
                effective_cost_limit_usd: None,
                pause_reason: Some(GoalPauseReason::NeedsUserInput),
                pause_description: Some("waiting".into()),
                blockers: vec![GoalBlocker {
                    kind: GoalBlockerKind::NeedsUserInput,
                    message: "Need operator answer".into(),
                    payload: None,
                }],
            },
            None,
        )
        .await
        .unwrap();

        assert!(
            s.resume_paused_goal_task("goal-resume", 42, "boot-2", None)
                .await
                .unwrap()
        );

        let task = s.get("goal-resume").await.unwrap().unwrap();
        let goal = s.get_goal_task("goal-resume").await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.owner_pid, 42);
        assert_eq!(task.owner_boot_id, "boot-2");
        assert!(goal.pause_reason.is_none());
        assert!(goal.pause_description.is_none());
        assert!(goal.blockers.is_empty());
    }

    #[tokio::test]
    async fn resume_paused_goal_task_rejects_a_duplicate_resume() {
        // The second controller must not publish a continuation after the first
        // one has already claimed the exact paused task and made it running.
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-duplicate-resume", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        task.status = TaskStatus::Paused;
        s.create_goal(
            task,
            GoalTaskRecord {
                task_id: "goal-duplicate-resume".into(),
                objective: "resume exactly once".into(),
                effective_token_limit: None,
                effective_cost_limit_usd: None,
                pause_reason: Some(GoalPauseReason::OperatorPaused),
                pause_description: None,
                blockers: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();

        assert!(
            s.resume_paused_goal_task("goal-duplicate-resume", 42, "boot-2", None)
                .await
                .unwrap()
        );
        assert!(
            !s.resume_paused_goal_task("goal-duplicate-resume", 43, "boot-3", None)
                .await
                .unwrap()
        );
        let task = s.get("goal-duplicate-resume").await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.owner_boot_id, "boot-2");
    }

    #[tokio::test]
    async fn stale_pause_and_cancel_do_not_overwrite_newer_goal_state() {
        // A controller acts on the status it resolved. Once another actor has
        // paused or completed the task, stale commands must leave both durable
        // lifecycle and pause payload untouched.
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-stale-cas", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        task.status = TaskStatus::Paused;
        s.create_goal(
            task,
            GoalTaskRecord {
                task_id: "goal-stale-cas".into(),
                objective: "preserve newer state".into(),
                effective_token_limit: None,
                effective_cost_limit_usd: None,
                pause_reason: Some(GoalPauseReason::OperatorPaused),
                pause_description: Some("newer operator pause".into()),
                blockers: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();

        assert!(
            !s.pause_goal_task_if_status(
                "goal-stale-cas",
                TaskStatus::Running,
                GoalPauseState {
                    reason: GoalPauseReason::BudgetExhausted,
                    description: Some("stale budget pause".into()),
                    blockers: Vec::new(),
                },
            )
            .await
            .unwrap()
        );
        assert!(
            !s.cancel_goal_task_if_status(
                "goal-stale-cas",
                TaskStatus::Running,
                "stale cancellation".into(),
            )
            .await
            .unwrap()
        );
        let task = s.get("goal-stale-cas").await.unwrap().unwrap();
        let goal = s.get_goal_task("goal-stale-cas").await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(
            goal.pause_description.as_deref(),
            Some("newer operator pause")
        );
    }

    #[tokio::test]
    async fn complete_running_goal_task_loses_to_paused_goal_without_output_write() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-completion-cas-loses", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        task.status = TaskStatus::Paused;
        s.create_goal(
            task,
            GoalTaskRecord {
                task_id: "goal-completion-cas-loses".into(),
                objective: "do not overwrite paused state".into(),
                effective_token_limit: None,
                effective_cost_limit_usd: None,
                pause_reason: Some(GoalPauseReason::OperatorPaused),
                pause_description: Some("operator won the race".into()),
                blockers: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();

        assert!(
            !s.complete_running_goal_task("goal-completion-cas-loses", "stale completion".into())
                .await
                .unwrap()
        );
        assert_eq!(
            s.get("goal-completion-cas-loses")
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Paused
        );
        let conn = s.conn.lock();
        let output: Option<String> = conn
            .query_row(
                "SELECT output FROM tasks WHERE id = ?1",
                params!["goal-completion-cas-loses"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(output.is_none());
    }

    #[tokio::test]
    async fn completion_with_verified_limits_loses_to_a_concurrent_budget_update() {
        let s = SqliteTaskStore::new_in_memory().unwrap();
        let mut task = rec("goal-limit-cas", "main", 1, "boot-1");
        task.kind = TaskKind::Goal;
        s.create_goal(
            task,
            GoalTaskRecord {
                task_id: "goal-limit-cas".into(),
                objective: "honor current policy".into(),
                effective_token_limit: Some(100),
                effective_cost_limit_usd: None,
                pause_reason: None,
                pause_description: None,
                blockers: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
        s.update_goal_limits("goal-limit-cas", Some(1), None)
            .await
            .unwrap();

        assert!(
            !s.complete_running_goal_task_if_limits(
                "goal-limit-cas",
                Some(100),
                None,
                "stale completion".into(),
            )
            .await
            .unwrap()
        );
        assert_eq!(
            s.get("goal-limit-cas").await.unwrap().unwrap().status,
            TaskStatus::Running
        );
    }
}
