//! Engine-level ordering checks for asynchronous accounting recovery.

use super::*;
use crate::accounting::{
    GoalOperationAccounting, GoalOperationGuard, GoalOperationRequest, GoalOperationSettlement,
    GoalUnsettledOperation,
};
use crate::task::{GoalDispatchStart, GoalTaskRecord, TaskContinuationContext};
use std::sync::Mutex;

type Events = Arc<Mutex<Vec<&'static str>>>;

struct RecordingAccountant(Events, Option<GoalAccountingFailure>);

#[async_trait::async_trait]
impl GoalOperationAccounting for RecordingAccountant {
    async fn admit(&self, _: GoalOperationRequest) -> Result<()> {
        unreachable!("this test does not admit provider work")
    }
    fn admission_guard(&self) -> Result<GoalOperationGuard> {
        unreachable!("this test does not admit provider work")
    }
    async fn begin_dispatch(&self) -> Result<GoalDispatchStart> {
        unreachable!("this test does not dispatch")
    }
    async fn settle(&self, _: GoalOperationSettlement) -> Result<()> {
        unreachable!("this test observes quiescence")
    }
    async fn release_unstarted(&self) -> Result<()> {
        unreachable!("this test observes quiescence")
    }
    async fn pricing_complete(&self) -> Result<bool> {
        unreachable!("the fixture has no cost limit")
    }
    async fn quiesce(&self) {
        tokio::task::yield_now().await;
        self.0.lock().unwrap().push("quiesce");
    }
    fn unsettled_failure(&self) -> Option<GoalAccountingFailure> {
        if self.1.is_some() {
            self.0.lock().unwrap().push("failure");
        }
        self.1.clone()
    }
}

struct UnusedFactory;

impl GoalOperationAccountingFactory for UnusedFactory {
    fn for_scope(&self, _: GoalExecutionScope) -> Arc<dyn GoalOperationAccounting> {
        unreachable!("the test supplies its accountant directly")
    }
}

struct RecordingRegistry(Events);

#[async_trait::async_trait]
impl GoalTaskRegistry for RecordingRegistry {
    async fn get_goal_task(&self, task_id: &str) -> Result<Option<GoalTaskRecord>> {
        self.0.lock().unwrap().push("read");
        Ok(Some(GoalTaskRecord {
            task_id: task_id.to_owned(),
            ..GoalTaskRecord::default()
        }))
    }

