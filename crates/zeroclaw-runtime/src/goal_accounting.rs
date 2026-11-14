//! Runtime persistence adapter for Goal-owned provider accounting contracts.

use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroclaw_api::model_provider::TokenUsage;
use zeroclaw_config::cost::{CostTracker, types::TokenUsage as CostTokenUsage};
use zeroclaw_goal::{
    GoalAccountingState, GoalExecutionScope, GoalTaskRegistry, GoalTransitionResult,
    accounting::{
        GoalAbandonmentSettlement, GoalAccountingFailure, GoalOperationAccounting,
        GoalOperationAccountingFactory, GoalOperationFenced, GoalOperationGuard,
        GoalOperationRequest, GoalOperationSettlement, GoalUsageEvent, GoalUsagePrice,
        GoalUsagePricer, combine_accounting_state,
    },
    task::{
        GoalBlocker, GoalBlockerKind, GoalDispatchStart, GoalPauseReason, GoalPauseState, TaskGoal,
    },
};

use crate::agent::cost::{ModelProviderPricing, cost_usage_with_pricing};

/// Runtime pricing adapter delegating to the existing configured/live/catalog
/// resolver used by ordinary turns.
pub struct ModelProviderGoalUsagePricer {
    pricing: Arc<ModelProviderPricing>,
}

impl ModelProviderGoalUsagePricer {
    #[must_use]
    pub fn new(pricing: Arc<ModelProviderPricing>) -> Self {
        Self { pricing }
    }
}

impl GoalUsagePricer for ModelProviderGoalUsagePricer {
    fn price(&self, provider_ref: &str, model: &str, usage: &TokenUsage) -> Result<GoalUsagePrice> {
        let input = usage.input_tokens.context("usage has no input tokens")?;
        let output = usage.output_tokens.context("usage has no output tokens")?;
        ensure!(
            input.checked_add(output).is_some(),
            "usage token total overflows"
        );
        let cached = usage.cached_input_tokens.unwrap_or(0);
        let cache_write = usage.cache_creation_input_tokens.unwrap_or(0);
        ensure!(cached <= input, "cached input exceeds input tokens");
        ensure!(
            cache_write <= input.saturating_sub(cached),
            "cache-write input exceeds uncached input tokens"
        );
        let priced = cost_usage_with_pricing(
            &self.pricing,
            provider_ref,
            model,
            input,
            cached,
            cache_write,
            output,
        );
        Ok(GoalUsagePrice {
            cost_usd: priced.cost_usd,
            unpriced_tokens: priced.unpriced_tokens,
            pricing_available: priced.pricing_available,
            timestamp: priced.timestamp,
        })
    }
}

/// Constructs one accountant per admitted Goal scope over the existing task
/// registry, cost ledger, and ordinary runtime pricing resolver.
pub struct RuntimeGoalOperationAccountingFactory {
    registry: Arc<dyn GoalTaskRegistry>,
    tracker: Arc<CostTracker>,
    agent_alias: String,
    pricer: Arc<dyn GoalUsagePricer>,
}

impl RuntimeGoalOperationAccountingFactory {
    pub fn new(
        registry: Arc<dyn GoalTaskRegistry>,
        tracker: Arc<CostTracker>,
        agent_alias: impl Into<String>,
        pricer: Arc<dyn GoalUsagePricer>,
    ) -> Result<Self> {
        let agent_alias = agent_alias.into();
        ensure!(
            !agent_alias.trim().is_empty(),
            "Goal accounting agent alias must be nonblank"
        );
        Ok(Self {
            registry,
            tracker,
            agent_alias,
            pricer,
        })
    }
}

impl GoalOperationAccountingFactory for RuntimeGoalOperationAccountingFactory {
    fn for_scope(&self, scope: GoalExecutionScope) -> Arc<dyn GoalOperationAccounting> {
        Arc::new_cyclic(|owner| {
            GoalOperationAccountant::new(
                Arc::clone(&self.registry),
                Arc::clone(&self.tracker),
                self.agent_alias.clone(),
                Arc::clone(&self.pricer),
                scope,
                owner.clone(),
            )
        })
    }
}

#[derive(Default)]
struct OperationProgress {
    finished: AtomicBool,
    dispatch_entered: AtomicBool,
    abandonment: Mutex<Option<GoalAbandonmentSettlement>>,
}

struct AdmittedOperation {
    id: String,
    _permit: OwnedSemaphorePermit,
    progress: Arc<OperationProgress>,
}

impl Drop for AdmittedOperation {
    fn drop(&mut self) {
        self.progress.finished.store(true, Ordering::Release);
    }
}

struct AdmissionWitness {
    progress: Arc<OperationProgress>,
    abandoned: CancellationToken,
    owner: Weak<GoalOperationAccountant>,
}

struct AbandonmentFailureFence {
    abandoned: CancellationToken,
    armed: bool,
}

impl Drop for AbandonmentFailureFence {
    fn drop(&mut self) {
        if self.armed {
            self.abandoned.cancel();
        }
    }
}

impl Drop for AdmissionWitness {
    fn drop(&mut self) {
        if self.progress.finished.load(Ordering::Acquire) {
            return;
        }
        let Some(owner) = self.owner.upgrade() else {
            self.abandoned.cancel();
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            self.abandoned.cancel();
            return;
        }
        let progress = Arc::clone(&self.progress);
        // Construct this before spawning: even a task dropped before its first
        // poll must wake waiters and retain the durable pending marker.
        let fence = AbandonmentFailureFence {
            abandoned: self.abandoned.clone(),
            armed: true,
        };
        let recovery = async move {
            let mut fence = fence;
            let is_current = owner
                .admitted
                .lock()
                .as_ref()
                .is_some_and(|operation| Arc::ptr_eq(&operation.progress, &progress));
            if !is_current {
                return;
            }
            let result = if progress.dispatch_entered.load(Ordering::Acquire) {
                let source = progress.abandonment.lock().take();
                let settlement = source.map_or_else(
                    || GoalOperationSettlement {
                        accounting_state: GoalAccountingState::OutcomeUnknown,
                        events: Vec::new(),
                    },
                    |source| source(),
                );
                owner.settle(settlement).await
            } else {
                owner.release_unstarted().await
            };
            if result.is_ok() {
                fence.armed = false;
            }
        };
        drop(::zeroclaw_spawn::spawn!(recovery));
    }
}

/// One Goal epoch's runtime-backed provider accountant. The permit spans
/// durable pending admission through ledger persistence and fenced settlement.
struct GoalOperationAccountant {
    registry: Arc<dyn GoalTaskRegistry>,
    tracker: Arc<CostTracker>,
    agent_alias: String,
    pricer: Arc<dyn GoalUsagePricer>,
    scope: GoalExecutionScope,
    permit: Arc<Semaphore>,
    admitted: Mutex<Option<AdmittedOperation>>,
    abandoned: CancellationToken,
    owner: Weak<Self>,
}

impl GoalOperationAccountant {
    fn new(
        registry: Arc<dyn GoalTaskRegistry>,
        tracker: Arc<CostTracker>,
        agent_alias: String,
        pricer: Arc<dyn GoalUsagePricer>,
        scope: GoalExecutionScope,
        owner: Weak<Self>,
    ) -> Self {
        Self {
            registry,
            tracker,
            agent_alias,
            pricer,
            scope,
            permit: Arc::new(Semaphore::new(1)),
            admitted: Mutex::new(None),
            abandoned: CancellationToken::new(),
            owner,
        }
    }

    async fn current_running_goal(&self) -> Result<TaskGoal> {
        let task = self
            .registry
            .current_goal_for_session(self.scope.session_id())
            .await?
            .ok_or_else(|| anyhow::Error::new(GoalOperationFenced::NO_CURRENT_TASK))?;
        if task.id != self.scope.task_id() {
            return Err(anyhow::Error::new(GoalOperationFenced::STALE_TASK));
        }
        if task.status != zeroclaw_api::task::TaskStatus::Running {
            return Err(anyhow::Error::new(GoalOperationFenced::NOT_RUNNING));
        }
        if task.execution_epoch != self.scope.execution_epoch() {
            return Err(anyhow::Error::new(GoalOperationFenced::STALE_EPOCH));
        }
        let extension = self
            .registry
            .get_goal_task(&task.id)
            .await?
            .context("Goal task extension is missing")?;
        ensure!(
            extension.task_id == task.id,
            "Goal task extension identity is stale"
        );
        Ok(TaskGoal::new(task, extension))
    }

    async fn pause_budget_exhausted(&self) -> Result<()> {
        let pause = GoalPauseState {
            reason: GoalPauseReason::BudgetExhausted,
            description: Some("Goal usage has reached its effective budget".to_owned()),
            blockers: vec![GoalBlocker {
                kind: GoalBlockerKind::Budget,
                message: "Increase the Goal budget, then resume explicitly.".to_owned(),
                payload: None,
            }],
        };
        match self
            .registry
            .pause_session_goal(
                self.scope.task_id(),
                self.scope.session_id(),
                self.scope.execution_epoch(),
                pause,
            )
            .await?
        {
            GoalTransitionResult::Applied => Ok(()),
            GoalTransitionResult::Stale | GoalTransitionResult::Missing => {
                bail!("Goal budget pause lost its execution fence")
            }
        }
    }

    async fn strict_totals(&self) -> Result<(u64, f64, bool)> {
        let tracker = Arc::clone(&self.tracker);
        let task_id = self.scope.task_id().to_owned();
        tokio::task::spawn_blocking(move || {
            tracker.get_strict_usage_totals_for_task_with_pricing(&task_id)
        })
        .await
        .context("join strict Goal usage lookup")?
    }

    /// Reject unusable token records, but retain structurally valid usage as
    /// unpriced when pricing fails, returning that failure separately.
    fn ledger_usage(
        &self,
        event: &GoalUsageEvent,
    ) -> Result<(CostTokenUsage, Option<anyhow::Error>)> {
        ensure!(
            !event.provider_ref.trim().is_empty() && !event.model.trim().is_empty(),
            "Goal usage event lacks actual provider/model attribution"
        );
        let input = event
            .usage
            .input_tokens
            .context("Goal usage event has no input token count")?;
        let output = event
            .usage
            .output_tokens
            .context("Goal usage event has no output token count")?;
        ensure!(
            input.checked_add(output).is_some(),
            "Goal usage total overflows"
        );
        ensure!(input != 0 || output != 0, "Goal usage is all zero");

        let mut pricing_error = None;
        let price = if event.priceable {
            match self
                .pricer
                .price(&event.provider_ref, &event.model, &event.usage)
                .and_then(|price| {
                    ensure!(
                        price.cost_usd.is_finite() && price.cost_usd >= 0.0,
                        "Goal usage cost is invalid"
                    );
                    Ok(price)
                }) {
                Ok(price) => Some(price),
                Err(error) => {
                    pricing_error = Some(error);
                    None
                }
            }
        } else {
            None
        };
        let price = price.unwrap_or_else(|| {
            // Keep the observed token lower bound, but do not report a dollar
            // figure based on an invalid cache-token partition or failed pricing.
            GoalUsagePrice {
                cost_usd: 0.0,
                unpriced_tokens: input + output,
                pricing_available: false,
                timestamp: chrono::Utc::now(),
            }
        });
        Ok((
            CostTokenUsage {
                model: event.model.clone(),
                input_tokens: input,
                output_tokens: output,
                cached_input_tokens: event.usage.cached_input_tokens.unwrap_or(0),
                cache_creation_input_tokens: event.usage.cache_creation_input_tokens.unwrap_or(0),
                total_tokens: input + output,
                cost_usd: price.cost_usd,
                unpriced_tokens: price.unpriced_tokens,
                pricing_available: price.pricing_available,
                timestamp: price.timestamp,
            },
            pricing_error,
        ))
    }

