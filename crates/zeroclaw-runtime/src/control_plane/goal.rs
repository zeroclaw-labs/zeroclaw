//! Goal-mode admission and controller helpers.
//!
//! This module is the single Rust admission path for slash-command and
//! agent-callable goal starts. Callers pass trusted runtime context explicitly;
//! model/user text supplies only the untrusted objective/action payload.

use anyhow::{Context, Result, bail};

use super::global::control_plane;
use super::task_registry::{GoalTaskRecord, TaskKind, TaskRecord, TaskRegistry, TaskStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalCommandAction {
    Start,
    Status,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalCommand {
    pub action: GoalCommandAction,
    pub objective: Option<String>,
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalAdmissionContext {
    pub agent_alias: String,
    pub originator_route: Option<String>,
    pub principal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalAdmission {
    pub task_id: String,
    pub status: TaskStatus,
    pub message: String,
}

impl GoalAdmissionContext {
    pub fn new(agent_alias: impl Into<String>) -> Self {
        Self {
            agent_alias: agent_alias.into(),
            originator_route: None,
            principal_id: None,
        }
    }

    #[must_use]
    pub fn with_originator_route(mut self, route: Option<String>) -> Self {
        self.originator_route = route;
        self
    }

    #[must_use]
    pub fn with_principal_id(mut self, principal_id: Option<String>) -> Self {
        self.principal_id = principal_id;
        self
    }
}

pub fn parse_goal_command(input: &str) -> Result<GoalCommand> {
    let trimmed = input.trim();
    let without_prefix = if trimmed.starts_with('/') {
        trimmed
            .split_once(char::is_whitespace)
            .map(|(_, rest)| rest.trim())
            .unwrap_or("")
    } else {
        trimmed
    };
    let mut parts = without_prefix.splitn(2, char::is_whitespace);
    let Some(action) = parts.next().filter(|s| !s.is_empty()) else {
        bail!("goal command requires an action: start, status, or cancel");
    };
    let action = action.to_ascii_lowercase();
    let rest = parts.next().unwrap_or("").trim();
    match action.as_str() {
        "start" => {
            if rest.is_empty() {
                bail!("goal start requires an objective");
            }
            Ok(GoalCommand {
                action: GoalCommandAction::Start,
                objective: Some(rest.to_string()),
                task_id: None,
            })
        }
        "status" => Ok(GoalCommand {
            action: GoalCommandAction::Status,
            objective: None,
            task_id: nonempty(rest),
        }),
        "cancel" => Ok(GoalCommand {
            action: GoalCommandAction::Cancel,
            objective: None,
            task_id: nonempty(rest),
        }),
        other => bail!("unknown goal action `{other}`; use start, status, or cancel"),
    }
}

pub async fn admit_goal_command(
    ctx: GoalAdmissionContext,
    command: GoalCommand,
) -> Result<GoalAdmission> {
    let cp = control_plane().context("goal mode requires a running control plane")?;
    match command.action {
        GoalCommandAction::Start => {
            let objective = command
                .objective
                .context("goal start requires an objective")?;
            start_goal(cp.store.as_ref(), &cp.boot_id, ctx, objective).await
        }
        GoalCommandAction::Status => status_goal(cp.store.as_ref(), &ctx, command.task_id).await,
        GoalCommandAction::Cancel => cancel_goal(cp.store.as_ref(), &ctx, command.task_id).await,
    }
}

async fn start_goal(
    store: &dyn TaskRegistry,
    boot_id: &str,
    ctx: GoalAdmissionContext,
    objective: String,
) -> Result<GoalAdmission> {
    let task_id = uuid::Uuid::new_v4().to_string();
    let started_at = chrono::Utc::now().to_rfc3339();
    store
        .create(TaskRecord {
            id: task_id.clone(),
            kind: TaskKind::Goal,
            agent: ctx.agent_alias,
            status: TaskStatus::Running,
            owner_pid: std::process::id(),
            owner_boot_id: boot_id.to_string(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: ctx.originator_route,
            delivered: false,
            idem_key: None,
            principal_id: ctx.principal_id,
            started_at,
            finished_at: None,
        })
        .await?;
    store
        .create_goal_task(GoalTaskRecord {
            task_id: task_id.clone(),
            objective,
            effective_token_limit: None,
            effective_cost_limit_usd: None,
        })
        .await?;
    Ok(GoalAdmission {
        task_id: task_id.clone(),
        status: TaskStatus::Running,
        message: format!("Goal `{task_id}` started."),
    })
}

async fn status_goal(
    store: &dyn TaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
) -> Result<GoalAdmission> {
    let task = resolve_goal_task(store, ctx, task_id).await?;
    let goal = store
        .get_goal_task(&task.id)
        .await?
        .with_context(|| format!("goal extension missing for task {}", task.id))?;
    Ok(GoalAdmission {
        task_id: task.id.clone(),
        status: task.status,
        message: format!(
            "Goal `{}` is {:?}: {}",
            task.id, task.status, goal.objective
        ),
    })
}

async fn cancel_goal(
    store: &dyn TaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
) -> Result<GoalAdmission> {
    let task = resolve_goal_task(store, ctx, task_id).await?;
    if task.status.is_terminal() {
        bail!("goal `{}` is already terminal ({:?})", task.id, task.status);
    }
    store
        .update_status(
            &task.id,
            TaskStatus::Cancelled,
            None,
            Some("cancelled by goal controller".to_string()),
        )
        .await?;
    Ok(GoalAdmission {
        task_id: task.id.clone(),
        status: TaskStatus::Cancelled,
        message: format!("Goal `{}` cancelled.", task.id),
    })
}

async fn resolve_goal_task(
    store: &dyn TaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
) -> Result<TaskRecord> {
    if let Some(task_id) = task_id {
        let task = store
            .get(&task_id)
            .await?
            .with_context(|| format!("goal `{task_id}` was not found"))?;
        ensure_goal_visible(&task, ctx)?;
        return Ok(task);
    }

    let mut goals = store
        .list_by_agent(&ctx.agent_alias)
        .await?
        .into_iter()
        .filter(|task| task.kind == TaskKind::Goal)
        .collect::<Vec<_>>();
    goals.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    goals
        .into_iter()
        .find(|task| !task.status.is_terminal())
        .context("no active goal for this agent")
}

fn ensure_goal_visible(task: &TaskRecord, ctx: &GoalAdmissionContext) -> Result<()> {
    if task.kind != TaskKind::Goal {
        bail!("task `{}` is not a goal", task.id);
    }
    if task.agent != ctx.agent_alias {
        bail!("goal `{}` is not owned by this agent", task.id);
    }
    Ok(())
}

fn nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::task_store_sqlite::SqliteTaskStore;

    #[test]
    fn parse_goal_start_keeps_objective_untrusted_payload_only() {
        let parsed = parse_goal_command("/goal start ship the thing").unwrap();
        assert_eq!(parsed.action, GoalCommandAction::Start);
        assert_eq!(parsed.objective.as_deref(), Some("ship the thing"));
        assert!(parsed.task_id.is_none());

        let parsed = parse_goal_command("/goal@zeroclaw_bot START ship the thing").unwrap();
        assert_eq!(parsed.action, GoalCommandAction::Start);
        assert_eq!(parsed.objective.as_deref(), Some("ship the thing"));
    }

    #[tokio::test]
    async fn goal_lifecycle_uses_task_record_for_status() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-1".into()));
        let started = start_goal(&store, "boot-a", ctx.clone(), "ship it".into())
            .await
            .unwrap();
        let task = store.get(&started.task_id).await.unwrap().unwrap();
        let goal = store
            .get_goal_task(&started.task_id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(task.kind, TaskKind::Goal);
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.originator_route.as_deref(), Some("telegram:chat-1"));
        assert_eq!(task.principal_id.as_deref(), Some("principal-1"));
        assert_eq!(goal.objective, "ship it");
        assert!(goal.effective_token_limit.is_none());

        let cancelled = cancel_goal(&store, &ctx, Some(started.task_id.clone()))
            .await
            .unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(
            store.get(&started.task_id).await.unwrap().unwrap().status,
            TaskStatus::Cancelled
        );

        let err = cancel_goal(&store, &ctx, Some(started.task_id.clone()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already terminal"));
    }
}
