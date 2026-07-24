//! Goal-mode admission and controller helpers.
//!
//! This module is the single Rust admission path for slash-command and
//! agent-callable goal start/resume requests. Callers pass trusted runtime
//! context explicitly; model/user text supplies only the untrusted
//! objective/action payload.

use anyhow::{Context, Result, bail};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use zeroclaw_commands::{BuiltinCommandId, CommandSurface, command_by_name};
use zeroclaw_config::cost::CostTracker;
use zeroclaw_config::schema::{AliasedAgentConfig, Config};

use crate::agent::cost::{is_goal_accounting_failure, is_goal_accounting_pricing_failure};

use super::global::control_plane;
use super::goal_task::{
    GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState, GoalTaskRecord,
    GoalTaskRegistry, TaskContinuationContext, TaskGoal,
};
use super::task_registry::{TaskKind, TaskRecord, TaskRegistry, TaskStatus};
use super::verifier::{
    GoalVerificationRequest, GoalVerifier, GoalVerifierDecision, LlmGoalVerifier,
    verifier_outage_pause,
};

tokio::task_local! {
    static GOAL_RUNTIME_SCOPE: GoalRuntimeScope;
    static GOAL_START_TOOL_BATCH: bool;
}

/// Ephemeral task-local context for one goal-aware model/tool turn.
///
/// This is deliberately not durable goal state. Durable lifecycle facts live in
/// `TaskRecord` plus the goal extension row; this scope only carries the live
/// channel/controller handles needed while polling one turn.
#[derive(Clone, Default)]
pub struct GoalRuntimeScope {
    /// Trusted admission facts attached by channel ingress.
    ///
    /// The inner context is shared by the tools in this one live turn so a
    /// successful exact goal admission can bind its returned task id before a
    /// later approval request. This remains transient trust plumbing; the
    /// canonical task and continuation rows remain authoritative.
    admission_context: Arc<parking_lot::RwLock<Option<GoalAdmissionContext>>>,
    /// Optional live channel sink for controller/verifier progress messages.
    state_update_sink: Option<GoalStateUpdateSink>,
    /// Shared marker promoted when the current turn becomes goal work.
    turn_evaluation_requested: Option<Arc<AtomicBool>>,
}

impl GoalRuntimeScope {
    pub fn new(
        admission_context: Option<GoalAdmissionContext>,
        state_update_sink: Option<GoalStateUpdateSink>,
        turn_evaluation_requested: Option<Arc<AtomicBool>>,
    ) -> Self {
        Self {
            admission_context: Arc::new(parking_lot::RwLock::new(admission_context)),
            state_update_sink,
            turn_evaluation_requested,
        }
    }

    fn with_admission_context(mut self, admission_context: Option<GoalAdmissionContext>) -> Self {
        self.admission_context = Arc::new(parking_lot::RwLock::new(admission_context));
        self
    }

    fn with_state_update_sink(mut self, state_update_sink: Option<GoalStateUpdateSink>) -> Self {
        self.state_update_sink = state_update_sink;
        self
    }

    fn with_turn_evaluation_marker(
        mut self,
        turn_evaluation_requested: Option<Arc<AtomicBool>>,
    ) -> Self {
        self.turn_evaluation_requested = turn_evaluation_requested;
        self
    }
}

/// Ephemeral channel backchannel for goal controller status messages.
///
/// The controller uses this while processing a live channel turn to publish
/// status transitions and verifier progress before the final model response is
/// ready. It is not persisted and is not replayed; restart-visible state remains
/// in the task/goal registries.
#[derive(Clone)]
pub struct GoalStateUpdateSink {
    /// Channel-local sender for controller-generated progress events.
    ///
    /// This is an ephemeral notification path. Durable lifecycle state stays in
    /// the task registry and goal extension table.
    tx: tokio::sync::mpsc::UnboundedSender<GoalStateUpdateEvent>,
}

/// User-visible progress event emitted by the goal controller while a channel
/// turn is still running.
///
/// The event carries render-ready text because localization happens at the
/// control-plane boundary where the status/verifier context is available. It is
/// not durable state and is intentionally not replayed after restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalStateUpdateEvent {
    /// Replace or append a visible lifecycle/status update.
    Status(String),
    /// Show a temporary "verification in progress" message while the verifier
    /// model call is pending.
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

fn task_status_label(status: TaskStatus) -> String {
    let key = match status {
        TaskStatus::Running => "goal-status-running",
        TaskStatus::Paused => "goal-status-paused",
        TaskStatus::Completed => "goal-status-completed",
        TaskStatus::Failed => "goal-status-failed",
        TaskStatus::Cancelled => "goal-status-cancelled",
        TaskStatus::Lost => "goal-status-lost",
        TaskStatus::TimedOut => "goal-status-timed-out",
    };
    msg(key, &[])
}

fn pause_reason_label(reason: GoalPauseReason) -> String {
    let key = match reason {
        GoalPauseReason::OperatorPaused => "goal-pause-reason-operator-paused",
        GoalPauseReason::NeedsUserInput => "goal-pause-reason-needs-user-input",
        GoalPauseReason::HumanEscalation => "goal-pause-reason-human-escalation",
        GoalPauseReason::ExternalDependency => "goal-pause-reason-external-dependency",
        GoalPauseReason::ProviderUnavailable => "goal-pause-reason-provider-unavailable",
        GoalPauseReason::VerifierBlocked => "goal-pause-reason-verifier-blocked",
        GoalPauseReason::BudgetExhausted => "goal-pause-reason-budget-exhausted",
        GoalPauseReason::BudgetUnavailable => "goal-pause-reason-budget-unavailable",
        GoalPauseReason::DaemonRestart => "goal-pause-reason-daemon-restarted",
    };
    msg(key, &[])
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

/// Ledger-derived usage snapshot for one goal task.
///
/// This is a per-call materialized view over persisted `CostRecord` rows.
/// It must never be stored back into `goal_tasks`; consumed and remaining
/// budget are always derived from the ledger so budget changes cannot drift
/// from usage history.
#[derive(Debug, Clone, Copy)]
struct GoalUsageTotals {
    /// Tokens attributed to this task by the canonical cost ledger.
    total_tokens: u64,
    /// USD cost attributed to this task by the canonical cost ledger.
    cost_usd: f64,
    /// Whether every cost-bearing row had reliable pricing.
    ///
    /// Token totals remain usable when this is false, but an active cost limit
    /// must pause because unknown cost cannot be treated as free.
    cost_pricing_available: bool,
    /// Whether the ledger is configured to calculate USD amounts. Token-only
    /// goal accounting deliberately leaves this false rather than displaying
    /// a fabricated zero-dollar total.
    cost_tracking_available: bool,
    /// Whether every attributed provider call supplied usable token counts.
    usage_available: bool,
}

/// Why the controller cannot produce trustworthy budget accounting.
///
/// This is not persisted as a separate state enum. It only shapes the blocker
/// payload at the moment a goal is paused so operators can tell "ledger missing"
/// from "ledger present but USD pricing unknown".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoalBudgetUnavailableCause {
    /// No canonical usage snapshot could be read for a budgeted goal.
    UsageUnavailable,
    /// Usage rows exist, but at least one cost-attributed call lacks pricing.
    CostPricingUnavailable,
}

impl Default for GoalUsageTotals {
    fn default() -> Self {
        Self {
            total_tokens: 0,
            cost_usd: 0.0,
            cost_pricing_available: true,
            cost_tracking_available: true,
            usage_available: true,
        }
    }
}

fn goal_usage_totals(config: Option<&Config>, task_id: &str) -> Option<GoalUsageTotals> {
    let tracker = goal_usage_ledger(config)?;
    goal_usage_totals_from_tracker(Some(tracker.as_ref()), task_id)
}

fn goal_usage_totals_from_tracker(
    tracker: Option<&CostTracker>,
    task_id: &str,
) -> Option<GoalUsageTotals> {
    let tracker = tracker?;
    if tracker.ensure_storage_ready().is_err() {
        return None;
    }
    match tracker.get_usage_totals_for_task_with_pricing(task_id) {
        Ok((total_tokens, cost_usd, cost_pricing_available, usage_available)) => {
            Some(GoalUsageTotals {
                total_tokens,
                cost_usd,
                cost_pricing_available,
                cost_tracking_available: tracker.is_enabled(),
                usage_available,
            })
        }
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
    let tracker = goal_usage_ledger(config)?;
    goal_usage_ledger_is_healthy(&tracker).then_some(GoalUsageTotals {
        cost_tracking_available: tracker.is_enabled(),
        ..GoalUsageTotals::default()
    })
}

fn goal_usage_ledger(config: Option<&Config>) -> Option<std::sync::Arc<CostTracker>> {
    let config = config?;
    CostTracker::get_or_init_global_goal_usage_ledger(config.cost.clone(), &config.data_dir)
}

fn goal_budget_summary(goal: &GoalTaskRecord, usage: Option<&GoalUsageTotals>) -> String {
    let token_limit = token_limit_label(goal.effective_token_limit);
    let cost_limit = cost_limit_label(goal.effective_cost_limit_usd);
    if let Some(usage) = usage.filter(|usage| usage.usage_available) {
        let tokens_used = usage.total_tokens.to_string();
        if !usage.cost_tracking_available || !usage.cost_pricing_available {
            return msg(
                "goal-budget-summary-cost-unavailable",
                &[("tokens_used", &tokens_used), ("token_limit", &token_limit)],
            );
        }
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

fn task_goal_budget_summary(task_goal: &TaskGoal, config: Option<&Config>) -> String {
    let usage = goal_usage_totals(config, task_goal.task_id());
    goal_budget_summary(task_goal.goal(), usage.as_ref())
}

/// Render the visible restart-recovery notice for a durable goal.
///
/// Recovery is queued by task id, but the user-facing message is derived from
/// the canonical goal extension record at delivery time so objective changes
/// and consumed budget stay consistent with the rest of the control plane.
pub fn goal_recovery_status_message(goal: &GoalTaskRecord, config: Option<&Config>) -> String {
    let usage = goal_usage_totals(config, &goal.task_id);
    let budget = goal_budget_summary(goal, usage.as_ref());
    msg(
        "goal-command-recovered",
        &[
            ("task_id", &goal.task_id),
            ("objective", &goal.objective),
            ("budget", &budget),
        ],
    )
}

/// Budget-gate decision for a single ledger snapshot.
///
/// The booleans distinguish which effective limit fired so the pause payload
/// can be explicit without duplicating the full budget state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GoalBudgetExhaustion {
    /// The task's attributed token usage has reached the effective token limit.
    tokens: bool,
    /// The task's attributed cost has reached the effective cost limit.
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

/// A finite cost budget is meaningful only when USD pricing is enabled and
/// its canonical ledger is writable. Token-only goals deliberately bypass
/// this check and continue to use the same ledger for exact task attribution.
fn ensure_cost_budget_tracking_available(
    config: Option<&Config>,
    cost_limit_usd: Option<f64>,
    ledger_healthy: Option<bool>,
) -> Result<()> {
    if cost_limit_usd.is_none() {
        return Ok(());
    }
    let Some(config) = config else {
        return Ok(());
    };
    if !config.cost.enabled
        || CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir).is_none_or(
            |tracker| {
                !tracker.is_enabled()
                    || !ledger_healthy.unwrap_or_else(|| goal_usage_ledger_is_healthy(&tracker))
            },
        )
    {
        bail!("{}", msg("goal-command-error-cost-tracking-required", &[]));
    }
    Ok(())
}

/// Verify that the canonical JSONL ledger can both read its existing rows and
/// accept a future exact-task observation. Tracker construction alone creates
/// only the parent directory, so it is not sufficient evidence of availability.
fn goal_usage_ledger_is_healthy(tracker: &CostTracker) -> bool {
    tracker.ensure_storage_ready().is_ok()
        && tracker
            .get_usage_totals_for_task_with_pricing("__goal_usage_ledger_health_check__")
            .is_ok()
}

fn goal_budget_pause(
    goal: &GoalTaskRecord,
    usage: Option<&GoalUsageTotals>,
) -> Option<GoalPauseState> {
    let usage = usage?;
    if goal_has_effective_budget(goal) && !usage.usage_available {
        return goal_budget_unavailable_pause(goal, GoalBudgetUnavailableCause::UsageUnavailable);
    }
    if goal.effective_cost_limit_usd.is_some()
        && (!usage.cost_tracking_available || !usage.cost_pricing_available)
    {
        return goal_budget_unavailable_pause(
            goal,
            GoalBudgetUnavailableCause::CostPricingUnavailable,
        );
    }
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

fn goal_budget_unavailable_pause(
    goal: &GoalTaskRecord,
    cause: GoalBudgetUnavailableCause,
) -> Option<GoalPauseState> {
    if !goal_has_effective_budget(goal) {
        return None;
    }
    Some(goal_accounting_unavailable_pause(goal, cause))
}

fn goal_accounting_unavailable_pause(
    goal: &GoalTaskRecord,
    cause: GoalBudgetUnavailableCause,
) -> GoalPauseState {
    let budget = goal_budget_summary(goal, None);
    let usage_unavailable = matches!(cause, GoalBudgetUnavailableCause::UsageUnavailable);
    let cost_pricing_unavailable =
        matches!(cause, GoalBudgetUnavailableCause::CostPricingUnavailable);
    GoalPauseState {
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
                "usage_unavailable": usage_unavailable,
                "cost_pricing_unavailable": cost_pricing_unavailable,
                "token_limit": goal.effective_token_limit,
                "cost_limit_usd": goal.effective_cost_limit_usd,
            })),
        }],
    }
}

