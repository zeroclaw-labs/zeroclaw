//! Goal-mode admission and controller helpers.
//!
//! This module is the single Rust admission path for slash-command and
//! agent-callable goal starts. Callers pass trusted runtime context explicitly;
//! model/user text supplies only the untrusted objective/action payload.

use anyhow::{Context, Result, bail};

use super::global::control_plane;
use super::task_registry::{
    GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState, GoalTaskRecord, TaskKind,
    TaskRecord, TaskRegistry, TaskStatus,
};

tokio::task_local! {
    static GOAL_ADMISSION_CONTEXT: Option<GoalAdmissionContext>;
}

fn msg(key: &str, args: &[(&str, &str)]) -> String {
    crate::i18n::get_required_cli_string_with_args(key, args)
}

fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Running => "running",
        TaskStatus::Paused => "paused",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Lost => "lost",
        TaskStatus::TimedOut => "timed_out",
    }
}

fn pause_reason_label(reason: GoalPauseReason) -> &'static str {
    match reason {
        GoalPauseReason::NeedsUserInput => "needs_user_input",
        GoalPauseReason::HumanEscalation => "human_escalation",
        GoalPauseReason::ExternalDependency => "external_dependency",
        GoalPauseReason::ProviderUnavailable => "provider_unavailable",
        GoalPauseReason::VerifierBlocked => "verifier_blocked",
        GoalPauseReason::DaemonRestart => "daemon_restart",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalCommandAction {
    Start,
    Status,
    Pause,
    Resume,
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

pub fn current_goal_admission_context() -> Option<GoalAdmissionContext> {
    GOAL_ADMISSION_CONTEXT.try_with(Clone::clone).ok().flatten()
}

pub async fn scope_goal_admission_context<F>(
    ctx: Option<GoalAdmissionContext>,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    GOAL_ADMISSION_CONTEXT.scope(ctx, future).await
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
        bail!("{}", msg("goal-command-error-missing-action", &[]));
    };
    let action = action.to_ascii_lowercase();
    let rest = parts.next().unwrap_or("").trim();
    match action.as_str() {
        "start" => {
            if rest.is_empty() {
                bail!("{}", msg("goal-command-error-missing-objective", &[]));
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
        "pause" => Ok(GoalCommand {
            action: GoalCommandAction::Pause,
            objective: nonempty(rest),
            task_id: None,
        }),
        "resume" => Ok(GoalCommand {
            action: GoalCommandAction::Resume,
            objective: None,
            task_id: nonempty(rest),
        }),
        "cancel" => Ok(GoalCommand {
            action: GoalCommandAction::Cancel,
            objective: None,
            task_id: nonempty(rest),
        }),
        other => {
            bail!(
                "{}",
                msg("goal-command-error-unknown-action", &[("action", other)])
            )
        }
    }
}

pub async fn admit_goal_command(
    ctx: GoalAdmissionContext,
    command: GoalCommand,
) -> Result<GoalAdmission> {
    let cp = control_plane()
        .with_context(|| msg("goal-command-error-control-plane-unavailable", &[]))?;
    match command.action {
        GoalCommandAction::Start => {
            let objective = command
                .objective
                .with_context(|| msg("goal-command-error-missing-objective", &[]))?;
            start_goal(cp.store.as_ref(), &cp.boot_id, ctx, objective).await
        }
        GoalCommandAction::Status => status_goal(cp.store.as_ref(), &ctx, command.task_id).await,
        GoalCommandAction::Pause => {
            let description = command.objective;
            pause_goal_for_blocker(
                cp.store.as_ref(),
                &ctx,
                command.task_id,
                GoalPauseState {
                    reason: GoalPauseReason::HumanEscalation,
                    description: description.clone(),
                    blockers: description
                        .map(|message| {
                            vec![GoalBlocker {
                                kind: GoalBlockerKind::HumanEscalation,
                                message,
                                payload: None,
                            }]
                        })
                        .unwrap_or_default(),
                },
            )
            .await
        }
        GoalCommandAction::Resume => resume_goal(cp.store.as_ref(), &ctx, command.task_id).await,
        GoalCommandAction::Cancel => cancel_goal(cp.store.as_ref(), &ctx, command.task_id).await,
    }
}

async fn start_goal(
    store: &dyn TaskRegistry,
    boot_id: &str,
    ctx: GoalAdmissionContext,
    objective: String,
) -> Result<GoalAdmission> {
    if let Some(active) = store
        .latest_active_goal_for_context(
            &ctx.agent_alias,
            ctx.originator_route.as_deref(),
            ctx.principal_id.as_deref(),
        )
        .await
        .with_context(|| msg("goal-command-error-active-goal-lookup-failed", &[]))?
    {
        bail!(
            "{}",
            msg(
                "goal-command-error-active-goal-exists",
                &[("task_id", &active.id)]
            )
        );
    }
    let task_id = uuid::Uuid::new_v4().to_string();
    let started_at = chrono::Utc::now().to_rfc3339();
    store
        .create_goal(
            TaskRecord {
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
            },
            GoalTaskRecord {
                task_id: task_id.clone(),
                objective,
                effective_token_limit: None,
                effective_cost_limit_usd: None,
                pause_reason: None,
                pause_description: None,
                blockers: Vec::new(),
            },
        )
        .await
        .map_err(|error| {
            if is_active_goal_context_conflict(&error) {
                anyhow::Error::msg(msg("goal-command-error-active-goal-conflict", &[]))
            } else {
                error.context(msg("goal-command-error-start-failed", &[]))
            }
        })?;
    Ok(GoalAdmission {
        task_id: task_id.clone(),
        status: TaskStatus::Running,
        message: msg("goal-command-started", &[("task_id", &task_id)]),
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
        .await
        .with_context(|| msg("goal-command-error-status-failed", &[]))?
        .with_context(|| {
            msg(
                "goal-command-error-extension-missing",
                &[("task_id", &task.id)],
            )
        })?;
    Ok(GoalAdmission {
        task_id: task.id.clone(),
        status: task.status,
        message: if let Some(reason) = goal.pause_reason {
            msg(
                "goal-command-status-paused",
                &[
                    ("task_id", &task.id),
                    ("status", status_label(task.status)),
                    ("objective", &goal.objective),
                    ("reason", pause_reason_label(reason)),
                ],
            )
        } else {
            msg(
                "goal-command-status",
                &[
                    ("task_id", &task.id),
                    ("status", status_label(task.status)),
                    ("objective", &goal.objective),
                ],
            )
        },
    })
}

async fn pause_goal_for_blocker(
    store: &dyn TaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
    pause: GoalPauseState,
) -> Result<GoalAdmission> {
    let task = resolve_goal_task(store, ctx, task_id).await?;
    if task.status.is_terminal() {
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", &task.id), ("status", status_label(task.status))]
            )
        );
    }
    store
        .update_goal_pause(&task.id, Some(pause))
        .await
        .with_context(|| msg("goal-command-error-pause-failed", &[("task_id", &task.id)]))?;
    store
        .update_status(&task.id, TaskStatus::Paused, None, None)
        .await
        .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task.id)]))?;
    Ok(GoalAdmission {
        task_id: task.id.clone(),
        status: TaskStatus::Paused,
        message: msg("goal-command-paused", &[("task_id", &task.id)]),
    })
}

