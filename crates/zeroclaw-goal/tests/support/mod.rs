//! In-memory protocol fixtures. SQLite semantics are tested by VT85 in runtime.

use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use async_trait::async_trait;
use zeroclaw_api::task::{TaskKind, TaskRecord, TaskStatus};
use zeroclaw_goal::{
    GoalAccountingState, GoalExecutionScope, GoalPauseState, GoalTaskRecord, GoalTaskRegistry,
    GoalTransitionResult, TaskContinuationContext,
    accounting::{GoalOperationAccounting, GoalOperationAccountingFactory},
    task::{GoalControlIntent, GoalControlIntentSettlement},
};

#[derive(Debug, Clone, PartialEq)]
pub enum IntentState {
    Pending,
    Claimed,
    Settled(GoalControlIntentSettlement),
}

#[derive(Default)]
pub struct RegistryState {
    pub task: Option<TaskRecord>,
    pub goal: Option<GoalTaskRecord>,
    pub intents: Vec<(GoalControlIntent, IntentState)>,
    pub reject_claim: bool,
    pub fail_record: bool,
    pub fail_settlement: bool,
    pub settlement_wait: Option<Arc<tokio::sync::Semaphore>>,
    pub settlement_entered: Option<Arc<tokio::sync::Semaphore>>,
    pub transitions: Vec<&'static str>,
    pub session_lookups: usize,
}

#[derive(Default)]
pub struct Registry(pub Mutex<RegistryState>);

impl Registry {
    pub fn seed(&self, session: &str, status: TaskStatus) {
        let mut state = self.0.lock().unwrap();
        state.task = Some(TaskRecord {
            id: "fixture-goal".into(),
            kind: TaskKind::Goal,
            agent: "fixture-agent".into(),
            status,
            owner_pid: 1,
            owner_boot_id: "fixture-boot".into(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: Some("fixture-route".into()),
            originator_chain: vec![],
            delivered: false,
            idem_key: None,
            principal_id: None,
            session_id: Some(session.into()),
            execution_epoch: 1,
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
        });
        state.goal = Some(GoalTaskRecord {
            task_id: "fixture-goal".into(),
            objective: "Complete the bounded fixture".into(),
            effective_token_limit: Some(1000),
            ..Default::default()
        });
    }

    fn guarded(
        &self,
        task_id: &str,
        session_id: &str,
        epoch: i64,
        change: impl FnOnce(&mut RegistryState),
    ) -> GoalTransitionResult {
        let mut state = self.0.lock().unwrap();
        let Some(task) = &state.task else {
            return GoalTransitionResult::Missing;
        };
        if task.id != task_id
            || task.kind != TaskKind::Goal
            || task.session_id.as_deref() != Some(session_id)
            || task.execution_epoch != epoch
            || task.status.is_terminal()
        {
            return GoalTransitionResult::Stale;
        }
        change(&mut state);
        GoalTransitionResult::Applied
    }
}

#[async_trait]
impl GoalTaskRegistry for Registry {
    async fn record_control_intent(&self, intent: &GoalControlIntent) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        if state.fail_record {
            bail!("fixture intent record unavailable");
        }
        assert!(
            state
                .intents
                .iter()
                .all(|(row, _)| row.intent_id != intent.intent_id)
        );
        state.intents.push((intent.clone(), IntentState::Pending));
        Ok(())
    }

    async fn claim_control_intent(&self, id: &str, boot: &str) -> Result<GoalTransitionResult> {
        let mut state = self.0.lock().unwrap();
        if state.reject_claim {
            return Ok(GoalTransitionResult::Stale);
        }
        let Some((intent, disposition)) = state
            .intents
            .iter_mut()
            .find(|(row, _)| row.intent_id == id)
        else {
            return Ok(GoalTransitionResult::Missing);
        };
        if intent.owner_boot_id != boot || *disposition != IntentState::Pending {
            return Ok(GoalTransitionResult::Stale);
        }
        *disposition = IntentState::Claimed;
        Ok(GoalTransitionResult::Applied)
    }

    async fn settle_control_intent(
        &self,
        id: &str,
        settlement: GoalControlIntentSettlement,
    ) -> Result<GoalTransitionResult> {
        let (wait, entered) = {
            let state = self.0.lock().unwrap();
            (
                state.settlement_wait.clone(),
                state.settlement_entered.clone(),
            )
        };
        if let Some(wait) = wait {
            if let Some(entered) = entered {
                entered.add_permits(1);
            }
            wait.acquire().await?.forget();
        }
        let mut state = self.0.lock().unwrap();
        if state.fail_settlement {
            bail!("fixture intent settlement unavailable");
        }
        let Some((_, disposition)) = state
            .intents
            .iter_mut()
            .find(|(row, _)| row.intent_id == id)
        else {
            return Ok(GoalTransitionResult::Missing);
        };
        if matches!(disposition, IntentState::Settled(_))
            || (settlement == GoalControlIntentSettlement::Applied
                && *disposition != IntentState::Claimed)
        {
            return Ok(GoalTransitionResult::Stale);
        }
        *disposition = IntentState::Settled(settlement);
        Ok(GoalTransitionResult::Applied)
    }