fn goal_budget_gate_pause(
    goal: &GoalTaskRecord,
    usage: Option<&GoalUsageTotals>,
) -> Option<GoalPauseState> {
    match usage {
        Some(usage) => goal_budget_pause(goal, Some(usage)),
        None => goal_budget_unavailable_pause(goal, GoalBudgetUnavailableCause::UsageUnavailable),
    }
}

/// A missing ledger snapshot is an infrastructure failure, unlike a durable
/// provider-usage-unavailable observation. Unlimited goals may continue after
/// the latter because no remaining budget is being derived, but no goal can
/// preserve exact-task attribution when the canonical ledger itself is absent.
fn goal_usage_ledger_gate_pause(
    goal: &GoalTaskRecord,
    usage: Option<&GoalUsageTotals>,
) -> Option<GoalPauseState> {
    usage.is_none().then(|| {
        goal_accounting_unavailable_pause(goal, GoalBudgetUnavailableCause::UsageUnavailable)
    })
}

/// Evaluate the ordered accounting contract shared by configured execution
/// paths: a missing canonical ledger is an infrastructure failure before an
/// otherwise-available budget can be evaluated.
fn goal_accounting_gate_pause(
    goal: &GoalTaskRecord,
    usage: Option<&GoalUsageTotals>,
) -> Option<GoalPauseState> {
    goal_usage_ledger_gate_pause(goal, usage).or_else(|| goal_budget_gate_pause(goal, usage))
}

fn reason_for_blocker_kind(kind: GoalBlockerKind) -> GoalPauseReason {
    match kind {
        GoalBlockerKind::OperatorPause => GoalPauseReason::OperatorPaused,
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

fn blocker_kind_label(kind: GoalBlockerKind) -> String {
    let key = match kind {
        GoalBlockerKind::OperatorPause => "goal-blocker-kind-operator-pause",
        GoalBlockerKind::NeedsUserInput => "goal-blocker-kind-needs-user-input",
        GoalBlockerKind::HumanEscalation => "goal-blocker-kind-human-escalation",
        GoalBlockerKind::ExternalDependency => "goal-blocker-kind-external-dependency",
        GoalBlockerKind::Provider => "goal-blocker-kind-provider",
        GoalBlockerKind::Verifier => "goal-blocker-kind-verifier",
        GoalBlockerKind::Budget => "goal-blocker-kind-budget",
        GoalBlockerKind::RestartRecovery => "goal-blocker-kind-restart-recovery",
    };
    msg(key, &[])
}

fn blockers_summary(blockers: &[GoalBlocker]) -> Option<String> {
    let items = blockers
        .iter()
        .map(|blocker| {
            let kind = blocker_kind_label(blocker.kind);
            let message = blocker.message.trim();
            if message.is_empty() {
                kind
            } else {
                msg(
                    "goal-command-blocker-summary-item",
                    &[("kind", &kind), ("message", message)],
                )
            }
        })
        .collect::<Vec<_>>();
    (!items.is_empty()).then(|| items.join("; "))
}

/// Transport-neutral goal command verb after parsing user/model input.
///
/// The action chooses controller behavior, but it is still only command input.
/// Authorization, route, principal, and owner identity come from
/// [`GoalAdmissionContext`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalCommandAction {
    /// Render localized help without mutating goal state.
    Help,
    /// Create a new durable goal task or continue admission through the model
    /// tool path.
    Start,
    /// Replace the current goal's durable objective text.
    Objective,
    /// Report the latest visible state for a goal.
    Status,
    /// Replace effective limits and potentially resume a budget-paused goal.
    Budget,
    /// Explicitly pause the goal without making it terminal.
    Pause,
    /// Claim and continue a paused goal.
    Resume,
    /// Transition the goal task to a terminal cancellation state.
    Cancel,
}

/// Parsed budget option for a `/goal` command.
///
/// `Default` means the command did not mention the limit and the controller
/// should keep or derive the configured effective value. `Unlimited` is an
/// explicit operator request to clear that effective limit. `Limited` is an
/// explicit finite limit.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum GoalBudgetValue<T> {
    /// Keep the existing or configured effective limit.
    #[default]
    Default,
    /// Explicitly remove this effective limit.
    Unlimited,
    /// Replace this effective limit with the supplied finite value.
    Limited(T),
}

/// Operator-supplied budget mutations carried by a goal command.
///
/// Effective limits are stored on `GoalTaskRecord`. Consumed and remaining
/// budget are never stored here; they are derived from ledger usage records.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GoalBudgetOverrides {
    /// Token limit mutation for this command.
    pub token_limit: GoalBudgetValue<u64>,
    /// USD cost limit mutation for this command.
    pub cost_limit_usd: GoalBudgetValue<f64>,
}

/// Transport-neutral representation of a parsed goal command.
///
/// This is command input, not trusted lifecycle state. Channel handlers and the
/// model-callable tool both normalize into this type, then pass trusted runtime
/// facts separately through [`GoalAdmissionContext`]. Do not add sender, route,
/// principal, or owner fields here: those belong to ingress/runtime state and
/// eventually to the canonical [`TaskRecord`](super::TaskRecord).
#[derive(Debug, Clone, PartialEq)]
pub struct GoalCommand {
    /// Requested controller action.
    pub action: GoalCommandAction,
    /// Untrusted operator/model objective text for `start` or `objective`.
    pub objective: Option<String>,
    /// Optional task id selector for inspection/control commands.
    ///
    /// `resume` deliberately does not use this selector: there is only one
    /// current paused goal in a trusted route/principal session, and completed
    /// goals are irreversible.
    pub task_id: Option<String>,
    /// Untrusted operator reason included with `/goal resume`.
    ///
    /// This is per-resume prompt input, not durable lifecycle state. The
    /// controller uses it to build the next continuation prompt and then drops
    /// it; trusted pause/blocker state remains in the task and goal registries.
    pub resume_reason: Option<String>,
    /// Requested effective budget changes.
    pub budgets: GoalBudgetOverrides,
}

/// Trusted runtime facts attached to goal admission.
///
/// The model and operator may provide the objective or subcommand text, but not
/// these fields. They are supplied by the ingress/runtime surface and are used
/// to bind goal lifecycle, routing, principal visibility, and continuation
/// delivery to the canonical task record. This struct is transient admission
/// input; only `continuation_context`, when present, is copied into durable
/// goal-continuation storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalAdmissionContext {
    /// Agent alias that owns the admitted goal.
    pub agent_alias: String,
    /// Runtime surface that parsed or invoked the command.
    pub command_surface: CommandSurface,
    /// Channel family, when the command originated from a channel turn.
    pub channel_type: Option<String>,
    /// Canonical route/reply target used for visibility and continuation.
    pub originator_route: Option<String>,
    /// Authenticated principal that originated the command, when available.
    pub principal_id: Option<String>,
    /// Exact durable goal task bound to this trusted controller turn.
    ///
    /// This is transient admission evidence, not another lifecycle source of
    /// truth. The canonical task record remains in the goal store; callers use
    /// this binding only to avoid attributing a concurrent replacement goal.
    pub goal_task_id: Option<String>,
    /// Minimal durable channel context needed to resume after restart.
    ///
    /// The context itself is persisted by the goal store. The rest of this
    /// admission struct is per-turn trust context and must not be stored here.
    pub continuation_context: Option<TaskContinuationContext>,
}

/// Result of applying a goal command to the durable control plane.
///
/// Callers use `continue_goal` to decide whether to enqueue another agent turn;
/// the task lifecycle itself is represented by `status` plus the canonical
/// task/goal rows, not by this transient result object. `message` is a localized
/// rendering of that state, not a policy input.
#[derive(Debug, Clone, PartialEq)]
pub struct GoalAdmission {
    /// Goal task affected by the command, when the action resolves one.
    pub task_id: Option<String>,
    /// Current canonical task status after command admission.
    pub status: TaskStatus,
    /// Localized user-visible status/error text.
    pub message: String,
    /// Untrusted operator text to include in the next continuation prompt.
    ///
    /// This is transient controller output for `/goal resume [reason]`, not
    /// durable lifecycle state. It must not be persisted into task or goal
    /// rows.
    pub continuation_reason: Option<String>,
    /// Whether the channel runtime should synthesize a continuation prompt.
    pub continue_goal: bool,
}

/// Verifier/controller decision for a completed model turn under goal mode.
///
/// This is the handoff object between the verifier and the goal controller. It
/// is intentionally transient: the controller is responsible for translating it
/// into task status, pause state, and user-visible messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalTurnEvaluation {
    /// The verifier accepted the work as complete. The controller still owns
    /// the canonical terminal write.
    Completed {
        /// Canonical task id whose goal was evaluated.
        task_id: String,
        /// Localized channel/user status message rendered from the evaluation.
        message: String,
    },
    /// The verifier found more work and supplied untrusted notes for the next
    /// continuation prompt. These notes are prompt input only, not durable
    /// controller state.
    Continue {
        /// Canonical task id whose goal should continue.
        task_id: String,
        /// Original untrusted objective text from the goal extension.
        objective: String,
        /// Verifier-supplied untrusted notes for the next prompt.
        notes: String,
        /// Localized channel/user status message rendered from the evaluation.
        message: String,
    },
    /// The turn could not proceed without operator/provider/external action.
    Paused {
        /// Canonical task id whose goal was paused.
        task_id: String,
        /// Localized channel/user status message rendered from the pause.
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
            goal_task_id: None,
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
    pub fn with_goal_task_id(mut self, goal_task_id: Option<String>) -> Self {
        self.goal_task_id = goal_task_id;
        self
    }

    #[must_use]
    pub fn with_continuation_context(mut self, context: Option<TaskContinuationContext>) -> Self {
        self.continuation_context = context;
        self
    }
}

pub fn current_goal_admission_context() -> Option<GoalAdmissionContext> {
    GOAL_RUNTIME_SCOPE
        .try_with(|scope| scope.admission_context.read().clone())
        .ok()
        .flatten()
}

/// Bind a just-admitted exact goal task to the current live turn.
///
/// This does not persist lifecycle state or resolve a goal by route: callers
/// may supply an id only after a successful controller transition returned it.
/// It lets later tools in the same turn use the durable continuation route
/// without falling back to mutable inbound delivery facts.
pub fn bind_current_goal_task(task_id: &str) -> bool {
    if task_id.trim().is_empty() {
        return false;
    }
    GOAL_RUNTIME_SCOPE
        .try_with(|scope| {
            let mut admission = scope.admission_context.write();
            let Some(admission) = admission.as_mut() else {
                return false;
            };
            match admission.goal_task_id.as_deref() {
                Some(existing) => existing == task_id,
                None => {
                    admission.goal_task_id = Some(task_id.to_string());
                    true
                }
            }
        })
        .unwrap_or(false)
}

/// Durably stop the exact active goal after a provider boundary reports that
/// its usage cannot be attributed. The task-local admission context supplies
/// the task id; this never falls back to route or principal lookup.
pub async fn pause_goal_for_accounting_failure(task_id: &str, error: &anyhow::Error) -> Result<()> {
    if !is_goal_accounting_failure(error) {
        return Ok(());
    }
    let Some(cp) = control_plane() else {
        return Ok(());
    };
    let task = cp
        .store
        .get(task_id)
        .await?
        .ok_or_else(|| anyhow::Error::msg("goal task missing"))?;
    let goal = cp
        .goal_store
        .get_goal_task(task_id)
        .await?
        .ok_or_else(|| anyhow::Error::msg("goal extension missing"))?;
    let resolved = TaskGoal::new(task, goal);
    if !resolved.is_running() {
        return Ok(());
    }
    let cause = if is_goal_accounting_pricing_failure(error) {
        GoalBudgetUnavailableCause::CostPricingUnavailable
    } else {
        GoalBudgetUnavailableCause::UsageUnavailable
    };
    let pause = goal_accounting_unavailable_pause(resolved.goal(), cause);
    let budget = goal_budget_summary(resolved.goal(), None);
    let admission =
        pause_goal_for_resolved_task_with_budget(cp.goal_store.as_ref(), resolved, pause, budget)
            .await?;
    publish_goal_state_update(&admission);
    Ok(())
}