    async fn latest_active_goal_for_agent(&self, _: &str) -> Result<Option<TaskRecord>> {
        unreachable!()
    }
    async fn latest_active_goal_for_context(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<Option<TaskRecord>> {
        unreachable!()
    }
    async fn latest_active_goal_id_for_context(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<Option<String>> {
        unreachable!()
    }
    async fn update_goal_limits(&self, _: &str, _: Option<u64>, _: Option<f64>) -> Result<()> {
        unreachable!()
    }
    async fn update_goal_pause(&self, _: &str, _: Option<GoalPauseState>) -> Result<()> {
        unreachable!()
    }
    async fn set_continuation_context(
        &self,
        _: &str,
        _: Option<TaskContinuationContext>,
    ) -> Result<()> {
        unreachable!()
    }
    async fn get_continuation_context(&self, _: &str) -> Result<Option<TaskContinuationContext>> {
        unreachable!()
    }
    async fn current_goal_for_session(&self, _: &str) -> Result<Option<TaskRecord>> {
        unreachable!()
    }
    async fn terminal_reason_for_session_goal(&self, _: &str, _: &str) -> Result<Option<String>> {
        unreachable!()
    }
    async fn create_or_replace_session_goal(
        &self,
        _: TaskRecord,
        _: GoalTaskRecord,
    ) -> Result<GoalTransitionResult> {
        unreachable!()
    }
    async fn begin_operation_dispatch(
        &self,
        _: &str,
        _: &str,
        _: i64,
        _: &str,
    ) -> Result<Option<GoalDispatchStart>> {
        unreachable!()
    }
    async fn pause_session_goal(
        &self,
        _: &str,
        _: &str,
        _: i64,
        _: GoalPauseState,
    ) -> Result<GoalTransitionResult> {
        unreachable!()
    }
    async fn resume_session_goal(
        &self,
        _: &str,
        _: &str,
        _: i64,
        _: u32,
        _: &str,
    ) -> Result<GoalTransitionResult> {
        unreachable!()
    }
    async fn finish_session_goal(
        &self,
        _: &str,
        _: &str,
        _: i64,
        status: TaskStatus,
        reason: Option<String>,
    ) -> Result<GoalTransitionResult> {
        assert_eq!(status, TaskStatus::Failed);
        assert_eq!(reason.as_deref(), Some("accounting_missing_or_invalid"));
        self.0.lock().unwrap().push("fail");
        Ok(GoalTransitionResult::Applied)
    }
    async fn admit_pending_operation(
        &self,
        _: &str,
        _: &str,
        _: i64,
        _: &str,
    ) -> Result<GoalTransitionResult> {
        unreachable!()
    }
    async fn settle_pending_operation(
        &self,
        _: &str,
        _: &str,
        _: i64,
        _: &str,
        _: GoalAccountingState,
    ) -> Result<GoalTransitionResult> {
        unreachable!()
    }
    async fn update_session_goal_limits(
        &self,
        _: &str,
        _: &str,
        _: i64,
        _: Option<u64>,
        _: Option<f64>,
    ) -> Result<GoalTransitionResult> {
        unreachable!()
    }
    async fn delete_session_goal(&self, _: &str, _: &str, _: i64) -> Result<GoalTransitionResult> {
        unreachable!()
    }
}

#[tokio::test]
async fn engine_quiesces_before_accounting_classification() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let registry: Arc<dyn GoalTaskRegistry> = Arc::new(RecordingRegistry(Arc::clone(&events)));
    let engine = GoalExecutionEngine::new(GoalRuntime::new(registry), Arc::new(UnusedFactory));
    let accountant = RecordingAccountant(Arc::clone(&events), None);
    let scope = GoalExecutionScope::new("task", "session", 1).unwrap();

    engine
        .require_complete_accounting(&scope, &accountant)
        .await
        .unwrap();
    assert_eq!(*events.lock().unwrap(), ["quiesce", "read"]);
    events.lock().unwrap().clear();

    let (reason, _) = engine
        .classify_operation_failure(
            &scope,
            &accountant,
            anyhow::Error::msg("provider failed"),
            "parent_operation_failed",
            "parent failed",
        )
        .await;
    assert_eq!(reason, "parent_operation_failed");
    assert_eq!(*events.lock().unwrap(), ["quiesce", "read"]);
    events.lock().unwrap().clear();

    let error = anyhow::Error::new(GoalAccountingFailure::incomplete(
        GoalAccountingState::OutcomeUnknown,
        true,
    ));
    let (reason, _) = engine
        .classify_operation_failure(
            &scope,
            &accountant,
            error,
            "parent_operation_failed",
            "parent failed",
        )
        .await;
    assert_eq!(reason, "accounting_missing_or_invalid");
    assert_eq!(*events.lock().unwrap(), ["quiesce"]);
}

#[tokio::test]
async fn engine_surfaces_retained_settlement_failure_after_quiescence() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let registry: Arc<dyn GoalTaskRegistry> = Arc::new(RecordingRegistry(Arc::clone(&events)));
    let engine = GoalExecutionEngine::new(GoalRuntime::new(registry), Arc::new(UnusedFactory));
    let failure = GoalAccountingFailure::unsettled(
        GoalUnsettledOperation::Dispatched,
        "fixture ledger failure",
    );
    let accountant = RecordingAccountant(Arc::clone(&events), Some(failure.clone()));
    let scope = GoalExecutionScope::new("task", "session", 1).unwrap();

    let error = engine
        .require_complete_accounting(&scope, &accountant)
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), failure.to_string());
    assert_eq!(*events.lock().unwrap(), ["quiesce", "failure", "fail"]);
    events.lock().unwrap().clear();

    let (reason, error) = engine
        .classify_operation_failure(
            &scope,
            &accountant,
            anyhow::Error::msg("provider failed"),
            "parent_operation_failed",
            "parent failed",
        )
        .await;
    assert_eq!(reason, "accounting_missing_or_invalid");
    let text = error.to_string();
    assert_eq!(text.matches("fixture ledger failure").count(), 1);
    assert!(text.contains("original operation error: provider failed"));
    assert_eq!(*events.lock().unwrap(), ["quiesce", "failure"]);
}
