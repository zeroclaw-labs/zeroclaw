//! Goal-mode admission and controller helpers.
//!
//! This module is the single Rust admission path for slash-command and
//! agent-callable goal starts. Callers pass trusted runtime context explicitly;
//! model/user text supplies only the untrusted objective/action payload.

use anyhow::{Context, Result, bail};
use zeroclaw_commands::CommandSurface;
use zeroclaw_config::cost::CostTracker;
use zeroclaw_config::schema::{AliasedAgentConfig, Config};

use super::global::control_plane;
use super::task_registry::{
    GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState, GoalTaskRecord,
    TaskContinuationContext, TaskKind, TaskRecord, TaskRegistry, TaskStatus,
};
use super::verifier::{GoalVerifierDecision, verifier_outage_pause, verify_goal_completion};

tokio::task_local! {
    static GOAL_ADMISSION_CONTEXT: Option<GoalAdmissionContext>;
    static GOAL_STATE_UPDATE_SINK: Option<GoalStateUpdateSink>;
}

#[derive(Clone)]
pub struct GoalStateUpdateSink {
    tx: tokio::sync::mpsc::UnboundedSender<GoalStateUpdateEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalStateUpdateEvent {
    Status(String),
    VerifierStarted(String),
}

impl GoalStateUpdateSink {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<GoalStateUpdateEvent>) -> Self {
        Self { tx }
    }

    pub fn send(&self, event: GoalStateUpdateEvent) {
        let _ = self.tx.send(event);
    }
}

fn msg(key: &str, args: &[(&str, &str)]) -> String {
    crate::i18n::get_required_cli_string_with_args(key, args)
}

fn enum_label<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn formatted_cost(value: f64) -> String {
    let amount = format!("{value:.4}");
    msg("goal-budget-cost-value", &[("amount", &amount)])
}

fn token_limit_label(limit: Option<u64>) -> String {
    limit
        .map(|value| value.to_string())
        .unwrap_or_else(|| msg("goal-budget-limit-unlimited", &[]))
}

fn cost_limit_label(limit: Option<f64>) -> String {
    limit
        .map(formatted_cost)
        .unwrap_or_else(|| msg("goal-budget-limit-unlimited", &[]))
}

#[derive(Debug, Clone, Copy, Default)]
struct GoalUsageTotals {
    total_tokens: u64,
    cost_usd: f64,
}

fn goal_usage_totals(config: Option<&Config>, task_id: &str) -> Option<GoalUsageTotals> {
    let tracker = enabled_cost_tracker(config)?;
    match tracker.get_usage_totals_for_goal(task_id) {
        Ok((total_tokens, cost_usd)) => Some(GoalUsageTotals {
            total_tokens,
            cost_usd,
        }),
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "task_id": task_id,
                        "error": format!("{error}"),
                    })),
                "Failed to derive goal usage summary"
            );
            None
        }
    }
}

fn initial_goal_usage_totals(config: Option<&Config>) -> Option<GoalUsageTotals> {
    if config.is_none() {
        return Some(GoalUsageTotals::default());
    }
    enabled_cost_tracker(config).map(|_| GoalUsageTotals::default())
}

fn enabled_cost_tracker(config: Option<&Config>) -> Option<std::sync::Arc<CostTracker>> {
    let config = config?;
    let tracker = CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir)?;
    tracker.is_enabled().then_some(tracker)
}