async fn resume_goal(
    store: &dyn TaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
) -> Result<GoalAdmission> {
    let task = resolve_goal_task(store, ctx, task_id).await?;
    if task.status.is_terminal() {
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", &task.id), ("status", status_label(task.status))]
            )
        );
    }
    store
        .update_goal_pause(&task.id, None)
        .await
        .with_context(|| msg("goal-command-error-resume-failed", &[("task_id", &task.id)]))?;
    store
        .update_status(&task.id, TaskStatus::Running, None, None)
        .await
        .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task.id)]))?;
    Ok(GoalAdmission {
        task_id: task.id.clone(),
        status: TaskStatus::Running,
        message: msg("goal-command-resumed", &[("task_id", &task.id)]),
    })
}

async fn cancel_goal(
    store: &dyn TaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
) -> Result<GoalAdmission> {
    let task = resolve_goal_task(store, ctx, task_id).await?;
    if task.status.is_terminal() {
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", &task.id), ("status", status_label(task.status))]
            )
        );
    }
    store
        .update_status(
            &task.id,
            TaskStatus::Cancelled,
            None,
            Some(msg("goal-terminal-reason-cancelled-by-controller", &[])),
        )
        .await
        .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task.id)]))?;
    Ok(GoalAdmission {
        task_id: task.id.clone(),
        status: TaskStatus::Cancelled,
        message: msg("goal-command-cancelled", &[("task_id", &task.id)]),
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
            .await
            .with_context(|| msg("goal-command-error-lookup-failed", &[]))?
            .with_context(|| msg("goal-command-error-not-found", &[("task_id", &task_id)]))?;
        ensure_goal_visible(&task, ctx)?;
        return Ok(task);
    }

    let task = store
        .latest_active_goal_for_context(
            &ctx.agent_alias,
            ctx.originator_route.as_deref(),
            ctx.principal_id.as_deref(),
        )
        .await
        .with_context(|| msg("goal-command-error-lookup-failed", &[]))?
        .with_context(|| msg("goal-command-error-no-active-goal", &[]))?;
    ensure_goal_visible(&task, ctx)?;
    Ok(task)
}

