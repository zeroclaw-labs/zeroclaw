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

#[derive(Debug)]
pub struct CleanupFault(pub &'static str);

impl std::fmt::Display for CleanupFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for CleanupFault {}

pub enum PauseWinner {
    Pause(GoalPauseState),
    Finish(TaskStatus),
    Delete,
}

#[derive(Default)]
pub struct RegistryState {
    pub task: Option<TaskRecord>,
    pub goal: Option<GoalTaskRecord>,
    pub intents: Vec<(GoalControlIntent, IntentState)>,
    pub reject_claim: bool,
    pub fail_record: bool,
    pub fail_settlement: bool,
    pub fail_finish: bool,
    pub fail_finish_once: bool,
    pub fail_restart_pause_once: bool,
    pub restart_pause_failed: Option<Arc<tokio::sync::Semaphore>>,
    pub fail_lookup_after_restart_pause: bool,
    pub lookup_failed: Option<Arc<tokio::sync::Semaphore>>,
    pub settlement_wait: Option<Arc<tokio::sync::Semaphore>>,
    pub settlement_entered: Option<Arc<tokio::sync::Semaphore>>,
    pub transitions: Vec<&'static str>,
    pub session_lookups: usize,
    pub fail_lookup_at: Option<usize>,
    pub fail_lookup: bool,
    pub lookup_failure_text: Option<&'static str>,
    pub panic_lookup_once: bool,
    pub lookup_wait_at: Option<usize>,
    pub lookup_wait: Option<Arc<tokio::sync::Semaphore>>,
    pub lookup_entered: Option<Arc<tokio::sync::Semaphore>>,
    pub gate_admitted_projection: bool,
    pub fail_pause: bool,
    pub pause_result: Option<GoalTransitionResult>,
    pub pause_winner: Option<PauseWinner>,
    pub pause_calls: usize,
    pub pause_failed: Option<Arc<tokio::sync::Semaphore>>,
    pub fail_delete: bool,
    pub delete_calls: usize,
    pub terminal_reason: Option<String>,
    pub pending_settlements: Vec<GoalAccountingState>,
    pub finish_calls: usize,
    pub finish_wait_at: Option<usize>,
    pub finish_wait: Option<Arc<tokio::sync::Semaphore>>,
    pub finish_entered: Option<Arc<tokio::sync::Semaphore>>,
    pub pending_admissions: usize,
    pub accounting_settlements: usize,
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
            return Err(anyhow::Error::new(CleanupFault(
                "fixture intent settlement unavailable",
            )));
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
        let (lookup_number, wait, entered, panic_lookup) = {
            let mut state = self.0.lock().unwrap();
            state.session_lookups += 1;
            let gated = state.lookup_wait_at == Some(state.session_lookups);
            (
                state.session_lookups,
                gated.then(|| state.lookup_wait.clone()).flatten(),
                gated.then(|| state.lookup_entered.clone()).flatten(),
                std::mem::take(&mut state.panic_lookup_once),
            )
        };
        // Panic outside the fixture mutex: cleanup-task failure must not also
        // poison the canonical store used by the following explicit retry.
        assert!(!panic_lookup, "fixture cleanup lookup panicked");
        if let Some(wait) = wait {
            if let Some(entered) = entered {
                entered.add_permits(1);
            }
            wait.acquire().await?.forget();
        }
        let state = self.0.lock().unwrap();
        if state.fail_lookup || state.fail_lookup_at == Some(lookup_number) {
            if let Some(signal) = &state.lookup_failed {
                signal.add_permits(1);
            }
            return Err(anyhow::Error::new(CleanupFault(
                state
                    .lookup_failure_text
                    .unwrap_or("fixture session lookup unavailable"),
            )));
        }
        Ok(state
            .task
            .clone()
            .filter(|task| task.session_id.as_deref() == Some(session)))
    }
    async fn terminal_reason_for_session_goal(
        &self,
        id: &str,
        session: &str,
    ) -> Result<Option<String>> {
        let state = self.0.lock().unwrap();
        Ok(state
            .task
            .as_ref()
            .filter(|task| task.id == id && task.session_id.as_deref() == Some(session))
            .and(state.terminal_reason.clone()))
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
        state.terminal_reason = None;
        state.transitions.push("start");
        if state.gate_admitted_projection {
            state.lookup_wait_at = Some(state.session_lookups + 1);
        }
        Ok(GoalTransitionResult::Applied)
    }
    async fn pause_session_goal(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        pause: GoalPauseState,
    ) -> Result<GoalTransitionResult> {
        let injected_winner = {
            let mut state = self.0.lock().unwrap();
            state.pause_calls += 1;
            if state.fail_pause {
                if let Some(signal) = &state.pause_failed {
                    signal.add_permits(1);
                }
                return Err(anyhow::Error::new(CleanupFault(
                    "fixture cleanup pause unavailable",
                )));
            }
            let winner = state.pause_winner.take();
            if winner.is_none()
                && let Some(result) = state.pause_result
            {
                return Ok(result);
            }
            if pause.reason == zeroclaw_goal::GoalPauseReason::DaemonRestart
                && std::mem::take(&mut state.fail_restart_pause_once)
            {
                if state.fail_lookup_after_restart_pause {
                    state.fail_lookup_at = Some(state.session_lookups + 1);
                }
                if let Some(signal) = &state.restart_pause_failed {
                    signal.add_permits(1);
                }
                bail!("fixture restart pause write failed");
            }
            winner.map(|winner| {
                (
                    winner,
                    state.pause_result.unwrap_or(GoalTransitionResult::Stale),
                )
            })
        };
        if let Some((winner, result)) = injected_winner {
            assert_eq!(
                self.guarded(id, session, epoch, |state| {
                    match winner {
                        PauseWinner::Pause(pause) => {
                            let task = state.task.as_mut().unwrap();
                            task.status = TaskStatus::Paused;
                            task.execution_epoch += 1;
                            let goal = state.goal.as_mut().unwrap();
                            goal.pause_reason = Some(pause.reason);
                            goal.pause_description = pause.description;
                            goal.blockers = pause.blockers;
                            state.transitions.push("pause");
                        }
                        PauseWinner::Finish(status) => {
                            let task = state.task.as_mut().unwrap();
                            task.status = status;
                            task.execution_epoch += 1;
                            state.terminal_reason = Some("fixture competing winner".into());
                            state.transitions.push("finish");
                        }
                        PauseWinner::Delete => {
                            state.task = None;
                            state.goal = None;
                            state.transitions.push("delete");
                        }
                    }
                }),
                GoalTransitionResult::Applied
            );
            return Ok(result);
        }
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
            if state.gate_admitted_projection {
                state.lookup_wait_at = Some(state.session_lookups + 1);
            }
        }))
    }
    async fn finish_session_goal(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        status: TaskStatus,
        error: Option<String>,
    ) -> Result<GoalTransitionResult> {
        let (wait, entered) = {
            let mut state = self.0.lock().unwrap();
            state.finish_calls += 1;
            let gated = state.finish_wait_at == Some(state.finish_calls);
            (
                gated.then(|| state.finish_wait.clone()).flatten(),
                gated.then(|| state.finish_entered.clone()).flatten(),
            )
        };
        if let Some(wait) = wait {
            if let Some(entered) = entered {
                entered.add_permits(1);
            }
            wait.acquire().await?.forget();
        }
        {
            let mut state = self.0.lock().unwrap();
            if state.fail_finish || std::mem::take(&mut state.fail_finish_once) {
                return Err(anyhow::Error::new(CleanupFault(
                    "fixture lifecycle settlement unavailable",
                )));
            }
        }
        Ok(self.guarded(id, session, epoch, |state| {
            let task = state.task.as_mut().unwrap();
            task.status = status;
            task.execution_epoch += 1;
            state.terminal_reason = error;
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
        self.0.lock().unwrap().pending_admissions += 1;
        bail!("fixture driver makes no physical provider attempt")
    }
    async fn settle_pending_operation(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        pending: &str,
        accounting: GoalAccountingState,
    ) -> Result<GoalTransitionResult> {
        let mut state = self.0.lock().unwrap();
        state.accounting_settlements += 1;
        let Some(task) = state.task.as_ref() else {
            return Ok(GoalTransitionResult::Missing);
        };
        if task.id != id
            || task.kind != TaskKind::Goal
            || task.session_id.as_deref() != Some(session)
        {
            return Ok(GoalTransitionResult::Stale);
        }
        let goal = state.goal.as_mut().unwrap();
        if goal.pending_call_id.is_none() {
            bail!("fixture driver makes no physical provider attempt");
        }
        if goal.pending_call_id.as_deref() != Some(pending)
            || goal.pending_call_epoch != Some(epoch)
        {
            return Ok(GoalTransitionResult::Stale);
        }
        goal.pending_call_id = None;
        goal.pending_call_epoch = None;
        goal.accounting_state = accounting;
        state.pending_settlements.push(accounting);
        Ok(GoalTransitionResult::Applied)
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
        state.delete_calls += 1;
        if state.fail_delete {
            return Err(anyhow::Error::new(CleanupFault(
                "fixture cleanup delete unavailable",
            )));
        }
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

#[tokio::test]
async fn competing_pause_winners_are_visible_after_a_rejected_transition() {
    let operator_pause = || GoalPauseState {
        reason: zeroclaw_goal::GoalPauseReason::OperatorPaused,
        description: None,
        blockers: vec![],
    };
    for rejected in [GoalTransitionResult::Stale, GoalTransitionResult::Missing] {
        for winner_status in [Some(TaskStatus::Paused), Some(TaskStatus::Completed), None] {
            let registry = Registry::default();
            registry.seed("fixture-session", TaskStatus::Running);
            {
                let mut state = registry.0.lock().unwrap();
                state.pause_result = Some(rejected);
                state.pause_winner = Some(match winner_status {
                    Some(TaskStatus::Paused) => PauseWinner::Pause(operator_pause()),
                    Some(status) => PauseWinner::Finish(status),
                    None => PauseWinner::Delete,
                });
            }
            assert_eq!(
                registry
                    .pause_session_goal("fixture-goal", "fixture-session", 1, operator_pause())
                    .await
                    .unwrap(),
                rejected
            );
            let state = registry.0.lock().unwrap();
            assert_eq!(state.task.as_ref().map(|task| task.status), winner_status);
            if let Some(task) = &state.task {
                assert_eq!(task.execution_epoch, 2);
                if task.status == TaskStatus::Paused {
                    assert_eq!(
                        state.goal.as_ref().unwrap().pause_reason,
                        Some(operator_pause().reason)
                    );
                    assert_eq!(state.transitions, ["pause"]);
                } else {
                    assert_eq!(state.transitions, ["finish"]);
                    assert_eq!(
                        state.terminal_reason.as_deref(),
                        Some("fixture competing winner")
                    );
                }
            } else {
                assert!(state.goal.is_none());
                assert_eq!(state.transitions, ["delete"]);
            }
            assert_eq!(state.pause_calls, 1);
            assert!(state.pause_winner.is_none());
            assert_eq!(state.pending_admissions, 0);
            assert_eq!(state.accounting_settlements, 0);
        }
    }
}

/// Component fixtures exercise no physical provider attempts or usage ledger.
pub struct NoModelAccounting;
impl GoalOperationAccountingFactory for NoModelAccounting {
    fn for_scope(&self, _scope: GoalExecutionScope) -> Arc<dyn GoalOperationAccounting> {
        panic!("boundary-only test unexpectedly launched model execution")
    }
}