fn goal_budget_summary(goal: &GoalTaskRecord, usage: Option<&GoalUsageTotals>) -> String {
    let token_limit = token_limit_label(goal.effective_token_limit);
    let cost_limit = cost_limit_label(goal.effective_cost_limit_usd);
    if let Some(usage) = usage {
        let tokens_used = usage.total_tokens.to_string();
        let cost_used = formatted_cost(usage.cost_usd);
        msg(
            "goal-budget-summary",
            &[
                ("tokens_used", &tokens_used),
                ("token_limit", &token_limit),
                ("cost_used", &cost_used),
                ("cost_limit", &cost_limit),
            ],
        )
    } else {
        msg(
            "goal-budget-summary-unavailable",
            &[("token_limit", &token_limit), ("cost_limit", &cost_limit)],
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GoalBudgetExhaustion {
    tokens: bool,
    cost: bool,
}

fn goal_budget_exhaustion(
    goal: &GoalTaskRecord,
    usage: Option<&GoalUsageTotals>,
) -> Option<GoalBudgetExhaustion> {
    let usage = usage?;
    let tokens = goal
        .effective_token_limit
        .is_some_and(|limit| usage.total_tokens >= limit);
    let cost = goal
        .effective_cost_limit_usd
        .is_some_and(|limit| usage.cost_usd >= limit);
    (tokens || cost).then_some(GoalBudgetExhaustion { tokens, cost })
}

fn goal_has_effective_budget(goal: &GoalTaskRecord) -> bool {
    goal.effective_token_limit.is_some() || goal.effective_cost_limit_usd.is_some()
}

fn goal_budget_pause(
    goal: &GoalTaskRecord,
    usage: Option<&GoalUsageTotals>,
) -> Option<GoalPauseState> {
    let usage = usage?;
    let exhaustion = goal_budget_exhaustion(goal, Some(usage))?;
    let budget = goal_budget_summary(goal, Some(usage));
    Some(GoalPauseState {
        reason: GoalPauseReason::BudgetExhausted,
        description: Some(msg(
            "goal-command-budget-exhausted-description",
            &[("budget", &budget)],
        )),
        blockers: vec![GoalBlocker {
            kind: GoalBlockerKind::Budget,
            message: msg(
                "goal-command-budget-exhausted-blocker",
                &[("budget", &budget)],
            ),
            payload: Some(serde_json::json!({
                "tokens": {
                    "exhausted": exhaustion.tokens,
                    "used": usage.total_tokens,
                    "limit": goal.effective_token_limit,
                },
                "cost": {
                    "exhausted": exhaustion.cost,
                    "used_usd": usage.cost_usd,
                    "limit_usd": goal.effective_cost_limit_usd,
                },
            })),
        }],
    })
}

fn goal_budget_unavailable_pause(goal: &GoalTaskRecord) -> Option<GoalPauseState> {
    if !goal_has_effective_budget(goal) {
        return None;
    }
    let budget = goal_budget_summary(goal, None);
    Some(GoalPauseState {
        reason: GoalPauseReason::BudgetUnavailable,
        description: Some(msg(
            "goal-command-budget-unavailable-description",
            &[("budget", &budget)],
        )),
        blockers: vec![GoalBlocker {
            kind: GoalBlockerKind::Budget,
            message: msg(
                "goal-command-budget-unavailable-blocker",
                &[("budget", &budget)],
            ),
            payload: Some(serde_json::json!({
                "usage_unavailable": true,
                "token_limit": goal.effective_token_limit,
                "cost_limit_usd": goal.effective_cost_limit_usd,
            })),
        }],
    })
}

fn goal_budget_gate_pause(
    goal: &GoalTaskRecord,
    usage: Option<&GoalUsageTotals>,
) -> Option<GoalPauseState> {
    match usage {
        Some(usage) => goal_budget_pause(goal, Some(usage)),
        None => goal_budget_unavailable_pause(goal),
    }
}

fn reason_for_blocker_kind(kind: GoalBlockerKind) -> GoalPauseReason {
    match kind {
        GoalBlockerKind::NeedsUserInput => GoalPauseReason::NeedsUserInput,
        GoalBlockerKind::HumanEscalation => GoalPauseReason::HumanEscalation,
        GoalBlockerKind::ExternalDependency => GoalPauseReason::ExternalDependency,
        GoalBlockerKind::Provider => GoalPauseReason::ProviderUnavailable,
        GoalBlockerKind::Verifier => GoalPauseReason::VerifierBlocked,
        GoalBlockerKind::Budget => GoalPauseReason::BudgetExhausted,
        GoalBlockerKind::RestartRecovery => GoalPauseReason::DaemonRestart,
    }
}

fn is_budget_pause_reason(reason: Option<GoalPauseReason>) -> bool {
    matches!(
        reason,
        Some(GoalPauseReason::BudgetExhausted | GoalPauseReason::BudgetUnavailable)
    )
}

fn merge_budget_pause(goal: &GoalTaskRecord, budget_pause: GoalPauseState) -> GoalPauseState {
    let mut blockers: Vec<_> = goal
        .blockers
        .iter()
        .filter(|blocker| blocker.kind != GoalBlockerKind::Budget)
        .cloned()
        .collect();
    blockers.extend(budget_pause.blockers);
    GoalPauseState {
        reason: goal.pause_reason.unwrap_or(budget_pause.reason),
        description: goal.pause_description.clone().or(budget_pause.description),
        blockers,
    }
}

fn remove_budget_pause(goal: &GoalTaskRecord) -> Option<GoalPauseState> {
    let blockers: Vec<_> = goal
        .blockers
        .iter()
        .filter(|blocker| blocker.kind != GoalBlockerKind::Budget)
        .cloned()
        .collect();
    if blockers.is_empty() {
        return None;
    }
    let reason = if is_budget_pause_reason(goal.pause_reason) {
        reason_for_blocker_kind(blockers[0].kind)
    } else {
        goal.pause_reason
            .unwrap_or_else(|| reason_for_blocker_kind(blockers[0].kind))
    };
    Some(GoalPauseState {
        reason,
        description: goal.pause_description.clone(),
        blockers,
    })
}

fn has_budget_blocker(goal: &GoalTaskRecord) -> bool {
    goal.blockers
        .iter()
        .any(|blocker| blocker.kind == GoalBlockerKind::Budget)
}

fn has_budget_pause(goal: &GoalTaskRecord) -> bool {
    is_budget_pause_reason(goal.pause_reason) || has_budget_blocker(goal)
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
    pub continuation_context: Option<TaskContinuationContext>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoalAdmission {
    pub task_id: Option<String>,
    pub status: TaskStatus,
    pub message: String,
    pub continue_goal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalTurnEvaluation {
    Completed {
        task_id: String,
        message: String,
    },
    Continue {
        task_id: String,
        objective: String,
        notes: String,
        message: String,
    },
    Paused {
        task_id: String,
        message: String,
    },
}

impl GoalAdmissionContext {
    pub fn new(agent_alias: impl Into<String>) -> Self {
        Self {
            agent_alias: agent_alias.into(),
            command_surface: CommandSurface::Channel,
            channel_type: None,
            originator_route: None,
            principal_id: None,
            continuation_context: None,
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

    #[must_use]
    pub fn with_continuation_context(mut self, context: Option<TaskContinuationContext>) -> Self {
        self.continuation_context = context;
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

pub async fn scope_goal_state_updates<F>(sink: Option<GoalStateUpdateSink>, future: F) -> F::Output
where
    F: std::future::Future,
{
    GOAL_STATE_UPDATE_SINK.scope(sink, future).await
}

fn publish_goal_state_update(admission: &GoalAdmission) {
    let _ = GOAL_STATE_UPDATE_SINK.try_with(|sink| {
        if let Some(sink) = sink {
            let message = msg(
                "channel-goal-state-update",
                &[("message", &admission.message)],
            );
            sink.send(GoalStateUpdateEvent::Status(message));
        }
    });
}

fn publish_goal_verifier_started(task_id: &str, budget: &str) {
    let message = msg(
        "goal-command-verifying",
        &[("task_id", task_id), ("budget", budget)],
    );
    let _ = GOAL_STATE_UPDATE_SINK.try_with(|sink| {
        if let Some(sink) = sink {
            sink.send(GoalStateUpdateEvent::VerifierStarted(message));
        }
    });
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
            continue_goal: false,
        });
    }
    let cp = control_plane()
        .with_context(|| msg("goal-command-error-control-plane-unavailable", &[]))?;
    let admission = match command.action {
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
                Some(config),
            )
            .await
        }
        GoalCommandAction::Status => {
            status_goal(cp.store.as_ref(), &ctx, command.task_id, Some(config)).await
        }
        GoalCommandAction::Budget => {
            update_goal_budget(
                cp.store.as_ref(),
                &cp.boot_id,
                &ctx,
                command.budgets,
                Some(config),
            )
            .await
        }
        GoalCommandAction::Pause => {
            let description = command.objective;
            pause_goal_for_blocker(
                cp.store.as_ref(),
                &ctx,
                command.task_id,
                Some(config),
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
        GoalCommandAction::Resume => {
            resume_goal(
                cp.store.as_ref(),
                &cp.boot_id,
                &ctx,
                command.task_id,
                Some(config),
            )
            .await
        }
        GoalCommandAction::Cancel => {
            cancel_goal(cp.store.as_ref(), &ctx, command.task_id, Some(config)).await
        }
    }?;
    publish_goal_state_update(&admission);
    Ok(admission)
}

pub async fn evaluate_goal_turn(
    ctx: &GoalAdmissionContext,
    config: &Config,
    candidate_summary: &str,
) -> Result<Option<GoalTurnEvaluation>> {
    let cp = match control_plane() {
        Some(cp) => cp,
        None => return Ok(None),
    };
    let Some(task) = cp
        .store
        .latest_active_goal_for_context(
            &ctx.agent_alias,
            ctx.originator_route.as_deref(),
            ctx.principal_id.as_deref(),
        )
        .await
        .with_context(|| msg("goal-command-error-active-goal-lookup-failed", &[]))?
    else {
        return Ok(None);
    };
    ensure_goal_visible(&task, ctx)?;
    if task.status != TaskStatus::Running {
        return Ok(None);
    }
    let goal = cp
        .store
        .get_goal_task(&task.id)
        .await
        .with_context(|| msg("goal-command-error-status-failed", &[]))?
        .with_context(|| {
            msg(
                "goal-command-error-extension-missing",
                &[("task_id", &task.id)],
            )
        })?;

    let usage = goal_usage_totals(Some(config), &task.id);
    if let Some(pause) = goal_budget_gate_pause(&goal, usage.as_ref()) {
        let task_id = task.id.clone();
        let budget = goal_budget_summary(&goal, usage.as_ref());
        let admission =
            pause_goal_for_resolved_task_with_budget(cp.store.as_ref(), task, pause, budget)
                .await?;
        publish_goal_state_update(&admission);
        return Ok(Some(GoalTurnEvaluation::Paused {
            task_id,
            message: admission.message,
        }));
    }

    let verifier_enabled = config.goal.verifier.enabled;
    if verifier_enabled {
        let budget = goal_budget_summary(&goal, usage.as_ref());
        publish_goal_verifier_started(&task.id, &budget);
    }

    match verify_goal_completion(config, &task.agent, ctx, &goal, candidate_summary).await {
        Ok(GoalVerifierDecision::Complete { notes: _ }) => {
            cp.store
                .update_status(
                    &task.id,
                    TaskStatus::Completed,
                    Some(candidate_summary.to_string()),
                    None,
                )
                .await
                .with_context(|| {
                    msg("goal-command-error-update-failed", &[("task_id", &task.id)])
                })?;
            let final_usage = if verifier_enabled {
                goal_usage_totals(Some(config), &task.id)
            } else {
                usage
            };
            let budget = goal_budget_summary(&goal, final_usage.as_ref());
            let admission = GoalAdmission {
                task_id: Some(task.id.clone()),
                status: TaskStatus::Completed,
                message: msg(
                    "goal-command-completed",
                    &[("task_id", &task.id), ("budget", &budget)],
                ),
                continue_goal: false,
            };
            publish_goal_state_update(&admission);
            Ok(Some(GoalTurnEvaluation::Completed {
                task_id: task.id,
                message: admission.message,
            }))
        }
        Ok(GoalVerifierDecision::Continue { notes }) => {
            let usage = goal_usage_totals(Some(config), &task.id);
            if let Some(pause) = goal_budget_gate_pause(&goal, usage.as_ref()) {
                let budget = goal_budget_summary(&goal, usage.as_ref());
                let task_id = task.id.clone();
                let admission = pause_goal_for_resolved_task_with_budget(
                    cp.store.as_ref(),
                    task,
                    pause,
                    budget,
                )
                .await?;
                publish_goal_state_update(&admission);
                return Ok(Some(GoalTurnEvaluation::Paused {
                    task_id,
                    message: admission.message,
                }));
            }
            let budget = goal_budget_summary(&goal, usage.as_ref());
            let admission = GoalAdmission {
                task_id: Some(task.id.clone()),
                status: TaskStatus::Running,
                message: msg(
                    "goal-command-continuing",
                    &[("task_id", &task.id), ("budget", &budget)],
                ),
                continue_goal: true,
            };
            publish_goal_state_update(&admission);
            Ok(Some(GoalTurnEvaluation::Continue {
                task_id: task.id,
                objective: goal.objective,
                notes,
                message: admission.message,
            }))
        }
        Ok(GoalVerifierDecision::Blocked { pause }) => {
            let admission = pause_goal_for_known_blocker(
                cp.store.as_ref(),
                task.clone(),
                &goal,
                Some(config),
                pause,
            )
            .await?;
            publish_goal_state_update(&admission);
            Ok(Some(GoalTurnEvaluation::Paused {
                task_id: task.id,
                message: admission.message,
            }))
        }
        Err(error) => {
            let admission = pause_goal_for_known_blocker(
                cp.store.as_ref(),
                task.clone(),
                &goal,
                Some(config),
                verifier_outage_pause(&error),
            )
            .await?;
            publish_goal_state_update(&admission);
            Ok(Some(GoalTurnEvaluation::Paused {
                task_id: task.id,
                message: admission.message,
            }))
        }
    }
}

async fn start_goal(
    store: &dyn TaskRegistry,
    boot_id: &str,
    ctx: GoalAdmissionContext,
    objective: String,
    token_limit: Option<u64>,
    cost_limit_usd: Option<f64>,
    config: Option<&Config>,
) -> Result<GoalAdmission> {
    let continuation_context = ctx.continuation_context.clone();
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
    let mut goal = GoalTaskRecord {
        task_id: task_id.clone(),
        objective,
        effective_token_limit: token_limit,
        effective_cost_limit_usd: cost_limit_usd,
        pause_reason: None,
        pause_description: None,
        blockers: Vec::new(),
    };
    let initial_usage = initial_goal_usage_totals(config);
    let initial_pause = config.and_then(|_| goal_budget_gate_pause(&goal, initial_usage.as_ref()));
    let (status, continue_goal, message_key) = if let Some(pause) = initial_pause {
        goal.pause_reason = Some(pause.reason);
        goal.pause_description = pause.description;
        goal.blockers = pause.blockers;
        (
            TaskStatus::Paused,
            false,
            "goal-command-started-budget-unavailable",
        )
    } else {
        (TaskStatus::Running, true, "goal-command-started")
    };
    let budget = goal_budget_summary(&goal, initial_usage.as_ref());
    store
        .create_goal(
            TaskRecord {
                id: task_id.clone(),
                kind: TaskKind::Goal,
                agent: ctx.agent_alias,
                status,
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
            goal,
            continuation_context,
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
        status,
        message: msg(message_key, &[("task_id", &task_id), ("budget", &budget)]),
        continue_goal,
    })
}

async fn status_goal(
    store: &dyn TaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
    config: Option<&Config>,
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
    let usage = goal_usage_totals(config, &task.id);
    let budget = goal_budget_summary(&goal, usage.as_ref());
    Ok(GoalAdmission {
        task_id: Some(task.id.clone()),
        status: task.status,
        message: if let Some(reason) = goal.pause_reason {
            let status = enum_label(task.status);
            let reason = enum_label(reason);
            msg(
                "goal-command-status-paused",
                &[
                    ("task_id", &task.id),
                    ("status", &status),
                    ("objective", &goal.objective),
                    ("reason", &reason),
                    ("budget", &budget),
                ],
            )
        } else {
            let status = enum_label(task.status);
            msg(
                "goal-command-status",
                &[
                    ("task_id", &task.id),
                    ("status", &status),
                    ("objective", &goal.objective),
                    ("budget", &budget),
                ],
            )
        },
        continue_goal: false,
    })
}

async fn update_goal_budget(
    store: &dyn TaskRegistry,
    boot_id: &str,
    ctx: &GoalAdmissionContext,
    budgets: GoalBudgetOverrides,
    config: Option<&Config>,
) -> Result<GoalAdmission> {
    if matches!(budgets.token_limit, GoalBudgetValue::Default)
        && matches!(budgets.cost_limit_usd, GoalBudgetValue::Default)
    {
        bail!("{}", msg("goal-command-error-missing-budget", &[]));
    }
    let task = resolve_goal_task(store, ctx, None).await?;
    if task.status.is_terminal() {
        let status = enum_label(task.status);
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", &task.id), ("status", &status)]
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
    let updated_goal = GoalTaskRecord {
        effective_token_limit: token_limit,
        effective_cost_limit_usd: cost_limit_usd,
        ..current
    };
    let usage = goal_usage_totals(config, &task.id);
    let budget = goal_budget_summary(&updated_goal, usage.as_ref());
    if let Some(budget_pause) = goal_budget_gate_pause(&updated_goal, usage.as_ref()) {
        store
            .update_goal_pause(
                &task.id,
                Some(merge_budget_pause(&updated_goal, budget_pause)),
            )
            .await
            .with_context(|| msg("goal-command-error-budget-failed", &[("task_id", &task.id)]))?;
        store
            .update_status(&task.id, TaskStatus::Paused, None, None)
            .await
            .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task.id)]))?;
        return Ok(GoalAdmission {
            task_id: Some(task.id.clone()),
            status: TaskStatus::Paused,
            message: msg(
                "goal-command-budget-updated-paused",
                &[("task_id", &task.id), ("budget", &budget)],
            ),
            continue_goal: false,
        });
    }

    if task.status == TaskStatus::Paused && has_budget_pause(&updated_goal) {
        if let Some(pause) = remove_budget_pause(&updated_goal) {
            store
                .update_goal_pause(&task.id, Some(pause))
                .await
                .with_context(|| {
                    msg("goal-command-error-budget-failed", &[("task_id", &task.id)])
                })?;
            return Ok(GoalAdmission {
                task_id: Some(task.id.clone()),
                status: TaskStatus::Paused,
                message: msg(
                    "goal-command-budget-updated-paused",
                    &[("task_id", &task.id), ("budget", &budget)],
                ),
                continue_goal: false,
            });
        }

        if let Some(context) = ctx.continuation_context.clone() {
            store
                .set_continuation_context(&task.id, Some(context))
                .await
                .with_context(|| {
                    msg("goal-command-error-budget-failed", &[("task_id", &task.id)])
                })?;
        }
        store
            .update_goal_pause(&task.id, None)
            .await
            .with_context(|| msg("goal-command-error-budget-failed", &[("task_id", &task.id)]))?;
        store
            .claim_owner(&task.id, std::process::id(), boot_id)
            .await
            .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task.id)]))?;
        store
            .update_status(&task.id, TaskStatus::Running, None, None)
            .await
            .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task.id)]))?;
        return Ok(GoalAdmission {
            task_id: Some(task.id.clone()),
            status: TaskStatus::Running,
            message: msg(
                "goal-command-budget-updated-resumed",
                &[("task_id", &task.id), ("budget", &budget)],
            ),
            continue_goal: true,
        });
    }

    Ok(GoalAdmission {
        task_id: Some(task.id.clone()),
        status: task.status,
        message: msg(
            "goal-command-budget-updated",
            &[("task_id", &task.id), ("budget", &budget)],
        ),
        continue_goal: false,
    })
}