fn current_goal_runtime_scope() -> GoalRuntimeScope {
    GOAL_RUNTIME_SCOPE
        .try_with(Clone::clone)
        .unwrap_or_default()
}

pub async fn scope_goal_runtime<F>(scope: GoalRuntimeScope, future: F) -> F::Output
where
    F: std::future::Future,
{
    GOAL_RUNTIME_SCOPE.scope(scope, future).await
}

pub async fn scope_goal_admission_context<F>(
    ctx: Option<GoalAdmissionContext>,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    GOAL_RUNTIME_SCOPE
        .scope(
            current_goal_runtime_scope().with_admission_context(ctx),
            future,
        )
        .await
}

pub async fn scope_goal_state_updates<F>(sink: Option<GoalStateUpdateSink>, future: F) -> F::Output
where
    F: std::future::Future,
{
    GOAL_RUNTIME_SCOPE
        .scope(
            current_goal_runtime_scope().with_state_update_sink(sink),
            future,
        )
        .await
}

/// Scope the per-turn marker that says the channel orchestrator should run
/// goal verifier/evaluation after the model turn completes.
///
/// Admission facts alone are not enough: ordinary same-route traffic must be
/// able to start a goal through the model tool without being treated as work
/// for a previously active goal.
pub async fn scope_goal_turn_evaluation_marker<F>(
    marker: Option<Arc<AtomicBool>>,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    GOAL_RUNTIME_SCOPE
        .scope(
            current_goal_runtime_scope().with_turn_evaluation_marker(marker),
            future,
        )
        .await
}

/// Promote the current turn into goal work after trusted goal admission
/// succeeds inside the model tool loop.
pub fn mark_current_goal_turn_for_evaluation() {
    let _ = GOAL_RUNTIME_SCOPE.try_with(|scope| {
        if let Some(marker) = &scope.turn_evaluation_requested {
            marker.store(true, Ordering::Release);
        }
    });
}

/// Report whether the current task-local turn should be subject to goal
/// verifier/evaluation and goal-only delegation policy.
pub fn current_goal_turn_evaluation_requested() -> bool {
    GOAL_RUNTIME_SCOPE
        .try_with(|scope| {
            scope
                .turn_evaluation_requested
                .as_ref()
                .is_some_and(|marker| marker.load(Ordering::Acquire))
        })
        .unwrap_or(false)
}

/// Clone the current turn-evaluation marker so spawned foreground work can
/// re-enter the same transient goal-work decision boundary.
///
/// This intentionally shares the marker instead of copying its boolean value:
/// if a child `goal_start` promotes the turn, the parent orchestrator must see
/// the same promotion before deciding whether to run post-turn goal evaluation.
pub fn current_goal_turn_evaluation_marker() -> Option<Arc<AtomicBool>> {
    GOAL_RUNTIME_SCOPE
        .try_with(|scope| scope.turn_evaluation_requested.clone())
        .ok()
        .flatten()
}

/// Scope whether the current model-requested tool batch contains a goal
/// admission/control tool.
///
/// This is a conservative policy marker, not goal state. It lets sibling tools
/// refuse actions that cannot be made safe until admission completes, even when
/// the model listed those siblings before the admission tool or the executor
/// would otherwise consider the batch parallelizable.
pub async fn scope_goal_start_tool_batch<F>(
    contains_goal_admission_tool: bool,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    let inherited = current_goal_start_tool_batch_requested();
    GOAL_START_TOOL_BATCH
        .scope(inherited || contains_goal_admission_tool, future)
        .await
}

/// Report whether the active tool batch is attempting goal admission/control.
pub fn current_goal_start_tool_batch_requested() -> bool {
    GOAL_START_TOOL_BATCH
        .try_with(|value| *value)
        .unwrap_or(false)
}

