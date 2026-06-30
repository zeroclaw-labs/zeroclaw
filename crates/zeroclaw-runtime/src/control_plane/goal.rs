//! Goal-mode admission and controller helpers.
//!
//! This module is the single Rust admission path for slash-command and
//! agent-callable goal starts. Callers pass trusted runtime context explicitly;
//! model/user text supplies only the untrusted objective/action payload.

use anyhow::{Context, Result, bail};
use zeroclaw_commands::CommandSurface;
use zeroclaw_config::schema::{AliasedAgentConfig, Config};

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
    Help,
    Start,
    Status,
    Budget,
    Pause,
    Resume,
    Cancel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum GoalBudgetValue<T> {
    #[default]
    Default,
    Unlimited,
    Limited(T),
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GoalBudgetOverrides {
    pub token_limit: GoalBudgetValue<u64>,
    pub cost_limit_usd: GoalBudgetValue<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoalCommand {
    pub action: GoalCommandAction,
    pub objective: Option<String>,
    pub task_id: Option<String>,
    pub budgets: GoalBudgetOverrides,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalAdmissionContext {
    pub agent_alias: String,
    pub command_surface: CommandSurface,
    pub channel_type: Option<String>,
    pub originator_route: Option<String>,
    pub principal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoalAdmission {
    pub task_id: Option<String>,
    pub status: TaskStatus,
    pub message: String,
}

impl GoalAdmissionContext {
    pub fn new(agent_alias: impl Into<String>) -> Self {
        Self {
            agent_alias: agent_alias.into(),
            command_surface: CommandSurface::Channel,
            channel_type: None,
            originator_route: None,
            principal_id: None,
        }
    }

    #[must_use]
    pub fn with_command_surface(mut self, command_surface: CommandSurface) -> Self {
        self.command_surface = command_surface;
        self
    }

    #[must_use]
    pub fn with_channel_type(mut self, channel_type: Option<String>) -> Self {
        self.channel_type = channel_type;
        self
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
        "help" | "--help" | "-h" => Ok(GoalCommand {
            action: GoalCommandAction::Help,
            objective: None,
            task_id: None,
            budgets: GoalBudgetOverrides::default(),
        }),
        "start" => {
            let (budgets, objective) = parse_start_payload(rest)?;
            if objective.is_empty() {
                bail!("{}", msg("goal-command-error-missing-objective", &[]));
            }
            Ok(GoalCommand {
                action: GoalCommandAction::Start,
                objective: Some(objective),
                task_id: None,
                budgets,
            })
        }
        "status" => Ok(GoalCommand {
            action: GoalCommandAction::Status,
            objective: None,
            task_id: nonempty(rest),
            budgets: GoalBudgetOverrides::default(),
        }),
        "budget" => {
            let budgets = parse_budget_payload(rest)?;
            Ok(GoalCommand {
                action: GoalCommandAction::Budget,
                objective: None,
                task_id: None,
                budgets,
            })
        }
        "pause" => Ok(GoalCommand {
            action: GoalCommandAction::Pause,
            objective: nonempty(rest),
            task_id: None,
            budgets: GoalBudgetOverrides::default(),
        }),
        "resume" => Ok(GoalCommand {
            action: GoalCommandAction::Resume,
            objective: None,
            task_id: nonempty(rest),
            budgets: GoalBudgetOverrides::default(),
        }),
        "cancel" => Ok(GoalCommand {
            action: GoalCommandAction::Cancel,
            objective: None,
            task_id: nonempty(rest),
            budgets: GoalBudgetOverrides::default(),
        }),
        other => {
            bail!(
                "{}",
                msg("goal-command-error-unknown-action", &[("action", other)])
            )
        }
    }
}

fn parse_start_payload(input: &str) -> Result<(GoalBudgetOverrides, String)> {
    let mut budgets = GoalBudgetOverrides::default();
    let mut rest = input.trim();
    while let Some(next) = rest.strip_prefix("--") {
        let (flag, tail) = next
            .split_once(char::is_whitespace)
            .map_or((next, ""), |(flag, tail)| (flag, tail.trim_start()));
        parse_budget_flag(flag, &mut budgets)?;
        rest = tail;
    }
    Ok((budgets, rest.trim().to_string()))
}

fn parse_budget_payload(input: &str) -> Result<GoalBudgetOverrides> {
    let mut budgets = GoalBudgetOverrides::default();
    let mut saw_value = false;
    for token in input.split_whitespace() {
        let flag = token.strip_prefix("--").ok_or_else(|| {
            anyhow::Error::msg(msg(
                "goal-command-error-invalid-budget-flag",
                &[("flag", token)],
            ))
        })?;
        parse_budget_flag(flag, &mut budgets)?;
        saw_value = true;
    }
    if !saw_value {
        bail!("{}", msg("goal-command-error-missing-budget", &[]));
    }
    Ok(budgets)
}

fn parse_budget_flag(flag: &str, budgets: &mut GoalBudgetOverrides) -> Result<()> {
    let (name, value) = flag.split_once('=').ok_or_else(|| {
        anyhow::Error::msg(msg(
            "goal-command-error-invalid-budget-flag",
            &[("flag", flag)],
        ))
    })?;
    match name {
        "tokens" => {
            budgets.token_limit = parse_token_budget_value(value)?;
            Ok(())
        }
        "cost" => {
            budgets.cost_limit_usd = parse_cost_budget_value(value)?;
            Ok(())
        }
        _ => bail!(
            "{}",
            msg("goal-command-error-invalid-budget-flag", &[("flag", flag)])
        ),
    }
}

fn parse_token_budget_value(value: &str) -> Result<GoalBudgetValue<u64>> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("unlimited") {
        return Ok(GoalBudgetValue::Unlimited);
    }
    let parsed = trimmed.parse::<u64>().map_err(|_| {
        anyhow::Error::msg(msg(
            "goal-command-error-invalid-token-budget",
            &[("value", trimmed)],
        ))
    })?;
    if parsed == 0 {
        bail!(
            "{}",
            msg(
                "goal-command-error-invalid-token-budget",
                &[("value", trimmed)]
            )
        );
    }
    Ok(GoalBudgetValue::Limited(parsed))
}

fn parse_cost_budget_value(value: &str) -> Result<GoalBudgetValue<f64>> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("unlimited") {
        return Ok(GoalBudgetValue::Unlimited);
    }
    let parsed = trimmed.parse::<f64>().map_err(|_| {
        anyhow::Error::msg(msg(
            "goal-command-error-invalid-cost-budget",
            &[("value", trimmed)],
        ))
    })?;
    if !parsed.is_finite() || parsed <= 0.0 {
        bail!(
            "{}",
            msg(
                "goal-command-error-invalid-cost-budget",
                &[("value", trimmed)]
            )
        );
    }
    Ok(GoalBudgetValue::Limited(parsed))
}

fn resolve_goal_limits(
    config: &Config,
    budgets: GoalBudgetOverrides,
) -> (Option<u64>, Option<f64>) {
    let token_limit = match budgets.token_limit {
        GoalBudgetValue::Default => config.goal.token_budget,
        GoalBudgetValue::Unlimited => None,
        GoalBudgetValue::Limited(value) => Some(value),
    };
    let cost_limit_usd = match budgets.cost_limit_usd {
        GoalBudgetValue::Default => config.goal.cost_budget_usd,
        GoalBudgetValue::Unlimited => None,
        GoalBudgetValue::Limited(value) => Some(value),
    };
    (token_limit, cost_limit_usd)
}

fn ensure_goal_admitted_by_config(
    ctx: &GoalAdmissionContext,
    config: &Config,
    agent_config: Option<&AliasedAgentConfig>,
) -> Result<()> {
    if !config.goal.enabled {
        bail!("{}", msg("goal-command-error-disabled", &[]));
    }
    if let Some(agent_config) = agent_config
        && !agent_config.goal.enabled
    {
        bail!("{}", msg("goal-command-error-agent-disabled", &[]));
    }
    let surface = ctx.command_surface.as_str();
    if !config
        .goal
        .allowed_command_surfaces
        .iter()
        .any(|candidate| candidate.trim() == surface)
    {
        bail!(
            "{}",
            msg(
                "goal-command-error-surface-disabled",
                &[("surface", surface)]
            )
        );
    }
    if ctx.command_surface == CommandSurface::Channel {
        let channel_type = ctx.channel_type.as_deref().unwrap_or("channel");
        if !config
            .goal
            .allowed_channel_types
            .iter()
            .any(|candidate| candidate.trim() == channel_type)
        {
            bail!(
                "{}",
                msg(
                    "goal-command-error-channel-disabled",
                    &[("channel_type", channel_type)]
                )
            );
        }
    }
    Ok(())
}

pub async fn admit_goal_command(
    ctx: GoalAdmissionContext,
    command: GoalCommand,
    config: &Config,
    agent_config: Option<&AliasedAgentConfig>,
) -> Result<GoalAdmission> {
    ensure_goal_admitted_by_config(&ctx, config, agent_config)?;
    if command.action == GoalCommandAction::Help {
        return Ok(GoalAdmission {
            task_id: None,
            status: TaskStatus::Running,
            message: msg("goal-command-help", &[]),
        });
    }
    let cp = control_plane()
        .with_context(|| msg("goal-command-error-control-plane-unavailable", &[]))?;
    match command.action {
        GoalCommandAction::Help => unreachable!("handled before control-plane access"),
        GoalCommandAction::Start => {
            let objective = command
                .objective
                .with_context(|| msg("goal-command-error-missing-objective", &[]))?;
            let (token_limit, cost_limit_usd) = resolve_goal_limits(config, command.budgets);
            start_goal(
                cp.store.as_ref(),
                &cp.boot_id,
                ctx,
                objective,
                token_limit,
                cost_limit_usd,
            )
            .await
        }
        GoalCommandAction::Status => status_goal(cp.store.as_ref(), &ctx, command.task_id).await,
        GoalCommandAction::Budget => {
            update_goal_budget(cp.store.as_ref(), &ctx, command.budgets).await
        }
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
    token_limit: Option<u64>,
    cost_limit_usd: Option<f64>,
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
                effective_token_limit: token_limit,
                effective_cost_limit_usd: cost_limit_usd,
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
        task_id: Some(task_id.clone()),
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
        task_id: Some(task.id.clone()),
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

async fn update_goal_budget(
    store: &dyn TaskRegistry,
    ctx: &GoalAdmissionContext,
    budgets: GoalBudgetOverrides,
) -> Result<GoalAdmission> {
    if matches!(budgets.token_limit, GoalBudgetValue::Default)
        && matches!(budgets.cost_limit_usd, GoalBudgetValue::Default)
    {
        bail!("{}", msg("goal-command-error-missing-budget", &[]));
    }
    let task = resolve_goal_task(store, ctx, None).await?;
    if task.status.is_terminal() {
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", &task.id), ("status", status_label(task.status))]
            )
        );
    }
    let current = store
        .get_goal_task(&task.id)
        .await
        .with_context(|| msg("goal-command-error-status-failed", &[]))?
        .with_context(|| {
            msg(
                "goal-command-error-extension-missing",
                &[("task_id", &task.id)],
            )
        })?;
    let token_limit = match budgets.token_limit {
        GoalBudgetValue::Default => current.effective_token_limit,
        GoalBudgetValue::Unlimited => None,
        GoalBudgetValue::Limited(value) => Some(value),
    };
    let cost_limit_usd = match budgets.cost_limit_usd {
        GoalBudgetValue::Default => current.effective_cost_limit_usd,
        GoalBudgetValue::Unlimited => None,
        GoalBudgetValue::Limited(value) => Some(value),
    };
    store
        .update_goal_limits(&task.id, token_limit, cost_limit_usd)
        .await
        .with_context(|| msg("goal-command-error-budget-failed", &[("task_id", &task.id)]))?;
    Ok(GoalAdmission {
        task_id: Some(task.id.clone()),
        status: task.status,
        message: msg("goal-command-budget-updated", &[("task_id", &task.id)]),
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
        task_id: Some(task.id.clone()),
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
        task_id: Some(task.id.clone()),
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
        task_id: Some(task.id.clone()),
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

    fn test_config() -> Config {
        let mut config = Config::default();
        config.goal.allowed_channel_types.push("channel".into());
        config
    }

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

    #[test]
    fn parse_goal_help_and_budget_flags() {
        let help = parse_goal_command("/goal --help").unwrap();
        assert_eq!(help.action, GoalCommandAction::Help);

        let start = parse_goal_command("/goal start --tokens=50000 --cost=2.50 ship it").unwrap();
        assert_eq!(start.objective.as_deref(), Some("ship it"));
        assert_eq!(start.budgets.token_limit, GoalBudgetValue::Limited(50_000));
        assert_eq!(start.budgets.cost_limit_usd, GoalBudgetValue::Limited(2.50));

        let budget = parse_goal_command("/goal budget --tokens=unlimited --cost=1.25").unwrap();
        assert_eq!(budget.action, GoalCommandAction::Budget);
        assert_eq!(budget.budgets.token_limit, GoalBudgetValue::Unlimited);
        assert_eq!(
            budget.budgets.cost_limit_usd,
            GoalBudgetValue::Limited(1.25)
        );
    }

    #[test]
    fn goal_policy_rejects_disabled_global_and_agent_config() {
        let ctx = GoalAdmissionContext::new("agent-a").with_channel_type(Some("channel".into()));
        let mut config = test_config();
        config.goal.enabled = false;
        let err = ensure_goal_admitted_by_config(&ctx, &config, None).unwrap_err();
        assert!(err.to_string().contains("disabled"));

        config.goal.enabled = true;
        let mut agent = AliasedAgentConfig::default();
        agent.goal.enabled = false;
        let err = ensure_goal_admitted_by_config(&ctx, &config, Some(&agent)).unwrap_err();
        assert!(err.to_string().contains("disabled for this agent"));
    }

    #[test]
    fn goal_policy_rejects_disallowed_surface_and_channel_type() {
        let mut config = test_config();
        config.goal.allowed_command_surfaces = vec!["web".into()];
        let ctx = GoalAdmissionContext::new("agent-a").with_channel_type(Some("channel".into()));
        let err = ensure_goal_admitted_by_config(&ctx, &config, None).unwrap_err();
        assert!(err.to_string().contains("command surface `channel`"));

        config.goal.allowed_command_surfaces = vec!["channel".into()];
        config.goal.allowed_channel_types = vec!["telegram".into()];
        let err = ensure_goal_admitted_by_config(&ctx, &config, None).unwrap_err();
        assert!(err.to_string().contains("channel type `channel`"));
    }

    #[tokio::test]
    async fn goal_start_resolves_config_default_and_explicit_budget_limits() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a").with_channel_type(Some("channel".into()));
        let mut config = test_config();
        config.goal.token_budget = Some(12_000);
        config.goal.cost_budget_usd = Some(3.25);
        let (token_limit, cost_limit_usd) = resolve_goal_limits(
            &config,
            GoalBudgetOverrides {
                token_limit: GoalBudgetValue::Unlimited,
                cost_limit_usd: GoalBudgetValue::Default,
            },
        );

        let started = start_goal(
            &store,
            "boot-a",
            ctx,
            "ship it".into(),
            token_limit,
            cost_limit_usd,
        )
        .await
        .unwrap();
        let task_id = started.task_id.unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(goal.effective_token_limit, None);
        assert_eq!(goal.effective_cost_limit_usd, Some(3.25));
    }

    #[tokio::test]
    async fn goal_lifecycle_uses_task_record_for_status() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-1".into()));
        let started = start_goal(&store, "boot-a", ctx.clone(), "ship it".into(), None, None)
            .await
            .unwrap();
        let task_id = started.task_id.clone().unwrap();
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();

        assert_eq!(task.kind, TaskKind::Goal);
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.originator_route.as_deref(), Some("telegram:chat-1"));
        assert_eq!(task.principal_id.as_deref(), Some("principal-1"));
        assert_eq!(goal.objective, "ship it");
        assert!(goal.effective_token_limit.is_none());

        let cancelled = cancel_goal(&store, &ctx, Some(task_id.clone()))
            .await
            .unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(
            store.get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Cancelled
        );

        let err = cancel_goal(&store, &ctx, Some(task_id)).await.unwrap_err();
        assert!(err.to_string().contains("already terminal"));
    }

    #[tokio::test]
    async fn goal_visibility_enforces_route_and_principal() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let owner = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-1".into()));
        let started = start_goal(
            &store,
            "boot-a",
            owner.clone(),
            "ship it".into(),
            None,
            None,
        )
        .await
        .unwrap();
        let task_id = started.task_id.clone().unwrap();

        status_goal(&store, &owner, Some(task_id.clone()))
            .await
            .unwrap();

        let wrong_route = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-2".into()))
            .with_principal_id(Some("principal-1".into()));
        let err = status_goal(&store, &wrong_route, Some(task_id.clone()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not visible from this route"));

        let wrong_principal = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-2".into()));
        let err = status_goal(&store, &wrong_principal, Some(task_id))
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

        start_goal(&store, "boot-a", ctx.clone(), "ship it".into(), None, None)
            .await
            .unwrap();
        let err = start_goal(&store, "boot-a", ctx, "ship another".into(), None, None)
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
            start_goal(&store, "boot-a", ctx.clone(), "ship one".into(), None, None),
            start_goal(&store, "boot-a", ctx, "ship two".into(), None, None)
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
        let started = start_goal(&store, "boot-a", ctx.clone(), "ship it".into(), None, None)
            .await
            .unwrap();
        let task_id = started.task_id.clone().unwrap();

        let paused = pause_goal_for_blocker(
            &store,
            &ctx,
            Some(task_id.clone()),
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
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::NeedsUserInput));
        assert_eq!(goal.blockers.len(), 1);

        let resumed = resume_goal(&store, &ctx, Some(task_id.clone()))
            .await
            .unwrap();
        assert_eq!(resumed.status, TaskStatus::Running);
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert!(goal.pause_reason.is_none());
        assert!(goal.blockers.is_empty());
    }
}