async fn pause_goal_for_blocker(
    store: &dyn TaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
    config: Option<&Config>,
    pause: GoalPauseState,
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
    let usage = goal_usage_totals(config, &task.id);
    let budget = goal_budget_summary(&goal, usage.as_ref());
    pause_goal_for_resolved_task_with_budget(store, task, pause, budget).await
}

async fn pause_goal_for_known_blocker(
    store: &dyn TaskRegistry,
    task: TaskRecord,
    goal: &GoalTaskRecord,
    config: Option<&Config>,
    pause: GoalPauseState,
) -> Result<GoalAdmission> {
    let usage = goal_usage_totals(config, &task.id);
    let budget = goal_budget_summary(goal, usage.as_ref());
    pause_goal_for_resolved_task_with_budget(store, task, pause, budget).await
}

async fn pause_goal_for_resolved_task_with_budget(
    store: &dyn TaskRegistry,
    task: TaskRecord,
    pause: GoalPauseState,
    budget: String,
) -> Result<GoalAdmission> {
    ensure_goal_not_terminal(&task)?;
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
        message: msg(
            "goal-command-paused",
            &[("task_id", &task.id), ("budget", &budget)],
        ),
        continue_goal: false,
    })
}

fn ensure_goal_not_terminal(task: &TaskRecord) -> Result<()> {
    if task.status.is_terminal() {
        let status = enum_label(task.status);
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", &task.id), ("status", &status)]
            )
        );
    }
    Ok(())
}

