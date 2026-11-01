use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use tokio::sync::Notify;
use zeroclaw_goal::{
    GoalAccountingState, GoalPauseState, GoalTaskRecord, GoalTaskRegistry, GoalTransitionResult,
    TaskContinuationContext,
};
use zeroclaw_runtime::control_plane::{SqliteTaskStore, TaskRecord, TaskStatus};

#[derive(Debug)]
pub(super) struct OriginalTransitionFailure;

impl std::fmt::Display for OriginalTransitionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("distinctive original Goal transition failure")
    }
}

impl std::error::Error for OriginalTransitionFailure {}

#[derive(Debug)]
pub(super) struct WorkerTransitionFailure;

impl std::fmt::Display for WorkerTransitionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("distinctive worker Goal transition failure")
    }
}

impl std::error::Error for WorkerTransitionFailure {}

enum FinishFault {
    Original(&'static str),
    Worker(Arc<FinishFaultReleaseGate>),
}

impl FinishFault {
    fn reason(&self) -> &'static str {
        match self {
            Self::Original(reason) => reason,
            Self::Worker(_) => "executor_failed",
        }
    }
}

/// Only the named ordered finish calls fail; the real store owns every row.
pub(super) struct FinishFaultRegistry {
    store: Arc<SqliteTaskStore>,
    remaining: Mutex<VecDeque<FinishFault>>,
    attempts: Mutex<Vec<String>>,
    failures: AtomicUsize,
    applied: AtomicUsize,
    pause_gate: Option<Arc<FinishFaultPauseGate>>,
}

/// Hold the failed worker transition until an explicit pause fence is durable.
#[derive(Default)]
pub(super) struct FinishFaultPauseGate {
    pub(super) observed: Notify,
    paused: Notify,
}

/// Hold only the actual worker fallback, independently of operator controls.
#[derive(Default)]
pub(super) struct FinishFaultReleaseGate {
    pub(super) observed: Notify,
    pub(super) released: Notify,
}

impl FinishFaultRegistry {
    pub(super) fn new(store: Arc<SqliteTaskStore>, reason: &'static str) -> Self {
        Self {
            store,
            remaining: Mutex::new(VecDeque::from([FinishFault::Original(reason)])),
            attempts: Mutex::new(Vec::new()),
            failures: AtomicUsize::new(0),
            applied: AtomicUsize::new(0),
            pause_gate: None,
        }
    }

    pub(super) fn candidate_then_worker(
        store: Arc<SqliteTaskStore>,
        gate: Arc<FinishFaultReleaseGate>,
    ) -> Self {
        let registry = Self::new(store, "candidate_persistence_failed");
        registry
            .remaining
            .lock()
            .unwrap()
            .push_back(FinishFault::Worker(gate));
        registry
    }

    pub(super) fn with_pause_gate(mut self, gate: Arc<FinishFaultPauseGate>) -> Self {
        self.pause_gate = Some(gate);
        self
    }

    pub(super) fn failures(&self) -> usize {
        self.failures.load(Ordering::SeqCst)
    }

    pub(super) fn attempts(&self) -> Vec<String> {
        self.attempts.lock().unwrap().clone()
    }