fn publish_goal_state_update(admission: &GoalAdmission) {
    let _ = GOAL_RUNTIME_SCOPE.try_with(|scope| {
        if let Some(sink) = &scope.state_update_sink {
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
    let _ = GOAL_RUNTIME_SCOPE.try_with(|scope| {
        if let Some(sink) = &scope.state_update_sink {
            sink.send(GoalStateUpdateEvent::VerifierStarted(message));
        }
    });
}

pub fn parse_goal_command(input: &str) -> Result<GoalCommand> {
    let without_prefix = strip_goal_command_prefix(input)?;
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
            resume_reason: None,
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
                resume_reason: None,
                budgets,
            })
        }
        "objective" => {
            let objective = parse_objective_payload(rest)?;
            Ok(GoalCommand {
                action: GoalCommandAction::Objective,
                objective: Some(objective),
                task_id: None,
                resume_reason: None,
                budgets: GoalBudgetOverrides::default(),
            })
        }
        "status" => Ok(GoalCommand {
            action: GoalCommandAction::Status,
            objective: None,
            task_id: parse_optional_task_id(rest)?,
            resume_reason: None,
            budgets: GoalBudgetOverrides::default(),
        }),
        "budget" => {
            let budgets = parse_budget_payload(rest)?;
            Ok(GoalCommand {
                action: GoalCommandAction::Budget,
                objective: None,
                task_id: None,
                resume_reason: None,
                budgets,
            })
        }
        "pause" => Ok(GoalCommand {
            action: GoalCommandAction::Pause,
            objective: nonempty(rest),
            task_id: None,
            resume_reason: None,
            budgets: GoalBudgetOverrides::default(),
        }),
        "resume" => {
            let resume_reason = parse_resume_payload(rest)?;
            Ok(GoalCommand {
                action: GoalCommandAction::Resume,
                objective: None,
                task_id: None,
                resume_reason,
                budgets: GoalBudgetOverrides::default(),
            })
        }
        "cancel" => Ok(GoalCommand {
            action: GoalCommandAction::Cancel,
            objective: None,
            task_id: parse_optional_task_id(rest)?,
            resume_reason: None,
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

fn strip_goal_command_prefix(input: &str) -> Result<&str> {
    let trimmed = input.trim();
    let (command_token, rest) = trimmed
        .split_once(char::is_whitespace)
        .map_or((trimmed, ""), |(token, rest)| (token, rest.trim()));
    if !command_token.starts_with('/') {
        bail!(
            "{}",
            msg(
                "goal-command-error-invalid-command",
                &[("command", command_token)]
            )
        );
    }
    let Some(command) = command_by_name(command_token) else {
        bail!(
            "{}",
            msg(
                "goal-command-error-invalid-command",
                &[("command", command_token)]
            )
        );
    };
    if command.id != BuiltinCommandId::Goal {
        bail!(
            "{}",
            msg(
                "goal-command-error-invalid-command",
                &[("command", command_token)]
            )
        );
    }
    Ok(rest)
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

fn parse_resume_payload(input: &str) -> Result<Option<String>> {
    let rest = input.trim();
    if rest.is_empty() {
        return Ok(None);
    }
    Ok(Some(rest.to_string()))
}

fn parse_objective_payload(input: &str) -> Result<String> {
    let objective = input.trim();
    if objective.is_empty() {
        bail!("{}", msg("goal-command-error-missing-objective", &[]));
    }
    Ok(objective.to_string())
}

fn parse_optional_task_id(input: &str) -> Result<Option<String>> {
    let rest = input.trim();
    if rest.is_empty() {
        return Ok(None);
    }
    if let Some((_task_id, tail)) = rest.split_once(char::is_whitespace) {
        let args = tail.trim();
        bail!(
            "{}",
            msg("goal-command-error-unexpected-arguments", &[("args", args)])
        );
    }
    Ok(Some(rest.to_string()))
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
        let Some(channel_type) = ctx
            .channel_type
            .as_deref()
            .map(str::trim)
            .filter(|channel_type| !channel_type.is_empty())
        else {
            bail!("{}", msg("goal-command-error-channel-type-missing", &[]));
        };
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
            continuation_reason: None,
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
                cp.goal_store.as_ref(),
                &cp.boot_id,
                ctx,
                objective,
                token_limit,
                cost_limit_usd,
                Some(config),
            )
            .await
        }
        GoalCommandAction::Objective => {
            let objective = command
                .objective
                .with_context(|| msg("goal-command-error-missing-objective", &[]))?;
            update_goal_objective(
                cp.store.as_ref(),
                cp.goal_store.as_ref(),
                &ctx,
                objective,
                Some(config),
            )
            .await
        }
        GoalCommandAction::Status => {
            status_goal(
                cp.store.as_ref(),
                cp.goal_store.as_ref(),
                &ctx,
                command.task_id,
                Some(config),
            )
            .await
        }
        GoalCommandAction::Budget => {
            update_goal_budget(
                cp.store.as_ref(),
                cp.goal_store.as_ref(),
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
                cp.goal_store.as_ref(),
                &ctx,
                command.task_id,
                Some(config),
                GoalPauseState {
                    reason: GoalPauseReason::OperatorPaused,
                    description: description.clone(),
                    blockers: description
                        .map(|message| {
                            vec![GoalBlocker {
                                kind: GoalBlockerKind::OperatorPause,
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
            let resume_reason = command.resume_reason;
            resume_goal(
                cp.store.as_ref(),
                cp.goal_store.as_ref(),
                &cp.boot_id,
                &ctx,
                resume_reason,
                Some(config),
            )
            .await
        }
        GoalCommandAction::Cancel => {
            cancel_goal(
                cp.store.as_ref(),
                cp.goal_store.as_ref(),
                &ctx,
                command.task_id,
                Some(config),
            )
            .await
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
    evaluate_goal_turn_with_verifier(ctx, config, candidate_summary, &LlmGoalVerifier).await
}

pub async fn evaluate_goal_turn_with_verifier(
    ctx: &GoalAdmissionContext,
    config: &Config,
    candidate_summary: &str,
    verifier: &dyn GoalVerifier,
) -> Result<Option<GoalTurnEvaluation>> {
    let cp = match control_plane() {
        Some(cp) => cp,
        None => return Ok(None),
    };
    let Some(resolved) = latest_active_resolved_goal(cp.goal_store.as_ref(), ctx).await? else {
        return Ok(None);
    };
    if !resolved.is_running() {
        return Ok(None);
    }

    let cost_tracker = goal_usage_ledger(Some(config));
    let usage = goal_usage_totals_from_tracker(cost_tracker.as_deref(), resolved.task_id());
    if let Some(pause) = goal_accounting_gate_pause(resolved.goal(), usage.as_ref()) {
        let task_id = resolved.task_id().to_string();
        let budget = goal_budget_summary(resolved.goal(), usage.as_ref());
        let admission = pause_goal_for_resolved_task_with_budget(
            cp.goal_store.as_ref(),
            resolved,
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

    let verifier_enabled = config.goal.verifier.enabled;
    if verifier_enabled {
        let budget = goal_budget_summary(resolved.goal(), usage.as_ref());
        publish_goal_verifier_started(resolved.task_id(), &budget);
    }

    let verifier_decision = if verifier_enabled {
        verifier
            .verify(GoalVerificationRequest {
                config,
                agent_alias: resolved.agent(),
                goal_context: ctx,
                goal: resolved.goal(),
                candidate_summary,
                cost_tracker: cost_tracker.clone(),
            })
            .await
    } else {
        Ok(GoalVerifierDecision::Complete {
            notes: crate::i18n::get_required_cli_string("goal-verifier-disabled-notes"),
        })
    };

    match verifier_decision {
        Ok(GoalVerifierDecision::Complete { notes: _ }) => {
            let current = resolve_goal(
                cp.store.as_ref(),
                cp.goal_store.as_ref(),
                ctx,
                Some(resolved.task_id().to_string()),
            )
            .await?;
            if !current.is_running() {
                return Ok(None);
            }
            let task_id = current.task_id().to_string();
            let final_usage = if verifier_enabled {
                goal_usage_totals_from_tracker(cost_tracker.as_deref(), &task_id)
            } else {
                usage
            };
            if let Some(pause) = goal_accounting_gate_pause(current.goal(), final_usage.as_ref()) {
                let budget = goal_budget_summary(current.goal(), final_usage.as_ref());
                let admission = pause_goal_for_resolved_task_with_budget(
                    cp.goal_store.as_ref(),
                    current,
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
            if !cp
                .goal_store
                .complete_running_goal_task_if_limits(
                    &task_id,
                    current.goal().effective_token_limit,
                    current.goal().effective_cost_limit_usd,
                    candidate_summary.to_string(),
                )
                .await
                .with_context(|| {
                    msg("goal-command-error-update-failed", &[("task_id", &task_id)])
                })?
            {
                return Ok(None);
            }
            let budget = goal_budget_summary(current.goal(), final_usage.as_ref());
            let admission = GoalAdmission {
                task_id: Some(task_id.clone()),
                status: TaskStatus::Completed,
                message: msg(
                    "goal-command-completed",
                    &[("task_id", &task_id), ("budget", &budget)],
                ),
                continuation_reason: None,
                continue_goal: false,
            };
            publish_goal_state_update(&admission);
            Ok(Some(GoalTurnEvaluation::Completed {
                task_id,
                message: admission.message,
            }))
        }
        Ok(GoalVerifierDecision::Continue { notes }) => {
            let current = resolve_goal(
                cp.store.as_ref(),
                cp.goal_store.as_ref(),
                ctx,
                Some(resolved.task_id().to_string()),
            )
            .await?;
            if !current.is_running() {
                return Ok(None);
            }
            let task_id = current.task_id().to_string();
            let usage = goal_usage_totals_from_tracker(cost_tracker.as_deref(), &task_id);
            if let Some(pause) = goal_accounting_gate_pause(current.goal(), usage.as_ref()) {
                let budget = goal_budget_summary(current.goal(), usage.as_ref());
                let admission = pause_goal_for_resolved_task_with_budget(
                    cp.goal_store.as_ref(),
                    current,
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
            let budget = goal_budget_summary(current.goal(), usage.as_ref());
            let admission = GoalAdmission {
                task_id: Some(task_id.clone()),
                status: TaskStatus::Running,
                message: msg(
                    "goal-command-continuing",
                    &[("task_id", &task_id), ("budget", &budget)],
                ),
                continuation_reason: None,
                continue_goal: true,
            };
            publish_goal_state_update(&admission);
            Ok(Some(GoalTurnEvaluation::Continue {
                task_id,
                objective: current.objective().to_string(),
                notes,
                message: admission.message,
            }))
        }
        Ok(GoalVerifierDecision::Blocked { pause }) => {
            let task_id = resolved.task_id().to_string();
            let admission =
                pause_goal_for_known_blocker(cp.goal_store.as_ref(), resolved, Some(config), pause)
                    .await?;
            publish_goal_state_update(&admission);
            Ok(Some(GoalTurnEvaluation::Paused {
                task_id,
                message: admission.message,
            }))
        }
        Err(error) => {
            let task_id = resolved.task_id().to_string();
            if is_goal_accounting_failure(&error) {
                let cause = if is_goal_accounting_pricing_failure(&error) {
                    GoalBudgetUnavailableCause::CostPricingUnavailable
                } else {
                    GoalBudgetUnavailableCause::UsageUnavailable
                };
                let pause = goal_accounting_unavailable_pause(resolved.goal(), cause);
                let budget = goal_budget_summary(resolved.goal(), None);
                let admission = pause_goal_for_resolved_task_with_budget(
                    cp.goal_store.as_ref(),
                    resolved,
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
            let admission = pause_goal_for_known_blocker(
                cp.goal_store.as_ref(),
                resolved,
                Some(config),
                verifier_outage_pause(&error),
            )
            .await?;
            publish_goal_state_update(&admission);
            Ok(Some(GoalTurnEvaluation::Paused {
                task_id,
                message: admission.message,
            }))
        }
    }
}

pub async fn pause_current_goal_for_human_gate(
    ctx: &GoalAdmissionContext,
    config: Option<&Config>,
    kind: GoalBlockerKind,
    message: String,
    payload: Option<serde_json::Value>,
) -> Result<Option<GoalAdmission>> {
    match kind {
        GoalBlockerKind::NeedsUserInput | GoalBlockerKind::HumanEscalation => {}
        _ => bail!("human gate pause requires a human-gate blocker kind"),
    }
    let Some(cp) = control_plane() else {
        return Ok(None);
    };
    let Some(resolved) = latest_active_resolved_goal(cp.goal_store.as_ref(), ctx).await? else {
        return Ok(None);
    };
    let admission = pause_goal_for_known_blocker(
        cp.goal_store.as_ref(),
        resolved,
        config,
        GoalPauseState {
            reason: reason_for_blocker_kind(kind),
            description: Some(message.clone()),
            blockers: vec![GoalBlocker {
                kind,
                message,
                payload,
            }],
        },
    )
    .await?;
    publish_goal_state_update(&admission);
    Ok(Some(admission))
}

async fn start_goal(
    goal_store: &dyn GoalTaskRegistry,
    boot_id: &str,
    ctx: GoalAdmissionContext,
    objective: String,
    token_limit: Option<u64>,
    cost_limit_usd: Option<f64>,
    config: Option<&Config>,
) -> Result<GoalAdmission> {
    let initial_usage = initial_goal_usage_totals(config);
    ensure_cost_budget_tracking_available(config, cost_limit_usd, Some(initial_usage.is_some()))?;
    let continuation_context = ctx.continuation_context.clone();
    if let Some(active) = goal_store
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
    let initial_pause =
        config.and_then(|_| goal_accounting_gate_pause(&goal, initial_usage.as_ref()));
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
    let message = msg(
        message_key,
        &[
            ("task_id", &task_id),
            ("objective", &goal.objective),
            ("budget", &budget),
        ],
    );
    goal_store
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
        message,
        continuation_reason: None,
        continue_goal,
    })
}

async fn status_goal(
    store: &dyn TaskRegistry,
    goal_store: &dyn GoalTaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
    config: Option<&Config>,
) -> Result<GoalAdmission> {
    let task_goal = resolve_goal(store, goal_store, ctx, task_id).await?;
    let usage = goal_usage_totals(config, task_goal.task_id());
    let budget = goal_budget_summary(task_goal.goal(), usage.as_ref());
    Ok(GoalAdmission {
        task_id: Some(task_goal.task_id().to_string()),
        status: task_goal.status(),
        message: task_goal_status_message(&task_goal, &budget),
        continuation_reason: None,
        continue_goal: false,
    })
}

async fn update_goal_budget(
    store: &dyn TaskRegistry,
    goal_store: &dyn GoalTaskRegistry,
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
    let current = resolve_goal(store, goal_store, ctx, None).await?;
    if current.is_terminal() {
        let status = task_status_label(current.status());
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", current.task_id()), ("status", &status)]
            )
        );
    }
    let token_limit = match budgets.token_limit {
        GoalBudgetValue::Default => current.goal().effective_token_limit,
        GoalBudgetValue::Unlimited => None,
        GoalBudgetValue::Limited(value) => Some(value),
    };
    let cost_limit_usd = match budgets.cost_limit_usd {
        GoalBudgetValue::Default => current.goal().effective_cost_limit_usd,
        GoalBudgetValue::Unlimited => None,
        GoalBudgetValue::Limited(value) => Some(value),
    };
    ensure_cost_budget_tracking_available(config, cost_limit_usd, None)?;
    let task_id = current.task_id().to_string();
    goal_store
        .update_goal_limits(&task_id, token_limit, cost_limit_usd)
        .await
        .with_context(|| msg("goal-command-error-budget-failed", &[("task_id", &task_id)]))?;
    let updated = current.with_effective_limits(token_limit, cost_limit_usd);
    let usage = goal_usage_totals(config, &task_id);
    let budget = goal_budget_summary(updated.goal(), usage.as_ref());
    let pause = config
        .and_then(|_| goal_usage_ledger_gate_pause(updated.goal(), usage.as_ref()))
        .or_else(|| goal_budget_gate_pause(updated.goal(), usage.as_ref()));
    if let Some(pause) = pause {
        if !goal_store
            .pause_goal_task_if_status(
                &task_id,
                updated.status(),
                merge_budget_pause(updated.goal(), pause),
            )
            .await
            .with_context(|| msg("goal-command-error-budget-failed", &[("task_id", &task_id)]))?
        {
            bail!(
                "{}",
                msg("goal-command-error-budget-failed", &[("task_id", &task_id)])
            );
        }
        return Ok(GoalAdmission {
            task_id: Some(task_id.clone()),
            status: TaskStatus::Paused,
            message: msg(
                "goal-command-budget-updated-paused",
                &[("task_id", &task_id), ("budget", &budget)],
            ),
            continuation_reason: None,
            continue_goal: false,
        });
    }

    if updated.status() == TaskStatus::Paused && has_budget_pause(updated.goal()) {
        if let Some(pause) = remove_budget_pause(updated.goal()) {
            let blockers = blockers_summary(&pause.blockers);
            if !goal_store
                .pause_goal_task_if_status(&task_id, updated.status(), pause)
                .await
                .with_context(|| {
                    msg("goal-command-error-budget-failed", &[("task_id", &task_id)])
                })?
            {
                bail!(
                    "{}",
                    msg("goal-command-error-budget-failed", &[("task_id", &task_id)])
                );
            }
            let message = if let Some(blockers) = blockers {
                msg(
                    "goal-command-budget-updated-paused-blocked",
                    &[
                        ("task_id", &task_id),
                        ("blockers", &blockers),
                        ("budget", &budget),
                    ],
                )
            } else {
                msg(
                    "goal-command-budget-updated-paused",
                    &[("task_id", &task_id), ("budget", &budget)],
                )
            };
            return Ok(GoalAdmission {
                task_id: Some(task_id.clone()),
                status: TaskStatus::Paused,
                message,
                continuation_reason: None,
                continue_goal: false,
            });
        }

        if !goal_store
            .resume_paused_goal_task(
                &task_id,
                std::process::id(),
                boot_id,
                ctx.continuation_context.clone(),
            )
            .await
            .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task_id)]))?
        {
            bail!(
                "{}",
                msg("goal-command-error-update-failed", &[("task_id", &task_id)])
            );
        }
        return Ok(GoalAdmission {
            task_id: Some(task_id.clone()),
            status: TaskStatus::Running,
            message: msg(
                "goal-command-budget-updated-resumed",
                &[("task_id", &task_id), ("budget", &budget)],
            ),
            continuation_reason: None,
            continue_goal: true,
        });
    }

    Ok(GoalAdmission {
        task_id: Some(task_id.clone()),
        status: updated.status(),
        message: msg(
            "goal-command-budget-updated",
            &[("task_id", &task_id), ("budget", &budget)],
        ),
        continuation_reason: None,
        continue_goal: false,
    })
}

async fn update_goal_objective(
    store: &dyn TaskRegistry,
    goal_store: &dyn GoalTaskRegistry,
    ctx: &GoalAdmissionContext,
    objective: String,
    config: Option<&Config>,
) -> Result<GoalAdmission> {
    let current = resolve_goal(store, goal_store, ctx, None).await?;
    if current.is_terminal() {
        let status = task_status_label(current.status());
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", current.task_id()), ("status", &status)]
            )
        );
    }
    let task_id = current.task_id().to_string();
    goal_store
        .update_goal_objective(&task_id, &objective)
        .await
        .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task_id)]))?;
    let usage = goal_usage_totals(config, &task_id);
    let budget = goal_budget_summary(current.goal(), usage.as_ref());
    Ok(GoalAdmission {
        task_id: Some(task_id.clone()),
        status: current.status(),
        message: msg(
            "goal-command-objective-updated",
            &[
                ("task_id", &task_id),
                ("objective", &objective),
                ("budget", &budget),
            ],
        ),
        continuation_reason: None,
        continue_goal: false,
    })
}

async fn pause_goal_for_blocker(
    store: &dyn TaskRegistry,
    goal_store: &dyn GoalTaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
    config: Option<&Config>,
    pause: GoalPauseState,
) -> Result<GoalAdmission> {
    let resolved = resolve_goal(store, goal_store, ctx, task_id).await?;
    let budget = task_goal_budget_summary(&resolved, config);
    pause_goal_for_resolved_task_with_budget(goal_store, resolved, pause, budget).await
}

async fn pause_goal_for_known_blocker(
    goal_store: &dyn GoalTaskRegistry,
    task_goal: TaskGoal,
    config: Option<&Config>,
    pause: GoalPauseState,
) -> Result<GoalAdmission> {
    let usage = goal_usage_totals(config, task_goal.task_id());
    let budget = goal_budget_summary(task_goal.goal(), usage.as_ref());
    pause_goal_for_resolved_task_with_budget(goal_store, task_goal, pause, budget).await
}

async fn pause_goal_for_resolved_task_with_budget(
    goal_store: &dyn GoalTaskRegistry,
    task_goal: TaskGoal,
    pause: GoalPauseState,
    budget: String,
) -> Result<GoalAdmission> {
    ensure_goal_not_terminal(task_goal.task())?;
    let task_id = task_goal.task_id().to_string();
    let message_key = goal_pause_message_key(pause.reason);
    if !goal_store
        .pause_goal_task_if_status(&task_id, task_goal.status(), pause)
        .await
        .with_context(|| msg("goal-command-error-pause-failed", &[("task_id", &task_id)]))?
    {
        bail!(
            "{}",
            msg("goal-command-error-pause-failed", &[("task_id", &task_id)])
        );
    }
    Ok(GoalAdmission {
        task_id: Some(task_id.clone()),
        status: TaskStatus::Paused,
        message: msg(message_key, &[("task_id", &task_id), ("budget", &budget)]),
        continuation_reason: None,
        continue_goal: false,
    })
}

fn goal_pause_message_key(reason: GoalPauseReason) -> &'static str {
    match reason {
        GoalPauseReason::BudgetExhausted => "goal-command-budget-exhausted",
        GoalPauseReason::BudgetUnavailable => "goal-command-budget-unavailable",
        _ => "goal-command-paused",
    }
}

fn ensure_goal_not_terminal(task: &TaskRecord) -> Result<()> {
    if task.status.is_terminal() {
        let status = task_status_label(task.status);
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
    goal_store: &dyn GoalTaskRegistry,
    boot_id: &str,
    ctx: &GoalAdmissionContext,
    resume_reason: Option<String>,
    config: Option<&Config>,
) -> Result<GoalAdmission> {
    let current = resolve_goal(store, goal_store, ctx, None).await?;
    if current.is_terminal() {
        let status = task_status_label(current.status());
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", current.task_id()), ("status", &status)]
            )
        );
    }
    let task_id = current.task_id().to_string();
    let current_usage = goal_usage_totals(config, &task_id);
    let pause = config
        .and_then(|_| goal_usage_ledger_gate_pause(current.goal(), current_usage.as_ref()))
        .or_else(|| goal_budget_gate_pause(current.goal(), current_usage.as_ref()));
    if let Some(pause) = pause {
        let message_key = goal_pause_message_key(pause.reason);
        let budget = goal_budget_summary(current.goal(), current_usage.as_ref());
        if !goal_store
            .pause_goal_task_if_status(
                &task_id,
                current.status(),
                merge_budget_pause(current.goal(), pause),
            )
            .await
            .with_context(|| msg("goal-command-error-resume-failed", &[("task_id", &task_id)]))?
        {
            bail!(
                "{}",
                msg("goal-command-error-resume-failed", &[("task_id", &task_id)])
            );
        }
        return Ok(GoalAdmission {
            task_id: Some(task_id.clone()),
            status: TaskStatus::Paused,
            message: msg(message_key, &[("task_id", &task_id), ("budget", &budget)]),
            continuation_reason: None,
            continue_goal: false,
        });
    }
    if !goal_store
        .resume_paused_goal_task(
            &task_id,
            std::process::id(),
            boot_id,
            ctx.continuation_context.clone(),
        )
        .await
        .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task_id)]))?
    {
        bail!(
            "{}",
            msg("goal-command-error-update-failed", &[("task_id", &task_id)])
        );
    }
    let budget = goal_budget_summary(current.goal(), current_usage.as_ref());
    Ok(GoalAdmission {
        task_id: Some(task_id.clone()),
        status: TaskStatus::Running,
        message: msg(
            "goal-command-resumed",
            &[("task_id", &task_id), ("budget", &budget)],
        ),
        continuation_reason: resume_reason,
        continue_goal: true,
    })
}