async fn resume_goal(
    store: &dyn TaskRegistry,
    boot_id: &str,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
    config: Option<&Config>,
) -> Result<GoalAdmission> {
    let task = resolve_goal_task(store, ctx, task_id).await?;
    if task.status.is_terminal() {
        let status = enum_label(task.status);
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", &task.id), ("status", &status)]
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
    let current_usage = goal_usage_totals(config, &task.id);
    if let Some(budget_pause) = goal_budget_gate_pause(&current, current_usage.as_ref()) {
        let message_key = match budget_pause.reason {
            GoalPauseReason::BudgetUnavailable => "goal-command-budget-unavailable",
            _ => "goal-command-budget-exhausted",
        };
        let budget = goal_budget_summary(&current, current_usage.as_ref());
        store
            .update_goal_pause(&task.id, Some(merge_budget_pause(&current, budget_pause)))
            .await
            .with_context(|| msg("goal-command-error-resume-failed", &[("task_id", &task.id)]))?;
        store
            .update_status(&task.id, TaskStatus::Paused, None, None)
            .await
            .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task.id)]))?;
        return Ok(GoalAdmission {
            task_id: Some(task.id.clone()),
            status: TaskStatus::Paused,
            message: msg(message_key, &[("task_id", &task.id), ("budget", &budget)]),
            continue_goal: false,
        });
    }
    if let Some(context) = ctx.continuation_context.clone() {
        store
            .set_continuation_context(&task.id, Some(context))
            .await
            .with_context(|| msg("goal-command-error-resume-failed", &[("task_id", &task.id)]))?;
    }
    store
        .update_goal_pause(&task.id, None)
        .await
        .with_context(|| msg("goal-command-error-resume-failed", &[("task_id", &task.id)]))?;
    store
        .claim_owner(&task.id, std::process::id(), boot_id)
        .await
        .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task.id)]))?;
    store
        .update_status(&task.id, TaskStatus::Running, None, None)
        .await
        .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task.id)]))?;
    let budget = goal_budget_summary(&current, current_usage.as_ref());
    Ok(GoalAdmission {
        task_id: Some(task.id.clone()),
        status: TaskStatus::Running,
        message: msg(
            "goal-command-resumed",
            &[("task_id", &task.id), ("budget", &budget)],
        ),
        continue_goal: true,
    })
}

