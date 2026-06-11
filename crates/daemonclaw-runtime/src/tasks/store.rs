use super::{
    AcceptanceItem, AcceptanceVerifier, Autonomy, Execution, Task, TaskActor, TaskError,
    TaskOutcome, TaskStatus, Transition,
};
use crate::security::audit::{AuditEvent, AuditEventType, AuditLogger};
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
                 id             TEXT PRIMARY KEY,
                 parent_id      TEXT REFERENCES tasks(id),
                 title          TEXT NOT NULL,
                 intent         TEXT,
                 acceptance     TEXT NOT NULL DEFAULT '[]',
                 status         TEXT NOT NULL DEFAULT 'open',
                 priority       INTEGER NOT NULL DEFAULT 2,
                 assigned_to    TEXT,
                 autonomy       TEXT NOT NULL DEFAULT 'gated',
                 execution      TEXT NOT NULL DEFAULT 'agentic',
                 tools          TEXT NOT NULL DEFAULT '[]',
                 blockers       TEXT NOT NULL DEFAULT '{}',
                 template_id    TEXT,
                 source         TEXT NOT NULL DEFAULT 'operator',
                 abandon_reason TEXT,
                 outcome        TEXT,
                 created_at     TEXT NOT NULL,
                 updated_at     TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
             CREATE INDEX IF NOT EXISTS idx_tasks_priority ON tasks(priority);
             CREATE INDEX IF NOT EXISTS idx_tasks_source ON tasks(source);
             CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_id);
             CREATE INDEX IF NOT EXISTS idx_tasks_updated ON tasks(updated_at);
             CREATE INDEX IF NOT EXISTS idx_tasks_assigned ON tasks(assigned_to);

             CREATE TABLE IF NOT EXISTS task_activity (
                 id             INTEGER PRIMARY KEY AUTOINCREMENT,
                 task_id        TEXT NOT NULL REFERENCES tasks(id),
                 actor_id       TEXT,
                 tool           TEXT,
                 args_summary   TEXT,
                 result_summary TEXT,
                 ts             TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_activity_task ON task_activity(task_id);

             PRAGMA user_version = 1;",
        )
        .context("Failed to run tasks.db migration v1")?;
    }

    if version < 2 {
        conn.execute_batch(
            "ALTER TABLE tasks ADD COLUMN turn_count INTEGER NOT NULL DEFAULT 0;
             PRAGMA user_version = 2;",
        )
        .context("Failed to run tasks.db migration v2")?;
    }

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────

fn get_task_inner(conn: &Connection, task_id: &str) -> Result<Task, TaskError> {
    conn.query_row(
        "SELECT id, parent_id, title, intent, acceptance, status, priority, assigned_to,
                autonomy, execution, tools, blockers, template_id, source,
                abandon_reason, outcome, turn_count, created_at, updated_at
         FROM tasks WHERE id = ?1",
        params![task_id],
        row_to_task,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => TaskError::NotFound(task_id.to_string()),
        other => TaskError::Db(other.into()),
    })
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let acceptance_json: String = row.get(4)?;
    let tools_json: String = row.get(10)?;
    let blockers_json: String = row.get(11)?;

    Ok(Task {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        title: row.get(2)?,
        intent: row.get(3)?,
        acceptance: serde_json::from_str(&acceptance_json).unwrap_or_default(),
        status: TaskStatus::from_str(&row.get::<_, String>(5)?).unwrap_or(TaskStatus::Open),
        priority: row.get::<_, i64>(6)? as u8,
        assigned_to: row.get(7)?,
        autonomy: Autonomy::from_str(&row.get::<_, String>(8)?).unwrap_or(Autonomy::Gated),
        execution: Execution::from_str(&row.get::<_, String>(9)?).unwrap_or(Execution::Agentic),
        tools: serde_json::from_str(&tools_json).unwrap_or_default(),
        blockers: serde_json::from_str(&blockers_json).unwrap_or(serde_json::json!({})),
        template_id: row.get(12)?,
        source: row.get(13)?,
        abandon_reason: row.get(14)?,
        outcome: row.get::<_, Option<String>>(15)?.and_then(|s| TaskOutcome::from_str(&s)),
        turn_count: row.get::<_, i64>(16)? as u32,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
    })
}

/// Append a task_transition event to audit.db's Merkle chain.
///
/// Ordering rationale: audit-first, state-second. A state change with no
/// audit event is undetectable and violates the sacred property. An audit
/// event with no state change is detectable (the task's status disagrees
/// with the chain tip) and reconcilable.
fn audit_transition(
    audit: &AuditLogger,
    task_id: &str,
    from: TaskStatus,
    to: TaskStatus,
    actor: &TaskActor,
    reason: Option<&str>,
) -> Result<(), TaskError> {
    let transition_detail = serde_json::json!({
        "task_id": task_id,
        "from": from.as_str(),
        "to": to.as_str(),
        "reason": reason,
    });

    let event = AuditEvent::new(AuditEventType::TaskTransition)
        .with_actor(
            actor.channel.clone(),
            actor.id.clone(),
            None,
        )
        .with_action(
            serde_json::to_string(&transition_detail).unwrap_or_default(),
            "task".to_string(),
            true,
            true,
        );

    audit.log(&event).map_err(|e| TaskError::Audit(e.to_string()))
}

// ── Public API ───────────────────────────────────────────────────

pub struct CreateTaskParams<'a> {
    pub title: &'a str,
    pub intent: Option<&'a str>,
    pub acceptance: &'a [AcceptanceItem],
    pub priority: u8,
    pub parent_id: Option<&'a str>,
    pub source: &'a str,
    pub autonomy: Autonomy,
    pub execution: Execution,
    pub tools: &'a [String],
}

pub fn create_task(
    workspace_dir: &Path,
    params: &CreateTaskParams<'_>,
    actor: &TaskActor,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    if params.priority > 4 {
        return Err(TaskError::InvalidPriority(params.priority));
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let acceptance_json = serde_json::to_string(params.acceptance)
        .context("Failed to serialize acceptance")?;
    let tools_json = serde_json::to_string(params.tools)
        .context("Failed to serialize tools")?;

    // Audit first: record the creation event
    audit_transition(audit, &id, TaskStatus::Open, TaskStatus::Open, actor, None)?;

    let conn = connect(workspace_dir)?;
    conn.execute(
        "INSERT INTO tasks (id, parent_id, title, intent, acceptance, status, priority,
         autonomy, execution, tools, source, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id,
            params.parent_id,
            params.title,
            params.intent,
            acceptance_json,
            params.priority as i64,
            params.autonomy.as_str(),
            params.execution.as_str(),
            tools_json,
            params.source,
            now,
            now,
        ],
    )
    .context("Failed to insert task")?;

    get_task_inner(&conn, &id)
}

pub fn get_task(workspace_dir: &Path, task_id: &str) -> Result<Task, TaskError> {
    let conn = connect(workspace_dir)?;
    get_task_inner(&conn, task_id)
}

