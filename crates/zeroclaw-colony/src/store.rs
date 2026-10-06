//! Colony continuation and scoped conversations extend canonical tasks. They
//! create the new execution-position/conversation facts; lifecycle stays on tasks.
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

use crate::{ColonyApproval, ColonyGoalExecution, ColonyMessage};
use zeroclaw_runtime::control_plane::task_store_sqlite::{
    SqliteTaskStore, insert_goal_task_record, insert_task_record, update_task_status_record,
};
use zeroclaw_runtime::control_plane::{GoalTaskRecord, TaskRecord, TaskStatus};

pub(super) fn migrate_schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS colony_messages (
            id TEXT PRIMARY KEY,
            colony_id TEXT NOT NULL,
            task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
            sender TEXT NOT NULL,
            recipient TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            in_reply_to TEXT REFERENCES colony_messages(id)
         );
         CREATE INDEX IF NOT EXISTS idx_colony_messages_scope
            ON colony_messages(colony_id, task_id, created_at);
         CREATE TABLE IF NOT EXISTS colony_inbox (
            message_id TEXT PRIMARY KEY REFERENCES colony_messages(id) ON DELETE CASCADE,
            goal_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            processed INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS colony_usage_gaps (
            goal_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
            diagnostic TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS colony_approvals (
            id TEXT PRIMARY KEY,
            goal_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            approval_json TEXT NOT NULL
         );",
    )
    .context("create colony task extensions")?;
    let has_reply = conn
        .prepare("PRAGMA table_info(colony_messages)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "in_reply_to");
    if !has_reply {
        conn.execute("ALTER TABLE colony_messages ADD COLUMN in_reply_to TEXT REFERENCES colony_messages(id)", [])?;
    }
    Ok(())
}

fn insert_message(conn: &rusqlite::Connection, message: &ColonyMessage) -> Result<()> {
    conn.execute(
        "INSERT INTO colony_messages(id,colony_id,task_id,sender,recipient,content,created_at,in_reply_to)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            message.id,
            message.colony_id,
            message.task_id,
            message.sender,
            message.recipient,
            message.content,
            message.created_at,
            message.in_reply_to
        ],
    )
    .context("persist colony conversation message")?;
    Ok(())
}