    async fn record_events(&self, events: Vec<(GoalUsageEvent, CostTokenUsage)>) -> Result<()> {
        let tracker = Arc::clone(&self.tracker);
        let agent_alias = self.agent_alias.clone();
        let task_id = self.scope.task_id().to_owned();
        tokio::task::spawn_blocking(move || {
            let events = events
                .into_iter()
                .map(|(event, usage)| (usage, event.provider_ref))
                .collect();
            tracker.record_scoped_usage_batch_with_owned_task_and_provider_attribution(
                events,
                Some(&agent_alias),
                Some(task_id),
            )
        })
        .await
        .context("join Goal ledger settlement")?
    }
}

fn usage_warning_attributes(event: &GoalUsageEvent, error: &anyhow::Error) -> serde_json::Value {
    serde_json::json!({
        "provider_ref": event.provider_ref,
        "model": event.model,
        "error": crate::security::scrub(&format!("{error:#}")),
    })
}

fn with_accounting_causes(error: anyhow::Error, causes: &[String]) -> anyhow::Error {
    if causes.is_empty() {
        error
    } else {
        error.context(format!(
            "Goal usage settlement did not complete, and Goal accounting also failed: {}",
            causes.join("; ")
        ))
    }
}

#[async_trait]
impl GoalOperationAccounting for GoalOperationAccountant {
    async fn admit(&self, request: GoalOperationRequest) -> Result<()> {
        let acquire = Arc::clone(&self.permit).acquire_owned();
        let permit = tokio::select! {
            biased;
            () = self.abandoned.cancelled() => {
                return Err(anyhow::Error::new(GoalAccountingFailure::incomplete(
                    GoalAccountingState::OutcomeUnknown,
                    true,
                )));
            }
            result = acquire => result.context("Goal operation permit closed")?,
        };
        if self.abandoned.is_cancelled() {
            return Err(anyhow::Error::new(GoalAccountingFailure::incomplete(
                GoalAccountingState::OutcomeUnknown,
                true,
            )));
        }
        ensure!(
            self.admitted.lock().is_none(),
            "Goal operation is already admitted"
        );

        let goal = self.current_running_goal().await?;
        if goal.goal().accounting_state != GoalAccountingState::Complete {
            return Err(anyhow::Error::new(GoalAccountingFailure::incomplete(
                goal.goal().accounting_state,
                false,
            )));
        }
        if goal.goal().pending_call_id.is_some() || goal.goal().pending_call_epoch.is_some() {
            return Err(anyhow::Error::new(GoalAccountingFailure::incomplete(
                goal.goal().accounting_state,
                true,
            ))
            .context("Goal already has a pending operation"));
        }
        let (tokens, cost, pricing_complete) = self.strict_totals().await?;
        if goal
            .goal()
            .effective_token_limit
            .is_some_and(|limit| tokens >= limit)
            || goal
                .goal()
                .effective_cost_limit_usd
                .is_some_and(|limit| cost >= limit)
        {
            drop(permit);
            self.pause_budget_exhausted().await?;
            return Err(anyhow::Error::new(GoalOperationFenced::BUDGET_EXHAUSTED));
        }
        let configured_route_usage = TokenUsage {
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_input_tokens: Some(0),
            cache_creation_input_tokens: Some(0),
        };
        let configured_route_pricing = self.pricer.price(
            &request.model_provider,
            &request.model,
            &configured_route_usage,
        )?;
        if goal.goal().effective_cost_limit_usd.is_some()
            && (!pricing_complete || !configured_route_pricing.pricing_available)
        {
            bail!("Goal cost budget lacks complete pricing for the configured route");
        }

        let operation_id = Uuid::new_v4().to_string();
        match self
            .registry
            .admit_pending_operation(
                self.scope.task_id(),
                self.scope.session_id(),
                self.scope.execution_epoch(),
                &operation_id,
            )
            .await?
        {
            GoalTransitionResult::Applied => {
                let mut admitted = self.admitted.lock();
                debug_assert!(admitted.is_none());
                *admitted = Some(AdmittedOperation {
                    id: operation_id,
                    _permit: permit,
                    progress: Arc::new(OperationProgress::default()),
                });
                Ok(())
            }
            GoalTransitionResult::Stale | GoalTransitionResult::Missing => Err(anyhow::Error::new(
                GoalOperationFenced::ADMISSION_FENCE_LOST,
            )),
        }
    }

    fn admission_guard(&self) -> Result<GoalOperationGuard> {
        let progress = self
            .admitted
            .lock()
            .as_ref()
            .map(|operation| Arc::clone(&operation.progress))
            .context("Goal admission guard requested without an admission")?;
        Ok(GoalOperationGuard::new(AdmissionWitness {
            progress,
            abandoned: self.abandoned.clone(),
            owner: self.owner.clone(),
        }))
    }

    fn mark_dispatch_entered(&self) {
        if let Some(operation) = self.admitted.lock().as_ref() {
            operation
                .progress
                .dispatch_entered
                .store(true, Ordering::Release);
        }
    }

    fn set_abandonment_settlement(&self, source: GoalAbandonmentSettlement) {
        if let Some(operation) = self.admitted.lock().as_ref() {
            *operation.progress.abandonment.lock() = Some(source);
        }
    }

    async fn quiesce(&self) {
        tokio::select! {
            biased;
            () = self.abandoned.cancelled() => {}
            _ = self.permit.acquire() => {}
        }
    }

    async fn begin_dispatch(&self) -> Result<GoalDispatchStart> {
        let id = self
            .admitted
            .lock()
            .as_ref()
            .map(|operation| operation.id.clone())
            .context("Goal operation dispatch began without a matching admission")?;
        self.registry
            .begin_operation_dispatch(
                self.scope.task_id(),
                self.scope.session_id(),
                self.scope.execution_epoch(),
                &id,
            )
            .await?
            .ok_or_else(|| anyhow::Error::new(GoalOperationFenced::ADMISSION_FENCE_LOST))
    }