async fn cancel_goal(
    store: &dyn TaskRegistry,
    goal_store: &dyn GoalTaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
    config: Option<&Config>,
) -> Result<GoalAdmission> {
    let current = resolve_goal(store, goal_store, ctx, task_id).await?;
    if current.is_terminal() {
        let status = task_status_label(current.status());
        bail!(
            "{}",
            msg(
                "goal-command-error-already-terminal",
                &[("task_id", current.task_id()), ("status", &status)]
            )
        );
    }
    let task_id = current.task_id().to_string();
    if !goal_store
        .cancel_goal_task_if_status(
            &task_id,
            current.status(),
            msg("goal-terminal-reason-cancelled-by-controller", &[]),
        )
        .await
        .with_context(|| msg("goal-command-error-update-failed", &[("task_id", &task_id)]))?
    {
        bail!(
            "{}",
            msg("goal-command-error-update-failed", &[("task_id", &task_id)])
        );
    }
    let usage = goal_usage_totals(config, &task_id);
    let budget = goal_budget_summary(current.goal(), usage.as_ref());
    Ok(GoalAdmission {
        task_id: Some(task_id.clone()),
        status: TaskStatus::Cancelled,
        message: msg(
            "goal-command-cancelled",
            &[("task_id", &task_id), ("budget", &budget)],
        ),
        continuation_reason: None,
        continue_goal: false,
    })
}

fn task_goal_status_message(task_goal: &TaskGoal, budget: &str) -> String {
    if let Some(reason) = task_goal.goal().pause_reason {
        let status = task_status_label(task_goal.status());
        let reason = pause_reason_label(reason);
        if let Some(blockers) = blockers_summary(&task_goal.goal().blockers) {
            msg(
                "goal-command-status-paused-blocked",
                &[
                    ("task_id", task_goal.task_id()),
                    ("status", &status),
                    ("objective", task_goal.objective()),
                    ("reason", &reason),
                    ("blockers", &blockers),
                    ("budget", budget),
                ],
            )
        } else {
            msg(
                "goal-command-status-paused",
                &[
                    ("task_id", task_goal.task_id()),
                    ("status", &status),
                    ("objective", task_goal.objective()),
                    ("reason", &reason),
                    ("budget", budget),
                ],
            )
        }
    } else {
        let status = task_status_label(task_goal.status());
        msg(
            "goal-command-status",
            &[
                ("task_id", task_goal.task_id()),
                ("status", &status),
                ("objective", task_goal.objective()),
                ("budget", budget),
            ],
        )
    }
}

/// Admit a controller-synthesized autonomous goal continuation.
///
/// This is a pre-model-call gate for synthetic goal turns. It does not create
/// usage state and does not cache budget counters: effective limits come from
/// the goal extension record, while consumed usage is derived from cost ledger
/// rows for the canonical task id. `Ok(None)` means the turn may proceed.
pub async fn admit_goal_autonomous_turn(
    ctx: &GoalAdmissionContext,
    config: &Config,
) -> Result<Option<GoalAdmission>> {
    let Some(cp) = control_plane() else {
        return Ok(None);
    };
    let Some(resolved) = latest_active_resolved_goal(cp.goal_store.as_ref(), ctx).await? else {
        return Ok(None);
    };
    if !resolved.is_running() {
        let usage = goal_usage_totals(Some(config), resolved.task_id());
        let budget = goal_budget_summary(resolved.goal(), usage.as_ref());
        return Ok(Some(GoalAdmission {
            task_id: Some(resolved.task_id().to_string()),
            status: resolved.status(),
            message: task_goal_status_message(&resolved, &budget),
            continuation_reason: None,
            continue_goal: false,
        }));
    }
    let usage = goal_usage_totals(Some(config), resolved.task_id());
    if let Some(pause) = goal_accounting_gate_pause(resolved.goal(), usage.as_ref()) {
        let budget = goal_budget_summary(resolved.goal(), usage.as_ref());
        return pause_goal_for_resolved_task_with_budget(
            cp.goal_store.as_ref(),
            resolved,
            pause,
            budget,
        )
        .await
        .map(Some);
    }
    Ok(None)
}