async fn cancel_goal(
    store: &dyn TaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
    config: Option<&Config>,
) -> Result<GoalAdmission> {
    let task = resolve_goal_task(store, ctx, task_id).await?;
    if task.status.is_terminal() {
        let status = enum_label(task.status);
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", &task.id), ("status", &status)]
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
    let usage = goal_usage_totals(config, &task.id);
    let budget = goal_budget_summary(&goal, usage.as_ref());
    Ok(GoalAdmission {
        task_id: Some(task.id.clone()),
        status: TaskStatus::Cancelled,
        message: msg(
            "goal-command-cancelled",
            &[("task_id", &task.id), ("budget", &budget)],
        ),
        continue_goal: false,
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
    use crate::control_plane::TaskContinuationConversationScope;
    use crate::control_plane::task_store_sqlite::SqliteTaskStore;
    use std::sync::Arc;

    fn test_config() -> Config {
        let mut config = Config::default();
        config.goal.allowed_channel_types.push("channel".into());
        config.cost.enabled = false;
        config
    }

    fn global_test_store() -> Arc<dyn TaskRegistry> {
        match crate::control_plane::control_plane() {
            Some(control_plane) => Arc::clone(&control_plane.store),
            None => {
                let store: Arc<dyn TaskRegistry> =
                    Arc::new(crate::control_plane::SqliteTaskStore::new_in_memory().unwrap());
                let _ = crate::control_plane::init_control_plane(
                    crate::control_plane::ControlPlaneHandle {
                        store: Arc::clone(&store),
                        boot_id: "test-boot".into(),
                        recovered_goal_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
                    },
                );
                Arc::clone(&crate::control_plane::control_plane().unwrap().store)
            }
        }
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
    fn goal_budget_pause_marks_exhausted_dimensions_from_ledger_summary() {
        let goal = GoalTaskRecord {
            task_id: "goal-1".into(),
            objective: "ship it".into(),
            effective_token_limit: Some(1_000),
            effective_cost_limit_usd: Some(0.50),
            pause_reason: None,
            pause_description: None,
            blockers: Vec::new(),
        };
        let usage = GoalUsageTotals {
            cost_usd: 0.75,
            total_tokens: 1_000,
            ..GoalUsageTotals::default()
        };

        let pause = goal_budget_pause(&goal, Some(&usage)).unwrap();

        assert_eq!(pause.reason, GoalPauseReason::BudgetExhausted);
        assert_eq!(pause.blockers.len(), 1);
        assert_eq!(pause.blockers[0].kind, GoalBlockerKind::Budget);
        let payload = pause.blockers[0].payload.as_ref().unwrap();
        assert_eq!(payload["tokens"]["exhausted"], true);
        assert_eq!(payload["cost"]["exhausted"], true);
        assert!(pause.blockers[0].message.contains("Budget:"));
    }

    #[test]
    fn goal_budget_gate_pauses_when_limits_exist_without_usage_summary() {
        let goal = GoalTaskRecord {
            task_id: "goal-1".into(),
            objective: "ship it".into(),
            effective_token_limit: Some(1_000),
            effective_cost_limit_usd: None,
            pause_reason: None,
            pause_description: None,
            blockers: Vec::new(),
        };

        let pause = goal_budget_gate_pause(&goal, None).unwrap();

        assert_eq!(pause.reason, GoalPauseReason::BudgetUnavailable);
        assert_eq!(pause.blockers.len(), 1);
        assert_eq!(pause.blockers[0].kind, GoalBlockerKind::Budget);
        assert_eq!(
            pause.blockers[0].payload.as_ref().unwrap()["usage_unavailable"],
            true
        );
    }

    #[test]
    fn removing_budget_pause_preserves_unrelated_blockers() {
        let goal = GoalTaskRecord {
            task_id: "goal-1".into(),
            objective: "ship it".into(),
            effective_token_limit: Some(1_000),
            effective_cost_limit_usd: None,
            pause_reason: Some(GoalPauseReason::BudgetExhausted),
            pause_description: Some("multiple blockers".into()),
            blockers: vec![
                GoalBlocker {
                    kind: GoalBlockerKind::NeedsUserInput,
                    message: "Need operator answer".into(),
                    payload: None,
                },
                GoalBlocker {
                    kind: GoalBlockerKind::Budget,
                    message: "Budget exhausted".into(),
                    payload: None,
                },
            ],
        };

        let pause = remove_budget_pause(&goal).unwrap();

        assert_eq!(pause.reason, GoalPauseReason::NeedsUserInput);
        assert_eq!(pause.blockers.len(), 1);
        assert_eq!(pause.blockers[0].kind, GoalBlockerKind::NeedsUserInput);

        let only_budget = GoalTaskRecord {
            blockers: vec![GoalBlocker {
                kind: GoalBlockerKind::Budget,
                message: "Budget exhausted".into(),
                payload: None,
            }],
            ..goal
        };
        assert!(remove_budget_pause(&only_budget).is_none());
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
    async fn scoped_goal_state_update_publishes_channel_message() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let admission = GoalAdmission {
            task_id: Some("goal-1".into()),
            status: TaskStatus::Running,
            message: "Goal `goal-1` started.".into(),
            continue_goal: true,
        };

        scope_goal_state_updates(Some(GoalStateUpdateSink::new(tx)), async {
            publish_goal_state_update(&admission);
        })
        .await;

        assert_eq!(
            rx.recv().await,
            Some(GoalStateUpdateEvent::Status(
                "Goal `goal-1` started.".into()
            ))
        );
    }

    #[tokio::test]
    async fn scoped_goal_verifier_start_publishes_progress_event() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = test_config();
        config.goal.verifier.enabled = true;
        let goal = GoalTaskRecord {
            task_id: "goal-1".into(),
            objective: "ship it".into(),
            effective_token_limit: Some(10_000),
            effective_cost_limit_usd: Some(1.25),
            pause_reason: None,
            pause_description: None,
            blockers: Vec::new(),
        };

        scope_goal_state_updates(Some(GoalStateUpdateSink::new(tx)), async {
            let usage = goal_usage_totals(Some(&config), "goal-1");
            let budget = goal_budget_summary(&goal, usage.as_ref());
            publish_goal_verifier_started("goal-1", &budget);
        })
        .await;

        let Some(GoalStateUpdateEvent::VerifierStarted(message)) = rx.recv().await else {
            panic!("verifier progress should use a typed progress event");
        };
        assert!(message.starts_with("🔎 Verifying goal `goal-1` status."));
        assert!(message.contains("Budget:"));
    }

    #[tokio::test]
    async fn evaluate_goal_turn_completes_running_goal_when_verifier_disabled() {
        let store = global_test_store();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        store
            .create_goal(
                TaskRecord {
                    id: task_id.clone(),
                    kind: TaskKind::Goal,
                    agent: agent.clone(),
                    status: TaskStatus::Running,
                    owner_pid: std::process::id(),
                    owner_boot_id: "test-boot".into(),
                    heartbeat_at: None,
                    depth: 0,
                    parent_id: None,
                    originator_route: Some(route),
                    delivered: false,
                    idem_key: None,
                    principal_id: Some(principal),
                    started_at: chrono::Utc::now().to_rfc3339(),
                    finished_at: None,
                },
                GoalTaskRecord {
                    task_id: task_id.clone(),
                    objective: "ship it".into(),
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
        let mut config = test_config();
        config.goal.verifier.enabled = false;

        let outcome = evaluate_goal_turn(&ctx, &config, "done").await.unwrap();

        let Some(GoalTurnEvaluation::Completed {
            task_id: completed_id,
            message,
        }) = outcome
        else {
            panic!("running goal should complete when verifier is disabled");
        };
        assert_eq!(completed_id, task_id);
        assert!(message.starts_with("✅ Goal"));
        assert!(message.contains("Budget:"));
        assert_eq!(
            store.get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
    }

    #[tokio::test]
    async fn evaluate_goal_turn_pauses_when_verifier_continue_exhausts_budget() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_config::schema::{CustomModelProviderConfig, ModelProviderConfig};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "CONTINUE\nMore work remains."
                        }
                    }
                ],
                "usage": {
                    "prompt_tokens": 75,
                    "completion_tokens": 50
                }
            })))
            .mount(&server)
            .await;

        let store = global_test_store();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        store
            .create_goal(
                TaskRecord {
                    id: task_id.clone(),
                    kind: TaskKind::Goal,
                    agent: agent.clone(),
                    status: TaskStatus::Running,
                    owner_pid: std::process::id(),
                    owner_boot_id: "test-boot".into(),
                    heartbeat_at: None,
                    depth: 0,
                    parent_id: None,
                    originator_route: Some(route),
                    delivered: false,
                    idem_key: None,
                    principal_id: Some(principal),
                    started_at: chrono::Utc::now().to_rfc3339(),
                    finished_at: None,
                },
                GoalTaskRecord {
                    task_id: task_id.clone(),
                    objective: "ship it".into(),
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

        let temp = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: temp.path().to_path_buf(),
            ..test_config()
        };
        config.cost.enabled = true;
        config.goal.verifier.enabled = true;
        config.goal.verifier.model_provider = "custom.verifier".into();
        config.goal.verifier.model = Some("model".into());
        config.providers.models.custom.insert(
            "verifier".into(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    api_key: Some("test-key".into()),
                    uri: Some(server.uri()),
                    model: Some("model".into()),
                    pricing: [("model.input".into(), 1.0), ("model.output".into(), 2.0)]
                        .into_iter()
                        .collect(),
                    ..ModelProviderConfig::default()
                },
            },
        );

        let outcome = evaluate_goal_turn(&ctx, &config, "looks done")
            .await
            .unwrap();

        let Some(GoalTurnEvaluation::Paused {
            task_id: paused_id,
            message,
        }) = outcome
        else {
            panic!("verifier usage should exhaust the goal budget before continuation");
        };
        assert_eq!(paused_id, task_id);
        assert!(message.starts_with("⏸️ Goal"));
        assert!(message.contains("Budget:"));
        let task = store.get(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::BudgetExhausted));
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
            None,
        )
        .await
        .unwrap();
        let task_id = started.task_id.unwrap();
        assert!(started.message.contains("Budget: tokens 0/unlimited"));
        assert!(started.message.contains("$0.0000/$3.2500"));
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(goal.effective_token_limit, None);
        assert_eq!(goal.effective_cost_limit_usd, Some(3.25));
    }

    #[tokio::test]
    async fn goal_start_with_budget_pauses_when_usage_ledger_unavailable() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a");
        let config = test_config();

        let started = start_goal(
            &store,
            "boot-a",
            ctx,
            "ship it".into(),
            Some(10),
            None,
            Some(&config),
        )
        .await
        .unwrap();

        assert_eq!(started.status, TaskStatus::Paused);
        assert!(!started.continue_goal);
        assert!(started.message.contains("budget accounting is unavailable"));
        let task_id = started.task_id.unwrap();
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::BudgetUnavailable));
        assert_eq!(goal.blockers[0].kind, GoalBlockerKind::Budget);
    }

    #[tokio::test]
    async fn goal_start_persists_restart_continuation_context() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let continuation_context = TaskContinuationContext {
            channel: "matrix".into(),
            channel_alias: Some("work".into()),
            reply_target: "!room:example.org".into(),
            sender: "@operator:example.org".into(),
            thread_ts: Some("$root".into()),
            interruption_scope_id: Some("$root".into()),
            conversation_scope: TaskContinuationConversationScope::ReplyTarget,
        };
        let ctx = GoalAdmissionContext::new("agent-a")
            .with_channel_type(Some("matrix".into()))
            .with_originator_route(Some("matrix_work__room_example_org".into()))
            .with_principal_id(Some("principal-a".into()))
            .with_continuation_context(Some(continuation_context.clone()));

        let started = start_goal(&store, "boot-a", ctx, "ship it".into(), None, None, None)
            .await
            .unwrap();
        let task_id = started.task_id.unwrap();

        assert_eq!(
            store.get_continuation_context(&task_id).await.unwrap(),
            Some(continuation_context)
        );
    }

    #[tokio::test]
    async fn goal_lifecycle_uses_task_record_for_status() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-1".into()));
        let started = start_goal(
            &store,
            "boot-a",
            ctx.clone(),
            "ship it".into(),
            None,
            None,
            None,
        )
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

        let cancelled = cancel_goal(&store, &ctx, Some(task_id.clone()), None)
            .await
            .unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(
            store.get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Cancelled
        );

        let err = cancel_goal(&store, &ctx, Some(task_id), None)
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
        let started = start_goal(
            &store,
            "boot-a",
            owner.clone(),
            "ship it".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let task_id = started.task_id.clone().unwrap();

        status_goal(&store, &owner, Some(task_id.clone()), None)
            .await
            .unwrap();

        let wrong_route = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-2".into()))
            .with_principal_id(Some("principal-1".into()));
        let err = status_goal(&store, &wrong_route, Some(task_id.clone()), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not visible from this route"));

        let wrong_principal = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-2".into()));
        let err = status_goal(&store, &wrong_principal, Some(task_id), None)
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

        start_goal(
            &store,
            "boot-a",
            ctx.clone(),
            "ship it".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let err = start_goal(
            &store,
            "boot-a",
            ctx,
            "ship another".into(),
            None,
            None,
            None,
        )
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
            start_goal(
                &store,
                "boot-a",
                ctx.clone(),
                "ship one".into(),
                None,
                None,
                None,
            ),
            start_goal(&store, "boot-a", ctx, "ship two".into(), None, None, None,)
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
        let started = start_goal(
            &store,
            "boot-a",
            ctx.clone(),
            "ship it".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let task_id = started.task_id.clone().unwrap();

        let paused = pause_goal_for_blocker(
            &store,
            &ctx,
            Some(task_id.clone()),
            None,
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

        let resumed = resume_goal(&store, "boot-resumed", &ctx, Some(task_id.clone()), None)
            .await
            .unwrap();
        assert_eq!(resumed.status, TaskStatus::Running);
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.owner_boot_id, "boot-resumed");
        assert!(goal.pause_reason.is_none());
        assert!(goal.blockers.is_empty());
    }

    #[tokio::test]
    async fn budget_update_resumes_goal_when_budget_blocker_clears() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let continuation_context = TaskContinuationContext {
            channel: "matrix".into(),
            channel_alias: Some("work".into()),
            reply_target: "!room:example.org".into(),
            sender: "@operator:example.org".into(),
            thread_ts: Some("$root".into()),
            interruption_scope_id: Some("$root".into()),
            conversation_scope: TaskContinuationConversationScope::ReplyTarget,
        };
        let ctx = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("matrix:room".into()))
            .with_principal_id(Some("principal-a".into()))
            .with_continuation_context(Some(continuation_context.clone()));
        let started = start_goal(
            &store,
            "boot-a",
            ctx.clone(),
            "ship it".into(),
            Some(10),
            None,
            None,
        )
        .await
        .unwrap();
        let task_id = started.task_id.clone().unwrap();
        pause_goal_for_blocker(
            &store,
            &ctx,
            Some(task_id.clone()),
            None,
            GoalPauseState {
                reason: GoalPauseReason::BudgetExhausted,
                description: Some("budget exhausted".into()),
                blockers: vec![GoalBlocker {
                    kind: GoalBlockerKind::Budget,
                    message: "Budget exhausted".into(),
                    payload: None,
                }],
            },
        )
        .await
        .unwrap();

        let admitted = update_goal_budget(
            &store,
            "boot-resumed",
            &ctx,
            GoalBudgetOverrides {
                token_limit: GoalBudgetValue::Unlimited,
                cost_limit_usd: GoalBudgetValue::Default,
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(admitted.status, TaskStatus::Running);
        assert!(admitted.continue_goal);
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.owner_boot_id, "boot-resumed");
        assert!(goal.effective_token_limit.is_none());
        assert!(goal.pause_reason.is_none());
        assert!(goal.blockers.is_empty());
        assert_eq!(
            store.get_continuation_context(&task_id).await.unwrap(),
            Some(continuation_context)
        );
    }
}