    async fn settle(&self, settlement: GoalOperationSettlement) -> Result<()> {
        let operation = {
            let mut admitted = self.admitted.lock();
            admitted
                .take()
                .context("Goal operation settled without a matching admission")?
        };

        let mut accounting_state = settlement.accounting_state;
        let mut events = Vec::with_capacity(settlement.events.len());
        let mut causes = Vec::new();
        for event in settlement.events {
            match self.ledger_usage(&event) {
                Ok((usage, None)) => events.push((event, usage)),
                Ok((usage, Some(error))) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(usage_warning_attributes(&event, &error)),
                        "Goal usage pricing failed; usage retained as unpriced"
                    );
                    causes.push(format!(
                        "usage pricing failed for provider `{}`, model `{}`; {} tokens are treated as unpriced: {error:#}",
                        event.provider_ref, event.model, usage.total_tokens
                    ));
                    accounting_state =
                        combine_accounting_state(accounting_state, GoalAccountingState::Invalid);
                    events.push((event, usage));
                }
                Err(error) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(usage_warning_attributes(&event, &error)),
                        "Goal usage event rejected"
                    );
                    causes.push(format!(
                        "a usage event for provider `{}`, model `{}` was rejected: {error:#}",
                        event.provider_ref, event.model
                    ));
                    accounting_state =
                        combine_accounting_state(accounting_state, GoalAccountingState::Invalid);
                }
            }
        }
        if accounting_state == GoalAccountingState::Complete && events.is_empty() {
            accounting_state =
                combine_accounting_state(accounting_state, GoalAccountingState::Missing);
        }

        // Preserve known usage before settling the operation state. If ledger
        // persistence fails, the durable pending marker remains for restart
        // recovery to classify as outcome-unknown rather than replaying.
        self.record_events(events)
            .await
            .map_err(|error| with_accounting_causes(error, &causes))?;
        match self
            .registry
            .settle_pending_operation(
                self.scope.task_id(),
                self.scope.session_id(),
                self.scope.execution_epoch(),
                &operation.id,
                accounting_state,
            )
            .await
            .map_err(|error| with_accounting_causes(error, &causes))?
        {
            GoalTransitionResult::Applied => {
                if causes.is_empty() {
                    Ok(())
                } else {
                    Err(anyhow::Error::new(GoalAccountingFailure::settled(
                        accounting_state,
                        &causes,
                    )))
                }
            }
            GoalTransitionResult::Stale | GoalTransitionResult::Missing => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "Goal operation settlement lost its execution fence"
                );
                Err(with_accounting_causes(
                    anyhow::Error::msg("Goal operation settlement lost its execution fence"),
                    &causes,
                ))
            }
        }
    }

    async fn release_unstarted(&self) -> Result<()> {
        let operation = self
            .admitted
            .lock()
            .take()
            .context("Goal operation released without a matching admission")?;
        match self
            .registry
            .settle_pending_operation(
                self.scope.task_id(),
                self.scope.session_id(),
                self.scope.execution_epoch(),
                &operation.id,
                GoalAccountingState::Complete,
            )
            .await?
        {
            GoalTransitionResult::Applied => Ok(()),
            GoalTransitionResult::Stale | GoalTransitionResult::Missing => {
                bail!("unstarted Goal operation release lost its execution fence")
            }
        }
    }

    async fn pricing_complete(&self) -> Result<bool> {
        Ok(self.strict_totals().await?.2)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::control_plane::{SqliteTaskStore, TaskKind, TaskRecord};
    use std::collections::HashMap;
    use tempfile::TempDir;
    use zeroclaw_goal::GoalTaskRecord;

    #[derive(Clone, Copy)]
    enum AdmissionFault {
        MissingExtension,
        StaleExtension,
        ReadFailure,
        Admission(GoalTransitionResult),
        StoreFailure,
        DispatchReadFailure,
        Pause(GoalTransitionResult),
        PauseStoreFailure,
        Settlement(GoalTransitionResult),
    }

    // Only the queried failure boundary is injected. All lifecycle and ledger
    // operations continue to use the real SQLite control plane.
    struct AdmissionFaultRegistry {
        store: Arc<SqliteTaskStore>,
        fault: AdmissionFault,
    }

    #[async_trait]
    impl GoalTaskRegistry for AdmissionFaultRegistry {
        async fn begin_operation_dispatch(
            &self,
            task_id: &str,
            session_id: &str,
            expected_epoch: i64,
            pending_call_id: &str,
        ) -> Result<Option<GoalDispatchStart>> {
            if matches!(self.fault, AdmissionFault::DispatchReadFailure) {
                bail!("fixture dispatch read failed");
            }
            self.store
                .begin_operation_dispatch(task_id, session_id, expected_epoch, pending_call_id)
                .await
        }

        async fn get_goal_task(&self, task_id: &str) -> Result<Option<GoalTaskRecord>> {
            match self.fault {
                AdmissionFault::MissingExtension => Ok(None),
                AdmissionFault::ReadFailure => bail!("fixture registry read failed"),
                AdmissionFault::StaleExtension => {
                    let mut goal = self.store.get_goal_task(task_id).await?.unwrap();
                    goal.task_id = "different-task".to_owned();
                    Ok(Some(goal))
                }
                _ => self.store.get_goal_task(task_id).await,
            }
        }

        async fn admit_pending_operation(
            &self,
            task_id: &str,
            session_id: &str,
            expected_epoch: i64,
            pending_call_id: &str,
        ) -> Result<GoalTransitionResult> {
            match self.fault {
                AdmissionFault::Admission(result) => Ok(result),
                AdmissionFault::StoreFailure => bail!("fixture registry admission failed"),
                _ => {
                    self.store
                        .admit_pending_operation(
                            task_id,
                            session_id,
                            expected_epoch,
                            pending_call_id,
                        )
                        .await
                }
            }
        }

        async fn latest_active_goal_for_agent(&self, agent: &str) -> Result<Option<TaskRecord>> {
            self.store.latest_active_goal_for_agent(agent).await
        }

        async fn latest_active_goal_for_context(
            &self,
            agent: &str,
            originator_route: Option<&str>,
            principal_id: Option<&str>,
        ) -> Result<Option<TaskRecord>> {
            self.store
                .latest_active_goal_for_context(agent, originator_route, principal_id)
                .await
        }

        async fn latest_active_goal_id_for_context(
            &self,
            agent: &str,
            originator_route: Option<&str>,
            principal_id: Option<&str>,
        ) -> Result<Option<String>> {
            self.store
                .latest_active_goal_id_for_context(agent, originator_route, principal_id)
                .await
        }

        async fn update_goal_limits(
            &self,
            task_id: &str,
            token_limit: Option<u64>,
            cost_limit_usd: Option<f64>,
        ) -> Result<()> {
            self.store
                .update_goal_limits(task_id, token_limit, cost_limit_usd)
                .await
        }

        async fn update_goal_pause(
            &self,
            task_id: &str,
            pause: Option<GoalPauseState>,
        ) -> Result<()> {
            self.store.update_goal_pause(task_id, pause).await
        }

        async fn set_continuation_context(
            &self,
            task_id: &str,
            context: Option<zeroclaw_goal::task::TaskContinuationContext>,
        ) -> Result<()> {
            self.store.set_continuation_context(task_id, context).await
        }

        async fn get_continuation_context(
            &self,
            task_id: &str,
        ) -> Result<Option<zeroclaw_goal::task::TaskContinuationContext>> {
            self.store.get_continuation_context(task_id).await
        }

        async fn current_goal_for_session(&self, session_id: &str) -> Result<Option<TaskRecord>> {
            self.store.current_goal_for_session(session_id).await
        }

        async fn terminal_reason_for_session_goal(
            &self,
            task_id: &str,
            session_id: &str,
        ) -> Result<Option<String>> {
            self.store
                .terminal_reason_for_session_goal(task_id, session_id)
                .await
        }

        async fn create_or_replace_session_goal(
            &self,
            task: TaskRecord,
            goal: GoalTaskRecord,
        ) -> Result<GoalTransitionResult> {
            self.store.create_or_replace_session_goal(task, goal).await
        }

        async fn pause_session_goal(
            &self,
            task_id: &str,
            session_id: &str,
            expected_epoch: i64,
            pause: GoalPauseState,
        ) -> Result<GoalTransitionResult> {
            match self.fault {
                AdmissionFault::Pause(result) => Ok(result),
                AdmissionFault::PauseStoreFailure => bail!("fixture registry pause failed"),
                _ => {
                    self.store
                        .pause_session_goal(task_id, session_id, expected_epoch, pause)
                        .await
                }
            }
        }

        async fn resume_session_goal(
            &self,
            task_id: &str,
            session_id: &str,
            expected_epoch: i64,
            owner_pid: u32,
            owner_boot_id: &str,
        ) -> Result<GoalTransitionResult> {
            self.store
                .resume_session_goal(
                    task_id,
                    session_id,
                    expected_epoch,
                    owner_pid,
                    owner_boot_id,
                )
                .await
        }

        async fn finish_session_goal(
            &self,
            task_id: &str,
            session_id: &str,
            expected_epoch: i64,
            status: zeroclaw_api::task::TaskStatus,
            error: Option<String>,
        ) -> Result<GoalTransitionResult> {
            self.store
                .finish_session_goal(task_id, session_id, expected_epoch, status, error)
                .await
        }

        async fn settle_pending_operation(
            &self,
            task_id: &str,
            session_id: &str,
            admitted_epoch: i64,
            pending_call_id: &str,
            accounting_state: GoalAccountingState,
        ) -> Result<GoalTransitionResult> {
            match self.fault {
                AdmissionFault::Settlement(result) => Ok(result),
                _ => {
                    self.store
                        .settle_pending_operation(
                            task_id,
                            session_id,
                            admitted_epoch,
                            pending_call_id,
                            accounting_state,
                        )
                        .await
                }
            }
        }

        async fn update_session_goal_limits(
            &self,
            task_id: &str,
            session_id: &str,
            expected_epoch: i64,
            token_limit: Option<u64>,
            cost_limit_usd: Option<f64>,
        ) -> Result<GoalTransitionResult> {
            self.store
                .update_session_goal_limits(
                    task_id,
                    session_id,
                    expected_epoch,
                    token_limit,
                    cost_limit_usd,
                )
                .await
        }

        async fn delete_session_goal(
            &self,
            task_id: &str,
            session_id: &str,
            expected_epoch: i64,
        ) -> Result<GoalTransitionResult> {
            self.store
                .delete_session_goal(task_id, session_id, expected_epoch)
                .await
        }
    }

    fn faulted_factory(
        factory: &RuntimeGoalOperationAccountingFactory,
        store: Arc<SqliteTaskStore>,
        fault: AdmissionFault,
    ) -> RuntimeGoalOperationAccountingFactory {
        RuntimeGoalOperationAccountingFactory::new(
            Arc::new(AdmissionFaultRegistry { store, fault }),
            Arc::clone(&factory.tracker),
            &factory.agent_alias,
            Arc::clone(&factory.pricer),
        )
        .unwrap()
    }

    #[derive(Clone, Copy)]
    pub(crate) enum DispatchProbeMode {
        ObserveOnly,
        FenceBeforeStart,
        QueueFenceAfterStart,
    }

    pub(crate) struct DispatchProbe {
        inner: Arc<dyn GoalOperationAccounting>,
        store: Arc<SqliteTaskStore>,
        scope: GoalExecutionScope,
        mode: DispatchProbeMode,
        pub admissions: std::sync::atomic::AtomicUsize,
        pub releases: std::sync::atomic::AtomicUsize,
        pub settlements: std::sync::atomic::AtomicUsize,
        pub admitted: Arc<tokio::sync::Notify>,
        fence: std::sync::Mutex<Option<tokio::task::JoinHandle<Result<GoalTransitionResult>>>>,
    }

    impl DispatchProbe {
        pub(crate) fn new(
            inner: Arc<dyn GoalOperationAccounting>,
            store: Arc<SqliteTaskStore>,
            scope: GoalExecutionScope,
            mode: DispatchProbeMode,
        ) -> Self {
            Self {
                inner,
                store,
                scope,
                mode,
                admissions: std::sync::atomic::AtomicUsize::new(0),
                releases: std::sync::atomic::AtomicUsize::new(0),
                settlements: std::sync::atomic::AtomicUsize::new(0),
                admitted: Arc::new(tokio::sync::Notify::new()),
                fence: std::sync::Mutex::new(None),
            }
        }

        pub(crate) async fn wait_for_fence(&self) -> Result<GoalTransitionResult> {
            let fence = self.fence.lock().unwrap().take().expect("queued fence");
            tokio::time::timeout(std::time::Duration::from_secs(2), fence)
                .await
                .context("waiting for fixture fence")??
        }
    }

    #[async_trait]
    impl GoalOperationAccounting for DispatchProbe {
        async fn admit(&self, request: GoalOperationRequest) -> Result<()> {
            self.admissions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.admit(request).await?;
            self.admitted.notify_one();
            Ok(())
        }

        fn admission_guard(&self) -> Result<GoalOperationGuard> {
            self.inner.admission_guard()
        }

        async fn begin_dispatch(&self) -> Result<GoalDispatchStart> {
            if matches!(self.mode, DispatchProbeMode::FenceBeforeStart) {
                assert_eq!(
                    self.store
                        .pause_session_goal(
                            self.scope.task_id(),
                            self.scope.session_id(),
                            self.scope.execution_epoch(),
                            operator_pause(GoalPauseReason::OperatorPaused),
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
            }
            let start = self.inner.begin_dispatch().await?;
            if matches!(self.mode, DispatchProbeMode::QueueFenceAfterStart) {
                let store = Arc::clone(&self.store);
                let scope = self.scope.clone();
                let fence = ::zeroclaw_spawn::spawn!(async move {
                    store
                        .pause_session_goal(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            operator_pause(GoalPauseReason::OperatorPaused),
                        )
                        .await
                });
                for _ in 0..10 {
                    tokio::task::yield_now().await;
                }
                assert!(
                    !fence.is_finished(),
                    "the fence must wait for dispatch entry"
                );
                *self.fence.lock().unwrap() = Some(fence);
            }
            Ok(start)
        }

        async fn settle(&self, settlement: GoalOperationSettlement) -> Result<()> {
            self.settlements
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.settle(settlement).await
        }

        async fn release_unstarted(&self) -> Result<()> {
            self.releases
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.release_unstarted().await
        }

        async fn pricing_complete(&self) -> Result<bool> {
            self.inner.pricing_complete().await
        }
    }

    #[tokio::test]
    async fn fenced_admission_after_durable_admission_rejection_preserves_its_error() {
        for result in [GoalTransitionResult::Stale, GoalTransitionResult::Missing] {
            let (store, factory, _tracker, scope, _directory) = fixture().await;
            let faulted = faulted_factory(&factory, store, AdmissionFault::Admission(result));
            assert_fenced_admission(
                &faulted,
                scope,
                "Goal operation admission lost its execution fence",
            )
            .await;
        }
    }

    #[tokio::test]
    async fn non_fence_registry_errors_do_not_carry_the_operation_marker() {
        for (fault, expected) in [
            (
                AdmissionFault::MissingExtension,
                "Goal task extension is missing",
            ),
            (
                AdmissionFault::StaleExtension,
                "Goal task extension identity is stale",
            ),
            (AdmissionFault::ReadFailure, "fixture registry read failed"),
            (
                AdmissionFault::StoreFailure,
                "fixture registry admission failed",
            ),
        ] {
            let (store, factory, _tracker, scope, _directory) = fixture().await;
            let faulted = faulted_factory(&factory, store, fault);
            let error = faulted
                .for_scope(scope)
                .admit(GoalOperationRequest::new("fallback", "model"))
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), expected);
            assert!(
                error
                    .downcast_ref::<zeroclaw_goal::accounting::GoalOperationFenced>()
                    .is_none()
            );
        }
    }

    pub(crate) async fn fixture() -> (
        Arc<SqliteTaskStore>,
        RuntimeGoalOperationAccountingFactory,
        Arc<CostTracker>,
        GoalExecutionScope,
        TempDir,
    ) {
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        fixture_with_store(store, TempDir::new().unwrap()).await
    }

    pub(crate) async fn fixture_with_store(
        store: Arc<SqliteTaskStore>,
        directory: TempDir,
    ) -> (
        Arc<SqliteTaskStore>,
        RuntimeGoalOperationAccountingFactory,
        Arc<CostTracker>,
        GoalExecutionScope,
        TempDir,
    ) {
        let scope =
            GoalExecutionScope::new("goal-accounting", "matrix_goal-accounting", 1).unwrap();
        let task = TaskRecord {
            id: scope.task_id().to_owned(),
            kind: TaskKind::Goal,
            agent: "main".to_owned(),
            status: zeroclaw_api::task::TaskStatus::Running,
            owner_pid: 1,
            owner_boot_id: "test-boot".to_owned(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: Some("matrix:test-room".to_owned()),
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: Some("@test:example.org".to_owned()),
            session_id: Some(scope.session_id().to_owned()),
            execution_epoch: 1,
            started_at: "2026-09-05T00:00:00Z".to_owned(),
            finished_at: None,
        };
        let goal = GoalTaskRecord {
            task_id: task.id.clone(),
            objective: "finish the work".to_owned(),
            ..GoalTaskRecord::default()
        };
        assert_eq!(
            store
                .create_or_replace_session_goal(task, goal)
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );

        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig {
                    enabled: false,
                    ..Default::default()
                },
                directory.path(),
            )
            .unwrap(),
        );
        let mut rates = HashMap::new();
        rates.insert("model.input".to_owned(), 1.0);
        rates.insert("model.output".to_owned(), 2.0);
        rates.insert("model.cached_input".to_owned(), 0.5);
        rates.insert("model.cache_write".to_owned(), 1.5);
        let pricer = Arc::new(ModelProviderGoalUsagePricer::new(Arc::new(HashMap::from(
            [("fallback".to_owned(), rates)],
        ))));
        let factory = RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            Arc::clone(&tracker),
            "main",
            pricer,
        )
        .unwrap();
        (store, factory, tracker, scope, directory)
    }

    fn usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cached_input_tokens: Some(0),
            cache_creation_input_tokens: Some(0),
        }
    }

    async fn assert_fenced_admission(
        factory: &RuntimeGoalOperationAccountingFactory,
        scope: GoalExecutionScope,
        expected: &str,
    ) {
        let error = factory
            .for_scope(scope)
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .expect_err("a non-running execution scope must refuse admission");
        assert_eq!(error.to_string(), expected);
        assert!(
            error
                .downcast_ref::<zeroclaw_goal::accounting::GoalOperationFenced>()
                .is_some(),
            "the committed lifecycle transition must remain distinguishable from an operation failure"
        );
    }

    fn assert_untyped_refusal(error: &anyhow::Error, expected: &str) {
        assert_eq!(error.to_string(), expected);
        assert!(
            error
                .downcast_ref::<zeroclaw_goal::accounting::GoalOperationFenced>()
                .is_none()
        );
    }

    struct FailingPricer;

    impl GoalUsagePricer for FailingPricer {
        fn price(
            &self,
            _provider_ref: &str,
            _model: &str,
            _usage: &TokenUsage,
        ) -> Result<GoalUsagePrice> {
            bail!("fixture pricing resolver failed")
        }
    }

    #[tokio::test]
    async fn pricing_resolver_errors_are_not_execution_fence_refusals() {
        let (_store, mut factory, _tracker, scope, _directory) = fixture().await;
        factory.pricer = Arc::new(FailingPricer);
        let error = factory
            .for_scope(scope)
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap_err();
        assert_untyped_refusal(&error, "fixture pricing resolver failed");
    }

    #[tokio::test]
    async fn ledger_read_errors_are_not_execution_fence_refusals() {
        let (store, factory, _tracker, scope, directory) = fixture().await;
        let ledger = directory.path().join("state/costs.jsonl");
        // A directory at the ledger path fails the existing strict reader;
        // no ledger format or production error handling is replaced.
        std::fs::create_dir_all(&ledger).unwrap();
        let error = factory
            .for_scope(scope.clone())
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cost storage"));
        assert!(
            error
                .downcast_ref::<zeroclaw_goal::accounting::GoalOperationFenced>()
                .is_none()
        );
        assert!(
            store
                .get_goal_task(scope.task_id())
                .await
                .unwrap()
                .unwrap()
                .pending_call_id
                .is_none()
        );
    }

    fn concrete_accountant(
        factory: &RuntimeGoalOperationAccountingFactory,
        scope: GoalExecutionScope,
    ) -> GoalOperationAccountant {
        GoalOperationAccountant::new(
            Arc::clone(&factory.registry),
            Arc::clone(&factory.tracker),
            factory.agent_alias.clone(),
            Arc::clone(&factory.pricer),
            scope,
            Weak::new(),
        )
    }

    #[tokio::test]
    async fn permit_closure_is_not_an_execution_fence_refusal() {
        let (_store, factory, _tracker, scope, _directory) = fixture().await;
        let accountant = concrete_accountant(&factory, scope);
        accountant.permit.close();
        let error = accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap_err();
        assert_untyped_refusal(&error, "Goal operation permit closed");
    }

    #[tokio::test]
    async fn an_existing_admission_is_not_an_execution_fence_refusal() {
        let (_store, factory, _tracker, scope, _directory) = fixture().await;
        let accountant = concrete_accountant(&factory, scope);
        // Exercise the defensive invariant independently of semaphore waiting:
        // a real admitted operation holds the accountant's only permit.
        let fixture_permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        *accountant.admitted.lock() = Some(AdmittedOperation {
            id: "fixture-existing-operation".to_owned(),
            _permit: fixture_permit,
            progress: Arc::new(OperationProgress::default()),
        });
        let error = accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap_err();
        assert_untyped_refusal(&error, "Goal operation is already admitted");
    }

    #[tokio::test]
    async fn abandoned_child_operation_refuses_the_next_admission() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        let (started, observed) = tokio::sync::oneshot::channel();
        let mut child = Box::pin(zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING.scope(
            Some(Arc::clone(&accountant)),
            async move {
                let _admission =
                    crate::agent::cost::admit_goal_operation_if_scoped("fallback", "model")
                        .await
                        .unwrap();
                let start = crate::agent::cost::begin_goal_dispatch_if_scoped()
                    .await
                    .unwrap();
                let mut dispatch = Box::pin(crate::agent::cost::release_after_first_poll(
                    start,
                    std::future::pending::<()>(),
                ));
                std::future::poll_fn(|context| {
                    assert!(std::future::Future::poll(dispatch.as_mut(), context).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                started.send(()).unwrap();
                std::future::pending::<()>().await;
            },
        ));
        tokio::select! {
            result = &mut child => panic!("child unexpectedly completed: {result:?}"),
            result = observed => result.unwrap(),
        }
        let mut successor =
            Box::pin(accountant.admit(GoalOperationRequest::new("fallback", "model")));
        tokio::select! {
            biased;
            result = &mut successor => panic!("live operation did not serialize: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        // An agentic delegate can drop its child loop on timeout after dispatch.
        drop(child);

        let error = tokio::time::timeout(std::time::Duration::from_secs(1), &mut successor)
            .await
            .expect("an abandoned operation must not block admission forever")
            .expect_err("uncertain accounting must refuse the next operation");
        assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert_eq!(goal.accounting_state, GoalAccountingState::OutcomeUnknown);
    }

    #[tokio::test]
    async fn live_sibling_waits_for_settlement_and_old_guard_does_not_poison_it() {
        let (store, factory, tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        let child_accountant = Arc::clone(&accountant);
        let (started, observed) = tokio::sync::oneshot::channel();
        let (finish, wait_for_finish) = tokio::sync::oneshot::channel();
        let mut child = Box::pin(zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING.scope(
            Some(Arc::clone(&accountant)),
            async move {
                let guard = crate::agent::cost::admit_goal_operation_if_scoped("fallback", "model")
                    .await
                    .unwrap();
                started.send(()).unwrap();
                wait_for_finish.await.unwrap();
                child_accountant
                    .settle(GoalOperationSettlement {
                        accounting_state: GoalAccountingState::Complete,
                        events: vec![usage_event("fallback", 10, 2)],
                    })
                    .await
                    .unwrap();
                guard
            },
        ));
        tokio::select! {
            _ = &mut child => panic!("child finished before its release"),
            result = observed => result.unwrap(),
        }
        let mut sibling =
            Box::pin(accountant.admit(GoalOperationRequest::new("fallback", "model")));
        tokio::select! {
            biased;
            result = &mut sibling => panic!("live sibling was not serialized: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        finish.send(()).unwrap();
        let (old_guard, sibling_result) = tokio::join!(child, sibling);
        sibling_result.expect("settled work must allow the waiting sibling");
        // A caller may retain the old witness after settlement. It must not
        // mark the newly admitted sibling as abandoned when it is dropped.
        drop(old_guard);
        accountant.release_unstarted().await.unwrap();
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .expect("the old witness must not close later admission");
        accountant.release_unstarted().await.unwrap();
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap()
                .0,
            12,
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
    }

    async fn tracked_admission(
        accountant: &Arc<dyn GoalOperationAccounting>,
    ) -> GoalOperationGuard {
        zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
            .scope(Some(Arc::clone(accountant)), async {
                crate::agent::cost::admit_goal_operation_if_scoped("fallback", "model")
                    .await
                    .unwrap()
                    .expect("Goal scope supplies a witness")
            })
            .await
    }

    #[tokio::test]
    async fn abandoned_unstarted_admission_is_released() {
        let (store, factory, _tracker, scope, directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        let guard = tracked_admission(&accountant).await;
        drop(guard);

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            accountant.admit(GoalOperationRequest::new("fallback", "model")),
        )
        .await
        .expect("unstarted abandonment must release the permit")
        .expect("unstarted abandonment must not close admission");
        accountant.release_unstarted().await.unwrap();
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
        assert!(!directory.path().join("state/costs.jsonl").exists());
    }

    #[tokio::test]
    async fn abandoned_unstarted_dispatch_after_pause_is_resumable() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        let guard = tracked_admission(&accountant).await;
        // Hold the session dispatch gate, queue pause first, then queue the
        // child's dispatch check behind it. Neither has polled a provider.
        let held = accountant.begin_dispatch().await.unwrap();
        let pause = store.pause_session_goal(
            scope.task_id(),
            scope.session_id(),
            scope.execution_epoch(),
            operator_pause(GoalPauseReason::OperatorPaused),
        );
        tokio::pin!(pause);
        tokio::select! {
            biased;
            result = &mut pause => panic!("pause unexpectedly passed the gate: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        let mut child_check = Box::pin(accountant.begin_dispatch());
        tokio::select! {
            biased;
            _ = &mut child_check => panic!("child unexpectedly passed the gate"),
            () = tokio::task::yield_now() => {}
        }
        drop(held);
        assert_eq!(pause.await.unwrap(), GoalTransitionResult::Applied);
        drop(child_check);
        drop(guard);
        accountant.quiesce().await;

        let task = store
            .current_goal_for_session(scope.session_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, zeroclaw_api::task::TaskStatus::Paused);
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
        assert_eq!(
            store
                .resume_session_goal(
                    scope.task_id(),
                    scope.session_id(),
                    task.execution_epoch,
                    2,
                    "resumed-boot",
                )
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
    }

    #[tokio::test]
    async fn dropped_waiter_does_not_abandon_the_live_operation() {
        let (_store, factory, tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        let guard = tracked_admission(&accountant).await;
        let mut waiter = Box::pin(accountant.admit(GoalOperationRequest::new("fallback", "model")));
        tokio::select! {
            biased;
            result = &mut waiter => panic!("waiter unexpectedly entered: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        drop(waiter);
        accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![usage_event("fallback", 10, 2)],
            })
            .await
            .unwrap();
        drop(guard);
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .expect("dropping a waiter must not fence its live sibling");
        accountant.release_unstarted().await.unwrap();
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap()
                .0,
            12,
        );
    }

    #[tokio::test]
    async fn abandoned_returned_report_settles_once_and_allows_successor() {
        let (store, factory, tracker, scope, directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        let guard = tracked_admission(&accountant).await;
        accountant.mark_dispatch_entered();
        accountant.set_abandonment_settlement(Box::new(|| GoalOperationSettlement {
            accounting_state: GoalAccountingState::Complete,
            events: vec![usage_event("fallback", 10, 2)],
        }));
        drop(guard);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            accountant.admit(GoalOperationRequest::new("fallback", "model")),
        )
        .await
        .expect("recovery must release the permit")
        .expect("a complete returned report permits a successor");
        accountant.release_unstarted().await.unwrap();
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap()
                .0,
            12,
        );
        let ledger = std::fs::read_to_string(directory.path().join("state/costs.jsonl")).unwrap();
        assert_eq!(ledger.lines().count(), 1);
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
    }

    #[tokio::test]
    async fn abandoned_settlement_storage_failure_keeps_pending_and_wakes_waiter() {
        let (store, factory, _tracker, scope, directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        let guard = tracked_admission(&accountant).await;
        accountant.mark_dispatch_entered();
        accountant.set_abandonment_settlement(Box::new(|| GoalOperationSettlement {
            accounting_state: GoalAccountingState::OutcomeUnknown,
            events: vec![usage_event("fallback", 10, 2)],
        }));
        std::fs::create_dir_all(directory.path().join("state/costs.jsonl")).unwrap();
        drop(guard);

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            accountant.admit(GoalOperationRequest::new("fallback", "model")),
        )
        .await
        .expect("failed recovery must wake the waiter")
        .expect_err("failed persistence must fence the next operation");
        assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert!(goal.pending_call_id.is_some());
        assert_eq!(goal.pending_call_epoch, Some(scope.execution_epoch()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_goal_scope_keeps_accountant_alive_for_recovery() {
        let (store, factory, tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        let weak = Arc::downgrade(&accountant);
        let (started, observed) = tokio::sync::oneshot::channel();
        let (recovering, recovery_observed) = tokio::sync::oneshot::channel();
        let mut outer = Box::pin(zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING.scope(
            Some(accountant),
            async move {
                let _guard =
                    crate::agent::cost::admit_goal_operation_if_scoped("fallback", "model")
                        .await
                        .unwrap();
                zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING.with(|accounting| {
                    let accounting = accounting.as_ref().unwrap();
                    accounting.mark_dispatch_entered();
                    accounting.set_abandonment_settlement(Box::new(move || {
                        let owner = weak.upgrade().expect("recovery retains the owner");
                        assert!(recovering.send(owner).is_ok());
                        GoalOperationSettlement {
                            accounting_state: GoalAccountingState::OutcomeUnknown,
                            events: vec![usage_event("fallback", 10, 2)],
                        }
                    }));
                });
                started.send(()).unwrap();
                std::future::pending::<()>().await;
            },
        ));
        tokio::select! {
            _ = &mut outer => panic!("Goal scope unexpectedly completed"),
            result = observed => result.unwrap(),
        }
        drop(outer);
        let owner = tokio::time::timeout(std::time::Duration::from_secs(1), recovery_observed)
            .await
            .expect("dropped Goal scope must start recovery")
            .unwrap();
        owner.quiesce().await;
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap()
                .0,
            12,
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::OutcomeUnknown);
        assert!(goal.pending_call_id.is_none());
    }

    #[test]
    fn abandonment_without_a_current_runtime_fails_closed() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (store, factory, _tracker, scope, _directory) = runtime.block_on(fixture());
        let accountant = factory.for_scope(scope.clone());
        let guard = runtime.block_on(tracked_admission(&accountant));
        // The runtime exists but is not entered on this thread.
        drop(guard);
        let error = runtime
            .block_on(accountant.admit(GoalOperationRequest::new("fallback", "model")))
            .unwrap_err();
        assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
        let goal = runtime
            .block_on(store.get_goal_task(scope.task_id()))
            .unwrap()
            .unwrap();
        assert!(goal.pending_call_id.is_some());
    }

    #[tokio::test]
    async fn an_exhausted_budget_is_a_committed_execution_fence_refusal() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        assert_eq!(
            store
                .update_session_goal_limits(scope.task_id(), scope.session_id(), 1, Some(1), None)
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![GoalUsageEvent {
                    provider_ref: "fallback".to_owned(),
                    model: "model".to_owned(),
                    usage: usage(10, 2),
                    priceable: true,
                }],
            })
            .await
            .unwrap();
        let error = accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Goal budget is exhausted");
        assert_eq!(
            error.to_string(),
            GoalOperationFenced::BUDGET_EXHAUSTED.to_string()
        );
        assert!(
            error.downcast_ref::<GoalOperationFenced>().is_some(),
            "a successfully committed budget pause must retain its fence identity"
        );
        let task = store
            .current_goal_for_session(scope.session_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.id, scope.task_id());
        assert_eq!(task.status, zeroclaw_api::task::TaskStatus::Paused);
        assert_eq!(task.execution_epoch, scope.execution_epoch() + 1);
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert_eq!(
            store
                .get_goal_task(scope.task_id())
                .await
                .unwrap()
                .unwrap()
                .pause_reason,
            Some(GoalPauseReason::BudgetExhausted)
        );
    }

    #[tokio::test]
    async fn a_non_exhausted_budget_admits_work_without_pausing() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        assert_eq!(
            store
                .update_session_goal_limits(scope.task_id(), scope.session_id(), 1, Some(100), None)
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        factory
            .for_scope(scope.clone())
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        let task = store
            .current_goal_for_session(scope.session_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, zeroclaw_api::task::TaskStatus::Running);
        assert_eq!(task.execution_epoch, scope.execution_epoch());
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert!(goal.pause_reason.is_none());
        assert!(goal.pending_call_id.is_some());
        assert_eq!(goal.pending_call_epoch, Some(scope.execution_epoch()));
    }

    #[tokio::test]
    async fn unstarted_operation_release_clears_pending_without_usage() {
        let (store, factory, _tracker, scope, directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        assert!(
            store
                .get_goal_task(scope.task_id())
                .await
                .unwrap()
                .unwrap()
                .pending_call_id
                .is_some()
        );
        accountant.release_unstarted().await.unwrap();
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert!(!directory.path().join("state/costs.jsonl").exists());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .expect("a known-unstarted operation does not close admission");
        accountant.release_unstarted().await.unwrap();
    }

    fn operator_pause(reason: GoalPauseReason) -> GoalPauseState {
        GoalPauseState {
            reason,
            description: None,
            blockers: Vec::new(),
        }
    }

    #[tokio::test]
    async fn dispatch_fence_committed_first_refuses_and_releases_unstarted_work() {
        use zeroclaw_api::task::TaskStatus;

        for reason in [
            Some(GoalPauseReason::OperatorPaused),
            Some(GoalPauseReason::DaemonRestart),
            None,
        ] {
            let (store, factory, _tracker, scope, _directory) = fixture().await;
            let accountant = factory.for_scope(scope.clone());
            accountant
                .admit(GoalOperationRequest::new("fallback", "model"))
                .await
                .unwrap();
            let result = match reason {
                Some(reason) => {
                    store
                        .pause_session_goal(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            operator_pause(reason),
                        )
                        .await
                }
                None => {
                    store
                        .finish_session_goal(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            TaskStatus::Cancelled,
                            None,
                        )
                        .await
                }
            }
            .unwrap();
            assert_eq!(result, GoalTransitionResult::Applied);
            let error = accountant.begin_dispatch().await.unwrap_err();
            assert!(error.downcast_ref::<GoalOperationFenced>().is_some());
            assert_eq!(
                error.to_string(),
                GoalOperationFenced::ADMISSION_FENCE_LOST.to_string()
            );
            accountant.release_unstarted().await.unwrap();
            let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
            assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
            assert!(goal.pending_call_id.is_none());
            assert!(goal.pending_call_epoch.is_none());
            let task = store
                .current_goal_for_session(scope.session_id())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                task.status,
                if reason.is_some() {
                    TaskStatus::Paused
                } else {
                    TaskStatus::Cancelled
                }
            );
            if let Some(reason) = reason {
                assert_eq!(goal.pause_reason, Some(reason));
                assert_eq!(
                    store
                        .resume_session_goal(
                            scope.task_id(),
                            scope.session_id(),
                            task.execution_epoch,
                            2,
                            "resumed-boot",
                        )
                        .await
                        .unwrap(),
                    GoalTransitionResult::Applied
                );
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_guard_orders_pause_and_allows_started_attempts_to_settle() {
        use zeroclaw_api::task::TaskStatus;

        for reason in [
            GoalPauseReason::OperatorPaused,
            GoalPauseReason::DaemonRestart,
        ] {
            let (store, factory, tracker, scope, _directory) = fixture().await;
            let accountant = factory.for_scope(scope.clone());
            accountant
                .admit(GoalOperationRequest::new("fallback", "model"))
                .await
                .unwrap();
            let start = accountant.begin_dispatch().await.unwrap();
            let fence_store = Arc::clone(&store);
            let fence_scope = scope.clone();
            let fencer = ::zeroclaw_spawn::spawn!(async move {
                fence_store
                    .pause_session_goal(
                        fence_scope.task_id(),
                        fence_scope.session_id(),
                        fence_scope.execution_epoch(),
                        operator_pause(reason),
                    )
                    .await
            });
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            assert!(!fencer.is_finished());
            assert_eq!(
                store
                    .current_goal_for_session(scope.session_id())
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                TaskStatus::Running
            );
            drop(start);
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(2), fencer)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                GoalTransitionResult::Applied
            );
            assert!(
                store
                    .get_goal_task(scope.task_id())
                    .await
                    .unwrap()
                    .unwrap()
                    .pending_call_id
                    .is_some()
            );
            accountant
                .settle(GoalOperationSettlement {
                    accounting_state: GoalAccountingState::Complete,
                    events: vec![
                        usage_event("fallback", 10, 2),
                        usage_event("fallback", 20, 3),
                    ],
                })
                .await
                .unwrap();
            let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
            assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
            assert!(goal.pending_call_id.is_none());
            assert_eq!(goal.pause_reason, Some(reason));
            assert_eq!(
                tracker
                    .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                    .unwrap()
                    .0,
                35
            );
            assert!(
                factory
                    .for_scope(scope.clone())
                    .admit(GoalOperationRequest::new("fallback", "model"))
                    .await
                    .is_err()
            );
            let task = store
                .current_goal_for_session(scope.session_id())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(task.status, TaskStatus::Paused);
            assert_eq!(
                store
                    .resume_session_goal(
                        scope.task_id(),
                        scope.session_id(),
                        task.execution_epoch,
                        2,
                        "resumed-boot"
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
        }
    }

    #[tokio::test]
    async fn dispatch_guard_checks_exact_goal_and_pending_marker() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        let marker = goal.pending_call_id.unwrap();
        for (task, session, epoch, pending) in [
            (
                "missing",
                scope.session_id(),
                scope.execution_epoch(),
                marker.as_str(),
            ),
            (
                scope.task_id(),
                "another-session",
                scope.execution_epoch(),
                marker.as_str(),
            ),
            (
                scope.task_id(),
                scope.session_id(),
                scope.execution_epoch() + 1,
                marker.as_str(),
            ),
            (
                scope.task_id(),
                scope.session_id(),
                scope.execution_epoch(),
                "wrong-marker",
            ),
        ] {
            assert!(
                store
                    .begin_operation_dispatch(task, session, epoch, pending)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                store.pause_session_goal(
                    scope.task_id(),
                    scope.session_id(),
                    scope.execution_epoch(),
                    operator_pause(GoalPauseReason::OperatorPaused),
                )
            )
            .await
            .unwrap()
            .unwrap(),
            GoalTransitionResult::Applied
        );
        assert!(
            store
                .begin_operation_dispatch(
                    scope.task_id(),
                    scope.session_id(),
                    scope.execution_epoch(),
                    &marker
                )
                .await
                .unwrap()
                .is_none()
        );
        accountant.release_unstarted().await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_guard_rejects_wrong_pending_epoch_and_terminal_tasks() {
        use zeroclaw_api::task::TaskStatus;

        for status in [TaskStatus::Cancelled, TaskStatus::Completed] {
            let directory = TempDir::new().unwrap();
            let store = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
            let (store, factory, _tracker, scope, directory) =
                fixture_with_store(store, directory).await;
            let accountant = factory.for_scope(scope.clone());
            accountant
                .admit(GoalOperationRequest::new("fallback", "model"))
                .await
                .unwrap();
            let marker = store
                .get_goal_task(scope.task_id())
                .await
                .unwrap()
                .unwrap()
                .pending_call_id
                .unwrap();
            let connection =
                rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
            connection
                .execute(
                    "UPDATE goal_tasks SET pending_call_epoch = 2 WHERE task_id = ?1",
                    [scope.task_id()],
                )
                .unwrap();
            assert!(
                store
                    .begin_operation_dispatch(
                        scope.task_id(),
                        scope.session_id(),
                        scope.execution_epoch(),
                        &marker
                    )
                    .await
                    .unwrap()
                    .is_none()
            );
            connection
                .execute(
                    "UPDATE goal_tasks SET pending_call_epoch = 1 WHERE task_id = ?1",
                    [scope.task_id()],
                )
                .unwrap();
            drop(connection);
            assert_eq!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    store.finish_session_goal(
                        scope.task_id(),
                        scope.session_id(),
                        scope.execution_epoch(),
                        status,
                        None,
                    )
                )
                .await
                .unwrap()
                .unwrap(),
                GoalTransitionResult::Applied
            );
            assert!(
                store
                    .begin_operation_dispatch(
                        scope.task_id(),
                        scope.session_id(),
                        scope.execution_epoch(),
                        &marker
                    )
                    .await
                    .unwrap()
                    .is_none()
            );
            accountant.release_unstarted().await.unwrap();
        }
    }

    #[tokio::test]
    async fn dispatch_read_failure_keeps_the_original_error_and_releases_admission() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        let faulted = faulted_factory(
            &factory,
            Arc::clone(&store),
            AdmissionFault::DispatchReadFailure,
        );
        let accountant = faulted.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        assert!(
            accountant
                .begin_dispatch()
                .await
                .unwrap_err()
                .to_string()
                .contains("fixture dispatch read failed")
        );
        accountant.release_unstarted().await.unwrap();
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert!(goal.pending_call_id.is_none());
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_guard_is_local_to_one_store_instance() {
        use zeroclaw_api::task::TaskStatus;

        let directory = TempDir::new().unwrap();
        let first = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
        let second = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
        let (store, factory, _tracker, scope, _directory) =
            fixture_with_store(first, directory).await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        let start = accountant.begin_dispatch().await.unwrap();
        let fence_scope = scope.clone();
        let fencer = ::zeroclaw_spawn::spawn!(async move {
            second
                .pause_session_goal(
                    fence_scope.task_id(),
                    fence_scope.session_id(),
                    fence_scope.execution_epoch(),
                    operator_pause(GoalPauseReason::OperatorPaused),
                )
                .await
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        drop(start);
        accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![usage_event("fallback", 1, 1)],
            })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), fencer)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            GoalTransitionResult::Applied
        );
        let task = store
            .current_goal_for_session(scope.session_id())
            .await
            .unwrap()
            .unwrap();
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::OperatorPaused));
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
        assert!(
            factory
                .for_scope(scope)
                .admit(GoalOperationRequest::new("fallback", "model"))
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_guard_does_not_soft_fence_a_failed_pause() {
        use zeroclaw_api::task::TaskStatus;

        let directory = TempDir::new().unwrap();
        let store = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
        let (store, factory, _tracker, scope, directory) =
            fixture_with_store(store, directory).await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        let connection =
            rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_fixture_pause BEFORE UPDATE OF status ON tasks
             WHEN NEW.status = 'paused'
             BEGIN SELECT RAISE(ABORT, 'fixture pause write failed'); END;",
            )
            .unwrap();
        drop(connection);
        let start = accountant.begin_dispatch().await.unwrap();
        let fence_store = Arc::clone(&store);
        let fence_scope = scope.clone();
        let fencer = ::zeroclaw_spawn::spawn!(async move {
            fence_store
                .pause_session_goal(
                    fence_scope.task_id(),
                    fence_scope.session_id(),
                    fence_scope.execution_epoch(),
                    operator_pause(GoalPauseReason::OperatorPaused),
                )
                .await
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!fencer.is_finished());
        drop(start);
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), fencer)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(format!("{error:#}").contains("fixture pause write failed"));
        assert_eq!(
            store
                .current_goal_for_session(scope.session_id())
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Running
        );
        accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![usage_event("fallback", 1, 1)],
            })
            .await
            .unwrap();
        let next = factory.for_scope(scope);
        next.admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        next.release_unstarted().await.unwrap();
    }

    #[tokio::test]
    async fn unstarted_release_after_pause_allows_explicit_resume() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        assert_eq!(
            store
                .pause_session_goal(
                    scope.task_id(),
                    scope.session_id(),
                    scope.execution_epoch(),
                    GoalPauseState {
                        reason: GoalPauseReason::OperatorPaused,
                        description: None,
                        blockers: Vec::new(),
                    },
                )
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        accountant.release_unstarted().await.unwrap();
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert_eq!(
            store
                .resume_session_goal(
                    scope.task_id(),
                    scope.session_id(),
                    scope.execution_epoch() + 1,
                    2,
                    "resumed-boot",
                )
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        let resumed_scope = GoalExecutionScope::new(
            scope.task_id(),
            scope.session_id(),
            scope.execution_epoch() + 2,
        )
        .unwrap();
        let resumed = factory.for_scope(resumed_scope);
        resumed
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .expect("explicitly resumed Goal can admit work after an unstarted release");
        resumed.release_unstarted().await.unwrap();
    }

    #[tokio::test]
    async fn failed_unstarted_release_keeps_the_pending_fence() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        let faulted = faulted_factory(
            &factory,
            Arc::clone(&store),
            AdmissionFault::Settlement(GoalTransitionResult::Stale),
        );
        let accountant = faulted.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        let error = accountant.release_unstarted().await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("release lost its execution fence")
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert!(goal.pending_call_id.is_some());
        assert_eq!(goal.pending_call_epoch, Some(scope.execution_epoch()));
        assert!(
            accountant
                .admit(GoalOperationRequest::new("fallback", "model"))
                .await
                .is_err(),
            "a failed release must not admit another operation"
        );
    }

    #[tokio::test]
    async fn failed_budget_pause_does_not_carry_a_committed_fence_identity() {
        for (fault, expected) in [
            (
                AdmissionFault::Pause(GoalTransitionResult::Stale),
                "Goal budget pause lost its execution fence",
            ),
            (
                AdmissionFault::Pause(GoalTransitionResult::Missing),
                "Goal budget pause lost its execution fence",
            ),
            (
                AdmissionFault::PauseStoreFailure,
                "fixture registry pause failed",
            ),
        ] {
            let (store, factory, _tracker, scope, _directory) = fixture().await;
            let accountant = factory.for_scope(scope.clone());
            accountant
                .admit(GoalOperationRequest::new("fallback", "model"))
                .await
                .unwrap();
            accountant
                .settle(GoalOperationSettlement {
                    accounting_state: GoalAccountingState::Complete,
                    events: vec![GoalUsageEvent {
                        provider_ref: "fallback".to_owned(),
                        model: "model".to_owned(),
                        usage: usage(10, 2),
                        priceable: true,
                    }],
                })
                .await
                .unwrap();
            assert_eq!(
                store
                    .update_session_goal_limits(
                        scope.task_id(),
                        scope.session_id(),
                        1,
                        Some(1),
                        None
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
            let error = faulted_factory(&factory, Arc::clone(&store), fault)
                .for_scope(scope.clone())
                .admit(GoalOperationRequest::new("fallback", "model"))
                .await
                .unwrap_err();
            assert_untyped_refusal(&error, expected);
            let task = store
                .current_goal_for_session(scope.session_id())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(task.status, zeroclaw_api::task::TaskStatus::Running);
            assert_eq!(task.execution_epoch, scope.execution_epoch());
            let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
            assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
            assert!(goal.pending_call_id.is_none());
            assert!(goal.pending_call_epoch.is_none());
            assert!(goal.pause_reason.is_none());
        }
    }

    #[tokio::test]
    async fn unavailable_route_pricing_is_not_an_execution_fence_refusal() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        assert_eq!(
            store
                .update_session_goal_limits(scope.task_id(), scope.session_id(), 1, None, Some(1.0))
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        let error = factory
            .for_scope(scope)
            .admit(GoalOperationRequest::new("unpriced-route", "model"))
            .await
            .unwrap_err();
        assert_untyped_refusal(
            &error,
            "Goal cost budget lacks complete pricing for the configured route",
        );
    }

    #[tokio::test]
    async fn fenced_admission_without_a_current_task_preserves_its_error() {
        let (_store, factory, _tracker, scope, _directory) = fixture().await;
        let missing = GoalExecutionScope::new(scope.task_id(), "missing-session", 1).unwrap();
        assert_fenced_admission(
            &factory,
            missing,
            "Goal session no longer has a current task",
        )
        .await;
    }

    #[tokio::test]
    async fn fenced_admission_with_a_stale_task_identity_preserves_its_error() {
        let (_store, factory, _tracker, scope, _directory) = fixture().await;
        let stale = GoalExecutionScope::new("stale-task", scope.session_id(), 1).unwrap();
        assert_fenced_admission(&factory, stale, "Goal task identity is stale").await;
    }

    #[tokio::test]
    async fn fenced_admission_after_a_committed_pause_preserves_its_error() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        assert_eq!(
            store
                .pause_session_goal(
                    scope.task_id(),
                    scope.session_id(),
                    scope.execution_epoch(),
                    GoalPauseState {
                        reason: GoalPauseReason::OperatorPaused,
                        description: None,
                        blockers: Vec::new(),
                    },
                )
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        assert_fenced_admission(&factory, scope, "Goal task is not running").await;
    }

    #[tokio::test]
    async fn fenced_admission_with_a_stale_epoch_preserves_its_error() {
        let (_store, factory, _tracker, scope, _directory) = fixture().await;
        let stale = GoalExecutionScope::new(scope.task_id(), scope.session_id(), 2).unwrap();
        assert_fenced_admission(&factory, stale, "Goal execution epoch is stale").await;
    }

    #[tokio::test]
    async fn known_usage_is_persisted_before_invalid_accounting_blocks_next_admission() {
        let (store, factory, _tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Invalid,
                events: vec![GoalUsageEvent {
                    provider_ref: "fallback".to_owned(),
                    model: "model".to_owned(),
                    usage: usage(41, 0),
                    priceable: true,
                }],
            })
            .await
            .unwrap();

        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert!(goal.pending_call_id.is_none());
        assert_eq!(goal.accounting_state, GoalAccountingState::Invalid);
        assert!(accountant.pricing_complete().await.unwrap());

        let error = accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .expect_err("incomplete accounting must fence the next provider operation");
        assert!(error.to_string().contains("incomplete"));
        assert!(
            error
                .downcast_ref::<zeroclaw_goal::accounting::GoalOperationFenced>()
                .is_none()
        );
        assert!(
            store
                .get_goal_task(scope.task_id())
                .await
                .unwrap()
                .unwrap()
                .pending_call_id
                .is_none()
        );
    }

    #[tokio::test]
    async fn actual_provider_usage_is_durable_before_the_pending_fence_is_cleared() {
        let (store, factory, tracker, scope, directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap();
        let admitted = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert!(admitted.pending_call_id.is_some());
        assert_eq!(admitted.pending_call_epoch, Some(scope.execution_epoch()));

        accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![GoalUsageEvent {
                    provider_ref: "fallback".to_owned(),
                    model: "model".to_owned(),
                    usage: TokenUsage {
                        input_tokens: Some(1_200),
                        output_tokens: Some(500),
                        cached_input_tokens: Some(200),
                        cache_creation_input_tokens: Some(300),
                    },
                    priceable: true,
                }],
            })
            .await
            .unwrap();

        let settled = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert!(settled.pending_call_id.is_none());
        assert!(settled.pending_call_epoch.is_none());
        assert_eq!(settled.accounting_state, GoalAccountingState::Complete);
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap()
                .0,
            1_700
        );
        let contents = std::fs::read_to_string(directory.path().join("state/costs.jsonl"))
            .expect("settled usage record is durable");
        let record: zeroclaw_config::cost::types::CostRecord =
            serde_json::from_str(contents.lines().next().expect("one cost record")).unwrap();
        assert_eq!(record.provider_ref.as_deref(), Some("fallback"));
        assert_eq!(record.task_id.as_deref(), Some(scope.task_id()));
        assert_eq!(record.usage.model, "model");
        assert_eq!(record.usage.input_tokens, 1_200);
        assert_eq!(record.usage.output_tokens, 500);
        assert_eq!(record.usage.cached_input_tokens, 200);
        assert_eq!(record.usage.cache_creation_input_tokens, 300);
        assert_eq!(record.usage.total_tokens, 1_700);
        let expected_cost = (700.0 + 300.0 * 1.5 + 200.0 * 0.5 + 500.0 * 2.0) / 1_000_000.0;
        assert!((record.usage.cost_usd - expected_cost).abs() < 1e-12);
        assert_eq!(record.usage.unpriced_tokens, 0);
        assert!(record.usage.pricing_available);
    }

    #[tokio::test]
    async fn ledger_persistence_failure_preserves_durable_pending_operation() {
        let directory = TempDir::new().unwrap();
        let control_plane_dir = directory.path().join("control");
        let store = Arc::new(SqliteTaskStore::new(&control_plane_dir).unwrap());
        let (store, factory, _tracker, scope, directory) =
            fixture_with_store(store, directory).await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        let admitted = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        let pending_call_id = admitted
            .pending_call_id
            .clone()
            .expect("admission must persist a pending operation");

        // A directory at the JSONL path deterministically rejects ledger
        // persistence without changing the already-durable task marker.
        std::fs::create_dir(directory.path().join("state/costs.jsonl")).unwrap();
        let error = accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![GoalUsageEvent {
                    provider_ref: "fallback".to_owned(),
                    model: "model".to_owned(),
                    usage: usage(41, 2),
                    priceable: true,
                }],
            })
            .await
            .expect_err("ledger persistence failure must not settle the operation");
        assert!(format!("{error:#}").contains("cost storage"));

        let after_failure = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(
            after_failure.pending_call_id.as_deref(),
            Some(pending_call_id.as_str())
        );
        assert_eq!(
            after_failure.pending_call_epoch,
            Some(scope.execution_epoch())
        );
        assert_eq!(
            after_failure.accounting_state,
            GoalAccountingState::Complete
        );

        let next_error = accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .expect_err("a pending operation must fence the next model call");
        assert!(next_error.to_string().contains("pending operation"));
        assert!(next_error.downcast_ref::<GoalAccountingFailure>().is_some());
        assert!(
            next_error
                .downcast_ref::<zeroclaw_goal::accounting::GoalOperationFenced>()
                .is_none()
        );
        drop(accountant);
        drop(factory);
        drop(store);

        let reopened = SqliteTaskStore::new(&directory.path().join("control")).unwrap();
        let durable = reopened
            .get_goal_task(scope.task_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            durable.pending_call_id.as_deref(),
            Some(pending_call_id.as_str())
        );
        assert_eq!(durable.pending_call_epoch, Some(scope.execution_epoch()));
        assert_eq!(durable.accounting_state, GoalAccountingState::Complete);
        drop(reopened);
    }

    #[tokio::test]
    async fn invalid_cache_partition_retains_tokens_without_reporting_cost() {
        let (store, factory, tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Invalid,
                events: vec![GoalUsageEvent {
                    provider_ref: "fallback".to_owned(),
                    model: "model".to_owned(),
                    usage: usage(10, 5),
                    priceable: false,
                }],
            })
            .await
            .unwrap();

        let (tokens, cost, pricing_complete) = tracker
            .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
            .unwrap();
        assert_eq!(tokens, 15);
        assert_eq!(cost, 0.0);
        assert!(!pricing_complete);
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Invalid);
    }

    #[tokio::test]
    async fn unknown_outcome_persists_observed_usage_and_blocks_successor_admission() {
        let (store, factory, tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap();
        accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::OutcomeUnknown,
                events: vec![GoalUsageEvent {
                    provider_ref: "primary".to_owned(),
                    model: "model".to_owned(),
                    usage: usage(8, 3),
                    priceable: true,
                }],
            })
            .await
            .unwrap();

        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap()
                .0,
            11
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::OutcomeUnknown);
        assert!(goal.pending_call_id.is_none());
        let error = accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .expect_err("unknown outcome must not admit another provider call");
        assert!(error.to_string().contains("incomplete"));
    }

    #[tokio::test]
    async fn missing_usage_blocks_successor_admission_without_fabricating_totals() {
        let (store, factory, tracker, scope, _directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap();
        accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Missing,
                events: Vec::new(),
            })
            .await
            .unwrap();

        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap()
                .0,
            0
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Missing);
        assert!(goal.pending_call_id.is_none());
        let error = accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .expect_err("missing usage must not admit another provider call");
        assert!(error.to_string().contains("incomplete"));
    }

    struct RouteFailingPricer {
        failing: &'static str,
        inner: ModelProviderGoalUsagePricer,
    }

    impl RouteFailingPricer {
        fn new(failing: &'static str) -> Self {
            let rates = HashMap::from([
                ("model.input".to_owned(), 1.0),
                ("model.output".to_owned(), 2.0),
                ("model.cached_input".to_owned(), 0.5),
                ("model.cache_write".to_owned(), 1.5),
            ]);
            Self {
                failing,
                inner: ModelProviderGoalUsagePricer::new(Arc::new(HashMap::from([
                    ("fallback".to_owned(), rates.clone()),
                    ("primary".to_owned(), rates),
                ]))),
            }
        }
    }

    impl GoalUsagePricer for RouteFailingPricer {
        fn price(
            &self,
            provider_ref: &str,
            model: &str,
            usage: &TokenUsage,
        ) -> Result<GoalUsagePrice> {
            if provider_ref == self.failing {
                bail!("fixture pricing failed for this route");
            }
            self.inner.price(provider_ref, model, usage)
        }
    }

    struct NanPricer;

    impl GoalUsagePricer for NanPricer {
        fn price(
            &self,
            _provider_ref: &str,
            _model: &str,
            _usage: &TokenUsage,
        ) -> Result<GoalUsagePrice> {
            Ok(GoalUsagePrice {
                cost_usd: f64::NAN,
                unpriced_tokens: 0,
                pricing_available: true,
                timestamp: chrono::Utc::now(),
            })
        }
    }

    fn usage_event(provider_ref: &str, input: u64, output: u64) -> GoalUsageEvent {
        GoalUsageEvent {
            provider_ref: provider_ref.to_owned(),
            model: "model".to_owned(),
            usage: usage(input, output),
            priceable: true,
        }
    }

    fn cost_records(directory: &TempDir) -> Vec<zeroclaw_config::cost::types::CostRecord> {
        std::fs::read_to_string(directory.path().join("state/costs.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn pricing_failure_on_one_event_retains_it_unpriced_with_its_priced_sibling() {
        // A failed fallback price must not erase either event or hide its original cause.
        let (store, mut factory, tracker, scope, directory) = fixture().await;
        factory.pricer = Arc::new(RouteFailingPricer::new("fallback"));
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap();
        let error = accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![usage_event("fallback", 41, 2), usage_event("primary", 8, 3)],
            })
            .await
            .unwrap_err();

        assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
        let text = error.to_string();
        for expected in [
            "Goal accounting is invalid after a durable usage settlement",
            "provider `fallback`, model `model`",
            "43 tokens are treated as unpriced",
            "fixture pricing failed for this route",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert_eq!(format!("{error:#}"), text);
        let records = cost_records(&directory);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].provider_ref.as_deref(), Some("fallback"));
        assert_eq!(records[0].task_id.as_deref(), Some(scope.task_id()));
        assert_eq!(records[0].usage.input_tokens, 41);
        assert_eq!(records[0].usage.output_tokens, 2);
        assert_eq!(records[0].usage.total_tokens, 43);
        assert_eq!(records[0].usage.cost_usd, 0.0);
        assert_eq!(records[0].usage.unpriced_tokens, 43);
        assert!(!records[0].usage.pricing_available);
        assert_eq!(records[1].provider_ref.as_deref(), Some("primary"));
        assert_eq!(records[1].usage.total_tokens, 11);
        assert!((records[1].usage.cost_usd - (8.0 + 3.0 * 2.0) / 1_000_000.0).abs() < 1e-12);
        assert_eq!(records[1].usage.unpriced_tokens, 0);
        assert!(records[1].usage.pricing_available);
        let (tokens, _, pricing_complete) = tracker
            .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
            .unwrap();
        assert_eq!(tokens, 54);
        assert!(!pricing_complete);
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Invalid);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert!(goal.effective_token_limit.is_none());
        assert!(goal.effective_cost_limit_usd.is_none());
        let next_error = accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap_err();
        assert!(next_error.downcast_ref::<GoalAccountingFailure>().is_some());
        assert!(next_error.to_string().contains("incomplete"));
        assert!(
            next_error
                .to_string()
                .contains("invalid or could not be priced")
        );
    }

    #[tokio::test]
    async fn pricing_failure_does_not_weaken_an_unknown_outcome() {
        // Pricing uncertainty must not replace the stronger unknown-operation outcome.
        let (store, mut factory, tracker, scope, directory) = fixture().await;
        factory.pricer = Arc::new(RouteFailingPricer::new("fallback"));
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap();
        let error = accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::OutcomeUnknown,
                events: vec![usage_event("fallback", 41, 2)],
            })
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
        assert!(
            error
                .to_string()
                .contains("Goal accounting is outcome-unknown after a durable usage settlement")
        );
        assert!(
            error
                .to_string()
                .contains("fixture pricing failed for this route")
        );
        assert_eq!(cost_records(&directory).len(), 1);
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap()
                .0,
            43
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::OutcomeUnknown);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
    }

    #[tokio::test]
    async fn invalid_price_value_is_retained_as_unpriced_usage() {
        // Invalid cost values retain the valid token record and surface the pricing condition.
        let (store, mut factory, tracker, scope, directory) = fixture().await;
        factory.pricer = Arc::new(NanPricer);
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        let error = accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![usage_event("fallback", 10, 5)],
            })
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
        assert!(
            error
                .to_string()
                .contains("15 tokens are treated as unpriced")
        );
        assert!(error.to_string().contains("Goal usage cost is invalid"));
        let records = cost_records(&directory);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].usage.total_tokens, 15);
        assert_eq!(records[0].usage.cost_usd, 0.0);
        assert_eq!(records[0].usage.unpriced_tokens, 15);
        assert!(!records[0].usage.pricing_available);
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap(),
            (15, 0.0, false)
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Invalid);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
    }

    #[tokio::test]
    async fn missing_route_rates_keep_complete_accounting() {
        // An ordinary successful unpriced result is not a pricing-resolver error.
        let (store, factory, tracker, scope, directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap();
        accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![usage_event("unrated", 7, 1)],
            })
            .await
            .unwrap();
        let records = cost_records(&directory);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].provider_ref.as_deref(), Some("unrated"));
        assert_eq!(records[0].usage.unpriced_tokens, 8);
        assert!(!records[0].usage.pricing_available);
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap(),
            (8, 0.0, false)
        );
        assert_eq!(
            store
                .get_goal_task(scope.task_id())
                .await
                .unwrap()
                .unwrap()
                .accounting_state,
            GoalAccountingState::Complete
        );
        accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rejected_usage_event_is_reported_after_its_valid_sibling_settles() {
        // A structurally rejected event does not suppress a valid sibling's durable usage.
        let (store, factory, tracker, scope, directory) = fixture().await;
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap();
        let error = accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![usage_event("fallback", 0, 0), usage_event("primary", 8, 3)],
            })
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
        assert!(
            error
                .to_string()
                .contains("a usage event for provider `fallback`, model `model` was rejected")
        );
        assert!(error.to_string().contains("Goal usage is all zero"));
        let records = cost_records(&directory);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].provider_ref.as_deref(), Some("primary"));
        assert_eq!(records[0].usage.total_tokens, 11);
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap()
                .0,
            11
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Invalid);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
    }

    #[tokio::test]
    async fn pricing_failure_with_ledger_failure_keeps_pending_operation_and_both_causes() {
        // Failed persistence keeps the pending operation and never claims its tokens were recorded.
        let directory = TempDir::new().unwrap();
        let store = Arc::new(SqliteTaskStore::new(&directory.path().join("control")).unwrap());
        let (store, mut factory, _tracker, scope, directory) =
            fixture_with_store(store, directory).await;
        factory.pricer = Arc::new(RouteFailingPricer::new("fallback"));
        let accountant = factory.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap();
        let admitted = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        std::fs::create_dir(directory.path().join("state/costs.jsonl")).unwrap();
        let error = accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![usage_event("fallback", 41, 2)],
            })
            .await
            .unwrap_err();
        assert!(
            !error
                .chain()
                .any(|cause| cause.is::<GoalAccountingFailure>())
        );
        let text = format!("{error:#}");
        assert!(text.contains("cost storage"));
        assert!(text.contains("fixture pricing failed for this route"));
        assert!(text.contains("43 tokens are treated as unpriced"));
        assert!(!text.contains("were recorded"));
        assert!(
            error.to_string().contains(
                "Goal usage settlement did not complete, and Goal accounting also failed"
            )
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.pending_call_id, admitted.pending_call_id);
        assert!(goal.pending_call_id.is_some());
        assert_eq!(goal.pending_call_epoch, Some(scope.execution_epoch()));
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
    }

    #[tokio::test]
    async fn pricing_failure_with_lost_settlement_fence_reports_both_causes() {
        // A lost settlement fence keeps persisted usage and the original pricing cause.
        let (store, mut factory, tracker, scope, directory) = fixture().await;
        factory.pricer = Arc::new(RouteFailingPricer::new("fallback"));
        let faulted = faulted_factory(
            &factory,
            Arc::clone(&store),
            AdmissionFault::Settlement(GoalTransitionResult::Stale),
        );
        let accountant = faulted.for_scope(scope.clone());
        accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap();
        let admitted = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        let error = accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![usage_event("fallback", 41, 2)],
            })
            .await
            .unwrap_err();
        assert!(
            !error
                .chain()
                .any(|cause| cause.is::<GoalAccountingFailure>())
        );
        let text = format!("{error:#}");
        assert!(text.contains("Goal operation settlement lost its execution fence"));
        assert!(text.contains("fixture pricing failed for this route"));
        let records = cost_records(&directory);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].usage.total_tokens, 43);
        assert_eq!(records[0].usage.unpriced_tokens, 43);
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap(),
            (43, 0.0, false)
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.pending_call_id, admitted.pending_call_id);
        assert!(goal.pending_call_id.is_some());
        assert_eq!(goal.pending_call_epoch, Some(scope.execution_epoch()));
    }

    #[tokio::test]
    async fn usage_warning_attributes_scrub_the_logged_error_but_keep_the_returned_cause() {
        // The log boundary scrubs the synthetic bot token while the returned cause remains intact.
        let canary = "123456:ABC-def_GHI";
        let message = "fixture pricing failed for this route: https://api.telegram.org/bot123456:ABC-def_GHI/getUpdates";
        let event = usage_event("fallback", 41, 2);
        let error = anyhow::Error::msg(message);
        let attrs = usage_warning_attributes(&event, &error);
        let logged_error = attrs["error"].as_str().unwrap();
        assert!(!logged_error.contains(canary));
        assert!(logged_error.contains("[REDACTED_BOT_TOKEN]"));
        assert!(logged_error.contains("fixture pricing failed for this route"));
        assert_eq!(attrs["provider_ref"], "fallback");
        assert_eq!(attrs["model"], "model");

        struct CanaryPricer {
            inner: RouteFailingPricer,
            message: String,
        }

        impl GoalUsagePricer for CanaryPricer {
            fn price(
                &self,
                provider_ref: &str,
                model: &str,
                usage: &TokenUsage,
            ) -> Result<GoalUsagePrice> {
                if provider_ref == "fallback" {
                    return Err(anyhow::Error::msg(self.message.clone()));
                }
                self.inner.price(provider_ref, model, usage)
            }
        }

        let (_store, mut factory, _tracker, scope, _directory) = fixture().await;
        factory.pricer = Arc::new(CanaryPricer {
            inner: RouteFailingPricer::new("fallback"),
            message: message.to_owned(),
        });
        let accountant = factory.for_scope(scope);
        accountant
            .admit(GoalOperationRequest::new("primary", "model"))
            .await
            .unwrap();
        let error = accountant
            .settle(GoalOperationSettlement {
                accounting_state: GoalAccountingState::Complete,
                events: vec![event],
            })
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
        assert!(error.to_string().contains(canary));
        assert!(error.to_string().contains(message));
    }

    #[test]
    fn injected_pricer_preserves_existing_cost_and_pricing_projection() {
        let mut rates = HashMap::new();
        rates.insert("model.input".to_owned(), 1.0);
        rates.insert("model.output".to_owned(), 2.0);
        rates.insert("model.cached_input".to_owned(), 0.5);
        rates.insert("model.cache_write".to_owned(), 1.5);
        let pricer = ModelProviderGoalUsagePricer::new(Arc::new(HashMap::from([(
            "fallback".to_owned(),
            rates,
        )])));
        let priced = pricer
            .price(
                "fallback",
                "model",
                &TokenUsage {
                    input_tokens: Some(1_200),
                    output_tokens: Some(500),
                    cached_input_tokens: Some(200),
                    cache_creation_input_tokens: Some(300),
                },
            )
            .unwrap();
        assert!(priced.pricing_available);
        assert_eq!(priced.unpriced_tokens, 0);
        let expected = (700.0 + 300.0 * 1.5 + 200.0 * 0.5 + 500.0 * 2.0) / 1_000_000.0;
        assert!((priced.cost_usd - expected).abs() < 1e-12);
    }
}