async fn resolve_goal_task(
    store: &dyn TaskRegistry,
    goal_store: &dyn GoalTaskRegistry,
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

    let task = goal_store
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

async fn load_goal_extension(
    goal_store: &dyn GoalTaskRegistry,
    task: TaskRecord,
) -> Result<TaskGoal> {
    let goal = goal_store
        .get_goal_task(&task.id)
        .await
        .with_context(|| msg("goal-command-error-status-failed", &[]))?
        .with_context(|| {
            msg(
                "goal-command-error-extension-missing",
                &[("task_id", &task.id)],
            )
        })?;
    Ok(TaskGoal::new(task, goal))
}

async fn resolve_goal(
    store: &dyn TaskRegistry,
    goal_store: &dyn GoalTaskRegistry,
    ctx: &GoalAdmissionContext,
    task_id: Option<String>,
) -> Result<TaskGoal> {
    let task = resolve_goal_task(store, goal_store, ctx, task_id).await?;
    load_goal_extension(goal_store, task).await
}

async fn latest_active_resolved_goal(
    goal_store: &dyn GoalTaskRegistry,
    ctx: &GoalAdmissionContext,
) -> Result<Option<TaskGoal>> {
    let Some(task) = goal_store
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
    load_goal_extension(goal_store, task).await.map(Some)
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
        config.cost.enabled = false;
        config.goal.enabled = true;
        config
    }

    fn global_test_stores() -> (Arc<dyn TaskRegistry>, Arc<dyn GoalTaskRegistry>) {
        match crate::control_plane::control_plane() {
            Some(control_plane) => (
                Arc::clone(&control_plane.store),
                Arc::clone(&control_plane.goal_store),
            ),
            None => {
                let sqlite_store =
                    Arc::new(crate::control_plane::SqliteTaskStore::new_in_memory().unwrap());
                let store: Arc<dyn TaskRegistry> = sqlite_store.clone();
                let goal_store: Arc<dyn GoalTaskRegistry> = sqlite_store;
                let _ = crate::control_plane::init_control_plane(
                    crate::control_plane::ControlPlaneHandle {
                        store: Arc::clone(&store),
                        goal_store: Arc::clone(&goal_store),
                        boot_id: "test-boot".into(),
                        recovered_goal_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
                        data_dir_lock: None,
                    },
                );
                (
                    Arc::clone(&crate::control_plane::control_plane().unwrap().store),
                    Arc::clone(&crate::control_plane::control_plane().unwrap().goal_store),
                )
            }
        }
    }

    static GOAL_COST_TRACKER_TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

    async fn goal_cost_tracker_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
        // Goal budget tests intentionally exercise the process-global
        // `CostTracker`, whose config and data-dir are hot-swapped on access.
        // Serializing only those tests prevents unrelated parallel tests from
        // disabling or retargeting the tracker between verifier usage recording
        // and the controller's budget read.
        GOAL_COST_TRACKER_TEST_LOCK.lock().await
    }

    fn cost_enabled_test_config(data_dir: &std::path::Path) -> Config {
        let mut config = test_config();
        config.data_dir = data_dir.to_path_buf();
        config.cost.enabled = true;
        config.cost.track_per_agent = true;
        config
    }

    /// Test-only fixture for a single running goal scoped to one route/principal.
    ///
    /// The fixture keeps only handles and identifiers needed by assertions; the
    /// canonical lifecycle and goal-specific state live in the in-memory task
    /// registry rows created by `create_running_goal_fixture`.
    struct RunningGoalFixture {
        /// Canonical task registry handle used to assert lifecycle transitions.
        store: Arc<dyn TaskRegistry>,
        /// Canonical goal extension registry handle used to assert pause data.
        goal_store: Arc<dyn GoalTaskRegistry>,
        /// Goal task id created for this test case.
        task_id: String,
        /// Trusted route/principal context that can see the fixture goal.
        ctx: GoalAdmissionContext,
    }

    async fn create_running_goal_fixture(objective: &str) -> RunningGoalFixture {
        let (store, goal_store) = global_test_stores();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        goal_store
            .create_goal(
                TaskRecord {
                    id: task_id.clone(),
                    kind: TaskKind::Goal,
                    agent,
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
                    objective: objective.into(),
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
        RunningGoalFixture {
            store,
            goal_store,
            task_id,
            ctx,
        }
    }

    fn record_goal_token_usage(config: &Config, agent: &str, task_id: &str, tokens: u64) {
        let tracker = CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir)
            .expect("enabled test cost tracker");
        tracker
            .record_usage_with_task_attribution(
                zeroclaw_config::cost::types::TokenUsage::new(
                    "test/model",
                    tokens,
                    0,
                    0,
                    1.0,
                    2.0,
                    0.0,
                ),
                Some(agent),
                Some(task_id),
            )
            .expect("record goal usage");
    }

    async fn create_budget_paused_goal(
        store: &SqliteTaskStore,
        ctx: &GoalAdmissionContext,
        task_id: &str,
        token_limit: u64,
        continuation_context: Option<TaskContinuationContext>,
    ) {
        store
            .create_goal(
                TaskRecord {
                    id: task_id.to_string(),
                    kind: TaskKind::Goal,
                    agent: ctx.agent_alias.clone(),
                    status: TaskStatus::Paused,
                    owner_pid: std::process::id(),
                    owner_boot_id: "boot-exhausted".into(),
                    heartbeat_at: None,
                    depth: 0,
                    parent_id: None,
                    originator_route: ctx.originator_route.clone(),
                    delivered: false,
                    idem_key: None,
                    principal_id: ctx.principal_id.clone(),
                    started_at: chrono::Utc::now().to_rfc3339(),
                    finished_at: None,
                },
                GoalTaskRecord {
                    task_id: task_id.to_string(),
                    objective: "finish budgeted work".into(),
                    effective_token_limit: Some(token_limit),
                    effective_cost_limit_usd: None,
                    pause_reason: Some(GoalPauseReason::BudgetExhausted),
                    pause_description: Some("token budget exhausted".into()),
                    blockers: vec![GoalBlocker {
                        kind: GoalBlockerKind::Budget,
                        message: "Token budget exhausted".into(),
                        payload: None,
                    }],
                },
                continuation_context,
            )
            .await
            .expect("create budget-paused goal fixture");
    }

    /// Deterministic verifier fixture for controller transition tests.
    ///
    /// The production verifier is a pluggable `GoalVerifier`; this fixture keeps
    /// tests focused on how the controller consumes typed verdicts without
    /// introducing model calls or mutating durable state itself.
    #[derive(Clone)]
    struct StubGoalVerifier {
        /// Verdict returned to the controller for this test case.
        decision: GoalVerifierDecision,
    }

    #[async_trait::async_trait]
    impl GoalVerifier for StubGoalVerifier {
        async fn verify(
            &self,
            request: GoalVerificationRequest<'_>,
        ) -> Result<GoalVerifierDecision> {
            assert!(!request.goal.objective.trim().is_empty());
            Ok(self.decision.clone())
        }
    }

    /// Verifier fixture that exercises controller handling of verifier outages.
    ///
    /// It deliberately returns no typed verdict so the controller must translate
    /// the failure into a durable verifier pause rather than completing the goal.
    struct FailingGoalVerifier;

    #[async_trait::async_trait]
    impl GoalVerifier for FailingGoalVerifier {
        async fn verify(
            &self,
            request: GoalVerificationRequest<'_>,
        ) -> Result<GoalVerifierDecision> {
            assert_eq!(request.candidate_summary, "looks done");
            Err(anyhow::Error::msg("provider offline"))
        }
    }

    #[test]
    fn parse_goal_start_keeps_objective_untrusted_payload_only() {
        let parsed = parse_goal_command("/goal start ship the thing").unwrap();
        assert_eq!(parsed.action, GoalCommandAction::Start);
        assert_eq!(parsed.objective.as_deref(), Some("ship the thing"));
        assert!(parsed.task_id.is_none());
        assert!(parsed.resume_reason.is_none());

        let parsed = parse_goal_command("/goal@zeroclaw_bot START ship the thing").unwrap();
        assert_eq!(parsed.action, GoalCommandAction::Start);
        assert_eq!(parsed.objective.as_deref(), Some("ship the thing"));

        let err = parse_goal_command("/unknown start ship the thing").unwrap_err();
        assert!(err.to_string().contains("must start with `/goal`"));

        let err = parse_goal_command("start ship the thing").unwrap_err();
        assert!(err.to_string().contains("must start with `/goal`"));

        let err = parse_goal_command("goal start ship the thing").unwrap_err();
        assert!(err.to_string().contains("must start with `/goal`"));

        let parsed = parse_goal_command("/goal resume fixed").unwrap();
        assert_eq!(parsed.action, GoalCommandAction::Resume);
        assert!(parsed.task_id.is_none());
        assert_eq!(parsed.resume_reason.as_deref(), Some("fixed"));
    }

    #[test]
    fn parse_goal_objective_requires_freeform_objective_payload() {
        let parsed = parse_goal_command("/goal objective revise scope after evidence").unwrap();
        assert_eq!(parsed.action, GoalCommandAction::Objective);
        assert_eq!(
            parsed.objective.as_deref(),
            Some("revise scope after evidence")
        );
        assert!(parsed.task_id.is_none());
        assert!(parsed.resume_reason.is_none());

        let err = parse_goal_command("/goal objective").unwrap_err();
        assert!(err.to_string().contains("non-empty objective"));
    }

    #[test]
    fn parse_goal_resume_accepts_freeform_reason_payloads() {
        let parsed = parse_goal_command("/goal resume blocker fixed, retry now").unwrap();
        assert!(parsed.task_id.is_none());
        assert_eq!(
            parsed.resume_reason.as_deref(),
            Some("blocker fixed, retry now")
        );

        let parsed = parse_goal_command("/goal resume goal-123").unwrap();
        assert!(parsed.task_id.is_none());
        assert_eq!(parsed.resume_reason.as_deref(), Some("goal-123"));

        let task_id = uuid::Uuid::new_v4().to_string();
        let parsed =
            parse_goal_command(&format!("/goal resume {task_id} retry after fix")).unwrap();
        assert!(parsed.task_id.is_none());
        let expected = format!("{task_id} retry after fix");
        assert_eq!(parsed.resume_reason.as_deref(), Some(expected.as_str()));

        let parsed = parse_goal_command("/goal resume --some-flag-looking reason").unwrap();
        assert!(parsed.task_id.is_none());
        assert_eq!(
            parsed.resume_reason.as_deref(),
            Some("--some-flag-looking reason")
        );
    }

    #[test]
    fn parse_goal_task_selectors_reject_extra_arguments() {
        let status = parse_goal_command("/goal status goal-123").unwrap();
        assert_eq!(status.task_id.as_deref(), Some("goal-123"));

        let cancel = parse_goal_command("/goal cancel goal-123").unwrap();
        assert_eq!(cancel.task_id.as_deref(), Some("goal-123"));

        let status_err = parse_goal_command("/goal status goal-123 extra").unwrap_err();
        assert!(status_err.to_string().contains("Unexpected goal arguments"));

        let cancel_err = parse_goal_command("/goal cancel goal-123 extra").unwrap_err();
        assert!(cancel_err.to_string().contains("Unexpected goal arguments"));
    }

    #[test]
    fn parse_goal_help_and_budget_flags() {
        let help = parse_goal_command("/goal help").unwrap();
        assert_eq!(help.action, GoalCommandAction::Help);

        let help = parse_goal_command("/goal --help").unwrap();
        assert_eq!(help.action, GoalCommandAction::Help);

        let help = parse_goal_command("/goal -h").unwrap();
        assert_eq!(help.action, GoalCommandAction::Help);

        let err = parse_goal_command("/goal").unwrap_err();
        assert!(err.to_string().contains("requires an action"));

        let help_text = msg("goal-command-help", &[]);
        for expected in [
            "/goal start [--tokens=N|unlimited] [--cost=N|unlimited] <objective>",
            "/goal objective <objective>",
            "/goal status [task_id]",
            "/goal budget [--tokens=N|unlimited] [--cost=N|unlimited]",
            "/goal pause [reason]",
            "/goal resume [reason]",
            "/goal cancel [task_id]",
            "/goal help | /goal --help | /goal -h",
        ] {
            assert!(
                help_text.contains(expected),
                "goal help must list supported syntax {expected:?}; help was: {help_text}"
            );
        }

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
            cost_pricing_available: true,
            cost_tracking_available: true,
            usage_available: true,
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
        assert_eq!(
            pause.blockers[0].payload.as_ref().unwrap()["cost_pricing_unavailable"],
            false
        );
    }

    #[test]
    fn unlimited_goal_does_not_pause_when_usage_is_unavailable() {
        let goal = GoalTaskRecord {
            task_id: "goal-1".into(),
            objective: "ship it".into(),
            effective_token_limit: None,
            effective_cost_limit_usd: None,
            pause_reason: None,
            pause_description: None,
            blockers: Vec::new(),
        };
        let usage = GoalUsageTotals {
            usage_available: false,
            ..GoalUsageTotals::default()
        };

        assert!(goal_budget_gate_pause(&goal, Some(&usage)).is_none());
    }

    #[test]
    fn unlimited_goal_pauses_when_the_canonical_ledger_is_unavailable() {
        let goal = GoalTaskRecord {
            task_id: "goal-1".into(),
            objective: "ship it".into(),
            effective_token_limit: None,
            effective_cost_limit_usd: None,
            pause_reason: None,
            pause_description: None,
            blockers: Vec::new(),
        };

        let pause = goal_usage_ledger_gate_pause(&goal, None)
            .expect("a missing ledger cannot retain goal-attributed usage");

        assert_eq!(pause.reason, GoalPauseReason::BudgetUnavailable);
        assert_eq!(pause.blockers[0].kind, GoalBlockerKind::Budget);
    }

    #[test]
    fn goal_budget_gate_treats_unpriced_cost_usage_as_unavailable() {
        let mut goal = GoalTaskRecord {
            task_id: "goal-1".into(),
            objective: "ship it".into(),
            effective_token_limit: Some(1_000),
            effective_cost_limit_usd: Some(0.50),
            pause_reason: None,
            pause_description: None,
            blockers: Vec::new(),
        };
        let usage = GoalUsageTotals {
            total_tokens: 250,
            cost_usd: 0.0,
            cost_pricing_available: false,
            cost_tracking_available: true,
            usage_available: true,
        };

        let pause = goal_budget_gate_pause(&goal, Some(&usage)).unwrap();

        assert_eq!(pause.reason, GoalPauseReason::BudgetUnavailable);
        assert_eq!(pause.blockers[0].kind, GoalBlockerKind::Budget);
        assert_eq!(
            pause.blockers[0].payload.as_ref().unwrap()["usage_unavailable"],
            false
        );
        assert_eq!(
            pause.blockers[0].payload.as_ref().unwrap()["cost_pricing_unavailable"],
            true
        );

        goal.effective_cost_limit_usd = None;
        assert!(
            goal_budget_gate_pause(&goal, Some(&usage)).is_none(),
            "unpriced cost must not block a token-only budget"
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
        let ctx = GoalAdmissionContext::new("agent-a").with_channel_type(Some("matrix".into()));
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

    #[tokio::test]
    async fn goal_help_admission_honors_disabled_config() {
        let ctx = GoalAdmissionContext::new("agent-a").with_channel_type(Some("matrix".into()));
        let command = GoalCommand {
            action: GoalCommandAction::Help,
            objective: None,
            task_id: None,
            resume_reason: None,
            budgets: GoalBudgetOverrides::default(),
        };
        let mut config = test_config();

        let help = admit_goal_command(ctx.clone(), command.clone(), &config, None)
            .await
            .unwrap();
        assert_eq!(help.message, msg("goal-command-help", &[]));
        assert!(help.message.contains("/goal help"));
        assert!(help.message.contains("/goal --help"));
        assert!(help.message.contains("/goal -h"));
        assert!(
            help.message
                .contains("/goal budget [--tokens=N|unlimited] [--cost=N|unlimited]")
        );
        assert!(!help.continue_goal);

        config.goal.enabled = false;
        let err = admit_goal_command(ctx, command, &config, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("goal.enabled = false"));
    }

    #[tokio::test]
    async fn goal_pause_command_records_operator_pause_reason() {
        let (_store, goal_store) = global_test_stores();
        let config = test_config();
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent)
            .with_channel_type(Some("matrix".into()))
            .with_originator_route(Some(format!("matrix:{}", uuid::Uuid::new_v4())))
            .with_principal_id(Some(format!("principal-{}", uuid::Uuid::new_v4())));

        let started = admit_goal_command(
            ctx.clone(),
            parse_goal_command("/goal start finish the operator pause test").unwrap(),
            &config,
            None,
        )
        .await
        .unwrap();
        let task_id = started.task_id.expect("start returns task id");

        let paused = admit_goal_command(
            ctx.clone(),
            parse_goal_command("/goal pause maintenance window").unwrap(),
            &config,
            None,
        )
        .await
        .unwrap();

        assert_eq!(paused.status, TaskStatus::Paused);
        let goal = goal_store
            .get_goal_task(&task_id)
            .await
            .unwrap()
            .expect("goal extension is persisted");
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::OperatorPaused));
        assert_eq!(goal.blockers[0].kind, GoalBlockerKind::OperatorPause);
        assert_eq!(goal.blockers[0].message, "maintenance window");

        let status = admit_goal_command(
            ctx,
            parse_goal_command("/goal status").unwrap(),
            &config,
            None,
        )
        .await
        .unwrap();
        assert!(status.message.contains("operator paused"));
        assert!(
            status
                .message
                .contains("operator pause: maintenance window")
        );
        assert!(!status.message.contains("human escalation"));
    }

    #[test]
    fn goal_policy_rejects_disallowed_surface_and_channel_type() {
        let mut config = test_config();
        config.goal.allowed_command_surfaces = vec!["web".into()];
        let ctx = GoalAdmissionContext::new("agent-a").with_channel_type(Some("matrix".into()));
        let err = ensure_goal_admitted_by_config(&ctx, &config, None).unwrap_err();
        assert!(err.to_string().contains("command surface `channel`"));

        config.goal.allowed_command_surfaces = vec!["channel".into()];
        config.goal.allowed_channel_types = vec!["telegram".into()];
        let err = ensure_goal_admitted_by_config(&ctx, &config, None).unwrap_err();
        assert!(err.to_string().contains("channel type `matrix`"));

        let missing_channel_type = GoalAdmissionContext::new("agent-a");
        let err = ensure_goal_admitted_by_config(&missing_channel_type, &config, None).unwrap_err();
        assert!(err.to_string().contains("channel type is unavailable"));
    }

    #[tokio::test]
    async fn scoped_goal_state_update_publishes_channel_message() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let admission = GoalAdmission {
            task_id: Some("goal-1".into()),
            status: TaskStatus::Running,
            message: "Goal `goal-1` started.".into(),
            continuation_reason: None,
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
    async fn autonomous_turn_budget_gate_allows_running_goal_under_limit() {
        let _cost_guard = goal_cost_tracker_test_lock().await;
        let (_store, goal_store) = global_test_stores();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        goal_store
            .create_goal(
                TaskRecord {
                    id: task_id.clone(),
                    kind: TaskKind::Goal,
                    agent,
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
                    task_id,
                    objective: "ship it".into(),
                    effective_token_limit: Some(10_000),
                    effective_cost_limit_usd: None,
                    pause_reason: None,
                    pause_description: None,
                    blockers: Vec::new(),
                },
                None,
            )
            .await
            .unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = test_config();
        config.data_dir = tmp.path().to_path_buf();
        config.cost.enabled = true;
        let _tracker =
            CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir).unwrap();

        let admission = admit_goal_autonomous_turn(&ctx, &config).await.unwrap();

        assert!(admission.is_none());
    }

    #[tokio::test]
    async fn autonomous_turn_budget_gate_pauses_before_next_model_turn() {
        let _cost_guard = goal_cost_tracker_test_lock().await;
        let (store, goal_store) = global_test_stores();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        goal_store
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
                    effective_token_limit: Some(1_000),
                    effective_cost_limit_usd: None,
                    pause_reason: None,
                    pause_description: None,
                    blockers: Vec::new(),
                },
                None,
            )
            .await
            .unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = test_config();
        config.data_dir = tmp.path().to_path_buf();
        config.cost.enabled = true;
        config.cost.track_per_agent = true;
        let tracker =
            CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir).unwrap();
        tracker
            .record_usage_with_task_attribution(
                zeroclaw_config::cost::types::TokenUsage::new(
                    "test/model",
                    1_000,
                    500,
                    0,
                    1.0,
                    2.0,
                    0.0,
                ),
                Some(&agent),
                Some(&task_id),
            )
            .unwrap();

        let admission = admit_goal_autonomous_turn(&ctx, &config)
            .await
            .unwrap()
            .expect("exhausted budget should block the autonomous turn");

        assert_eq!(admission.status, TaskStatus::Paused);
        assert!(!admission.continue_goal);
        assert!(admission.message.contains("Budget:"));
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = goal_store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::BudgetExhausted));
        assert_eq!(goal.blockers[0].kind, GoalBlockerKind::Budget);
    }

    #[tokio::test]
    async fn autonomous_turn_gate_reports_already_paused_goal() {
        let (store, goal_store) = global_test_stores();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        goal_store
            .create_goal(
                TaskRecord {
                    id: task_id.clone(),
                    kind: TaskKind::Goal,
                    agent,
                    status: TaskStatus::Paused,
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
                    objective: "wait for operator".into(),
                    effective_token_limit: None,
                    effective_cost_limit_usd: None,
                    pause_reason: Some(GoalPauseReason::NeedsUserInput),
                    pause_description: Some("Need operator answer".into()),
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

        let admission = admit_goal_autonomous_turn(&ctx, &test_config())
            .await
            .unwrap()
            .expect("paused goal should stop stale autonomous continuation");

        assert_eq!(admission.status, TaskStatus::Paused);
        assert!(!admission.continue_goal);
        assert!(admission.message.contains("needs user input"));
        assert!(!admission.message.contains("needs_user_input"));
        assert_eq!(
            store.get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Paused
        );
        assert_eq!(
            goal_store
                .get_goal_task(&task_id)
                .await
                .unwrap()
                .unwrap()
                .pause_reason,
            Some(GoalPauseReason::NeedsUserInput)
        );
    }

    #[tokio::test]
    async fn evaluate_goal_turn_completes_running_goal_when_verifier_disabled() {
        let (store, goal_store) = global_test_stores();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        goal_store
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
    async fn evaluate_goal_turn_uses_injected_verifier() {
        let (store, goal_store) = global_test_stores();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        goal_store
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
        config.goal.verifier.enabled = true;
        let verifier = StubGoalVerifier {
            decision: GoalVerifierDecision::Continue {
                notes: "CONTINUE\nstub says keep going".into(),
            },
        };

        let outcome = evaluate_goal_turn_with_verifier(&ctx, &config, "not done", &verifier)
            .await
            .unwrap();

        let Some(GoalTurnEvaluation::Continue {
            task_id: continued_id,
            objective,
            notes,
            message,
        }) = outcome
        else {
            panic!("stub verifier should request another autonomous turn");
        };
        assert_eq!(continued_id, task_id);
        assert_eq!(objective, "ship it");
        assert!(notes.contains("stub says keep going"));
        assert!(message.starts_with("🔁 Goal"));
        assert!(message.contains("Budget:"));
        assert_eq!(
            store.get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
    }

    #[tokio::test]
    async fn evaluate_goal_turn_pauses_when_verifier_blocks_completion() {
        let fixture = create_running_goal_fixture("ship it").await;
        let mut config = test_config();
        config.goal.verifier.enabled = true;
        let verifier = StubGoalVerifier {
            decision: GoalVerifierDecision::Blocked {
                pause: GoalPauseState {
                    reason: GoalPauseReason::VerifierBlocked,
                    description: Some("verifier requested operator review".into()),
                    blockers: vec![GoalBlocker {
                        kind: GoalBlockerKind::Verifier,
                        message: "Verifier requested operator review".into(),
                        payload: Some(serde_json::json!({"verdict": "blocked"})),
                    }],
                },
            },
        };

        let outcome =
            evaluate_goal_turn_with_verifier(&fixture.ctx, &config, "looks done", &verifier)
                .await
                .unwrap();

        let Some(GoalTurnEvaluation::Paused {
            task_id: paused_id,
            message,
        }) = outcome
        else {
            panic!("blocked verifier verdict must pause the goal");
        };
        assert_eq!(paused_id, fixture.task_id);
        assert!(message.starts_with("⏸️ Goal"));
        assert_eq!(
            fixture
                .store
                .get(&fixture.task_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Paused
        );
        let goal = fixture
            .goal_store
            .get_goal_task(&fixture.task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::VerifierBlocked));
        assert_eq!(
            goal.pause_description.as_deref(),
            Some("verifier requested operator review")
        );
        assert_eq!(goal.blockers.len(), 1);
        assert_eq!(goal.blockers[0].kind, GoalBlockerKind::Verifier);
        assert_eq!(
            goal.blockers[0].payload.as_ref().unwrap()["verdict"],
            "blocked"
        );
    }

    #[tokio::test]
    async fn evaluate_goal_turn_pauses_when_verifier_errors() {
        let fixture = create_running_goal_fixture("ship it").await;
        let mut config = test_config();
        config.goal.verifier.enabled = true;

        let outcome = evaluate_goal_turn_with_verifier(
            &fixture.ctx,
            &config,
            "looks done",
            &FailingGoalVerifier,
        )
        .await
        .unwrap();

        let Some(GoalTurnEvaluation::Paused {
            task_id: paused_id,
            message,
        }) = outcome
        else {
            panic!("verifier outage must pause the goal");
        };
        assert_eq!(paused_id, fixture.task_id);
        assert!(message.starts_with("⏸️ Goal"));
        assert_eq!(
            fixture
                .store
                .get(&fixture.task_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Paused
        );
        let goal = fixture
            .goal_store
            .get_goal_task(&fixture.task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::VerifierBlocked));
        assert!(
            goal.pause_description
                .as_deref()
                .unwrap()
                .contains("provider offline")
        );
        assert_eq!(goal.blockers.len(), 1);
        assert_eq!(goal.blockers[0].kind, GoalBlockerKind::Verifier);
        assert!(goal.blockers[0].message.contains("provider offline"));
    }

    #[tokio::test]
    async fn evaluate_goal_turn_pauses_when_verifier_continue_exhausts_budget() {
        let _cost_guard = goal_cost_tracker_test_lock().await;
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

        let (store, goal_store) = global_test_stores();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        goal_store
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
        let goal = goal_store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::BudgetExhausted));
    }

    #[tokio::test]
    async fn unlimited_goal_completes_when_verifier_omits_usage_and_cost_tracking_is_disabled() {
        let _cost_guard = goal_cost_tracker_test_lock().await;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_config::schema::{CustomModelProviderConfig, ModelProviderConfig};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {"role": "assistant", "content": "COMPLETE\nverified"}
                }]
            })))
            .mount(&server)
            .await;

        let fixture = create_running_goal_fixture("ship it").await;

        let temp = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: temp.path().to_path_buf(),
            ..test_config()
        };
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
                    ..ModelProviderConfig::default()
                },
            },
        );

        let outcome = evaluate_goal_turn(&fixture.ctx, &config, "looks done")
            .await
            .unwrap();

        assert!(
            matches!(outcome, Some(GoalTurnEvaluation::Completed { .. })),
            "unlimited token-only verifier outcome: {outcome:?}"
        );
        assert_eq!(
            fixture
                .store
                .get(&fixture.task_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Completed
        );
        let usage = goal_usage_totals(Some(&config), &fixture.task_id).unwrap();
        assert!(!usage.usage_available);
        assert!(!usage.cost_tracking_available);
    }

    #[tokio::test]
    async fn evaluate_goal_turn_pauses_when_verifier_usage_is_unpriced_under_cost_budget() {
        let _cost_guard = goal_cost_tracker_test_lock().await;
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

        let (store, goal_store) = global_test_stores();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let model = format!("unpriced-goal-model-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        goal_store
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
                    effective_cost_limit_usd: Some(0.01),
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
        config.goal.verifier.model = Some(model.clone());
        config.providers.models.custom.insert(
            "verifier".into(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    api_key: Some("test-key".into()),
                    uri: Some(server.uri()),
                    model: Some(model),
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
            panic!("unpriced verifier usage under a cost budget must pause");
        };
        assert_eq!(paused_id, task_id);
        assert!(
            message.contains("budget accounting is unavailable"),
            "{message}"
        );
        let task = store.get(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        let goal = goal_store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::BudgetUnavailable));
    }

    #[tokio::test]
    async fn goal_start_resolves_config_default_and_explicit_budget_limits() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a").with_channel_type(Some("matrix".into()));
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
        assert!(started.message.contains("Goal `"));
        assert!(started.message.contains("started"));
        assert!(started.message.contains("Objective:"));
        assert!(started.message.contains("ship it"));
        assert!(started.message.contains("Budget: tokens 0/unlimited"));
        assert!(started.message.contains("$0.0000/$3.2500"));
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(goal.effective_token_limit, None);
        assert_eq!(goal.effective_cost_limit_usd, Some(3.25));
    }

    #[tokio::test]
    async fn token_limited_goal_starts_when_cost_tracking_is_disabled() {
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

        assert_eq!(started.status, TaskStatus::Running);
        assert!(started.continue_goal);
        assert!(started.message.contains("cost unavailable"));
        assert!(started.message.contains("Objective:"));
        assert!(started.message.contains("ship it"));
        let task_id = started.task_id.unwrap();
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(goal.pause_reason, None);
        assert!(goal.blockers.is_empty());
    }

    #[tokio::test]
    async fn unlimited_goal_starts_paused_when_the_canonical_ledger_is_unusable() {
        let _cost_guard = goal_cost_tracker_test_lock().await;
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("state/costs.jsonl")).unwrap();
        let mut config = test_config();
        config.data_dir = temp.path().to_path_buf();

        let started = start_goal(
            &store,
            "boot-a",
            GoalAdmissionContext::new("agent-a"),
            "ship it".into(),
            None,
            None,
            Some(&config),
        )
        .await
        .unwrap();

        assert_eq!(started.status, TaskStatus::Paused);
        assert!(!started.continue_goal);
        let task_id = started.task_id.unwrap();
        assert_eq!(
            store.get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Paused
        );
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::BudgetUnavailable));
    }

    #[tokio::test]
    async fn cost_limited_goal_is_rejected_before_task_creation_when_cost_tracking_is_disabled() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let config = test_config();

        let error = start_goal(
            &store,
            "boot-a",
            GoalAdmissionContext::new("agent-a"),
            "ship it".into(),
            None,
            Some(1.0),
            Some(&config),
        )
        .await
        .expect_err("disabled cost tracking must reject a finite cost limit");

        assert!(
            error
                .to_string()
                .contains("finite goal cost budget requires enabled cost tracking")
        );
        assert!(
            store
                .latest_active_goal_for_agent("agent-a")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn goal_recovery_status_message_includes_objective_and_budget() {
        let mut config = test_config();
        config.goal.token_budget = Some(25_000);
        let goal = GoalTaskRecord {
            task_id: "goal-recovered".into(),
            objective: "finish the restart smoke".into(),
            effective_token_limit: Some(12_000),
            effective_cost_limit_usd: None,
            pause_reason: None,
            pause_description: None,
            blockers: Vec::new(),
        };

        let message = goal_recovery_status_message(&goal, Some(&config));

        assert!(message.contains("recovered after service restart"));
        assert!(message.contains("Objective:"));
        assert!(message.contains("finish the restart smoke"));
        assert!(message.contains("Budget:"));
        assert!(message.contains("tokens"));
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

        let cancelled = cancel_goal(&store, &store, &ctx, Some(task_id.clone()), None)
            .await
            .unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(
            store.get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Cancelled
        );

        let err = cancel_goal(&store, &store, &ctx, Some(task_id), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already terminal"));
    }

    #[tokio::test]
    async fn goal_objective_updates_canonical_goal_extension_only() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("matrix:room-1".into()))
            .with_principal_id(Some("principal-1".into()));
        let started = start_goal(
            &store,
            "boot-a",
            ctx.clone(),
            "ship initial scope".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let task_id = started.task_id.clone().unwrap();

        let amended = update_goal_objective(
            &store,
            &store,
            &ctx,
            "ship amended scope after evidence".into(),
            Some(&test_config()),
        )
        .await
        .unwrap();

        assert_eq!(amended.task_id.as_deref(), Some(task_id.as_str()));
        assert_eq!(amended.status, TaskStatus::Running);
        assert!(
            !amended.continue_goal,
            "objective edits must not synthesize a second model turn"
        );
        assert!(amended.message.contains("objective updated"));
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(goal.objective, "ship amended scope after evidence");
        assert!(goal.effective_token_limit.is_none());
        assert!(goal.effective_cost_limit_usd.is_none());
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

        status_goal(&store, &store, &owner, Some(task_id.clone()), None)
            .await
            .unwrap();

        let wrong_route = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-2".into()))
            .with_principal_id(Some("principal-1".into()));
        let err = status_goal(&store, &store, &wrong_route, Some(task_id.clone()), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not visible from this route"));

        let wrong_principal = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some("telegram:chat-1".into()))
            .with_principal_id(Some("principal-2".into()));
        let err = status_goal(&store, &store, &wrong_principal, Some(task_id), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not visible to this principal"));
    }

    #[tokio::test]
    async fn goal_status_reports_recovered_daemon_restart_pause() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let route = "matrix:room-1".to_string();
        let principal = "principal-1".to_string();
        store
            .create_goal(
                TaskRecord {
                    id: "goal-recovered-paused".into(),
                    kind: TaskKind::Goal,
                    agent: "agent-a".into(),
                    status: TaskStatus::Running,
                    owner_pid: 999_999,
                    owner_boot_id: "boot-old".into(),
                    heartbeat_at: None,
                    depth: 0,
                    parent_id: None,
                    originator_route: Some(route.clone()),
                    delivered: false,
                    idem_key: None,
                    principal_id: Some(principal.clone()),
                    started_at: chrono::Utc::now().to_rfc3339(),
                    finished_at: None,
                },
                GoalTaskRecord {
                    task_id: "goal-recovered-paused".into(),
                    objective: "finish restart validation".into(),
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

        let report = crate::control_plane::reaper::recovery_pass(
            &store,
            &store,
            "boot-new",
            zeroclaw_config::schema::GoalRestartRecovery::Paused,
        )
        .await
        .unwrap();
        assert_eq!(report.recovered, 1);

        let ctx = GoalAdmissionContext::new("agent-a")
            .with_originator_route(Some(route))
            .with_principal_id(Some(principal));
        let status = status_goal(
            &store,
            &store,
            &ctx,
            Some("goal-recovered-paused".into()),
            Some(&test_config()),
        )
        .await
        .unwrap();

        assert_eq!(status.status, TaskStatus::Paused);
        assert!(!status.continue_goal);
        assert!(status.message.contains("daemon restarted"));
        assert!(!status.message.contains("daemon_restarted"));
        assert!(status.message.contains("restart recovery"));
        assert!(status.message.contains("finish restart validation"));
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

        let resumed = resume_goal(&store, &store, "boot-resumed", &ctx, None, None)
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
    async fn resume_reason_survives_as_transient_continuation_input_only() {
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

        pause_goal_for_blocker(
            &store,
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

        let resumed = resume_goal(
            &store,
            &store,
            "boot-resumed",
            &ctx,
            Some("operator confirmed the external deploy is healthy".into()),
            None,
        )
        .await
        .unwrap();

        assert!(resumed.continue_goal);
        assert_eq!(
            resumed.continuation_reason.as_deref(),
            Some("operator confirmed the external deploy is healthy")
        );
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert!(goal.pause_reason.is_none());
        assert!(goal.pause_description.is_none());
        assert!(goal.blockers.is_empty());
    }

    #[tokio::test]
    async fn human_gate_pause_uses_scoped_active_goal() {
        let (store, goal_store) = global_test_stores();
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent)
            .with_originator_route(Some(route))
            .with_principal_id(Some(principal));
        let started = start_goal(
            goal_store.as_ref(),
            "boot-a",
            ctx.clone(),
            "wait for operator input".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let task_id = started.task_id.unwrap();

        let admission = pause_current_goal_for_human_gate(
            &ctx,
            None,
            GoalBlockerKind::NeedsUserInput,
            "Need operator answer".into(),
            Some(serde_json::json!({"tool": "ask_user", "question": "continue?"})),
        )
        .await
        .unwrap()
        .expect("active scoped goal should be paused");

        assert_eq!(admission.task_id.as_deref(), Some(task_id.as_str()));
        assert_eq!(admission.status, TaskStatus::Paused);
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = goal_store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::NeedsUserInput));
        assert_eq!(goal.blockers[0].kind, GoalBlockerKind::NeedsUserInput);
        assert_eq!(
            goal.blockers[0].payload.as_ref().unwrap()["tool"],
            "ask_user"
        );
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

    #[tokio::test]
    async fn budget_update_does_not_resume_when_the_canonical_ledger_is_unusable() {
        let _cost_guard = goal_cost_tracker_test_lock().await;
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.data_dir = temp.path().to_path_buf();
        let ctx = GoalAdmissionContext::new("agent-a");
        let started = start_goal(
            &store,
            "boot-a",
            ctx.clone(),
            "ship it".into(),
            Some(10),
            None,
            Some(&config),
        )
        .await
        .unwrap();
        let task_id = started.task_id.unwrap();
        pause_goal_for_blocker(
            &store,
            &store,
            &ctx,
            Some(task_id.clone()),
            Some(&config),
            GoalPauseState {
                reason: GoalPauseReason::BudgetUnavailable,
                description: Some("ledger unavailable".into()),
                blockers: vec![GoalBlocker {
                    kind: GoalBlockerKind::Budget,
                    message: "Ledger unavailable".into(),
                    payload: None,
                }],
            },
        )
        .await
        .unwrap();
        std::fs::remove_file(temp.path().join("state/costs.jsonl")).unwrap();
        std::fs::create_dir_all(temp.path().join("state/costs.jsonl")).unwrap();

        let admitted = update_goal_budget(
            &store,
            &store,
            "boot-resumed",
            &ctx,
            GoalBudgetOverrides {
                token_limit: GoalBudgetValue::Unlimited,
                cost_limit_usd: GoalBudgetValue::Unlimited,
            },
            Some(&config),
        )
        .await
        .unwrap();

        assert_eq!(admitted.status, TaskStatus::Paused);
        assert!(!admitted.continue_goal);
        assert_eq!(
            store.get(&task_id).await.unwrap().unwrap().status,
            TaskStatus::Paused
        );
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::BudgetUnavailable));
    }

    #[tokio::test]
    async fn budget_update_keeps_goal_paused_when_new_token_limit_is_still_exhausted() {
        let _cost_guard = goal_cost_tracker_test_lock().await;
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(format!("route-{}", uuid::Uuid::new_v4())))
            .with_principal_id(Some(format!("principal-{}", uuid::Uuid::new_v4())));
        let tmp = tempfile::TempDir::new().unwrap();
        let config = cost_enabled_test_config(tmp.path());

        // User-visible policy: lowering an exhausted 100k-token goal to 80k is
        // only a limit update. It must not clear the budget blocker or spend
        // another autonomous turn.
        create_budget_paused_goal(&store, &ctx, &task_id, 100_000, None).await;
        record_goal_token_usage(&config, &agent, &task_id, 100_000);

        let admitted = update_goal_budget(
            &store,
            &store,
            "boot-budget-update",
            &ctx,
            GoalBudgetOverrides {
                token_limit: GoalBudgetValue::Limited(80_000),
                cost_limit_usd: GoalBudgetValue::Default,
            },
            Some(&config),
        )
        .await
        .unwrap();

        assert_eq!(admitted.status, TaskStatus::Paused);
        assert!(!admitted.continue_goal);
        assert!(admitted.message.contains("budget updated; goal is paused"));
        assert!(admitted.message.contains("tokens 100000/80000"));
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.effective_token_limit, Some(80_000));
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::BudgetExhausted));
        assert_eq!(goal.blockers.len(), 1);
        assert_eq!(goal.blockers[0].kind, GoalBlockerKind::Budget);
    }

    #[tokio::test]
    async fn budget_update_resumes_goal_when_new_token_limit_clears_exhaustion() {
        let _cost_guard = goal_cost_tracker_test_lock().await;
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let continuation_context = TaskContinuationContext {
            channel: "matrix".into(),
            channel_alias: Some("work".into()),
            reply_target: "!room:example.org".into(),
            sender: "@operator:example.org".into(),
            thread_ts: Some("$root".into()),
            interruption_scope_id: Some("$root".into()),
            conversation_scope: TaskContinuationConversationScope::ReplyTarget,
        };
        let ctx = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some(format!("route-{}", uuid::Uuid::new_v4())))
            .with_principal_id(Some(format!("principal-{}", uuid::Uuid::new_v4())))
            .with_continuation_context(Some(continuation_context.clone()));
        let tmp = tempfile::TempDir::new().unwrap();
        let config = cost_enabled_test_config(tmp.path());

        // User-visible policy: raising an exhausted 100k-token goal to 120k
        // clears a pure budget pause and re-enters the trusted continuation
        // path instead of requiring a separate `/goal resume`.
        create_budget_paused_goal(
            &store,
            &ctx,
            &task_id,
            100_000,
            Some(continuation_context.clone()),
        )
        .await;
        record_goal_token_usage(&config, &agent, &task_id, 100_000);

        let admitted = update_goal_budget(
            &store,
            &store,
            "boot-budget-update",
            &ctx,
            GoalBudgetOverrides {
                token_limit: GoalBudgetValue::Limited(120_000),
                cost_limit_usd: GoalBudgetValue::Default,
            },
            Some(&config),
        )
        .await
        .unwrap();

        assert_eq!(admitted.status, TaskStatus::Running);
        assert!(admitted.continue_goal);
        assert!(admitted.message.contains("budget updated and resumed"));
        assert!(admitted.message.contains("tokens 100000/120000"));
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.owner_boot_id, "boot-budget-update");
        assert_eq!(goal.effective_token_limit, Some(120_000));
        assert!(goal.pause_reason.is_none());
        assert!(goal.blockers.is_empty());
        assert_eq!(
            store.get_continuation_context(&task_id).await.unwrap(),
            Some(continuation_context)
        );
    }

    #[tokio::test]
    async fn budget_update_reports_remaining_non_budget_blockers() {
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let ctx = GoalAdmissionContext::new("agent-a");
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
            &store,
            &ctx,
            Some(task_id.clone()),
            None,
            GoalPauseState {
                reason: GoalPauseReason::BudgetExhausted,
                description: Some("multiple blockers".into()),
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
            },
        )
        .await
        .unwrap();

        let admitted = update_goal_budget(
            &store,
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

        assert_eq!(admitted.status, TaskStatus::Paused);
        assert!(!admitted.continue_goal);
        assert!(admitted.message.contains("still paused"));
        assert!(
            admitted
                .message
                .contains("user input: Need operator answer")
        );
        let task = store.get(&task_id).await.unwrap().unwrap();
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::NeedsUserInput));
        assert_eq!(goal.blockers.len(), 1);
        assert_eq!(goal.blockers[0].kind, GoalBlockerKind::NeedsUserInput);
    }
}