    pub(super) fn applied(&self) -> usize {
        self.applied.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl GoalTaskRegistry for FinishFaultRegistry {
    async fn latest_active_goal_for_agent(
        &self,
        agent: &str,
    ) -> anyhow::Result<Option<TaskRecord>> {
        self.store.latest_active_goal_for_agent(agent).await
    }

    async fn latest_active_goal_for_context(
        &self,
        agent: &str,
        originator_route: Option<&str>,
        principal_id: Option<&str>,
    ) -> anyhow::Result<Option<TaskRecord>> {
        self.store
            .latest_active_goal_for_context(agent, originator_route, principal_id)
            .await
    }

    async fn latest_active_goal_id_for_context(
        &self,
        agent: &str,
        originator_route: Option<&str>,
        principal_id: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        self.store
            .latest_active_goal_id_for_context(agent, originator_route, principal_id)
            .await
    }

    async fn get_goal_task(&self, task_id: &str) -> anyhow::Result<Option<GoalTaskRecord>> {
        self.store.get_goal_task(task_id).await
    }

    async fn update_goal_limits(
        &self,
        task_id: &str,
        token_limit: Option<u64>,
        cost_limit_usd: Option<f64>,
    ) -> anyhow::Result<()> {
        self.store
            .update_goal_limits(task_id, token_limit, cost_limit_usd)
            .await
    }

    async fn update_goal_pause(
        &self,
        task_id: &str,
        pause: Option<GoalPauseState>,
    ) -> anyhow::Result<()> {
        self.store.update_goal_pause(task_id, pause).await
    }

    async fn set_continuation_context(
        &self,
        task_id: &str,
        context: Option<TaskContinuationContext>,
    ) -> anyhow::Result<()> {
        self.store.set_continuation_context(task_id, context).await
    }

    async fn get_continuation_context(
        &self,
        task_id: &str,
    ) -> anyhow::Result<Option<TaskContinuationContext>> {
        self.store.get_continuation_context(task_id).await
    }

    async fn current_goal_for_session(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<TaskRecord>> {
        self.store.current_goal_for_session(session_id).await
    }

    async fn terminal_reason_for_session_goal(
        &self,
        task_id: &str,
        session_id: &str,
    ) -> anyhow::Result<Option<String>> {
        self.store
            .terminal_reason_for_session_goal(task_id, session_id)
            .await
    }

    async fn create_or_replace_session_goal(
        &self,
        task: TaskRecord,
        goal: GoalTaskRecord,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store.create_or_replace_session_goal(task, goal).await
    }

    async fn pause_session_goal(
        &self,
        task_id: &str,
        session_id: &str,
        expected_epoch: i64,
        pause: GoalPauseState,
    ) -> anyhow::Result<GoalTransitionResult> {
        let result = self
            .store
            .pause_session_goal(task_id, session_id, expected_epoch, pause)
            .await?;
        if result == GoalTransitionResult::Applied
            && let Some(gate) = &self.pause_gate
        {
            gate.paused.notify_one();
        }
        Ok(result)
    }

    async fn resume_session_goal(
        &self,
        task_id: &str,
        session_id: &str,
        expected_epoch: i64,
        owner_pid: u32,
        owner_boot_id: &str,
    ) -> anyhow::Result<GoalTransitionResult> {
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
        status: TaskStatus,
        error: Option<String>,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.attempts
            .lock()
            .unwrap()
            .push(error.clone().unwrap_or_default());
        let fault = {
            let mut remaining = self.remaining.lock().unwrap();
            if status == TaskStatus::Failed
                && remaining
                    .front()
                    .is_some_and(|fault| Some(fault.reason()) == error.as_deref())
            {
                remaining.pop_front()
            } else {
                None
            }
        };
        if let Some(fault) = fault {
            self.failures.fetch_add(1, Ordering::SeqCst);
            match fault {
                FinishFault::Original(_) => {
                    if let Some(gate) = &self.pause_gate {
                        gate.observed.notify_one();
                        gate.paused.notified().await;
                    }
                    return Err(OriginalTransitionFailure.into());
                }
                FinishFault::Worker(gate) => {
                    gate.observed.notify_one();
                    gate.released.notified().await;
                    return Err(WorkerTransitionFailure.into());
                }
            }
        }
        let result = self
            .store
            .finish_session_goal(task_id, session_id, expected_epoch, status, error)
            .await?;
        if result == GoalTransitionResult::Applied {
            self.applied.fetch_add(1, Ordering::SeqCst);
        }
        Ok(result)
    }

    async fn admit_pending_operation(
        &self,
        task_id: &str,
        session_id: &str,
        expected_epoch: i64,
        operation_id: &str,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store
            .admit_pending_operation(task_id, session_id, expected_epoch, operation_id)
            .await
    }

    async fn settle_pending_operation(
        &self,
        task_id: &str,
        session_id: &str,
        admitted_epoch: i64,
        operation_id: &str,
        accounting_state: GoalAccountingState,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store
            .settle_pending_operation(
                task_id,
                session_id,
                admitted_epoch,
                operation_id,
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
    ) -> anyhow::Result<GoalTransitionResult> {
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
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store
            .delete_session_goal(task_id, session_id, expected_epoch)
            .await
    }
}