    async fn latest_active_goal_for_agent(&self, _agent: &str) -> Result<Option<TaskRecord>> {
        bail!("fixture forbids observational lookup as control authority")
    }
    async fn latest_active_goal_for_context(
        &self,
        _agent: &str,
        _route: Option<&str>,
        _principal: Option<&str>,
    ) -> Result<Option<TaskRecord>> {
        bail!("fixture forbids observational lookup as control authority")
    }
    async fn latest_active_goal_id_for_context(
        &self,
        _agent: &str,
        _route: Option<&str>,
        _principal: Option<&str>,
    ) -> Result<Option<String>> {
        bail!("fixture forbids observational lookup as control authority")
    }
    async fn get_goal_task(&self, id: &str) -> Result<Option<GoalTaskRecord>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .goal
            .clone()
            .filter(|goal| goal.task_id == id))
    }
    async fn update_goal_limits(
        &self,
        _id: &str,
        _tokens: Option<u64>,
        _cost: Option<f64>,
    ) -> Result<()> {
        bail!("fixture requires session-guarded limits")
    }
    async fn update_goal_pause(&self, _id: &str, _pause: Option<GoalPauseState>) -> Result<()> {
        bail!("fixture requires session-guarded pause")
    }
    async fn set_continuation_context(
        &self,
        _id: &str,
        _context: Option<TaskContinuationContext>,
    ) -> Result<()> {
        bail!("fixture has no detached continuation")
    }
    async fn get_continuation_context(&self, _id: &str) -> Result<Option<TaskContinuationContext>> {
        Ok(None)
    }
    async fn current_goal_for_session(&self, session: &str) -> Result<Option<TaskRecord>> {
        let mut state = self.0.lock().unwrap();
        state.session_lookups += 1;
        Ok(state
            .task
            .clone()
            .filter(|task| task.session_id.as_deref() == Some(session)))
    }
    async fn terminal_reason_for_session_goal(
        &self,
        _id: &str,
        _session: &str,
    ) -> Result<Option<String>> {
        Ok(None)
    }
    async fn create_or_replace_session_goal(
        &self,
        task: TaskRecord,
        goal: GoalTaskRecord,
    ) -> Result<GoalTransitionResult> {
        let mut state = self.0.lock().unwrap();
        if state
            .task
            .as_ref()
            .is_some_and(|row| !row.status.is_terminal())
        {
            return Ok(GoalTransitionResult::Stale);
        }
        state.task = Some(task);
        state.goal = Some(goal);
        state.transitions.push("start");
        Ok(GoalTransitionResult::Applied)
    }
    async fn pause_session_goal(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        pause: GoalPauseState,
    ) -> Result<GoalTransitionResult> {
        Ok(self.guarded(id, session, epoch, |state| {
            let task = state.task.as_mut().unwrap();
            assert_eq!(task.status, TaskStatus::Running);
            task.status = TaskStatus::Paused;
            task.execution_epoch += 1;
            let goal = state.goal.as_mut().unwrap();
            goal.pause_reason = Some(pause.reason);
            goal.pause_description = pause.description;
            goal.blockers = pause.blockers;
            state.transitions.push("pause");
        }))
    }
    async fn resume_session_goal(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        pid: u32,
        boot: &str,
    ) -> Result<GoalTransitionResult> {
        Ok(self.guarded(id, session, epoch, |state| {
            let task = state.task.as_mut().unwrap();
            assert_eq!(task.status, TaskStatus::Paused);
            task.status = TaskStatus::Running;
            task.execution_epoch += 1;
            task.owner_pid = pid;
            task.owner_boot_id = boot.into();
            state.goal.as_mut().unwrap().pause_reason = None;
            state.transitions.push("resume");
        }))
    }
    async fn finish_session_goal(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        status: TaskStatus,
        _error: Option<String>,
    ) -> Result<GoalTransitionResult> {
        Ok(self.guarded(id, session, epoch, |state| {
            let task = state.task.as_mut().unwrap();
            task.status = status;
            task.execution_epoch += 1;
            state.transitions.push("finish");
        }))
    }
    async fn admit_pending_operation(
        &self,
        _id: &str,
        _session: &str,
        _epoch: i64,
        _pending: &str,
    ) -> Result<GoalTransitionResult> {
        bail!("fixture driver makes no physical provider attempt")
    }
    async fn settle_pending_operation(
        &self,
        _id: &str,
        _session: &str,
        _epoch: i64,
        _pending: &str,
        _accounting: GoalAccountingState,
    ) -> Result<GoalTransitionResult> {
        bail!("fixture driver makes no physical provider attempt")
    }
    async fn update_session_goal_limits(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        tokens: Option<u64>,
        cost: Option<f64>,
    ) -> Result<GoalTransitionResult> {
        Ok(self.guarded(id, session, epoch, |state| {
            let goal = state.goal.as_mut().unwrap();
            goal.effective_token_limit = tokens;
            goal.effective_cost_limit_usd = cost;
            state.transitions.push("budget");
        }))
    }
    async fn delete_session_goal(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
    ) -> Result<GoalTransitionResult> {
        let mut state = self.0.lock().unwrap();
        let Some(task) = state.task.as_ref() else {
            return Ok(GoalTransitionResult::Missing);
        };
        if task.id != id
            || task.session_id.as_deref() != Some(session)
            || task.execution_epoch != epoch
            || !task.status.is_terminal()
        {
            return Ok(GoalTransitionResult::Stale);
        }
        state.task = None;
        state.goal = None;
        Ok(GoalTransitionResult::Applied)
    }
}

/// Component fixtures exercise no physical provider attempts or usage ledger.
pub struct NoModelAccounting;
impl GoalOperationAccountingFactory for NoModelAccounting {
    fn for_scope(&self, _scope: GoalExecutionScope) -> Arc<dyn GoalOperationAccounting> {
        panic!("boundary-only test unexpectedly launched model execution")
    }
}