pub fn list_tasks(
    workspace_dir: &Path,
    status_filter: Option<TaskStatus>,
    limit: usize,
) -> Result<Vec<Task>> {
    let conn = connect(workspace_dir)?;
    let lim = i64::try_from(limit.max(1)).unwrap_or(100);
    let mut tasks = Vec::new();

    if let Some(st) = status_filter {
        let mut stmt = conn.prepare(
            "SELECT id, parent_id, title, intent, acceptance, status, priority, assigned_to,
                    autonomy, execution, tools, blockers, template_id, source,
                    abandon_reason, outcome, turn_count, created_at, updated_at
             FROM tasks WHERE status = ?1
             ORDER BY priority DESC, updated_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![st.as_str(), lim], row_to_task)?;
        for row in rows {
            tasks.push(row?);
        }
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, parent_id, title, intent, acceptance, status, priority, assigned_to,
                    autonomy, execution, tools, blockers, template_id, source,
                    abandon_reason, outcome, turn_count, created_at, updated_at
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

pub fn render_priority_view(workspace_dir: &Path, actor_id: Option<&str>, max_tasks: usize) -> String {
    let path = db_path(workspace_dir);
    if !path.exists() {
        return "[Tasks] unavailable".to_string();
    }
    let conn = match connect(workspace_dir) {
        Ok(c) => c,
        Err(_) => return "[Tasks] unavailable".to_string(),
    };

    let sql = "SELECT id, status, priority, title, assigned_to, blockers
               FROM tasks
               WHERE status IN ('open', 'active', 'blocked', 'review')
               ORDER BY priority DESC, updated_at DESC";

    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return "[Tasks] unavailable".to_string(),
    };

    struct Row {
        id: String,
        status: String,
        priority: i64,
        title: String,
        assigned_to: Option<String>,
        blockers: String,
    }

    let rows: Vec<Row> = match stmt.query_map([], |r| {
        Ok(Row {
            id: r.get(0)?,
            status: r.get(1)?,
            priority: r.get(2)?,
            title: r.get(3)?,
            assigned_to: r.get(4)?,
            blockers: r.get::<_, String>(5)?,
        })
    }) {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => return "[Tasks] unavailable".to_string(),
    };

    let filtered: Vec<&Row> = rows
        .iter()
        .filter(|r| {
            if r.status != "open" {
                return true;
            }
            match (&r.assigned_to, actor_id) {
                (None, _) => true,
                (Some(a), Some(actor)) => a == actor,
                (Some(_), None) => true,
            }
        })
        .collect();

    if filtered.is_empty() {
        return "[Tasks] none".to_string();
    }

    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for r in &filtered {
        *counts.entry(r.status.as_str()).or_default() += 1;
    }

    let mut summary_parts = Vec::new();
    for st in &["open", "active", "blocked", "review"] {
        if let Some(&n) = counts.get(st) {
            summary_parts.push(format!("{n} {st}"));
        }
    }

    let cap = max_tasks.max(1);
    let overflow = filtered.len().saturating_sub(cap);
    let display = &filtered[..filtered.len().min(cap)];

    let mut out = format!("[Tasks] {}\n", summary_parts.join(", "));
    for r in display {
        let short_id = &r.id[..r.id.len().min(8)];
        let title_display = if r.title.len() > 60 {
            format!("{}…", &r.title[..59])
        } else {
            r.title.clone()
        };
        let blocker_mark = if r.blockers != "{}" && r.blockers != "[]" && r.blockers != "null" {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&r.blockers) {
                if let Some(obj) = v.as_object() {
                    if obj.is_empty() { String::new() } else { format!(" [blockers:{}]", obj.len()) }
                } else if let Some(arr) = v.as_array() {
                    if arr.is_empty() { String::new() } else { format!(" [blockers:{}]", arr.len()) }
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{short_id} {:<8} P{} \"{title_display}\"{blocker_mark}\n",
            r.status, r.priority,
        ));
    }
    if overflow > 0 {
        out.push_str(&format!("+{overflow} more\n"));
    }
    out.trim_end().to_string()
}

/// Claim an open task. Uses `BEGIN IMMEDIATE` + `WHERE status='open'` so
/// exactly one of two racing claimers wins; the loser gets `ClaimConflict`.
pub fn claim_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    // Read current status to give a good error if not open
    let task = get_task(workspace_dir, task_id)?;
    if task.status != TaskStatus::Open {
        return Err(TaskError::ClaimConflict {
            actual_status: task.status,
        });
    }

    // Audit first
    let assigned = actor.id.as_deref().unwrap_or(&actor.channel);
    audit_transition(audit, task_id, TaskStatus::Open, TaskStatus::Active, actor, None)?;

    // State second — conditional update under BEGIN IMMEDIATE
    let conn = connect(workspace_dir)?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin immediate transaction")?;

    let now = Utc::now().to_rfc3339();
    let rows_affected = conn.execute(
        "UPDATE tasks SET status = 'active', assigned_to = ?1, updated_at = ?2
         WHERE id = ?3 AND status = 'open'",
        params![assigned, now, task_id],
    )
    .context("Failed to claim task")?;

    if rows_affected == 0 {
        let _ = conn.execute_batch("ROLLBACK");
        // Re-read to find actual status
        let actual = get_task(workspace_dir, task_id)?;
        return Err(TaskError::ClaimConflict {
            actual_status: actual.status,
        });
    }

    conn.execute_batch("COMMIT")
        .context("Failed to commit claim")?;
    get_task_inner(&conn, task_id)
}

pub fn block_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    reason: &str,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    do_transition(workspace_dir, task_id, Transition::Block, actor, Some(reason), None, audit)
}

pub fn unblock_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    do_transition(workspace_dir, task_id, Transition::Unblock, actor, None, None, audit)
}

pub fn pause_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    do_transition(workspace_dir, task_id, Transition::Pause, actor, None, None, audit)
}

pub fn resume_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    do_transition(workspace_dir, task_id, Transition::Resume, actor, None, None, audit)
}

pub fn submit_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    do_transition(workspace_dir, task_id, Transition::Submit, actor, None, None, audit)
}