pub struct ColonyStore {
    inner: SqliteTaskStore,
}
impl std::ops::Deref for ColonyStore {
    type Target = SqliteTaskStore;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl ColonyStore {
    /// Missing/incomplete usage is a new confidence fact: the cost ledger
    /// cannot infer an unreported billable attempt from its recorded totals.
    pub fn mark_usage_gap(&self, goal_id: &str, diagnostic: &str) -> Result<()> {
        self.inner.extension_connection().execute("INSERT INTO colony_usage_gaps(goal_id,diagnostic) VALUES (?1,?2) ON CONFLICT(goal_id) DO UPDATE SET diagnostic=excluded.diagnostic",params![goal_id,diagnostic])?;
        Ok(())
    }
    pub fn usage_gap(&self, goal_id: &str) -> Result<Option<String>> {
        self.inner
            .extension_connection()
            .query_row(
                "SELECT diagnostic FROM colony_usage_gaps WHERE goal_id=?1",
                [goal_id],
                |row| row.get(0),
            )
            .optional()
            .context("read durable goal usage uncertainty")
    }

    pub fn append_approval(&self, approval: &ColonyApproval) -> Result<()> {
        self.inner.extension_connection().execute(
            "INSERT INTO colony_approvals(id,goal_id,approval_json)
            VALUES (?1,?2,?3)",
            params![
                approval.id,
                approval.goal_id,
                serde_json::to_string(approval)?
            ],
        )?;
        Ok(())
    }
    pub fn approvals(&self, goal_id: &str) -> Result<Vec<ColonyApproval>> {
        let conn = self.inner.extension_connection();
        let mut stmt = conn.prepare(
            "SELECT approval_json FROM colony_approvals WHERE goal_id=?1 ORDER BY rowid",
        )?;
        let raws = stmt
            .query_map([goal_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raws.into_iter()
            .map(|s| serde_json::from_str(&s).context("decode colony approval"))
            .collect()
    }
    pub fn decide_approval(&self, goal_id: &str, id: &str, approved: bool) -> Result<()> {
        let mut conn = self.inner.extension_connection();
        let tx = conn.transaction()?;
        let raw: String = tx.query_row(
            "SELECT approval_json FROM colony_approvals WHERE goal_id=?1 AND id=?2",
            params![goal_id, id],
            |r| r.get(0),
        )?;
        let mut approval: ColonyApproval = serde_json::from_str(&raw)?;
        anyhow::ensure!(approval.decision.is_none(), "approval already answered");
        approval.decision = Some(approved);
        tx.execute(
            "UPDATE colony_approvals SET approval_json=?2 WHERE id=?1",
            params![id, serde_json::to_string(&approval)?],
        )?;
        tx.commit().context("persist tool approval decision")
    }
    pub fn open(data_dir: &std::path::Path) -> Result<Self> {
        let inner = SqliteTaskStore::new(data_dir)?;
        inner.read_extension(migrate_schema)?;
        Ok(Self { inner })
    }

    pub fn colony_task_status(&self, id: &str) -> Result<TaskStatus> {
        let status: String = self.inner.extension_connection().query_row(
            "SELECT status FROM tasks WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        serde_json::from_value(serde_json::Value::String(status)).context("decode task status")
    }

    pub fn finish_colony_goal(&self, id: &str, message_id: &str) -> Result<bool> {
        let mut conn = self.inner.extension_connection();
        let tx = conn.transaction()?;
        let (status,pending):(String,bool)=tx.query_row(
            "SELECT status,EXISTS(SELECT 1 FROM colony_inbox WHERE goal_id=?1 AND processed=0) FROM tasks WHERE id=?1",
            [id],|row|Ok((row.get(0)?,row.get(1)?)))?;
        if status != "running" || pending {
            return Ok(false);
        }
        let completed = update_task_status_record(
            &tx,
            id,
            TaskStatus::Completed,
            Some(format!("colony-message:{message_id}")),
            None,
        )? == 1;
        tx.commit().context("complete canonical colony goal")?;
        Ok(completed)
    }

    pub fn reconcile_colony_turn(&self, id: &str, retry: bool) -> Result<()> {
        let mut conn = self.inner.extension_connection();
        let tx = conn.transaction()?;
        let status: String = tx.query_row("SELECT status FROM tasks WHERE id=?1", [id], |row| {
            row.get(0)
        })?;
        anyhow::ensure!(status == "paused", "goal must be paused");
        let raw: String = tx.query_row(
            "SELECT execution_json FROM task_execution_continuations WHERE task_id=?1",
            [id],
            |row| row.get(0),
        )?;
        let mut execution: ColonyGoalExecution = serde_json::from_str(&raw)?;
        let child = execution
            .active_child_id
            .take()
            .context("no interrupted turn to reconcile")?;
        update_task_status_record(
            &tx,
            &child,
            TaskStatus::Cancelled,
            None,
            Some(
                if retry {
                    "User acknowledged retry of uncertain outcome"
                } else {
                    "User skipped uncertain outcome"
                }
                .to_string(),
            ),
        )?;
        if let Some(inbox) = execution.active_inbox_id.take() {
            if !retry {
                tx.execute(
                    "UPDATE colony_inbox SET processed=1 WHERE message_id=?1",
                    [inbox],
                )?;
            }
        } else if !retry && !execution.summarizing {
            execution.next_assignment += 1;
        }
        tx.execute(
            "UPDATE task_execution_continuations SET execution_json=?2 WHERE task_id=?1",
            params![id, serde_json::to_string(&execution)?],
        )?;
        tx.commit()
            .context("commit interrupted colony turn reconciliation")
    }
    pub fn accept_pending_plan(&self, id: &str) -> Result<()> {
        let mut conn = self.inner.extension_connection();
        let tx = conn.transaction()?;
        let status: String =
            tx.query_row("SELECT status FROM tasks WHERE id=?1", [id], |r| r.get(0))?;
        anyhow::ensure!(status == "paused", "goal is no longer paused");
        let raw: String = tx.query_row(
            "SELECT execution_json FROM task_execution_continuations WHERE task_id=?1",
            [id],
            |r| r.get(0),
        )?;
        let mut execution: ColonyGoalExecution = serde_json::from_str(&raw)?;
        anyhow::ensure!(execution.active_child_id.is_none(), "turn has not settled");
        let plan = execution.pending_plan.take().context("no pending plan")?;
        anyhow::ensure!(
            plan.questions.is_empty() && execution.plan_rounds < 8,
            "plan needs further review"
        );
        anyhow::ensure!(
            execution.proposal.assignments.len() + plan.assignments.len() <= 64,
            "too many goal assignments"
        );
        execution.proposal.assignments.extend(plan.assignments);
        for agent in plan.new_agents {
            if !execution
                .proposal
                .new_agents
                .iter()
                .any(|a| a.alias == agent.alias)
            {
                execution.proposal.new_agents.push(agent);
            }
        }
        anyhow::ensure!(
            execution.proposal.new_agents.len() <= 8,
            "too many goal specialists"
        );
        execution.plan_rounds += 1;
        execution.summary_message_id = None;
        tx.execute(
            "UPDATE task_execution_continuations SET execution_json=?2 WHERE task_id=?1",
            params![id, serde_json::to_string(&execution)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn colony_goal_limits(&self, id: &str) -> Result<(Option<u64>, Option<f64>)> {
        let conn = self.inner.extension_connection();
        let (tokens,cost):(Option<i64>,Option<f64>)=conn.query_row(
            "SELECT effective_token_limit,effective_cost_limit_usd FROM goal_tasks WHERE task_id=?1",
            [id],|row|Ok((row.get(0)?,row.get(1)?)))?;
        Ok((tokens.map(u64::try_from).transpose()?, cost))
    }
    pub fn create_colony_goal(
        &self,
        task: TaskRecord,
        goal: GoalTaskRecord,
        execution: &ColonyGoalExecution,
    ) -> Result<()> {
        anyhow::ensure!(
            task.id == goal.task_id && task.id == execution.task_id,
            "colony goal identities differ"
        );
        let mut conn = self.inner.extension_connection();
        let tx = conn.transaction().context("begin colony goal admission")?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_execution_continuations c JOIN tasks t ON t.id=c.task_id
             WHERE c.owner_key=?1 AND t.status IN ('running','paused'))",
            [&execution.colony_id], |row| row.get(0),
        )?;
        anyhow::ensure!(!exists, "colony already has an active goal");
        insert_task_record(&tx, task)?;
        insert_goal_task_record(&tx, goal)?;
        tx.execute(
            "INSERT INTO task_execution_continuations(task_id,owner_key,execution_json)
            VALUES (?1,?2,?3)",
            params![
                execution.task_id,
                execution.colony_id,
                serde_json::to_string(execution)?
            ],
        )?;
        tx.commit().context("commit colony goal admission")
    }

    pub fn colony_execution(&self, id: &str) -> Result<Option<ColonyGoalExecution>> {
        let conn = self.inner.extension_connection();
        let value: Option<String> = conn
            .query_row(
                "SELECT execution_json FROM task_execution_continuations WHERE task_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| serde_json::from_str(&value).context("decode colony checkpoint"))
            .transpose()
    }

    pub fn colony_goal_ids(&self, colony_id: &str) -> Result<Vec<String>> {
        let conn = self.inner.extension_connection();
        let mut stmt = conn.prepare(
            "SELECT c.task_id FROM task_execution_continuations c
            JOIN tasks t ON t.id=c.task_id WHERE c.owner_key=?1 ORDER BY t.started_at DESC",
        )?;
        let values = stmt
            .query_map([colony_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(values)
    }

    pub fn begin_colony_turn(
        &self,
        child: TaskRecord,
        execution: &ColonyGoalExecution,
    ) -> Result<()> {
        let mut conn = self.inner.extension_connection();
        let tx = conn.transaction()?;
        let status: String = tx.query_row(
            "SELECT status FROM tasks WHERE id=?1",
            [&execution.task_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(status == "running", "colony goal is no longer running");
        let before: String = tx.query_row(
            "SELECT execution_json FROM task_execution_continuations
            WHERE task_id=?1",
            [&execution.task_id],
            |row| row.get(0),
        )?;
        let before: ColonyGoalExecution = serde_json::from_str(&before)?;
        anyhow::ensure!(
            before.active_child_id.is_none(),
            "colony turn outcome needs review"
        );
        anyhow::ensure!(
            execution.active_child_id.as_deref() == Some(child.id.as_str()),
            "colony child does not match checkpoint"
        );
        insert_task_record(&tx, child)?;
        tx.execute(
            "UPDATE task_execution_continuations SET execution_json=?2 WHERE task_id=?1",
            params![execution.task_id, serde_json::to_string(execution)?],
        )?;
        tx.commit().context("commit colony turn admission")
    }

    /// Commit turn output, child settlement, and the next safe checkpoint in
    /// one transaction. A competing goal cancellation cannot publish a result.
    pub fn finish_colony_turn(
        &self,
        child_id: &str,
        execution: &ColonyGoalExecution,
        message: &ColonyMessage,
    ) -> Result<bool> {
        let mut conn = self.inner.extension_connection();
        let tx = conn.transaction()?;
        let status: String = tx.query_row(
            "SELECT status FROM tasks WHERE id=?1",
            [&execution.task_id],
            |row| row.get(0),
        )?;
        if status != "running" && status != "paused" {
            return Ok(false);
        }
        let before: String = tx.query_row(
            "SELECT execution_json FROM task_execution_continuations
            WHERE task_id=?1",
            [&execution.task_id],
            |row| row.get(0),
        )?;
        let before: ColonyGoalExecution = serde_json::from_str(&before)?;
        anyhow::ensure!(
            before.active_child_id.as_deref() == Some(child_id),
            "colony child changed before settlement"
        );
        let won = update_task_status_record(
            &tx,
            child_id,
            TaskStatus::Completed,
            Some(format!("colony-message:{}", message.id)),
            None,
        )?;
        if won == 0 {
            return Ok(false);
        }
        insert_message(&tx, message)?;
        tx.execute(
            "UPDATE task_execution_continuations SET execution_json=?2 WHERE task_id=?1",
            params![execution.task_id, serde_json::to_string(execution)?],
        )?;
        tx.commit().context("commit colony turn settlement")?;
        Ok(true)
    }

    pub fn append_live_colony_message(&self, message: &ColonyMessage) -> Result<()> {
        let mut conn = self.inner.extension_connection();
        let tx = conn.transaction()?;
        if let Some(task) = &message.task_id {
            let status: String =
                tx.query_row("SELECT status FROM tasks WHERE id=?1", [task], |r| r.get(0))?;
            anyhow::ensure!(
                status == "running" || status == "paused",
                "colony goal has ended"
            );
        }
        insert_message(&tx, message)?;
        tx.commit()?;
        Ok(())
    }

    pub fn append_colony_message(&self, message: &ColonyMessage) -> Result<()> {
        insert_message(&self.inner.extension_connection(), message)
    }

    /// Human ingress and its dispatch intent are one transaction. A terminal
    /// goal refuses the insert so the caller can retry against current ownership.
    pub fn queue_colony_message(&self, message: &ColonyMessage, goal_id: &str) -> Result<bool> {
        let mut conn = self.inner.extension_connection();
        let tx = conn.transaction()?;
        let status: String =
            tx.query_row("SELECT status FROM tasks WHERE id=?1", [goal_id], |r| {
                r.get(0)
            })?;
        if status != "running" && status != "paused" {
            return Ok(false);
        }
        insert_message(&tx, message)?;
        tx.execute(
            "INSERT INTO colony_inbox(message_id,goal_id) VALUES (?1,?2)",
            params![message.id, goal_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn attach_colony_inbox(&self, message_id: &str, goal_id: &str) -> Result<()> {
        let mut conn = self.inner.extension_connection();
        let tx = conn.transaction()?;
        let status: String =
            tx.query_row("SELECT status FROM tasks WHERE id=?1", [goal_id], |r| {
                r.get(0)
            })?;
        anyhow::ensure!(
            status == "running" || status == "paused",
            "goal is no longer active"
        );
        tx.execute(
            "UPDATE colony_messages SET task_id=?2 WHERE id=?1 AND task_id IS NULL",
            params![message_id, goal_id],
        )?;
        tx.execute(
            "INSERT INTO colony_inbox(message_id,goal_id) VALUES (?1,?2)",
            params![message_id, goal_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn pending_inbox(&self, goal_id: &str) -> Result<Option<String>> {
        self.inner.extension_connection().query_row(
            "SELECT message_id FROM colony_inbox WHERE goal_id=?1 AND processed=0 ORDER BY rowid LIMIT 1",
            [goal_id], |r|r.get(0)).optional().context("read colony inbox")
    }
    pub fn settle_inbox(&self, message_id: &str) -> Result<()> {
        self.inner.extension_connection().execute(
            "UPDATE colony_inbox SET processed=1 WHERE message_id=?1",
            [message_id],
        )?;
        Ok(())
    }

    /// Exact goal scope, including NULL for standalone conversations. The
    /// SQL limit bounds materialization before old messages enter memory.
    pub fn colony_room_messages(
        &self,
        colony_id: &str,
        task_id: Option<&str>,
        recipient: &str,
        limit: u32,
    ) -> Result<Vec<ColonyMessage>> {
        let conn = self.inner.extension_connection();
        let mut stmt = conn.prepare("SELECT id,colony_id,task_id,sender,recipient,content,created_at,in_reply_to FROM colony_messages WHERE colony_id=?1 AND task_id IS ?2 AND recipient=?3 ORDER BY created_at DESC,rowid DESC LIMIT ?4")?;
        let mut messages = stmt
            .query_map(
                params![colony_id, task_id, recipient, limit.clamp(1, 100)],
                |row| {
                    Ok(ColonyMessage {
                        id: row.get(0)?,
                        colony_id: row.get(1)?,
                        task_id: row.get(2)?,
                        sender: row.get(3)?,
                        recipient: row.get(4)?,
                        content: row.get(5)?,
                        created_at: row.get(6)?,
                        in_reply_to: row.get(7)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        messages.reverse();
        Ok(messages)
    }

    pub fn colony_messages(
        &self,
        colony_id: &str,
        task_id: Option<&str>,
        recipient: Option<&str>,
    ) -> Result<Vec<ColonyMessage>> {
        let conn = self.inner.extension_connection();
        let mut stmt = conn.prepare(
            "SELECT id,colony_id,task_id,sender,recipient,content,created_at,in_reply_to
            FROM colony_messages WHERE colony_id=?1 AND (?2 IS NULL OR task_id=?2)
            AND (?3 IS NULL OR recipient=?3) ORDER BY created_at,rowid",
        )?;
        let rows = stmt
            .query_map(params![colony_id, task_id, recipient], |row| {
                Ok(ColonyMessage {
                    id: row.get(0)?,
                    colony_id: row.get(1)?,
                    task_id: row.get(2)?,
                    sender: row.get(3)?,
                    recipient: row.get(4)?,
                    content: row.get(5)?,
                    created_at: row.get(6)?,
                    in_reply_to: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
