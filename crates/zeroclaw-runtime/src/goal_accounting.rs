//! Runtime persistence adapter for Goal-owned provider accounting contracts.

use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;
use zeroclaw_api::model_provider::TokenUsage;
use zeroclaw_config::cost::{CostTracker, types::TokenUsage as CostTokenUsage};
use zeroclaw_goal::{
    GoalAccountingState, GoalExecutionScope, GoalTaskRegistry, GoalTransitionResult,
    accounting::{
        GoalOperationAccounting, GoalOperationAccountingFactory, GoalOperationFenced,
        GoalOperationRequest, GoalOperationSettlement, GoalUsageEvent, GoalUsagePrice,
        GoalUsagePricer,
    },
    task::{GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState, TaskGoal},
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
        Arc::new(GoalOperationAccountant::new(
            Arc::clone(&self.registry),
            Arc::clone(&self.tracker),
            self.agent_alias.clone(),
            Arc::clone(&self.pricer),
            scope,
        ))
    }
}

struct AdmittedOperation {
    id: String,
    _permit: OwnedSemaphorePermit,
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
}

impl GoalOperationAccountant {
    fn new(
        registry: Arc<dyn GoalTaskRegistry>,
        tracker: Arc<CostTracker>,
        agent_alias: String,
        pricer: Arc<dyn GoalUsagePricer>,
        scope: GoalExecutionScope,
    ) -> Self {
        Self {
            registry,
            tracker,
            agent_alias,
            pricer,
            scope,
            permit: Arc::new(Semaphore::new(1)),
            admitted: Mutex::new(None),
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

    fn validated_cost_usage(&self, event: &GoalUsageEvent) -> Result<CostTokenUsage> {
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

        let mut price = self
            .pricer
            .price(&event.provider_ref, &event.model, &event.usage)?;
        if !event.priceable {
            // Keep the observed token lower bound, but do not report a dollar
            // figure based on an invalid cache-token partition.
            price.cost_usd = 0.0;
            price.unpriced_tokens = input + output;
            price.pricing_available = false;
        }
        ensure!(
            price.cost_usd.is_finite() && price.cost_usd >= 0.0,
            "Goal usage cost is invalid"
        );
        Ok(CostTokenUsage {
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
        })
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

#[async_trait]
impl GoalOperationAccounting for GoalOperationAccountant {
    async fn admit(&self, request: GoalOperationRequest) -> Result<()> {
        let permit = Arc::clone(&self.permit)
            .acquire_owned()
            .await
            .context("Goal operation permit closed")?;
        ensure!(
            self.admitted.lock().await.is_none(),
            "Goal operation is already admitted"
        );

        let goal = self.current_running_goal().await?;
        ensure!(
            goal.goal().accounting_state == GoalAccountingState::Complete,
            "Goal accounting is incomplete"
        );
        ensure!(
            goal.goal().pending_call_id.is_none() && goal.goal().pending_call_epoch.is_none(),
            "Goal already has a pending operation"
        );
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
                let mut admitted = self.admitted.lock().await;
                debug_assert!(admitted.is_none());
                *admitted = Some(AdmittedOperation {
                    id: operation_id,
                    _permit: permit,
                });
                Ok(())
            }
            GoalTransitionResult::Stale | GoalTransitionResult::Missing => Err(anyhow::Error::new(
                GoalOperationFenced::ADMISSION_FENCE_LOST,
            )),
        }
    }

    async fn settle(&self, settlement: GoalOperationSettlement) -> Result<()> {
        let operation = {
            let mut admitted = self.admitted.lock().await;
            admitted
                .take()
                .context("Goal operation settled without a matching admission")?
        };

        let mut accounting_state = settlement.accounting_state;
        let mut events = Vec::with_capacity(settlement.events.len());
        for event in settlement.events {
            match self.validated_cost_usage(&event) {
                Ok(usage) => events.push((event, usage)),
                Err(_) => accounting_state = GoalAccountingState::Invalid,
            }
        }
        if accounting_state == GoalAccountingState::Complete && events.is_empty() {
            accounting_state = GoalAccountingState::Missing;
        }

        // Preserve known usage before settling the operation state. If ledger
        // persistence fails, the durable pending marker remains for restart
        // recovery to classify as outcome-unknown rather than replaying.
        self.record_events(events).await?;
        match self
            .registry
            .settle_pending_operation(
                self.scope.task_id(),
                self.scope.session_id(),
                self.scope.execution_epoch(),
                &operation.id,
                accounting_state,
            )
            .await?
        {
            GoalTransitionResult::Applied => Ok(()),
            GoalTransitionResult::Stale | GoalTransitionResult::Missing => {
                bail!("Goal operation settlement lost its execution fence")
            }
        }
    }

    async fn pricing_complete(&self) -> Result<bool> {
        Ok(self.strict_totals().await?.2)
    }
}

#[cfg(test)]
mod tests {
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
        Pause(GoalTransitionResult),
        PauseStoreFailure,
    }

    // Only the queried failure boundary is injected. All lifecycle and ledger
    // operations continue to use the real SQLite control plane.
    struct AdmissionFaultRegistry {
        store: Arc<SqliteTaskStore>,
        fault: AdmissionFault,
    }

    #[async_trait]
    impl GoalTaskRegistry for AdmissionFaultRegistry {
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

    async fn fixture() -> (
        Arc<SqliteTaskStore>,
        RuntimeGoalOperationAccountingFactory,
        Arc<CostTracker>,
        GoalExecutionScope,
        TempDir,
    ) {
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        fixture_with_store(store, TempDir::new().unwrap()).await
    }

    async fn fixture_with_store(
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
        *accountant.admitted.lock().await = Some(AdmittedOperation {
            id: "fixture-existing-operation".to_owned(),
            _permit: Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap(),
        });
        let error = accountant
            .admit(GoalOperationRequest::new("fallback", "model"))
            .await
            .unwrap_err();
        assert_untyped_refusal(&error, "Goal operation is already admitted");
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