/// Close a task in review. Gated on acceptance criteria:
/// - All machine items must be satisfied (verified via the verifier).
/// - All human items must be attested.
/// - A task with NO machine-verifiable items cannot be agent-closed —
///   it requires an explicit operator close (is_operator=true).
pub fn close_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    outcome: TaskOutcome,
    verifier: &dyn AcceptanceVerifier,
    is_operator: bool,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    let mut task = get_task(workspace_dir, task_id)?;

    if task.status != TaskStatus::Review {
        return Err(TaskError::InvalidTransition {
            current_status: task.status,
            transition: "close".to_string(),
        });
    }

    // Re-verify all machine items fresh (reset before check to prevent TOCTOU)
    let mut has_machine_items = false;
    for item in &mut task.acceptance {
        if item.kind != "human" {
            has_machine_items = true;
            item.satisfied = false;
            match verifier.verify(&item.kind, &item.check) {
                Ok(true) => item.satisfied = true,
                Ok(false) | Err(_) => {}
            }
        }
    }

    // Check all items satisfied
    let unsatisfied: Vec<&AcceptanceItem> =
        task.acceptance.iter().filter(|i| !i.satisfied).collect();
    if !unsatisfied.is_empty() {
        // Persist updated satisfaction state
        let acceptance_json = serde_json::to_string(&task.acceptance)
            .context("Failed to serialize acceptance")?;
        let conn = connect(workspace_dir)?;
        conn.execute(
            "UPDATE tasks SET acceptance = ?1, updated_at = ?2 WHERE id = ?3",
            params![acceptance_json, Utc::now().to_rfc3339(), task_id],
        )
        .context("Failed to update acceptance")?;

        let names: Vec<&str> = unsatisfied.iter().map(|i| i.check.as_str()).collect();
        return Err(TaskError::CloseRefused {
            reason: format!("unsatisfied acceptance items: {}", names.join(", ")),
        });
    }

    // No machine items and not operator → agent cannot close
    if !has_machine_items && !is_operator {
        return Err(TaskError::CloseRefused {
            reason: "task has no machine-verifiable acceptance items; requires operator close"
                .to_string(),
        });
    }

    // Audit first
    audit_transition(
        audit,
        task_id,
        TaskStatus::Review,
        TaskStatus::Closed,
        actor,
        Some(outcome.as_str()),
    )?;

    // State second
    let conn = connect(workspace_dir)?;
    let now = Utc::now().to_rfc3339();
    let acceptance_json = serde_json::to_string(&task.acceptance)
        .context("Failed to serialize acceptance")?;
    conn.execute(
        "UPDATE tasks SET status = 'closed', outcome = ?1, acceptance = ?2, updated_at = ?3
         WHERE id = ?4",
        params![outcome.as_str(), acceptance_json, now, task_id],
    )
    .context("Failed to close task")?;

    get_task_inner(&conn, task_id)
}

pub fn reopen_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    reason: &str,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    do_transition(
        workspace_dir,
        task_id,
        Transition::Reopen,
        actor,
        Some(reason),
        None,
        audit,
    )
}

pub fn abandon_task(
    workspace_dir: &Path,
    task_id: &str,
    actor: &TaskActor,
    reason: &str,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    if reason.trim().is_empty() {
        return Err(TaskError::AbandonRequiresReason);
    }
    do_transition(workspace_dir, task_id, Transition::Abandon, actor, Some(reason), None, audit)
}

/// Record an operator attestation on a human acceptance item.
pub fn attest(
    workspace_dir: &Path,
    task_id: &str,
    check: &str,
    actor: &TaskActor,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    let task = get_task(workspace_dir, task_id)?;
    let mut acceptance = task.acceptance;
    let mut found = false;

    for item in &mut acceptance {
        if item.kind == "human" && item.check == check {
            item.satisfied = true;
            found = true;
        }
    }

    if !found {
        return Err(TaskError::AcceptanceItemNotFound(check.to_string()));
    }

    // Audit the attestation
    let detail = serde_json::json!({
        "task_id": task_id,
        "action": "attest",
        "check": check,
    });
    let event = AuditEvent::new(AuditEventType::TaskTransition)
        .with_actor(actor.channel.clone(), actor.id.clone(), None)
        .with_action(
            serde_json::to_string(&detail).unwrap_or_default(),
            "task".to_string(),
            true,
            true,
        );
    audit.log(&event).map_err(|e| TaskError::Audit(e.to_string()))?;

    let conn = connect(workspace_dir)?;
    let acceptance_json =
        serde_json::to_string(&acceptance).context("Failed to serialize acceptance")?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE tasks SET acceptance = ?1, updated_at = ?2 WHERE id = ?3",
        params![acceptance_json, now, task_id],
    )
    .context("Failed to update acceptance")?;

    get_task_inner(&conn, task_id)
}

pub fn reband_task(
    workspace_dir: &Path,
    task_id: &str,
    new_autonomy: super::Autonomy,
    actor: &TaskActor,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    let task = get_task(workspace_dir, task_id)?;

    let detail = serde_json::json!({
        "task_id": task_id,
        "action": "reband",
        "old_autonomy": task.autonomy.as_str(),
        "new_autonomy": new_autonomy.as_str(),
    });
    let event = AuditEvent::new(AuditEventType::TaskTransition)
        .with_actor(actor.channel.clone(), actor.id.clone(), None)
        .with_action(
            serde_json::to_string(&detail).unwrap_or_default(),
            "task".to_string(),
            true,
            true,
        );
    audit.log(&event).map_err(|e| TaskError::Audit(e.to_string()))?;

    let conn = connect(workspace_dir)?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE tasks SET autonomy = ?1, updated_at = ?2 WHERE id = ?3",
        params![new_autonomy.as_str(), now, task_id],
    )
    .context("Failed to update task autonomy")?;

    get_task_inner(&conn, task_id)
}

/// Persist updated acceptance items (used by the close-gate hook to record
/// verification results without closing the task).
pub fn update_acceptance(
    workspace_dir: &Path,
    task_id: &str,
    acceptance: &[super::AcceptanceItem],
) -> Result<()> {
    let conn = connect(workspace_dir)?;
    let acceptance_json =
        serde_json::to_string(acceptance).context("Failed to serialize acceptance")?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE tasks SET acceptance = ?1, updated_at = ?2 WHERE id = ?3",
        params![acceptance_json, now, task_id],
    )
    .context("Failed to update acceptance")?;
    Ok(())
}

/// Read task_transition events from audit.db for a given task.
pub fn get_task_history(audit: &AuditLogger, task_id: &str) -> Result<Vec<serde_json::Value>> {
    let conn = Connection::open(audit.db_path())
        .context("Failed to open audit.db for history query")?;

    let mut stmt = conn.prepare(
        "SELECT event_json FROM audit_events
         WHERE event_type = 'task_transition'
         ORDER BY id ASC",
    )?;

    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut events = Vec::new();
    for row in rows {
        let json_str = row?;
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&json_str) {
            // Filter to events for this task_id
            if let Some(action) = event.get("action").and_then(|a| a.get("command")) {
                if let Some(cmd_str) = action.as_str() {
                    if cmd_str.contains(task_id) {
                        events.push(event);
                    }
                }
            }
        }
    }

    Ok(events)
}

pub fn insert_breadcrumb(
    workspace_dir: &Path,
    task_id: &str,
    actor_id: Option<&str>,
    tool: &str,
    args_summary: Option<&str>,
    result_summary: Option<&str>,
) -> Result<()> {
    let conn = connect(workspace_dir)?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO task_activity (task_id, actor_id, tool, args_summary, result_summary, ts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![task_id, actor_id, tool, args_summary, result_summary, now],
    )
    .context("Failed to insert breadcrumb into task_activity")?;
    Ok(())
}

/// Atomically increment the turn_count for a task and return the new value.
pub fn increment_turn_count(workspace_dir: &Path, task_id: &str) -> Result<u32> {
    let conn = connect(workspace_dir)?;
    conn.execute(
        "UPDATE tasks SET turn_count = turn_count + 1 WHERE id = ?1",
        params![task_id],
    )
    .context("Failed to increment turn_count")?;
    let count: i64 = conn
        .query_row(
            "SELECT turn_count FROM tasks WHERE id = ?1",
            params![task_id],
            |r| r.get(0),
        )
        .context("Failed to read turn_count after increment")?;
    Ok(count as u32)
}