fn is_active_goal_context_conflict(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let text = cause.to_string();
        text.contains("idx_tasks_active_goal_context")
            || text.contains("UNIQUE constraint failed: index 'idx_tasks_active_goal_context'")
    })
}

fn ensure_goal_visible(task: &TaskRecord, ctx: &GoalAdmissionContext) -> Result<()> {
    if task.kind != TaskKind::Goal {
        bail!(
            "{}",
            msg("goal-command-error-not-goal", &[("task_id", &task.id)])
        );
    }
    if task.agent != ctx.agent_alias {
        bail!(
            "{}",
            msg("goal-command-error-wrong-agent", &[("task_id", &task.id)])
        );
    }
    if let Some(route) = task.originator_route.as_deref()
        && ctx.originator_route.as_deref() != Some(route)
    {
        bail!(
            "{}",
            msg("goal-command-error-wrong-route", &[("task_id", &task.id)])
        );
    }
    if let Some(principal_id) = task.principal_id.as_deref()
        && ctx.principal_id.as_deref() != Some(principal_id)
    {
        bail!(
            "{}",
            msg(
                "goal-command-error-wrong-principal",
                &[("task_id", &task.id)]
            )
        );
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

        let parsed = parse_goal_command("/goal resume goal-123").unwrap();
        assert_eq!(parsed.action, GoalCommandAction::Resume);
        assert_eq!(parsed.task_id.as_deref(), Some("goal-123"));
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

    #[tokio::test]
    async fn goal_visibility_enforces_route_and_principal() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let owner = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-1".into()));
        let started = start_goal(&store, "boot-a", owner.clone(), "ship it".into())
            .await
            .unwrap();

        status_goal(&store, &owner, Some(started.task_id.clone()))
            .await
            .unwrap();

        let wrong_route = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-2".into()))
            .with_principal_id(Some("principal-1".into()));
        let err = status_goal(&store, &wrong_route, Some(started.task_id.clone()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not visible from this route"));

        let wrong_principal = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-2".into()));
        let err = status_goal(&store, &wrong_principal, Some(started.task_id.clone()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not visible to this principal"));
    }

    #[tokio::test]
    async fn goal_start_rejects_duplicate_active_context() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-1".into()));

        start_goal(&store, "boot-a", ctx.clone(), "ship it".into())
            .await
            .unwrap();
        let err = start_goal(&store, "boot-a", ctx, "ship another".into())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already active"));
    }

    #[tokio::test]
    async fn concurrent_goal_start_allows_one_active_context() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-1".into()));

        let (a, b) = tokio::join!(
            start_goal(&store, "boot-a", ctx.clone(), "ship one".into()),
            start_goal(&store, "boot-a", ctx, "ship two".into())
        );
        let successes = usize::from(a.is_ok()) + usize::from(b.is_ok());
        assert_eq!(successes, 1);
        let errors = [a.err(), b.err()]
            .into_iter()
            .flatten()
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("already active"));
    }

    #[tokio::test]
    async fn pause_and_resume_store_goal_specific_blockers_only() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a");
        let started = start_goal(&store, "boot-a", ctx.clone(), "ship it".into())
            .await
            .unwrap();

        let paused = pause_goal_for_blocker(
            &store,
            &ctx,
            Some(started.task_id.clone()),
            GoalPauseState {
                reason: GoalPauseReason::NeedsUserInput,
                description: Some("need answer".into()),
                blockers: vec![GoalBlocker {
                    kind: GoalBlockerKind::NeedsUserInput,
                    message: "Need operator answer".into(),
                    payload: Some(serde_json::json!({"prompt": "continue?"})),
                }],
            },
        )
        .await
        .unwrap();
        assert_eq!(paused.status, TaskStatus::Paused);
        let task = store.get(&started.task_id).await.unwrap().unwrap();
        let goal = store
            .get_goal_task(&started.task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::NeedsUserInput));
        assert_eq!(goal.blockers.len(), 1);

        let resumed = resume_goal(&store, &ctx, Some(started.task_id.clone()))
            .await
            .unwrap();
        assert_eq!(resumed.status, TaskStatus::Running);
        let task = store.get(&started.task_id).await.unwrap().unwrap();
        let goal = store
            .get_goal_task(&started.task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert!(goal.pause_reason.is_none());
        assert!(goal.blockers.is_empty());
    }
}