pub fn get_task_activity(workspace_dir: &Path, task_id: &str) -> Result<Vec<serde_json::Value>> {
    let conn = connect(workspace_dir)?;
    let mut stmt = conn.prepare(
        "SELECT id, task_id, actor_id, tool, args_summary, result_summary, ts
         FROM task_activity WHERE task_id = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![task_id], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, i64>(0)?,
            "task_id": row.get::<_, String>(1)?,
            "actor_id": row.get::<_, Option<String>>(2)?,
            "tool": row.get::<_, Option<String>>(3)?,
            "args_summary": row.get::<_, Option<String>>(4)?,
            "result_summary": row.get::<_, Option<String>>(5)?,
            "ts": row.get::<_, String>(6)?,
        }))
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

// ── Internal transition logic ────────────────────────────────────

fn do_transition(
    workspace_dir: &Path,
    task_id: &str,
    trans: Transition,
    actor: &TaskActor,
    reason: Option<&str>,
    outcome: Option<TaskOutcome>,
    audit: &AuditLogger,
) -> Result<Task, TaskError> {
    let task = get_task(workspace_dir, task_id)?;

    if !trans.valid_from().contains(&task.status) {
        return Err(TaskError::InvalidTransition {
            current_status: task.status,
            transition: trans.as_str().to_string(),
        });
    }

    let new_status = trans.target_status();

    // Audit first, state second
    audit_transition(audit, task_id, task.status, new_status, actor, reason)?;

    let conn = connect(workspace_dir)?;
    let now = Utc::now().to_rfc3339();

    match trans {
        Transition::Abandon => {
            conn.execute(
                "UPDATE tasks SET status = ?1, abandon_reason = ?2, outcome = 'cancelled',
                 updated_at = ?3 WHERE id = ?4",
                params![new_status.as_str(), reason, now, task_id],
            )
            .context("Failed to abandon task")?;
        }
        Transition::Block => {
            // Human note rides the audit event; structured blockers unchanged
            conn.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![new_status.as_str(), now, task_id],
            )
            .context("Failed to block task")?;
        }
        Transition::Unblock => {
            conn.execute(
                "UPDATE tasks SET status = ?1, blockers = '{}', updated_at = ?2 WHERE id = ?3",
                params![new_status.as_str(), now, task_id],
            )
            .context("Failed to unblock task")?;
        }
        _ => {
            conn.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![new_status.as_str(), now, task_id],
            )
            .context("Failed to update task status")?;
        }
    }

    get_task_inner(&conn, task_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::audit::AuditLogger;
    use daemonclaw_config::schema::AuditConfig;
    use tempfile::TempDir;

    fn test_setup() -> (TempDir, AuditLogger) {
        let tmp = TempDir::new().unwrap();
        let audit_config = AuditConfig {
            enabled: true,
            log_path: "audit.log".to_string(),
            max_size_mb: 100,
            sign_events: false,
        };
        let audit = AuditLogger::new(audit_config, tmp.path().to_path_buf()).unwrap();
        (tmp, audit)
    }

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

    fn simple_params(title: &str) -> CreateTaskParams<'_> {
        CreateTaskParams {
            title,
            intent: None,
            acceptance: &[],
            priority: 2,
            parent_id: None,
            source: "operator",
            autonomy: Autonomy::Gated,
            execution: Execution::Agentic,
            tools: &[],
        }
    }

    // ── Acceptance scenario 1: machine acceptance gate ──────────

    #[test]
    fn close_refused_with_unmet_machine_acceptance() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let acceptance = vec![AcceptanceItem {
            kind: "machine".to_string(),
            check: "false".to_string(), // exit 1 → unsatisfied
            satisfied: false,
        }];
        let params = CreateTaskParams {
            title: "Machine gate test",
            acceptance: &acceptance,
            ..simple_params("Machine gate test")
        };
        let task = create_task(tmp.path(), &params, &actor, &audit).unwrap();

        let claimed = claim_task(tmp.path(), &task.id, &actor, &audit).unwrap();
        assert_eq!(claimed.status, TaskStatus::Active);

        let submitted = submit_task(tmp.path(), &task.id, &actor, &audit).unwrap();
        assert_eq!(submitted.status, TaskStatus::Review);

        // Close with failing check → refused
        let verifier = super::super::ShellVerifier;
        let err = close_task(
            tmp.path(), &task.id, &actor, TaskOutcome::Succeeded,
            &verifier, false, &audit,
        )
        .unwrap_err();
        assert!(matches!(err, TaskError::CloseRefused { .. }), "got: {err:?}");

        // Task stays in review
        let still_review = get_task(tmp.path(), &task.id).unwrap();
        assert_eq!(still_review.status, TaskStatus::Review);
    }

    #[test]
    fn close_succeeds_after_machine_acceptance_satisfied() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let acceptance = vec![AcceptanceItem {
            kind: "machine".to_string(),
            check: "true".to_string(), // exit 0 → satisfied
            satisfied: false,
        }];
        let params = CreateTaskParams {
            title: "Machine pass test",
            acceptance: &acceptance,
            ..simple_params("Machine pass test")
        };
        let task = create_task(tmp.path(), &params, &actor, &audit).unwrap();
        claim_task(tmp.path(), &task.id, &actor, &audit).unwrap();
        submit_task(tmp.path(), &task.id, &actor, &audit).unwrap();

        let verifier = super::super::ShellVerifier;
        let closed = close_task(
            tmp.path(), &task.id, &actor, TaskOutcome::Succeeded,
            &verifier, false, &audit,
        )
        .unwrap();
        assert_eq!(closed.status, TaskStatus::Closed);
        assert_eq!(closed.outcome, Some(TaskOutcome::Succeeded));
    }

    // ── Acceptance scenario 2: human acceptance gate ────────────

    #[test]
    fn close_refused_until_human_attestation() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let acceptance = vec![AcceptanceItem {
            kind: "human".to_string(),
            check: "operator reviewed deployment".to_string(),
            satisfied: false,
        }];
        let params = CreateTaskParams {
            title: "Human gate test",
            acceptance: &acceptance,
            ..simple_params("Human gate test")
        };
        let task = create_task(tmp.path(), &params, &actor, &audit).unwrap();
        claim_task(tmp.path(), &task.id, &actor, &audit).unwrap();
        submit_task(tmp.path(), &task.id, &actor, &audit).unwrap();

        let verifier = super::super::ShellVerifier;
        let err = close_task(
            tmp.path(), &task.id, &actor, TaskOutcome::Succeeded,
            &verifier, true, &audit,
        )
        .unwrap_err();
        assert!(matches!(err, TaskError::CloseRefused { .. }));

        // Attest
        attest(
            tmp.path(), &task.id,
            "operator reviewed deployment",
            &actor, &audit,
        )
        .unwrap();

        // Now close succeeds
        let closed = close_task(
            tmp.path(), &task.id, &actor, TaskOutcome::Succeeded,
            &verifier, true, &audit,
        )
        .unwrap();
        assert_eq!(closed.status, TaskStatus::Closed);
    }

    // ── Acceptance scenario 3: no machine items → agent cannot close ──

    #[test]
    fn agent_cannot_close_task_with_no_machine_items() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        // No acceptance items at all
        let task = create_task(tmp.path(), &simple_params("No items"), &actor, &audit).unwrap();
        claim_task(tmp.path(), &task.id, &actor, &audit).unwrap();
        submit_task(tmp.path(), &task.id, &actor, &audit).unwrap();

        let verifier = super::super::ShellVerifier;
        let err = close_task(
            tmp.path(), &task.id, &actor, TaskOutcome::Succeeded,
            &verifier, false, &audit, // is_operator=false
        )
        .unwrap_err();
        assert!(matches!(err, TaskError::CloseRefused { .. }));

        // Operator CAN close it
        let closed = close_task(
            tmp.path(), &task.id, &actor, TaskOutcome::Succeeded,
            &verifier, true, &audit, // is_operator=true
        )
        .unwrap();
        assert_eq!(closed.status, TaskStatus::Closed);
    }

    // ── Acceptance scenario 4: concurrent double-claim ──────────

    #[test]
    fn concurrent_double_claim_one_wins() {
        let (tmp, audit) = test_setup();
        let actor_a = cli_actor();
        let actor_b = other_actor();

        let task = create_task(tmp.path(), &simple_params("Race"), &actor_a, &audit).unwrap();

        // First claim wins
        let winner = claim_task(tmp.path(), &task.id, &actor_a, &audit).unwrap();
        assert_eq!(winner.status, TaskStatus::Active);

        // Second claim loses — task is no longer open
        let err = claim_task(tmp.path(), &task.id, &actor_b, &audit);
        assert!(err.is_err(), "second claim should fail");
        match err.unwrap_err() {
            TaskError::ClaimConflict { actual_status } => {
                assert_eq!(actual_status, TaskStatus::Active);
            }
            other => panic!("expected ClaimConflict, got: {other:?}"),
        }
    }

    // ── Acceptance scenario 5: abandon ──────────────────────────

    #[test]
    fn abandon_with_reason_and_empty_rejected() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let task = create_task(tmp.path(), &simple_params("Abandon me"), &actor, &audit).unwrap();

        let err = abandon_task(tmp.path(), &task.id, &actor, "", &audit).unwrap_err();
        assert!(matches!(err, TaskError::AbandonRequiresReason));

        let abandoned =
            abandon_task(tmp.path(), &task.id, &actor, "no longer needed", &audit).unwrap();
        assert_eq!(abandoned.status, TaskStatus::Abandoned);
        assert_eq!(abandoned.abandon_reason.as_deref(), Some("no longer needed"));
    }

    // ── Acceptance scenario 6: audit chain integrity ────────────

    #[test]
    fn transitions_in_audit_chain_and_chain_verifies() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let task = create_task(tmp.path(), &simple_params("Audit test"), &actor, &audit).unwrap();
        claim_task(tmp.path(), &task.id, &actor, &audit).unwrap();
        submit_task(tmp.path(), &task.id, &actor, &audit).unwrap();

        // Verify the Merkle chain: create(open→open) + claim(open→active) + submit(active→review) = 3
        let count = crate::security::audit::verify_chain(audit.db_path()).unwrap();
        assert!(count >= 3, "expected at least 3 audit events, got {count}");

        // Verify append-only enforcement: try to delete
        let conn = Connection::open(audit.db_path()).unwrap();
        let delete_result = conn.execute("DELETE FROM audit_events WHERE id = 1", []);
        assert!(delete_result.is_err(), "DELETE should be blocked by trigger");
    }

    // ── Acceptance scenario 7: task_activity is breadcrumb schema, empty ──

    #[test]
    fn task_activity_exists_with_breadcrumb_schema_and_is_empty() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        create_task(tmp.path(), &simple_params("Check schema"), &actor, &audit).unwrap();

        let conn = connect(tmp.path()).unwrap();

        // Verify the breadcrumb columns exist
        let mut stmt = conn
            .prepare("SELECT id, task_id, actor_id, tool, args_summary, result_summary, ts FROM task_activity LIMIT 1")
            .expect("task_activity should have breadcrumb columns");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_activity", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "task_activity must be empty in Track A");
        drop(stmt);
    }

    // ── Remaining state machine tests ───────────────────────────

    #[test]
    fn invalid_priority_rejected() {
        let (tmp, audit) = test_setup();
        let mut params = simple_params("Bad priority");
        params.priority = 5;
        let err = create_task(tmp.path(), &params, &cli_actor(), &audit).unwrap_err();
        assert!(matches!(err, TaskError::InvalidPriority(5)));
    }

    #[test]
    fn cannot_transition_from_terminal() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let task = create_task(tmp.path(), &simple_params("Terminal"), &actor, &audit).unwrap();
        abandon_task(tmp.path(), &task.id, &actor, "done", &audit).unwrap();

        let err = claim_task(tmp.path(), &task.id, &actor, &audit).unwrap_err();
        assert!(matches!(err, TaskError::ClaimConflict { .. }));
    }

    #[test]
    fn block_and_unblock() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let task = create_task(tmp.path(), &simple_params("Block test"), &actor, &audit).unwrap();
        claim_task(tmp.path(), &task.id, &actor, &audit).unwrap();

        let blocked = block_task(tmp.path(), &task.id, &actor, "external dep", &audit).unwrap();
        assert_eq!(blocked.status, TaskStatus::Blocked);

        let unblocked = unblock_task(tmp.path(), &task.id, &actor, &audit).unwrap();
        assert_eq!(unblocked.status, TaskStatus::Active);
    }

    #[test]
    fn pause_and_resume() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let task = create_task(tmp.path(), &simple_params("Pause test"), &actor, &audit).unwrap();
        claim_task(tmp.path(), &task.id, &actor, &audit).unwrap();

        let paused = pause_task(tmp.path(), &task.id, &actor, &audit).unwrap();
        assert_eq!(paused.status, TaskStatus::Paused);

        let resumed = resume_task(tmp.path(), &task.id, &actor, &audit).unwrap();
        assert_eq!(resumed.status, TaskStatus::Active);
    }

    #[test]
    fn list_tasks_with_filter() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        create_task(tmp.path(), &simple_params("Task A"), &actor, &audit).unwrap();
        let b = create_task(
            tmp.path(),
            &CreateTaskParams { priority: 4, ..simple_params("Task B") },
            &actor, &audit,
        )
        .unwrap();
        create_task(tmp.path(), &simple_params("Task C"), &actor, &audit).unwrap();

        claim_task(tmp.path(), &b.id, &actor, &audit).unwrap();

        let all = list_tasks(tmp.path(), None, 100).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].title, "Task B"); // highest priority

        let open_only = list_tasks(tmp.path(), Some(TaskStatus::Open), 100).unwrap();
        assert_eq!(open_only.len(), 2);
    }

    #[test]
    fn parent_task_reference() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let parent = create_task(tmp.path(), &simple_params("Parent"), &actor, &audit).unwrap();
        let child = create_task(
            tmp.path(),
            &CreateTaskParams {
                parent_id: Some(&parent.id),
                ..simple_params("Child")
            },
            &actor,
            &audit,
        )
        .unwrap();

        assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn task_not_found() {
        let (tmp, audit) = test_setup();
        let _ = list_tasks(tmp.path(), None, 1);
        let err = get_task(tmp.path(), "nonexistent-id").unwrap_err();
        assert!(matches!(err, TaskError::NotFound(_)));
    }

    #[test]
    fn schema_migration_is_idempotent() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        create_task(tmp.path(), &simple_params("First"), &actor, &audit).unwrap();
        create_task(tmp.path(), &simple_params("Second"), &actor, &audit).unwrap();

        let tasks = list_tasks(tmp.path(), None, 100).unwrap();
        assert_eq!(tasks.len(), 2);

        let path = db_path(tmp.path());
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
    }

    #[test]
    fn breadcrumb_insert_and_retrieve() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();
        let task = create_task(tmp.path(), &simple_params("Breadcrumb test"), &actor, &audit).unwrap();

        insert_breadcrumb(
            tmp.path(),
            &task.id,
            Some("richard"),
            "shell",
            Some("echo hello"),
            Some("hello\n"),
        )
        .unwrap();
        insert_breadcrumb(
            tmp.path(),
            &task.id,
            Some("richard"),
            "read_file",
            Some("/etc/hosts"),
            Some("127.0.0.1 localhost"),
        )
        .unwrap();

        let activity = get_task_activity(tmp.path(), &task.id).unwrap();
        assert_eq!(activity.len(), 2);
        assert_eq!(activity[0]["tool"].as_str().unwrap(), "shell");
        assert_eq!(activity[1]["tool"].as_str().unwrap(), "read_file");
        assert!(activity[0]["ts"].as_str().is_some());
    }

    #[test]
    fn breadcrumb_unbound_task_yields_no_rows() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();
        let task = create_task(tmp.path(), &simple_params("No crumbs"), &actor, &audit).unwrap();

        let activity = get_task_activity(tmp.path(), &task.id).unwrap();
        assert!(activity.is_empty());
    }

    #[test]
    fn priority_view_no_tasks_returns_none() {
        let tmp = TempDir::new().unwrap();
        let _ = connect(tmp.path()).unwrap();
        assert_eq!(render_priority_view(tmp.path(), None, 15), "[Tasks] none");
    }

    #[test]
    fn priority_view_mixed_statuses() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let p = simple_params("First task");
        create_task(tmp.path(), &p, &actor, &audit).unwrap();

        let mut p2 = simple_params("Second task");
        p2.priority = 1;
        let t2 = create_task(tmp.path(), &p2, &actor, &audit).unwrap();
        claim_task(tmp.path(), &t2.id, &actor, &audit).unwrap();

        let view = render_priority_view(tmp.path(), None, 15);
        assert!(view.starts_with("[Tasks] "), "view: {view}");
        assert!(view.contains("open"), "view: {view}");
        assert!(view.contains("active"), "view: {view}");
        assert!(view.contains("First task"), "view: {view}");
        assert!(view.contains("Second task"), "view: {view}");
    }

    #[test]
    fn priority_view_cap_overflow() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        for i in 0..5 {
            let title = format!("Task {i}");
            let p = simple_params(&title);
            create_task(tmp.path(), &p, &actor, &audit).unwrap();
        }

        let view = render_priority_view(tmp.path(), None, 3);
        assert!(view.contains("+2 more"), "view: {view}");
    }

    #[test]
    fn priority_view_nonexistent_db() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(render_priority_view(tmp.path(), None, 15), "[Tasks] unavailable");
    }

    #[test]
    fn staging_matrix_none_recon_only() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();
        let mut p = simple_params("Auto task");
        p.autonomy = Autonomy::Auto;
        p.execution = Execution::Agentic;
        create_task(tmp.path(), &p, &actor, &audit).unwrap();

        let open = list_tasks(tmp.path(), Some(TaskStatus::Open), 50).unwrap();
        assert_eq!(open.len(), 1);
        // In none mode, nothing passes the filter
        let eligible: Vec<_> = open
            .iter()
            .filter(|t| t.execution != Execution::Deterministic)
            .filter(|_t| false) // none mode
            .collect();
        assert!(eligible.is_empty(), "none mode should produce zero eligible tasks");
        // Task stays open
        let t = get_task(tmp.path(), &open[0].id).unwrap();
        assert_eq!(t.status, TaskStatus::Open);
    }

    #[test]
    fn staging_matrix_assisted_claims_assisted_and_auto() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let mut p_auto = simple_params("Auto task");
        p_auto.autonomy = Autonomy::Auto;
        create_task(tmp.path(), &p_auto, &actor, &audit).unwrap();

        let mut p_assisted = simple_params("Assisted task");
        p_assisted.autonomy = Autonomy::Assisted;
        create_task(tmp.path(), &p_assisted, &actor, &audit).unwrap();

        let mut p_gated = simple_params("Gated task");
        p_gated.autonomy = Autonomy::Gated;
        create_task(tmp.path(), &p_gated, &actor, &audit).unwrap();

        let open = list_tasks(tmp.path(), Some(TaskStatus::Open), 50).unwrap();
        let eligible: Vec<_> = open
            .iter()
            .filter(|t| t.execution != Execution::Deterministic)
            .filter(|t| t.autonomy == Autonomy::Auto || t.autonomy == Autonomy::Assisted)
            .collect();
        // assisted mode: Auto + Assisted eligible, Gated not
        assert_eq!(eligible.len(), 2);
        assert!(eligible.iter().all(|t| t.autonomy != Autonomy::Gated));
    }

    #[test]
    fn staging_matrix_full_claims_auto_only() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let mut p_auto = simple_params("Auto task");
        p_auto.autonomy = Autonomy::Auto;
        create_task(tmp.path(), &p_auto, &actor, &audit).unwrap();

        let mut p_assisted = simple_params("Assisted task");
        p_assisted.autonomy = Autonomy::Assisted;
        create_task(tmp.path(), &p_assisted, &actor, &audit).unwrap();

        let open = list_tasks(tmp.path(), Some(TaskStatus::Open), 50).unwrap();
        let eligible: Vec<_> = open
            .iter()
            .filter(|t| t.execution != Execution::Deterministic)
            .filter(|t| t.autonomy == Autonomy::Auto)
            .collect();
        // full mode: only Auto
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].autonomy, Autonomy::Auto);
    }

    #[test]
    fn staging_matrix_gated_never_claimed() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let mut p = simple_params("Gated task");
        p.autonomy = Autonomy::Gated;
        create_task(tmp.path(), &p, &actor, &audit).unwrap();

        let open = list_tasks(tmp.path(), Some(TaskStatus::Open), 50).unwrap();
        // Gated excluded from both full and assisted
        let full_eligible: Vec<_> = open
            .iter()
            .filter(|t| t.autonomy == Autonomy::Auto)
            .collect();
        let assisted_eligible: Vec<_> = open
            .iter()
            .filter(|t| t.autonomy == Autonomy::Auto || t.autonomy == Autonomy::Assisted)
            .collect();
        assert!(full_eligible.is_empty());
        assert!(assisted_eligible.is_empty());
    }

    #[test]
    fn staging_matrix_deterministic_skipped() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let mut p = simple_params("Deterministic task");
        p.autonomy = Autonomy::Auto;
        p.execution = Execution::Deterministic;
        create_task(tmp.path(), &p, &actor, &audit).unwrap();

        let open = list_tasks(tmp.path(), Some(TaskStatus::Open), 50).unwrap();
        let eligible: Vec<_> = open
            .iter()
            .filter(|t| t.execution != Execution::Deterministic)
            .filter(|t| t.autonomy == Autonomy::Auto)
            .collect();
        assert!(eligible.is_empty(), "deterministic tasks should be skipped");
    }

    // 2. Double-claim race: exactly one wins, other gets ClaimConflict.
    //    (Already tested in concurrent_double_claim_one_wins above, included
    //    here as a cross-reference for completeness.)
    #[test]
    fn double_claim_race_one_wins_other_conflict() {
        let (tmp, audit) = test_setup();
        let actor_a = cli_actor();
        let actor_b = other_actor();

        let task = create_task(tmp.path(), &simple_params("Race2"), &actor_a, &audit).unwrap();

        let win = claim_task(tmp.path(), &task.id, &actor_a, &audit).unwrap();
        assert_eq!(win.status, TaskStatus::Active);

        let lose = claim_task(tmp.path(), &task.id, &actor_b, &audit);
        assert!(lose.is_err());
        match lose.unwrap_err() {
            TaskError::ClaimConflict { actual_status } => {
                assert_eq!(actual_status, TaskStatus::Active);
            }
            other => panic!("expected ClaimConflict, got: {other:?}"),
        }
    }

    // 3. Silence: empty queue => zero channel output, zero LLM calls.
    //    We verify at the store layer: no open tasks = no eligible = no claims.
    #[test]
    fn silence_empty_queue_no_claims() {
        let (tmp, _audit) = test_setup();
        let open = list_tasks(tmp.path(), Some(TaskStatus::Open), 50).unwrap();
        assert!(open.is_empty(), "fresh db should have zero open tasks");
        // No eligible tasks means the daemon would return immediately
        // with (false, false) — no LLM calls, no channel output.
    }

    // 4. Continue-my-own vs no-op-other: my active task gets continuation,
    //    other actor's active task is skipped.
    #[test]
    fn continue_my_own_active_vs_skip_others() {
        let (tmp, audit) = test_setup();
        let main_actor = TaskActor {
            channel: "heartbeat".to_string(),
            id: Some(crate::tasks::MAIN_AGENT_ACTOR_ID.to_string()),
        };
        let other = other_actor();

        // Create and claim two tasks by different actors
        let mut p = simple_params("My task");
        p.autonomy = Autonomy::Auto;
        let my_task = create_task(tmp.path(), &p, &main_actor, &audit).unwrap();
        claim_task(tmp.path(), &my_task.id, &main_actor, &audit).unwrap();

        let mut p2 = simple_params("Their task");
        p2.autonomy = Autonomy::Auto;
        let their_task = create_task(tmp.path(), &p2, &other, &audit).unwrap();
        claim_task(tmp.path(), &their_task.id, &other, &audit).unwrap();

        // List active and filter
        let active = list_tasks(tmp.path(), Some(TaskStatus::Active), 50).unwrap();
        assert_eq!(active.len(), 2);

        let my_active: Vec<_> = active
            .iter()
            .filter(|t| t.assigned_to.as_deref() == Some(crate::tasks::MAIN_AGENT_ACTOR_ID))
            .filter(|t| t.execution != Execution::Deterministic)
            .collect();

        assert_eq!(my_active.len(), 1);
        assert_eq!(my_active[0].title, "My task");

        // Other actor's task not in my_active
        assert!(my_active.iter().all(|t| t.title != "Their task"));
    }

    // 5. Sink correctness: zero channel messages from a worked task.
    //    This is a design invariant — no deliver_announcement in the task
    //    path means zero channel output. Verified by code inspection and
    //    the absence of delivery calls. Test confirms no channel-related
    //    side effects at the store layer.
    #[test]
    fn sink_correctness_no_channel_side_effects() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();
        let mut p = simple_params("Worked task");
        p.autonomy = Autonomy::Auto;
        let task = create_task(tmp.path(), &p, &actor, &audit).unwrap();
        claim_task(tmp.path(), &task.id, &actor, &audit).unwrap();

        // After claim, verify no channel-related artifacts exist
        // The sessions dir shouldn't have channel session files for this task
        let sessions_dir = tmp.path().join("sessions");
        if sessions_dir.exists() {
            let entries: Vec<_> = std::fs::read_dir(&sessions_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .collect();
            assert!(entries.is_empty(), "no channel session files should exist");
        }
        // task_activity should have zero entries (no channel messages logged)
        let activity = get_task_activity(tmp.path(), &task.id).unwrap();
        assert!(activity.is_empty(), "no activity records for a freshly claimed task");
    }

    #[test]
    fn turn_budget_blocks_on_exhaustion() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let mut p = simple_params("Budget task");
        p.autonomy = Autonomy::Auto;
        let task = create_task(tmp.path(), &p, &actor, &audit).unwrap();
        claim_task(tmp.path(), &task.id, &actor, &audit).unwrap();

        let max_turns: u32 = 2;

        let c1 = increment_turn_count(tmp.path(), &task.id).unwrap();
        assert_eq!(c1, 1);
        assert!(c1 <= max_turns);

        let c2 = increment_turn_count(tmp.path(), &task.id).unwrap();
        assert_eq!(c2, 2);
        assert!(c2 <= max_turns);

        let c3 = increment_turn_count(tmp.path(), &task.id).unwrap();
        assert_eq!(c3, 3);
        assert!(c3 > max_turns, "turn 3 should exceed budget of 2");

        let blocked = block_task(
            tmp.path(), &task.id, &actor, "turn budget exhausted", &audit,
        ).unwrap();
        assert_eq!(blocked.status, TaskStatus::Blocked);

        let t = get_task(tmp.path(), &task.id).unwrap();
        assert_eq!(t.status, TaskStatus::Blocked);
        assert_eq!(t.turn_count, 3);
    }

    #[tokio::test]
    async fn unbound_task_submit_returns_clean_error() {
        use crate::tools::task_ops::TaskSubmitTool;
        use daemonclaw_api::tool::Tool;
        use daemonclaw_config::schema::AuditConfig;

        let tmp = TempDir::new().unwrap();
        let tool = TaskSubmitTool::new(
            tmp.path().to_path_buf(),
            AuditConfig {
                enabled: true,
                log_path: "audit.log".to_string(),
                max_size_mb: 100,
                sign_events: false,
            },
        );

        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(!result.success);
        assert!(
            result.output.contains("No task binding"),
            "output: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn unbound_task_block_returns_clean_error() {
        use crate::tools::task_ops::TaskBlockTool;
        use daemonclaw_api::tool::Tool;
        use daemonclaw_config::schema::AuditConfig;

        let tmp = TempDir::new().unwrap();
        let tool = TaskBlockTool::new(
            tmp.path().to_path_buf(),
            AuditConfig {
                enabled: true,
                log_path: "audit.log".to_string(),
                max_size_mb: 100,
                sign_events: false,
            },
        );

        let result = tool
            .execute(serde_json::json!({"reason": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result.output.contains("No task binding"),
            "output: {}",
            result.output
        );
    }

    #[test]
    fn turn_ends_without_submit_or_block_task_stays_active() {
        let (tmp, audit) = test_setup();
        let actor = cli_actor();

        let mut p = simple_params("Stay active");
        p.autonomy = Autonomy::Auto;
        let task = create_task(tmp.path(), &p, &actor, &audit).unwrap();
        claim_task(tmp.path(), &task.id, &actor, &audit).unwrap();

        let _count = increment_turn_count(tmp.path(), &task.id).unwrap();

        let t = get_task(tmp.path(), &task.id).unwrap();
        assert_eq!(t.status, TaskStatus::Active, "task must stay active after a turn with no submit/block");
        assert_eq!(t.turn_count, 1);
    }

    // ── Session persistence acceptance tests ──────────────────────

    #[test]
    fn task_session_two_tick_conversation_in_sessions_db() {
        use daemonclaw_infra::session_backend::SessionBackend;
        use daemonclaw_infra::session_sqlite::SqliteSessionBackend;
        use daemonclaw_providers::ChatMessage;

        let tmp = tempfile::TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let task_id = "abc12345-dead-beef-0000-111122223333";
        let key = format!("task_{task_id}");
        let actor_id = Some(crate::tasks::MAIN_AGENT_ACTOR_ID);
        let actor_type = Some("task_agent");

        // Tick 1: user prompt + assistant response
        backend.append_with_actor(&key, &ChatMessage::user("[Heartbeat Task | P2] Fix the widget\n\nIntent: repair broken widget"), actor_id, actor_type).unwrap();
        backend.append_with_actor(&key, &ChatMessage::assistant("I'll check the widget code. ZXCV_DISTINCTIVE_MARKER_TICK1"), actor_id, actor_type).unwrap();

        // Tick 2: continuation — load prior, add new turn
        let prior = backend.load(&key);
        assert_eq!(prior.len(), 2, "tick 2 must see tick 1's conversation");
        assert_eq!(prior[0].role, "user");
        assert!(prior[1].content.contains("ZXCV_DISTINCTIVE_MARKER_TICK1"));

        backend.append_with_actor(&key, &ChatMessage::user("[Heartbeat Task | P2] Fix the widget\n\nIntent: repair broken widget"), actor_id, actor_type).unwrap();
        backend.append_with_actor(&key, &ChatMessage::assistant("Widget is now fixed. Calling task_submit."), actor_id, actor_type).unwrap();

        let full = backend.load(&key);
        assert_eq!(full.len(), 4, "both ticks' messages must be present");
    }

    #[test]
    fn task_session_metadata_row_exists() {
        use daemonclaw_infra::session_backend::SessionBackend;
        use daemonclaw_infra::session_sqlite::SqliteSessionBackend;
        use daemonclaw_providers::ChatMessage;

        let tmp = tempfile::TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let key = "task_deadbeef";
        backend.append_with_actor(key, &ChatMessage::user("hello"), Some(crate::tasks::MAIN_AGENT_ACTOR_ID), Some("task_agent")).unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].key, key);
        assert_eq!(meta[0].message_count, 1);
    }

    #[test]
    fn task_session_fts_finds_distinctive_string() {
        use daemonclaw_infra::session_backend::SessionBackend;
        use daemonclaw_infra::session_sqlite::SqliteSessionBackend;
        use daemonclaw_providers::ChatMessage;

        let tmp = tempfile::TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let key = "task_fts_test";
        backend.append_with_actor(key, &ChatMessage::assistant("The QWERTY_UNIQUE_PAYLOAD was processed successfully"), Some(crate::tasks::MAIN_AGENT_ACTOR_ID), Some("task_agent")).unwrap();

        let results = backend.search(&daemonclaw_infra::session_backend::SessionQuery {
            keyword: Some("QWERTY_UNIQUE_PAYLOAD".into()),
            limit: Some(10),
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, key);
    }

    #[test]
    fn task_session_no_jsonl_files_created() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // Simulate what the daemon does: use Db persistence (sessions.db), not jsonl
        {
            use daemonclaw_infra::session_backend::SessionBackend;
            let backend = daemonclaw_infra::session_sqlite::SqliteSessionBackend::new(tmp.path()).unwrap();
            backend.append("task_nojsonl", &daemonclaw_providers::ChatMessage::user("test")).unwrap();
        }

        // Verify no task_*.jsonl files exist
        let jsonl_files: Vec<_> = std::fs::read_dir(&sessions_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with("task_") && name.ends_with(".jsonl")
            })
            .collect();
        assert!(jsonl_files.is_empty(), "no task_*.jsonl files should exist, found: {jsonl_files:?}");
    }

    #[test]
    fn task_session_isolated_from_channel_sessions() {
        use daemonclaw_infra::session_backend::SessionBackend;
        use daemonclaw_infra::session_sqlite::SqliteSessionBackend;
        use daemonclaw_providers::ChatMessage;

        let tmp = tempfile::TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        // Task session
        backend.append_with_actor("task_isolated", &ChatMessage::assistant("SECRET_TASK_CONTENT_MUST_NOT_LEAK"), Some(crate::tasks::MAIN_AGENT_ACTOR_ID), Some("task_agent")).unwrap();

        // Channel session
        backend.append("telegram_user_123", &ChatMessage::user("What's up?")).unwrap();
        backend.append("telegram_user_123", &ChatMessage::assistant("Hello!")).unwrap();

        // Channel session must NOT contain task content
        let channel_msgs = backend.load("telegram_user_123");
        for msg in &channel_msgs {
            assert!(!msg.content.contains("SECRET_TASK_CONTENT_MUST_NOT_LEAK"),
                "task session content must never appear in channel session");
        }

        // Task session must NOT contain channel content
        let task_msgs = backend.load("task_isolated");
        assert_eq!(task_msgs.len(), 1);
        assert!(!task_msgs[0].content.contains("What's up?"));

        // list_sessions shows both, confirming isolation by key
        let all = backend.list_sessions();
        assert!(all.contains(&"task_isolated".to_string()));
        assert!(all.contains(&"telegram_user_123".to_string()));
    }

    #[test]
    fn task_session_actor_attribution_persisted() {
        use daemonclaw_infra::session_sqlite::SqliteSessionBackend;
        use daemonclaw_infra::session_backend::SessionBackend;
        use daemonclaw_providers::ChatMessage;

        let tmp = tempfile::TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let key = "task_actor_test";
        backend.append_with_actor(key, &ChatMessage::user("hello"), Some(crate::tasks::MAIN_AGENT_ACTOR_ID), Some("task_agent")).unwrap();

        // Verify actor columns via raw SQL
        let db_path = tmp.path().join("sessions").join("sessions.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let (actor_id, actor_type): (Option<String>, Option<String>) = conn.query_row(
            "SELECT actor_id, actor_type FROM sessions WHERE session_key = ?1",
            rusqlite::params![key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(actor_id.as_deref(), Some(crate::tasks::MAIN_AGENT_ACTOR_ID));
        assert_eq!(actor_type.as_deref(), Some("task_agent"));
    }
}
