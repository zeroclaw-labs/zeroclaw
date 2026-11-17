use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use std::{collections::VecDeque, path::PathBuf, sync::Mutex as StdMutex};
use tempfile::TempDir;
use tokio::{
    sync::Notify,
    time::{Duration, timeout},
};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_commands::goal::GoalCommand;
use zeroclaw_config::{
    api_error::{ConfigApiCode, ConfigApiError},
    cost::CostTracker,
    goal::{GoalBudgetLimits as ConfigGoalBudgetLimits, GoalConfig},
    live::LiveConfig,
    providers::ModelProviderRef,
    schema::{Config, OpenAIModelProviderConfig},
};
use zeroclaw_goal::accounting::{
    GoalAccountingFailure, GoalOperationAccounting, GoalOperationAccountingFactory,
    GoalOperationRequest, GoalOperationSettlement,
};
use zeroclaw_goal::{
    GoalController, GoalExecutionHost, GoalExecutionRestartCoordinator, GoalExecutionScope,
    GoalExecutionSupervisor, GoalHostSettings, GoalHostSettingsSource, GoalIngressContext,
    GoalIngressPrincipal, GoalParentTurn, GoalParentTurnCheckpoint, GoalPauseReason, GoalResponse,
    GoalRuntime, GoalSessionBinding, GoalSessionDriver, GoalSessionExecutionLease, GoalSessionKey,
    GoalSessionLease, GoalVerifierResult, GoalVerifierTurn,
};
use zeroclaw_infra::{session_backend::SessionBackend, session_sqlite::SqliteSessionBackend};
use zeroclaw_runtime::control_plane::{
    GoalAccountingState, GoalPauseState, GoalTaskRecord, GoalTaskRegistry, GoalTransitionResult,
    SqliteTaskStore, TaskContinuationContext, TaskRecord, TaskStatus,
};
use zeroclaw_runtime::goal_accounting::{
    ModelProviderGoalUsagePricer, RuntimeGoalOperationAccountingFactory,
};

struct RecordingDriver {
    binding: GoalSessionBinding,
    binds: AtomicUsize,
    execution_acquires: AtomicUsize,
}

struct LaunchRebindFailureDriver {
    inner: Arc<RecordingDriver>,
    refuse_bind: AtomicBool,
}

#[async_trait]
impl GoalSessionDriver for LaunchRebindFailureDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.inner.session_key()
    }

    async fn bind(&self, ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        if self.refuse_bind.load(Ordering::SeqCst) {
            anyhow::bail!("session binding revoked after acknowledgement");
        }
        self.inner.bind(ingress).await
    }

    async fn acquire_execution(
        &self,
        ingress: &GoalIngressContext,
        scope: &GoalExecutionScope,
        checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        self.inner
            .acquire_execution(ingress, scope, checkpoint)
            .await
    }
}

struct RecordingExecutionLease {
    session_key: GoalSessionKey,
    delivered: Arc<AtomicUsize>,
}

#[async_trait]
impl GoalSessionExecutionLease for RecordingExecutionLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        Ok(Vec::new())
    }

    async fn run_parent_turn(
        &mut self,
        _scope: &GoalExecutionScope,
        turn: GoalParentTurn,
    ) -> anyhow::Result<String> {
        Ok(format!("parent:{}", turn.objective))
    }

    async fn run_verifier(
        &mut self,
        _scope: &GoalExecutionScope,
        turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        Ok(GoalVerifierResult {
            response: format!("verifier:{}", turn.candidate),
            attribution: None,
        })
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
        self.delivered.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct ExecutionDriver {
    binding: GoalSessionBinding,
    execution_acquires: AtomicUsize,
    delivered: Arc<AtomicUsize>,
}

struct PausingExecutionLease {
    session_key: GoalSessionKey,
    parent_started: Arc<Notify>,
    release_parent: Arc<Notify>,
    verifier_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl GoalSessionExecutionLease for PausingExecutionLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        Ok(Vec::new())
    }

    async fn run_parent_turn(
        &mut self,
        _operation: &GoalExecutionScope,
        _turn: GoalParentTurn,
    ) -> anyhow::Result<String> {
        self.parent_started.notify_one();
        self.release_parent.notified().await;
        Ok("candidate that settled before pause".to_owned())
    }

    async fn run_verifier(
        &mut self,
        _operation: &GoalExecutionScope,
        _turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        self.verifier_calls.fetch_add(1, Ordering::SeqCst);
        Ok(GoalVerifierResult {
            response: r#"{"decision":"complete","reason":"done","blockers":[]}"#.to_owned(),
            attribution: None,
        })
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
        Ok(())
    }
}

struct PausingExecutionDriver {
    binding: GoalSessionBinding,
    execution_acquires: AtomicUsize,
    parent_started: Arc<Notify>,
    release_parent: Arc<Notify>,
    verifier_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl GoalSessionDriver for PausingExecutionDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        self.execution_acquires.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(PausingExecutionLease {
            session_key: self.binding.session_key().clone(),
            parent_started: Arc::clone(&self.parent_started),
            release_parent: Arc::clone(&self.release_parent),
            verifier_calls: Arc::clone(&self.verifier_calls),
        }))
    }
}

struct UserInputExecutionDriver {
    binding: GoalSessionBinding,
    events: Arc<Mutex<Vec<&'static str>>>,
    candidates: Arc<Mutex<Vec<String>>>,
}

struct UserInputExecutionLease {
    session_key: GoalSessionKey,
    events: Arc<Mutex<Vec<&'static str>>>,
    candidates: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl GoalSessionExecutionLease for UserInputExecutionLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        Ok(Vec::new())
    }

    async fn run_parent_turn(
        &mut self,
        _scope: &GoalExecutionScope,
        _turn: GoalParentTurn,
    ) -> anyhow::Result<String> {
        self.events.lock().unwrap().push("parent");
        Ok("Which option should I use?".to_owned())
    }

    async fn run_verifier(
        &mut self,
        _scope: &GoalExecutionScope,
        _turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        self.events.lock().unwrap().push("verifier");
        Ok(GoalVerifierResult {
            response: r#"{"decision":"blocked","reason":"A user choice is required","blockers":[{"kind":"needs_user_input","message":"Which option should I use?"}]}"#.to_owned(),
            attribution: None,
        })
    }

    async fn append_parent_candidate(&mut self, candidate: String) -> anyhow::Result<()> {
        self.events.lock().unwrap().push("candidate");
        self.candidates.lock().unwrap().push(candidate);
        Ok(())
    }
}

#[async_trait]
impl GoalSessionDriver for UserInputExecutionDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        Ok(Box::new(UserInputExecutionLease {
            session_key: self.binding.session_key().clone(),
            events: Arc::clone(&self.events),
            candidates: Arc::clone(&self.candidates),
        }))
    }
}

#[async_trait]
impl GoalSessionDriver for ExecutionDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        self.execution_acquires.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(RecordingExecutionLease {
            session_key: self.binding.session_key().clone(),
            delivered: self.delivered.clone(),
        }))
    }
}

struct WrongExecutionLeaseDriver {
    binding: GoalSessionBinding,
    returned_session_key: GoalSessionKey,
    execution_acquires: AtomicUsize,
}

#[async_trait]
impl GoalSessionDriver for WrongExecutionLeaseDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        self.execution_acquires.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(RecordingExecutionLease {
            session_key: self.returned_session_key.clone(),
            delivered: Arc::new(AtomicUsize::new(0)),
        }))
    }
}

struct WrongBindingDriver {
    advertised: GoalSessionKey,
    returned_binding: GoalSessionBinding,
    binds: AtomicUsize,
}

#[async_trait]
impl GoalSessionDriver for WrongBindingDriver {
    fn session_key(&self) -> &GoalSessionKey {
        &self.advertised
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        self.binds.fetch_add(1, Ordering::SeqCst);
        Ok(GoalSessionLease::new(self.returned_binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        anyhow::bail!("wrong binding driver has no execution lease")
    }
}

#[async_trait]
impl GoalSessionDriver for RecordingDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        self.binds.fetch_add(1, Ordering::SeqCst);
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &zeroclaw_goal::GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn zeroclaw_goal::GoalSessionExecutionLease>> {
        self.execution_acquires.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("recording driver has no execution lease")
    }
}

struct LeaseDriver {
    binding: GoalSessionBinding,
    available_for_reconnect: Arc<AtomicUsize>,
}

impl LeaseDriver {
    fn reconnect_is_allowed(&self) -> bool {
        self.available_for_reconnect.load(Ordering::SeqCst) == 1
    }

    fn acquire_reconnect_lease(&self) -> anyhow::Result<ReconnectLease> {
        if self
            .available_for_reconnect
            .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            anyhow::bail!("session replacement is already in progress");
        }
        Ok(ReconnectLease {
            session_key: self.binding.session_key().clone(),
            available_for_reconnect: self.available_for_reconnect.clone(),
        })
    }
}

struct ReconnectLease {
    session_key: GoalSessionKey,
    available_for_reconnect: Arc<AtomicUsize>,
}

impl Drop for ReconnectLease {
    fn drop(&mut self) {
        self.available_for_reconnect.store(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl GoalSessionExecutionLease for ReconnectLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        Ok(Vec::new())
    }

    async fn run_parent_turn(
        &mut self,
        _operation: &GoalExecutionScope,
        _turn: GoalParentTurn,
    ) -> anyhow::Result<String> {
        anyhow::bail!("lease execution test does not run parent turns")
    }

    async fn run_verifier(
        &mut self,
        _operation: &GoalExecutionScope,
        _turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        anyhow::bail!("lease execution test does not run verifier turns")
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
        anyhow::bail!("lease execution test does not deliver candidates")
    }
}

#[async_trait]
impl GoalSessionDriver for LeaseDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(
            self.binding.clone(),
            self.acquire_reconnect_lease()?,
        ))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &zeroclaw_goal::GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn zeroclaw_goal::GoalSessionExecutionLease>> {
        Ok(Box::new(self.acquire_reconnect_lease()?))
    }
}

struct LeaseObservingRegistry {
    task: TaskRecord,
    goal: GoalTaskRecord,
    reconnect_available: Arc<AtomicUsize>,
    pause_observed: AtomicUsize,
    replacement_after_missing_goal: Option<TaskRecord>,
    missing_goal_observed: AtomicBool,
    task_after_goal_read: Option<TaskRecord>,
    goal_after_goal_read: Option<GoalTaskRecord>,
    goal_read_observed: AtomicBool,
}

impl LeaseObservingRegistry {
    fn running_for(ingress: &GoalIngressContext, reconnect_available: Arc<AtomicUsize>) -> Self {
        let task_id = "goal-under-lease".to_owned();
        Self {
            task: TaskRecord {
                id: task_id.clone(),
                kind: zeroclaw_runtime::control_plane::TaskKind::Goal,
                agent: ingress.agent().to_owned(),
                status: TaskStatus::Running,
                owner_pid: 42,
                owner_boot_id: "test-boot".to_owned(),
                heartbeat_at: None,
                depth: 0,
                parent_id: None,
                originator_route: Some(ingress.route().to_owned()),
                originator_chain: Vec::new(),
                delivered: false,
                idem_key: None,
                principal_id: Some("@alice:example.org".to_owned()),
                session_id: Some(ingress.session_key().durable_id()),
                execution_epoch: 1,
                started_at: "2026-09-02T00:00:00Z".to_owned(),
                finished_at: None,
            },
            goal: GoalTaskRecord {
                task_id,
                ..GoalTaskRecord::default()
            },
            reconnect_available,
            pause_observed: AtomicUsize::new(0),
            replacement_after_missing_goal: None,
            missing_goal_observed: AtomicBool::new(false),
            task_after_goal_read: None,
            goal_after_goal_read: None,
            goal_read_observed: AtomicBool::new(false),
        }
    }

    fn replaced_before_goal_read(ingress: &GoalIngressContext) -> Self {
        let reconnect_available = Arc::new(AtomicUsize::new(1));
        let mut registry = Self::running_for(ingress, reconnect_available);
        let mut replacement = registry.task.clone();
        replacement.id = "replacement-goal".to_owned();
        registry.replacement_after_missing_goal = Some(replacement);
        registry
    }

    fn paused_with_change_after_goal_read(
        ingress: &GoalIngressContext,
        changed_status: TaskStatus,
        changed_epoch: i64,
    ) -> Self {
        let mut registry = Self::running_for(ingress, Arc::new(AtomicUsize::new(1)));
        registry.task.status = TaskStatus::Paused;
        registry.goal.pause_reason = Some(GoalPauseReason::OperatorPaused);
        registry.goal.pause_description = Some("paused by operator".to_owned());
        let mut changed_task = registry.task.clone();
        changed_task.status = changed_status;
        changed_task.execution_epoch = changed_epoch;
        if changed_status == TaskStatus::Running {
            let mut resumed_goal = registry.goal.clone();
            resumed_goal.pause_reason = None;
            resumed_goal.pause_description = None;
            resumed_goal.blockers.clear();
            registry.goal_after_goal_read = Some(resumed_goal);
        }
        registry.task_after_goal_read = Some(changed_task);
        registry
    }

    fn assert_lease_is_held(&self) {
        assert_eq!(
            self.reconnect_available.load(Ordering::SeqCst),
            0,
            "the session lease must remain held during the durable transition"
        );
    }
}

#[async_trait]
impl GoalTaskRegistry for LeaseObservingRegistry {
    async fn begin_operation_dispatch(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
        _pending_call_id: &str,
    ) -> anyhow::Result<Option<zeroclaw_goal::task::GoalDispatchStart>> {
        panic!("unexpected dispatch in lifecycle-only registry")
    }

    async fn latest_active_goal_for_agent(
        &self,
        _agent: &str,
    ) -> anyhow::Result<Option<TaskRecord>> {
        panic!("unexpected legacy agent lookup")
    }

    async fn latest_active_goal_for_context(
        &self,
        _agent: &str,
        _originator_route: Option<&str>,
        _principal_id: Option<&str>,
    ) -> anyhow::Result<Option<TaskRecord>> {
        panic!("unexpected legacy context lookup")
    }

    async fn latest_active_goal_id_for_context(
        &self,
        _agent: &str,
        _originator_route: Option<&str>,
        _principal_id: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        panic!("unexpected legacy context-id lookup")
    }

    async fn get_goal_task(&self, task_id: &str) -> anyhow::Result<Option<GoalTaskRecord>> {
        assert_eq!(task_id, self.task.id);
        if self.replacement_after_missing_goal.is_some() {
            self.missing_goal_observed.store(true, Ordering::SeqCst);
            return Ok(None);
        }
        self.goal_read_observed.store(true, Ordering::SeqCst);
        Ok(Some(
            self.goal_after_goal_read
                .as_ref()
                .cloned()
                .unwrap_or_else(|| self.goal.clone()),
        ))
    }

    async fn update_goal_limits(
        &self,
        _task_id: &str,
        _token_limit: Option<u64>,
        _cost_limit_usd: Option<f64>,
    ) -> anyhow::Result<()> {
        panic!("unexpected legacy limits update")
    }

    async fn update_goal_pause(
        &self,
        _task_id: &str,
        _pause: Option<GoalPauseState>,
    ) -> anyhow::Result<()> {
        panic!("unexpected legacy pause update")
    }

    async fn set_continuation_context(
        &self,
        _task_id: &str,
        _context: Option<TaskContinuationContext>,
    ) -> anyhow::Result<()> {
        panic!("unexpected continuation update")
    }

    async fn get_continuation_context(
        &self,
        _task_id: &str,
    ) -> anyhow::Result<Option<TaskContinuationContext>> {
        panic!("unexpected continuation lookup")
    }

    async fn current_goal_for_session(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<TaskRecord>> {
        assert_eq!(self.task.session_id.as_deref(), Some(session_id));
        Ok(Some(if self.goal_read_observed.load(Ordering::SeqCst) {
            self.task_after_goal_read
                .as_ref()
                .cloned()
                .unwrap_or_else(|| self.task.clone())
        } else {
            self.replacement_after_missing_goal
                .as_ref()
                .filter(|_| self.missing_goal_observed.load(Ordering::SeqCst))
                .cloned()
                .unwrap_or_else(|| self.task.clone())
        }))
    }

    async fn terminal_reason_for_session_goal(
        &self,
        task_id: &str,
        session_id: &str,
    ) -> anyhow::Result<Option<String>> {
        assert_eq!(task_id, self.task.id);
        assert_eq!(self.task.session_id.as_deref(), Some(session_id));
        Ok(None)
    }

    async fn create_or_replace_session_goal(
        &self,
        _task: TaskRecord,
        _goal: GoalTaskRecord,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected create-or-replace")
    }

    async fn pause_session_goal(
        &self,
        task_id: &str,
        session_id: &str,
        expected_epoch: i64,
        _pause: GoalPauseState,
    ) -> anyhow::Result<GoalTransitionResult> {
        assert_eq!(task_id, self.task.id);
        assert_eq!(self.task.session_id.as_deref(), Some(session_id));
        assert_eq!(expected_epoch, self.task.execution_epoch);
        self.assert_lease_is_held();
        self.pause_observed.fetch_add(1, Ordering::SeqCst);
        Ok(GoalTransitionResult::Applied)
    }

    async fn resume_session_goal(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
        _owner_pid: u32,
        _owner_boot_id: &str,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected resume")
    }

    async fn finish_session_goal(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
        _status: TaskStatus,
        _error: Option<String>,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected finish")
    }

    async fn admit_pending_operation(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
        _operation_id: &str,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected operation admission")
    }

    async fn settle_pending_operation(
        &self,
        _task_id: &str,
        _session_id: &str,
        _admitted_epoch: i64,
        _operation_id: &str,
        _accounting_state: GoalAccountingState,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected operation settlement")
    }

    async fn update_session_goal_limits(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
        _token_limit: Option<u64>,
        _cost_limit_usd: Option<f64>,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected session limits update")
    }

    async fn delete_session_goal(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected goal deletion")
    }
}

fn matrix_ingress() -> GoalIngressContext {
    GoalIngressContext::trusted(
        GoalSessionKey::matrix("matrix_room__room_example_org__alice_example_org").unwrap(),
        "main",
        "matrix:primary",
        GoalIngressPrincipal::Matrix {
            raw_mxid: "@alice:example.org".into(),
        },
    )
    .unwrap()
}

fn zerocode_ingress(agent: &str) -> GoalIngressContext {
    GoalIngressContext::trusted(
        GoalSessionKey::zero_code("same-live-session").unwrap(),
        agent,
        "zerocode:local",
        GoalIngressPrincipal::ZeroCode {
            tui_id: "current-tui-connection".into(),
        },
    )
    .unwrap()
}

fn goal_config(enabled: bool, verifier: &str, limits: ConfigGoalBudgetLimits) -> Config {
    let mut config = Config {
        goal: GoalConfig {
            enabled,
            // A normalized `None` means explicitly unlimited in these legacy
            // fixtures, not an unconfigured default.
            default_token_limit: Some(limits.token_limit.unwrap_or(0)),
            default_cost_limit_usd: Some(limits.cost_limit_usd.unwrap_or(0.0)),
            verifier: zeroclaw_config::goal::GoalVerifierConfig {
                model_provider: ModelProviderRef::new(verifier),
                model: None,
            },
        },
        ..Default::default()
    };
    config.providers.models.openai.insert(
        "default".to_owned(),
        OpenAIModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                model: Some("gpt-test".to_owned()),
                ..Default::default()
            },
        },
    );
    config
}

#[test]
fn goal_config_fixture_resolves_the_configured_openai_model() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    assert!(config.validate_goal_config().is_ok());
}

fn valid_goal_config(enabled: bool) -> Config {
    goal_config(
        enabled,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    )
}

fn host_settings(config: &Config) -> GoalHostSettings<'_> {
    GoalHostSettings::from_config(config, 42, "test-boot").unwrap()
}

fn assert_admission_error(error: anyhow::Error, code: ConfigApiCode, path: &str) {
    assert_eq!(
        error.to_string(),
        "Goal configuration does not admit execution"
    );
    let structured = ConfigApiError::from_validation(error);
    assert_eq!(structured.code, code);
    assert_eq!(structured.path.as_deref(), Some(path));
}

#[tokio::test]
async fn host_rejects_missing_verifier_before_binding_start_or_resume() {
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();

    for verifier in ["", "   "] {
        let config = goal_config(
            true,
            verifier,
            ConfigGoalBudgetLimits {
                token_limit: Some(100),
                cost_limit_usd: Some(1.0),
            },
        );
        for command in [
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            },
            GoalCommand::Resume { response: None },
        ] {
            let error = host
                .submit(
                    &GoalHostSettings::from_config(&config, 42, "test-boot").unwrap(),
                    ingress.clone(),
                    driver.clone(),
                    command,
                )
                .await
                .unwrap_err();
            assert_admission_error(
                error,
                ConfigApiCode::RequiredFieldEmpty,
                "goal.verifier.model_provider",
            );
        }
    }

    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn host_rejects_malformed_and_dangling_verifiers_before_binding() {
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();
    for (verifier, code) in [
        ("openai.missing", ConfigApiCode::DanglingReference),
        ("unknownfamily.default", ConfigApiCode::DanglingReference),
        ("openai", ConfigApiCode::InvalidFormat),
        (".default", ConfigApiCode::InvalidFormat),
        ("openai.", ConfigApiCode::InvalidFormat),
    ] {
        let config = goal_config(
            true,
            verifier,
            ConfigGoalBudgetLimits {
                token_limit: Some(100),
                cost_limit_usd: Some(1.0),
            },
        );
        let error = host
            .submit(
                &GoalHostSettings::from_config(&config, 42, "test-boot").unwrap(),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "finish the task".into(),
                },
            )
            .await
            .unwrap_err();
        assert_admission_error(error, code, "goal.verifier.model_provider");
    }
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn controller_rechecks_invalid_config_without_replacing_terminal_predecessor() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();
    let valid = valid_goal_config(true);

    let start = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "preserve the terminal predecessor".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&valid), &start)
        .await
        .unwrap()
    else {
        panic!("expected initial Goal to start");
    };
    let cancel = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&valid), &cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));
    let predecessor = store
        .current_goal_for_session(&ingress.session_key().durable_id())
        .await
        .unwrap()
        .expect("cancelled predecessor remains current");
    assert_eq!(predecessor.id, started.task_id);
    assert_eq!(predecessor.status, TaskStatus::Cancelled);

    let attempted_start = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "must not replace predecessor".into(),
            },
        )
        .await
        .unwrap();
    let invalid_config = goal_config(
        true,
        "openai.missing",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let invalid = GoalHostSettings::from_config(&invalid_config, 42, "test-boot").unwrap();
    assert_admission_error(
        controller
            .submit(&invalid, &attempted_start)
            .await
            .unwrap_err(),
        ConfigApiCode::DanglingReference,
        "goal.verifier.model_provider",
    );
    let unchanged = store
        .current_goal_for_session(&ingress.session_key().durable_id())
        .await
        .unwrap()
        .expect("failed controller admission must retain predecessor");
    assert_eq!(unchanged.id, predecessor.id);
    assert_eq!(unchanged.status, TaskStatus::Cancelled);
    assert_eq!(unchanged.execution_epoch, predecessor.execution_epoch);
}

#[tokio::test]
async fn acquisition_rechecks_current_config_and_restored_config_can_acquire() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = Arc::new(ExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        delivered: Arc::new(AtomicUsize::new(0)),
    });
    let host = GoalExecutionHost::new();
    let valid = valid_goal_config(true);
    let start = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "acquire only while the verifier remains configured".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&valid), &start)
        .await
        .unwrap()
    else {
        panic!("expected Goal to start under valid config");
    };
    let scope = GoalExecutionScope::new(
        started.task_id.as_str(),
        ingress.session_key().durable_id(),
        started.execution_epoch,
    )
    .unwrap();
    let invalid_config = goal_config(
        true,
        "openai.missing",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let invalid = GoalHostSettings::from_config(&invalid_config, 42, "test-boot").unwrap();
    assert_admission_error(
        match host
            .acquire_execution(&invalid, start, &scope, &GoalParentTurnCheckpoint::new())
            .await
        {
            Ok(_) => panic!("invalid current config must not acquire execution"),
            Err(error) => error,
        },
        ConfigApiCode::DanglingReference,
        "goal.verifier.model_provider",
    );
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);

    let restored = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let fresh_submission = host
        .submit(
            &host_settings(&restored),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    drop(
        host.acquire_execution(
            &host_settings(&restored),
            fresh_submission,
            &scope,
            &GoalParentTurnCheckpoint::new(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_verifier_preserves_admin_controls_and_rejected_resume_epoch() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();
    let valid = valid_goal_config(true);
    let start = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "remain paused until valid config returns".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(_) = controller
        .submit(&host_settings(&valid), &start)
        .await
        .unwrap()
    else {
        panic!("expected Goal start");
    };
    let pause = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap();
    let GoalResponse::Paused(paused) = controller
        .submit(&host_settings(&valid), &pause)
        .await
        .unwrap()
    else {
        panic!("expected Goal pause");
    };

    let invalid_config = goal_config(
        true,
        "",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let resume = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    assert_admission_error(
        controller
            .submit(
                &GoalHostSettings::from_config(&invalid_config, 42, "test-boot").unwrap(),
                &resume,
            )
            .await
            .unwrap_err(),
        ConfigApiCode::RequiredFieldEmpty,
        "goal.verifier.model_provider",
    );
    let after_rejection = store
        .current_goal_for_session(&ingress.session_key().durable_id())
        .await
        .unwrap()
        .expect("rejected resume leaves the paused task current");
    assert_eq!(after_rejection.status, TaskStatus::Paused);
    assert_eq!(after_rejection.execution_epoch, paused.execution_epoch);

    let invalid_limits = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(i64::MAX as u64 + 1),
            cost_limit_usd: Some(1.0),
        },
    );
    let status = host
        .submit(
            &GoalHostSettings::from_config(&invalid_limits, 42, "test-boot").unwrap(),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    let GoalResponse::Status(status) = controller
        .submit(
            &GoalHostSettings::from_config(&invalid_limits, 42, "test-boot").unwrap(),
            &status,
        )
        .await
        .unwrap()
    else {
        panic!("invalid defaults must not block Status");
    };
    assert_eq!(status.status, TaskStatus::Paused);
    assert_eq!(status.execution_epoch, paused.execution_epoch);

    let commands = [
        (GoalCommand::Status, 0),
        (GoalCommand::Budget, 1),
        (
            GoalCommand::SetBudget(zeroclaw_commands::goal::GoalBudgetSelection::Limits(
                zeroclaw_commands::goal::GoalBudgetLimits {
                    token_limit: Some(12),
                    cost_limit_usd: None,
                },
            )),
            2,
        ),
        (GoalCommand::Pause { immediate: false }, 3),
        (GoalCommand::Cancel, 4),
        (GoalCommand::Help, 5),
    ];
    for (command, expected) in commands {
        let submission = host
            .submit(
                &GoalHostSettings::from_config(&invalid_config, 42, "test-boot").unwrap(),
                ingress.clone(),
                driver.clone(),
                command,
            )
            .await
            .unwrap();
        let response = controller
            .submit(
                &GoalHostSettings::from_config(&invalid_config, 42, "test-boot").unwrap(),
                &submission,
            )
            .await
            .unwrap();
        assert!(
            matches!(
                (&response, expected),
                (GoalResponse::Status(_), 0)
                    | (GoalResponse::Budget(_), 1)
                    | (GoalResponse::BudgetUpdated(_), 2)
                    | (GoalResponse::AlreadyPaused(_), 3)
                    | (GoalResponse::Cancelled(_), 4)
                    | (GoalResponse::Help, 5)
            ),
            "unexpected response for command {expected}: {response:?}"
        );
    }
    assert_eq!(driver.binds.load(Ordering::SeqCst), 9);
}

#[tokio::test]
async fn disabled_config_keeps_invalid_verifier_nonblocking() {
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();
    for verifier in ["openai.missing", ""] {
        let config = goal_config(
            false,
            verifier,
            ConfigGoalBudgetLimits {
                token_limit: Some(100),
                cost_limit_usd: Some(1.0),
            },
        );
        assert!(config.validate().is_ok());
        let submission = host
            .submit(
                &GoalHostSettings::from_config(&config, 42, "test-boot").unwrap(),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Status,
            )
            .await
            .unwrap();
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
        assert!(matches!(
            controller
                .submit(
                    &GoalHostSettings::from_config(&config, 42, "test-boot").unwrap(),
                    &submission,
                )
                .await
                .unwrap(),
            GoalResponse::Disabled
        ));
        let scope =
            GoalExecutionScope::new("goal-1", ingress.session_key().durable_id(), 1).unwrap();
        let error = match host
            .acquire_execution(
                &GoalHostSettings::from_config(&config, 42, "test-boot").unwrap(),
                submission,
                &scope,
                &GoalParentTurnCheckpoint::new(),
            )
            .await
        {
            Ok(_) => panic!("disabled Goal Mode must not acquire execution"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("disabled"));
    }
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_unlimited_goal_defaults_remain_unlimited() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(0),
            cost_limit_usd: Some(0.0),
        },
    );
    let ingress = matrix_ingress();
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            recording_driver(&matrix_ingress()),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "use explicitly unlimited defaults".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&config), &submission)
        .await
        .unwrap()
    else {
        panic!("expected Goal start with unlimited defaults");
    };
    assert_eq!(started.token_limit, None);
    assert_eq!(started.cost_limit_usd, None);
}

#[tokio::test]
async fn start_without_budget_flags_or_defaults_returns_budget_required() {
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let session_id = ingress.session_key().durable_id();
        let driver = recording_driver(&ingress);
        let mut config = valid_goal_config(true);
        config.goal.default_token_limit = None;
        config.goal.default_cost_limit_usd = None;
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let submission = GoalExecutionHost::new()
            .submit(
                &host_settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "start without a configured budget".into(),
                },
            )
            .await
            .unwrap();

        assert!(matches!(
            controller
                .submit(&host_settings(&config), &submission)
                .await
                .unwrap(),
            GoalResponse::BudgetRequired
        ));
        assert!(
            store
                .current_goal_for_session(&session_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn goal_budget_value_has_one_type_across_public_paths() {
    let configured = ConfigGoalBudgetLimits {
        token_limit: Some(10),
        cost_limit_usd: Some(2.5),
    };
    let command: zeroclaw_commands::goal::GoalBudgetLimits = configured;
    let shared: zeroclaw_api::goal::GoalBudgetLimits = command;
    assert_eq!(shared, configured);
}

#[tokio::test]
async fn explicit_command_budgets_start_without_configured_defaults() {
    use zeroclaw_commands::goal::{GoalBudgetLimits, GoalBudgetSelection};

    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        for (budget, expected_tokens, expected_cost) in [
            (
                GoalBudgetSelection::Limits(GoalBudgetLimits {
                    token_limit: Some(10),
                    cost_limit_usd: None,
                }),
                Some(10),
                None,
            ),
            (
                GoalBudgetSelection::Limits(GoalBudgetLimits {
                    token_limit: None,
                    cost_limit_usd: Some(2.5),
                }),
                None,
                Some(2.5),
            ),
            (
                GoalBudgetSelection::Limits(GoalBudgetLimits {
                    token_limit: Some(10),
                    cost_limit_usd: Some(2.5),
                }),
                Some(10),
                Some(2.5),
            ),
            (GoalBudgetSelection::Unlimited, None, None),
        ] {
            let mut config = valid_goal_config(true);
            config.goal.default_token_limit = None;
            config.goal.default_cost_limit_usd = None;
            let driver = recording_driver(&ingress);
            let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
            let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
            let submission = GoalExecutionHost::new()
                .submit(
                    &host_settings(&config),
                    ingress.clone(),
                    driver,
                    GoalCommand::Start {
                        budget,
                        objective: "start with the explicit command budget".into(),
                    },
                )
                .await
                .unwrap();
            let GoalResponse::Started(started) = controller
                .submit(&host_settings(&config), &submission)
                .await
                .unwrap()
            else {
                panic!("expected explicit command budget to admit Goal start");
            };
            assert_eq!(started.token_limit, expected_tokens);
            assert_eq!(started.cost_limit_usd, expected_cost);
        }
    }
}

#[tokio::test]
async fn single_positive_default_starts_with_that_limit() {
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let mut config = goal_config(
            true,
            "openai.default",
            ConfigGoalBudgetLimits {
                token_limit: Some(500),
                cost_limit_usd: None,
            },
        );
        config.goal.default_cost_limit_usd = None;
        let driver = recording_driver(&ingress);
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
        let submission = GoalExecutionHost::new()
            .submit(
                &host_settings(&config),
                ingress,
                driver,
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "use the single configured token limit".into(),
                },
            )
            .await
            .unwrap();
        let GoalResponse::Started(started) = controller
            .submit(&host_settings(&config), &submission)
            .await
            .unwrap()
        else {
            panic!("expected positive token default to admit Goal start");
        };
        assert_eq!(started.token_limit, Some(500));
        assert_eq!(started.cost_limit_usd, None);
    }
}

#[tokio::test]
async fn zero_token_default_without_cost_default_returns_budget_required() {
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let session_id = ingress.session_key().durable_id();
        let mut config = goal_config(
            true,
            "openai.default",
            ConfigGoalBudgetLimits {
                token_limit: Some(0),
                cost_limit_usd: None,
            },
        );
        config.goal.default_cost_limit_usd = None;
        let driver = recording_driver(&ingress);
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let submission = GoalExecutionHost::new()
            .submit(
                &host_settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "do not treat a zero-only default as a budget".into(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            controller
                .submit(&host_settings(&config), &submission)
                .await
                .unwrap(),
            GoalResponse::BudgetRequired
        ));
        assert!(
            store
                .current_goal_for_session(&session_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn invalid_defaults_reject_explicit_budget_start_with_typed_path() {
    use zeroclaw_commands::goal::{GoalBudgetLimits, GoalBudgetSelection};

    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let config = goal_config(
            true,
            "openai.default",
            ConfigGoalBudgetLimits {
                token_limit: None,
                cost_limit_usd: Some(-1.0),
            },
        );
        let driver = recording_driver(&ingress);
        let error = GoalExecutionHost::new()
            .submit(
                &host_settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Limits(GoalBudgetLimits {
                        token_limit: Some(10),
                        cost_limit_usd: None,
                    }),
                    objective: "invalid defaults must reject before binding".into(),
                },
            )
            .await
            .unwrap_err();
        assert_admission_error(
            error,
            ConfigApiCode::InvalidNumericRange,
            "goal.default_cost_limit_usd",
        );
        assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn invalid_goal_limits_are_rejected_before_binding_with_typed_path() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(i64::MAX as u64 + 1),
            cost_limit_usd: Some(1.0),
        },
    );
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let error = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "must not start with an invalid limit".into(),
            },
        )
        .await
        .unwrap_err();
    assert_admission_error(
        error,
        ConfigApiCode::InvalidNumericRange,
        "goal.default_token_limit",
    );
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
}

fn recording_driver(ingress: &GoalIngressContext) -> Arc<RecordingDriver> {
    Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    })
}

#[tokio::test]
async fn supplied_driver_must_match_the_trusted_ingress_before_binding() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(GoalSessionKey::zero_code("wrong-session").unwrap()),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    });

    let error = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("does not match"));
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn execution_scope_mismatch_is_rejected_before_driver_acquisition() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    });
    let scope = zeroclaw_goal::GoalExecutionScope::new("goal-1", "rpc_wrong-session", 1).unwrap();
    let host = GoalExecutionHost::new();
    let submission = host
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    let checkpoint = GoalParentTurnCheckpoint::new();
    let error = match host
        .acquire_execution(&host_settings(&config), submission, &scope, &checkpoint)
        .await
    {
        Ok(_) => panic!("mismatched scope must not acquire an execution lease"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("does not match"));
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mismatched_driver_key_is_rejected_before_binding() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let other_key = GoalSessionKey::matrix("matrix_other_room").unwrap();
    let driver = Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(other_key),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    });
    let error = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("trusted ingress"));
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn driver_returning_a_different_binding_is_rejected_after_binding() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let returned_key = GoalSessionKey::matrix("matrix_other_room").unwrap();
    let driver = Arc::new(WrongBindingDriver {
        advertised: ingress.session_key().clone(),
        returned_binding: GoalSessionBinding::new(returned_key),
        binds: AtomicUsize::new(0),
    });

    let error = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("binding does not match"));
    assert_eq!(driver.binds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn matching_execution_scope_returns_a_working_session_lease() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let delivered = Arc::new(AtomicUsize::new(0));
    let driver = Arc::new(ExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        delivered: delivered.clone(),
    });
    let scope = GoalExecutionScope::new("goal-1", ingress.session_key().durable_id(), 1).unwrap();
    let host = GoalExecutionHost::new();
    let submission = host
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    let checkpoint = GoalParentTurnCheckpoint::new();
    let mut lease = host
        .acquire_execution(&host_settings(&config), submission, &scope, &checkpoint)
        .await
        .unwrap();

    assert!(lease.canonical_history().unwrap().is_empty());
    assert_eq!(
        lease
            .run_parent_turn(
                &scope,
                GoalParentTurn {
                    objective: "finish the task".into(),
                    working_history: Vec::new(),
                }
            )
            .await
            .unwrap(),
        "parent:finish the task"
    );
    assert_eq!(
        lease
            .run_verifier(
                &scope,
                GoalVerifierTurn {
                    objective: "finish the task".into(),
                    candidate: "candidate".into(),
                }
            )
            .await
            .unwrap(),
        GoalVerifierResult {
            response: "verifier:candidate".to_owned(),
            attribution: None,
        }
    );
    lease
        .append_parent_candidate("candidate".into())
        .await
        .unwrap();

    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 1);
    assert_eq!(delivered.load(Ordering::SeqCst), 1);
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScriptedStep {
    Parent(Vec<(String, String)>),
    AdmissionRefused(String),
    Append(String, TaskStatus, i64),
    Verifier(String),
    Resume(String),
}

#[derive(Clone, Copy)]
enum ScriptedFault {
    FailAppend,
    PauseDuringAppend,
    PauseDuringVerifier,
    ParentError,
    ParentPanic,
    PauseThenParentError,
    VerifierError,
    PauseThenVerifierError,
    PauseThenEmptyParent,
    ParentAccountingFailure,
    VerifierAccountingFailure,
    SettleIncomplete,
    ParentOutcomeUnknown,
    ParentFallbackOutcomeUnknown,
    ParentAnthropic(AnthropicCase),
    PauseAfterParentOutcomeUnknown,
    VerifierOutcomeUnknown,
    CorruptLedgerAfterParent,
    CorruptGoalAfterParent,
    CorruptGoalAfterVerifier,
    CorruptTaskAfterAppend,
    CorruptTaskAfterContinue,
    AbandonWithBlockedLedger,
    DropSettlement,
}

#[derive(Clone, Copy, Debug)]
enum AnthropicCase {
    Handoff,
    HandoffRefusal,
    NoHandoff,
    MissingTerminalModel,
    CompleteRefusal,
    CompleteHandoff,
    CompleteHandoffStreamed,
    UnattributedTerminal,
    CostLimitedUnpricedHandoff,
}

struct ExecutionScript {
    parents: VecDeque<String>,
    verifiers: VecDeque<String>,
    steps: Vec<ScriptedStep>,
    events: Vec<zeroclaw_api::agent::TurnEvent>,
    fault: Option<ScriptedFault>,
}

struct ScriptedDriver {
    binding: GoalSessionBinding,
    store: Arc<SqliteTaskStore>,
    sessions: Arc<SqliteSessionBackend>,
    script: Arc<StdMutex<ExecutionScript>>,
    control_db: PathBuf,
    ledger_file: PathBuf,
}

struct ScriptedLease {
    session_key: GoalSessionKey,
    store: Arc<SqliteTaskStore>,
    sessions: Arc<SqliteSessionBackend>,
    script: Arc<StdMutex<ExecutionScript>>,
    scope: Option<GoalExecutionScope>,
    control_db: PathBuf,
    ledger_file: PathBuf,
}

impl ScriptedLease {
    async fn run_anthropic(&self, case: AnthropicCase) -> anyhow::Result<String> {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method, matchers::path};
        use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
        use zeroclaw_api::model_provider::{ChatRequest, ChatResponse, ModelProvider};
        use zeroclaw_providers::anthropic::AnthropicModelProvider;
        use zeroclaw_runtime::tools::scoped::{ScopedAssembly, ScopedToolRegistry};

        struct NonStreamingAnthropic(AnthropicModelProvider);

        impl Attributable for NonStreamingAnthropic {
            fn role(&self) -> Role {
                Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
            }

            fn alias(&self) -> &str {
                "test"
            }
        }

        #[async_trait]
        impl ModelProvider for NonStreamingAnthropic {
            async fn chat_with_system(
                &self,
                system: Option<&str>,
                message: &str,
                model: &str,
                temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                self.0
                    .chat_with_system(system, message, model, temperature)
                    .await
            }

            async fn chat(
                &self,
                request: ChatRequest<'_>,
                model: &str,
                temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                self.0.chat(request, model, temperature).await
            }
        }

        let server = MockServer::start().await;
        let streamed = matches!(case, AnthropicCase::CompleteHandoffStreamed);
        let content = if matches!(case, AnthropicCase::NoHandoff) {
            serde_json::json!([{"type": "text", "text": "done"}])
        } else {
            serde_json::json!([{"type": "fallback"}, {"type": "text", "text": "done"}])
        };
        let refusal = matches!(
            case,
            AnthropicCase::HandoffRefusal | AnthropicCase::CompleteRefusal
        );
        let terminal_input = if streamed { 12 } else { 50 };
        let mut usage = serde_json::json!({"input_tokens": terminal_input, "output_tokens": 5});
        if matches!(
            case,
            AnthropicCase::MissingTerminalModel
                | AnthropicCase::CompleteRefusal
                | AnthropicCase::CompleteHandoff
                | AnthropicCase::CompleteHandoffStreamed
                | AnthropicCase::UnattributedTerminal
                | AnthropicCase::CostLimitedUnpricedHandoff
        ) {
            let terminal = if matches!(
                case,
                AnthropicCase::CompleteRefusal
                    | AnthropicCase::CompleteHandoff
                    | AnthropicCase::CompleteHandoffStreamed
                    | AnthropicCase::CostLimitedUnpricedHandoff
            ) {
                serde_json::json!({"type": "fallback_message", "model": "model-b", "input_tokens": terminal_input, "output_tokens": 5})
            } else {
                serde_json::json!({"type": "fallback_message"})
            };
            let first = if streamed {
                serde_json::json!({"type": "message", "model": "model-a", "input_tokens": 10,
                    "cache_read_input_tokens": 4, "cache_creation_input_tokens": 1, "output_tokens": 2})
            } else {
                serde_json::json!({"type": "message", "model": "model-a", "input_tokens": 100, "output_tokens": 10})
            };
            usage["iterations"] = serde_json::json!([first, terminal]);
        }
        let mut response_body = serde_json::json!({
            "model": "model-b",
            "content": content,
            "stop_reason": if refusal { "refusal" } else { "end_turn" },
            "usage": usage
        });
        if matches!(case, AnthropicCase::UnattributedTerminal) {
            response_body.as_object_mut().unwrap().remove("model");
        }
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .expect(1)
            .mount(&server)
            .await;
        let provider = AnthropicModelProvider::builder("test")
            .credential(Some("test-key"))
            .base_url(&server.uri())
            .server_fallback_models(vec!["model-b".to_owned()])
            .build();
        let provider: Box<dyn ModelProvider> = if streamed {
            Box::new(NonStreamingAnthropic(provider))
        } else {
            Box::new(provider)
        };
        let config = Config::default();
        let security = Arc::new(zeroclaw_runtime::security::SecurityPolicy::default());
        let tools = ScopedToolRegistry::assemble(ScopedAssembly {
            config: &config,
            agent_alias: "main",
            security: &security,
            built: zeroclaw_runtime::tools::AllToolsResult::from_prebuilt_tools(Vec::new()),
            skills: &[],
            runtime: Arc::new(zeroclaw_runtime::platform::NativeRuntime::new()),
            caller_allowed: None,
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory: true,
            acp_delivery: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        })
        .await
        .registry;
        let directory = tempfile::tempdir()?;
        let mut agent = zeroclaw_runtime::agent::Agent::builder()
            .model_provider(provider)
            .tools(tools)
            .exclude_memory(true)
            .observer(Arc::new(zeroclaw_runtime::observability::NoopObserver {}))
            .tool_dispatcher(Box::new(
                zeroclaw_runtime::agent::dispatcher::NativeToolDispatcher,
            ))
            .workspace_dir(directory.path().to_path_buf())
            .model_name("model-a".to_owned())
            .model_provider_name("primary".to_owned())
            .build()?;
        if streamed {
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
            let result = agent
                .turn_streamed("work on the goal", event_tx, None)
                .await
                .map(|(response, _)| response);
            self.script.lock().unwrap().events =
                std::iter::from_fn(|| event_rx.try_recv().ok()).collect();
            result
        } else {
            agent.turn("work on the goal").await
        }
    }

    async fn pause(&self, scope: &GoalExecutionScope) {
        assert_eq!(
            self.store
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
    }

    fn accounting_error() -> anyhow::Error {
        anyhow::Error::new(GoalAccountingFailure::settled(
            GoalAccountingState::Invalid,
            &["fixture pricing failed".to_owned()],
        ))
        .context("fixture turn context")
    }

    async fn settle_unknown(&self, scope: &GoalExecutionScope) -> anyhow::Result<()> {
        assert_eq!(
            self.store
                .admit_pending_operation(
                    scope.task_id(),
                    scope.session_id(),
                    scope.execution_epoch(),
                    "unknown-operation",
                )
                .await?,
            GoalTransitionResult::Applied
        );
        assert_eq!(
            self.store
                .settle_pending_operation(
                    scope.task_id(),
                    scope.session_id(),
                    scope.execution_epoch(),
                    "unknown-operation",
                    GoalAccountingState::OutcomeUnknown,
                )
                .await?,
            GoalTransitionResult::Applied
        );
        Ok(())
    }

    fn corrupt_goal_record(&self, scope: &GoalExecutionScope) -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&self.control_db)?;
        assert_eq!(
            conn.execute(
                "UPDATE goal_tasks SET blockers_json = 'not-json' WHERE task_id = ?1",
                rusqlite::params![scope.task_id()],
            )?,
            1
        );
        Ok(())
    }

    fn corrupt_task_record(&self, scope: &GoalExecutionScope) -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(&self.control_db)?;
        assert_eq!(
            conn.execute(
                "UPDATE tasks SET depth = 'corrupt' WHERE id = ?1",
                rusqlite::params![scope.task_id()],
            )?,
            1
        );
        Ok(())
    }
}

#[async_trait]
impl GoalSessionDriver for ScriptedDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        Ok(Box::new(ScriptedLease {
            session_key: self.binding.session_key().clone(),
            store: Arc::clone(&self.store),
            sessions: Arc::clone(&self.sessions),
            script: Arc::clone(&self.script),
            scope: None,
            control_db: self.control_db.clone(),
            ledger_file: self.ledger_file.clone(),
        }))
    }
}

#[async_trait]
impl GoalSessionExecutionLease for ScriptedLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<ChatMessage>> {
        Ok(self.sessions.try_load(&self.session_key.durable_id())?)
    }

    async fn append_resume_response(&mut self, response: String) -> anyhow::Result<()> {
        self.sessions.append(
            &self.session_key.durable_id(),
            &ChatMessage::user(&response),
        )?;
        self.script
            .lock()
            .unwrap()
            .steps
            .push(ScriptedStep::Resume(response));
        Ok(())
    }

    async fn run_parent_turn(
        &mut self,
        scope: &GoalExecutionScope,
        turn: GoalParentTurn,
    ) -> anyhow::Result<String> {
        if self.scope.is_none() {
            self.scope = Some(scope.clone());
        }
        let fault = {
            let mut script = self.script.lock().unwrap();
            script.steps.push(ScriptedStep::Parent(
                turn.working_history
                    .into_iter()
                    .map(|message| (message.role, message.content))
                    .collect(),
            ));
            script.fault
        };
        match fault {
            Some(ScriptedFault::ParentError) => anyhow::bail!("fixture provider failed"),
            Some(ScriptedFault::ParentPanic) => panic!("fixture parent panicked"),
            Some(ScriptedFault::PauseThenParentError) => {
                self.pause(scope).await;
                anyhow::bail!("fixture provider failed");
            }
            Some(ScriptedFault::PauseThenEmptyParent) => self.pause(scope).await,
            Some(ScriptedFault::ParentAccountingFailure) => {
                return Err(Self::accounting_error());
            }
            Some(
                ScriptedFault::ParentOutcomeUnknown | ScriptedFault::PauseAfterParentOutcomeUnknown,
            ) => {
                self.settle_unknown(scope).await?;
                if matches!(fault, Some(ScriptedFault::PauseAfterParentOutcomeUnknown)) {
                    self.pause(scope).await;
                }
                anyhow::bail!("fixture parent interrupted");
            }
            Some(ScriptedFault::SettleIncomplete) => {
                assert_eq!(
                    self.store
                        .admit_pending_operation(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            "fixture-operation",
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
                assert_eq!(
                    self.store
                        .settle_pending_operation(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            "fixture-operation",
                            GoalAccountingState::Invalid,
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
            }
            Some(ScriptedFault::ParentFallbackOutcomeUnknown) => {
                let accounting = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                    .try_with(Clone::clone)?
                    .expect("fixture requires Goal accounting");
                accounting
                    .admit(GoalOperationRequest::new("primary", "model"))
                    .await?;
                let _guard = accounting.admission_guard()?;
                accounting.mark_dispatch_entered();
                let event =
                    |provider: &str, input, output| zeroclaw_goal::accounting::GoalUsageEvent {
                        provider_ref: provider.into(),
                        model: "model".into(),
                        usage: zeroclaw_api::model_provider::TokenUsage {
                            input_tokens: Some(input),
                            output_tokens: Some(output),
                            cached_input_tokens: Some(0),
                            cache_creation_input_tokens: Some(0),
                        },
                        priceable: true,
                    };
                accounting
                    .settle(GoalOperationSettlement {
                        accounting_state: GoalAccountingState::OutcomeUnknown,
                        events: vec![event("primary", 10, 2), event("fallback", 15, 3)],
                    })
                    .await?;
            }
            Some(ScriptedFault::ParentAnthropic(case)) => {
                let candidate = self.run_anthropic(case).await?;
                if matches!(case, AnthropicCase::CostLimitedUnpricedHandoff) {
                    let accounting = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                        .try_with(Clone::clone)?
                        .expect("fixture requires Goal accounting");
                    let error = accounting
                        .admit(GoalOperationRequest::new("primary", "model-a"))
                        .await
                        .expect_err("unpriced Goal usage must refuse the next admission");
                    self.script
                        .lock()
                        .unwrap()
                        .steps
                        .push(ScriptedStep::AdmissionRefused(format!("{error:#}")));
                }
                return Ok(candidate);
            }
            Some(ScriptedFault::CorruptLedgerAfterParent) => {
                use std::io::Write as _;

                let accounting = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                    .try_with(Clone::clone)?
                    .expect("fixture requires Goal accounting");
                accounting
                    .admit(GoalOperationRequest::new("primary", "model"))
                    .await?;
                let guard = accounting.admission_guard()?;
                accounting.mark_dispatch_entered();
                accounting
                    .settle(GoalOperationSettlement {
                        accounting_state: GoalAccountingState::Complete,
                        events: vec![zeroclaw_goal::accounting::GoalUsageEvent {
                            provider_ref: "primary".into(),
                            model: "model".into(),
                            usage: zeroclaw_api::model_provider::TokenUsage {
                                input_tokens: Some(10),
                                output_tokens: Some(2),
                                cached_input_tokens: Some(0),
                                cache_creation_input_tokens: Some(0),
                            },
                            priceable: true,
                        }],
                    })
                    .await?;
                drop(guard);
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&self.ledger_file)?
                    .write_all(b"{\"partial\":")?;
            }
            Some(ScriptedFault::CorruptGoalAfterParent) => self.corrupt_goal_record(scope)?,
            Some(ScriptedFault::AbandonWithBlockedLedger | ScriptedFault::DropSettlement) => {
                let accounting = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                    .try_with(Clone::clone)?
                    .expect("fixture requires Goal accounting");
                accounting
                    .admit(GoalOperationRequest::new("fallback", "model"))
                    .await?;
                let guard = accounting.admission_guard()?;
                accounting.mark_dispatch_entered();
                let settlement = GoalOperationSettlement {
                    accounting_state: GoalAccountingState::Complete,
                    events: vec![zeroclaw_goal::accounting::GoalUsageEvent {
                        provider_ref: "fallback".into(),
                        model: "model".into(),
                        usage: zeroclaw_api::model_provider::TokenUsage {
                            input_tokens: Some(10),
                            output_tokens: Some(2),
                            cached_input_tokens: Some(0),
                            cache_creation_input_tokens: Some(0),
                        },
                        priceable: true,
                    }],
                };
                if matches!(fault, Some(ScriptedFault::AbandonWithBlockedLedger)) {
                    std::fs::create_dir_all(&self.ledger_file)?;
                    accounting.set_abandonment_settlement(Box::new(move || settlement));
                } else {
                    let mut pending = Box::pin(accounting.settle(settlement));
                    std::future::poll_fn(|cx| {
                        assert!(std::future::Future::poll(pending.as_mut(), cx).is_pending());
                        std::task::Poll::Ready(())
                    })
                    .await;
                    drop(pending);
                }
                drop(guard);
            }
            _ => {}
        }
        Ok(self.script.lock().unwrap().parents.pop_front().unwrap())
    }

    async fn run_verifier(
        &mut self,
        scope: &GoalExecutionScope,
        turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        let fault = {
            let mut script = self.script.lock().unwrap();
            script.steps.push(ScriptedStep::Verifier(turn.candidate));
            script.fault
        };
        match fault {
            Some(ScriptedFault::PauseDuringVerifier) => self.pause(scope).await,
            Some(ScriptedFault::VerifierError) => anyhow::bail!("fixture verifier failed"),
            Some(ScriptedFault::PauseThenVerifierError) => {
                self.pause(scope).await;
                anyhow::bail!("fixture verifier failed");
            }
            Some(ScriptedFault::ParentAnthropic(AnthropicCase::CompleteHandoffStreamed)) => {
                let accounting = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                    .try_with(Clone::clone)?
                    .expect("fixture requires Goal accounting");
                accounting
                    .admit(GoalOperationRequest::new("primary", "model-a"))
                    .await?;
                accounting.release_unstarted().await?;
            }
            Some(ScriptedFault::VerifierAccountingFailure) => {
                return Err(Self::accounting_error());
            }
            Some(ScriptedFault::VerifierOutcomeUnknown) => {
                self.settle_unknown(scope).await?;
                anyhow::bail!("fixture verifier interrupted");
            }
            Some(ScriptedFault::CorruptGoalAfterVerifier) => self.corrupt_goal_record(scope)?,
            Some(ScriptedFault::CorruptTaskAfterContinue) => self.corrupt_task_record(scope)?,
            _ => {}
        }
        Ok(GoalVerifierResult {
            response: self.script.lock().unwrap().verifiers.pop_front().unwrap(),
            attribution: None,
        })
    }

    async fn append_parent_candidate(&mut self, candidate: String) -> anyhow::Result<()> {
        let fault = self.script.lock().unwrap().fault;
        if matches!(fault, Some(ScriptedFault::FailAppend)) {
            anyhow::bail!("fixture append failed");
        }
        let scope = self.scope.as_ref().unwrap();
        let task = self
            .store
            .current_goal_for_session(scope.session_id())
            .await?
            .unwrap();
        self.sessions
            .append(scope.session_id(), &ChatMessage::assistant(&candidate))?;
        self.script.lock().unwrap().steps.push(ScriptedStep::Append(
            candidate,
            task.status,
            task.execution_epoch,
        ));
        if matches!(fault, Some(ScriptedFault::PauseDuringAppend)) {
            self.pause(scope).await;
        }
        if matches!(fault, Some(ScriptedFault::CorruptTaskAfterAppend)) {
            self.corrupt_task_record(scope)?;
        }
        Ok(())
    }
}

struct ScriptedAccounting;

#[async_trait]
impl GoalOperationAccounting for ScriptedAccounting {
    fn admission_guard(&self) -> anyhow::Result<zeroclaw_goal::accounting::GoalOperationGuard> {
        Ok(zeroclaw_goal::accounting::GoalOperationGuard::new(()))
    }
    async fn begin_dispatch(&self) -> anyhow::Result<zeroclaw_goal::task::GoalDispatchStart> {
        Ok(zeroclaw_goal::task::GoalDispatchStart::new(
            std::sync::Arc::new(tokio::sync::Mutex::new(()))
                .lock_owned()
                .await,
        ))
    }
    async fn admit(&self, _request: GoalOperationRequest) -> anyhow::Result<()> {
        Ok(())
    }

    async fn settle(&self, _settlement: GoalOperationSettlement) -> anyhow::Result<()> {
        Ok(())
    }

    async fn release_unstarted(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn pricing_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct ScriptedAccountingFactory;

impl GoalOperationAccountingFactory for ScriptedAccountingFactory {
    fn for_scope(&self, _scope: GoalExecutionScope) -> Arc<dyn GoalOperationAccounting> {
        Arc::new(ScriptedAccounting)
    }
}

struct FailingPricingFactory {
    fail_on_call: usize,
    pause_before_failure: Option<Arc<SqliteTaskStore>>,
}

struct FailingPricing {
    calls: AtomicUsize,
    fail_on_call: usize,
    pause_before_failure: Option<Arc<SqliteTaskStore>>,
    scope: GoalExecutionScope,
}

#[async_trait]
impl GoalOperationAccounting for FailingPricing {
    fn admission_guard(&self) -> anyhow::Result<zeroclaw_goal::accounting::GoalOperationGuard> {
        Ok(zeroclaw_goal::accounting::GoalOperationGuard::new(()))
    }
    async fn begin_dispatch(&self) -> anyhow::Result<zeroclaw_goal::task::GoalDispatchStart> {
        Ok(zeroclaw_goal::task::GoalDispatchStart::new(
            std::sync::Arc::new(tokio::sync::Mutex::new(()))
                .lock_owned()
                .await,
        ))
    }
    async fn admit(&self, _request: GoalOperationRequest) -> anyhow::Result<()> {
        Ok(())
    }
    async fn settle(&self, _settlement: GoalOperationSettlement) -> anyhow::Result<()> {
        Ok(())
    }
    async fn release_unstarted(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn pricing_complete(&self) -> anyhow::Result<bool> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_on_call {
            if let Some(store) = &self.pause_before_failure {
                assert_eq!(
                    store
                        .pause_session_goal(
                            self.scope.task_id(),
                            self.scope.session_id(),
                            self.scope.execution_epoch(),
                            GoalPauseState {
                                reason: GoalPauseReason::OperatorPaused,
                                description: None,
                                blockers: Vec::new()
                            },
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
            }
            anyhow::bail!("fixture pricing ledger read failed");
        }
        Ok(true)
    }
}

impl GoalOperationAccountingFactory for FailingPricingFactory {
    fn for_scope(&self, scope: GoalExecutionScope) -> Arc<dyn GoalOperationAccounting> {
        Arc::new(FailingPricing {
            calls: AtomicUsize::new(0),
            fail_on_call: self.fail_on_call,
            pause_before_failure: self.pause_before_failure.clone(),
            scope,
        })
    }
}

struct PauseDuringPricingFactory(Arc<SqliteTaskStore>);

struct PauseDuringPricing {
    store: Arc<SqliteTaskStore>,
    scope: GoalExecutionScope,
}

#[async_trait]
impl GoalOperationAccounting for PauseDuringPricing {
    fn admission_guard(&self) -> anyhow::Result<zeroclaw_goal::accounting::GoalOperationGuard> {
        Ok(zeroclaw_goal::accounting::GoalOperationGuard::new(()))
    }
    async fn begin_dispatch(&self) -> anyhow::Result<zeroclaw_goal::task::GoalDispatchStart> {
        Ok(zeroclaw_goal::task::GoalDispatchStart::new(
            std::sync::Arc::new(tokio::sync::Mutex::new(()))
                .lock_owned()
                .await,
        ))
    }
    async fn admit(&self, _request: GoalOperationRequest) -> anyhow::Result<()> {
        Ok(())
    }

    async fn settle(&self, _settlement: GoalOperationSettlement) -> anyhow::Result<()> {
        Ok(())
    }

    async fn release_unstarted(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn pricing_complete(&self) -> anyhow::Result<bool> {
        assert_eq!(
            self.store
                .pause_session_goal(
                    self.scope.task_id(),
                    self.scope.session_id(),
                    self.scope.execution_epoch(),
                    GoalPauseState {
                        reason: GoalPauseReason::OperatorPaused,
                        description: None,
                        blockers: Vec::new(),
                    },
                )
                .await?,
            GoalTransitionResult::Applied
        );
        Ok(false)
    }
}

impl GoalOperationAccountingFactory for PauseDuringPricingFactory {
    fn for_scope(&self, scope: GoalExecutionScope) -> Arc<dyn GoalOperationAccounting> {
        Arc::new(PauseDuringPricing {
            store: Arc::clone(&self.0),
            scope,
        })
    }
}

struct EngineFixture {
    config: Config,
    runtime: GoalRuntime,
    store: Arc<SqliteTaskStore>,
    driver: Arc<ScriptedDriver>,
    scope: GoalExecutionScope,
    request: Option<zeroclaw_goal::GoalExecutionRequest>,
    session_data: PathBuf,
    // Drop both databases and their shared handles before the temporary files.
    _directory: tempfile::TempDir,
}

impl EngineFixture {
    async fn new(parents: &[&str], verifiers: &[&str], fault: Option<ScriptedFault>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteTaskStore::new(&directory.path().join("control")).unwrap());
        let session_data = directory.path().join("history");
        let config = valid_goal_config(true);
        let ingress = matrix_ingress();
        let driver = Arc::new(ScriptedDriver {
            binding: GoalSessionBinding::new(ingress.session_key().clone()),
            store: Arc::clone(&store),
            sessions: Arc::new(SqliteSessionBackend::new(&session_data).unwrap()),
            script: Arc::new(StdMutex::new(ExecutionScript {
                parents: parents.iter().map(|text| (*text).to_owned()).collect(),
                verifiers: verifiers.iter().map(|text| (*text).to_owned()).collect(),
                steps: Vec::new(),
                events: Vec::new(),
                fault,
            })),
            control_db: directory.path().join("control/control_plane.db"),
            ledger_file: directory.path().join("state/costs.jsonl"),
        });
        let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let (_, request) = runtime
            .submit(
                &host_settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Limits(
                        zeroclaw_commands::goal::GoalBudgetLimits {
                            token_limit: Some(100),
                            cost_limit_usd: None,
                        },
                    ),
                    objective: "finish the bounded task".into(),
                },
            )
            .await
            .unwrap()
            .into_parts();
        let request = request.unwrap();
        let scope = request.scope().clone();
        Self {
            config,
            runtime,
            store,
            driver,
            scope,
            request: Some(request),
            session_data,
            _directory: directory,
        }
    }

    async fn run(&mut self) -> anyhow::Result<zeroclaw_goal::GoalExecutionOutcome> {
        self.run_with_factory(Arc::new(ScriptedAccountingFactory))
            .await
    }

    async fn run_with_factory(
        &mut self,
        factory: Arc<dyn GoalOperationAccountingFactory>,
    ) -> anyhow::Result<zeroclaw_goal::GoalExecutionOutcome> {
        // The engine resolves settings from a live handle at acquisition.
        let live = LiveConfig::new(self.config.clone());
        let source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot")?;
        self.runtime
            .execution_engine(factory)
            .run(
                &source,
                self.request.take().unwrap(),
                GoalParentTurnCheckpoint::new(),
            )
            .await
    }

    async fn task(&self) -> TaskRecord {
        self.store
            .current_goal_for_session(self.scope.session_id())
            .await
            .unwrap()
            .unwrap()
    }

    async fn reason(&self) -> Option<String> {
        self.store
            .terminal_reason_for_session_goal(self.scope.task_id(), self.scope.session_id())
            .await
            .unwrap()
    }

    fn steps(&self) -> Vec<ScriptedStep> {
        self.driver.script.lock().unwrap().steps.clone()
    }

    fn history(&self) -> Vec<(String, String)> {
        // A separate backend handle proves the transcript is not only a cache.
        SqliteSessionBackend::new(&self.session_data)
            .unwrap()
            .try_load(self.scope.session_id())
            .unwrap()
            .into_iter()
            .map(|message| (message.role, message.content))
            .collect()
    }

    fn raw_status_and_reason(&self) -> (String, Option<String>) {
        rusqlite::Connection::open(&self.driver.control_db)
            .unwrap()
            .query_row(
                "SELECT status, error FROM tasks WHERE id = ?1",
                rusqlite::params![self.scope.task_id()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn reject_status_write(&self, status: &str) {
        assert!(matches!(status, "completed" | "paused"));
        let sql = format!(
            "CREATE TRIGGER reject_fixture_status BEFORE UPDATE OF status ON tasks \
             WHEN NEW.status = '{status}' BEGIN \
             SELECT RAISE(ABORT, 'fixture status write failed'); END"
        );
        rusqlite::Connection::open(&self.driver.control_db)
            .unwrap()
            .execute_batch(&sql)
            .unwrap();
    }
}

mod supervisor_error_contract {
    use super::*;
    use tokio::sync::Semaphore;

    #[derive(Clone, Copy)]
    enum HeldTurn {
        Parent,
        Verifier,
        AfterParent,
    }

    struct HeldDriver {
        inner: Arc<ScriptedDriver>,
        turn: HeldTurn,
        entered: Arc<Semaphore>,
        release: Arc<Semaphore>,
        accounting: Option<GoalAccountingState>,
        command: Arc<tokio::sync::Mutex<()>>,
        binds: AtomicUsize,
        hold_rebind: AtomicBool,
        rebind_entered: Semaphore,
        release_rebind: Semaphore,
    }

    struct HeldLease {
        inner: Box<dyn GoalSessionExecutionLease>,
        store: Arc<SqliteTaskStore>,
        turn: HeldTurn,
        entered: Arc<Semaphore>,
        release: Arc<Semaphore>,
        accounting: Option<GoalAccountingState>,
    }

    #[async_trait]
    impl GoalSessionDriver for HeldDriver {
        fn session_key(&self) -> &GoalSessionKey {
            self.inner.session_key()
        }

        async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
            let ordinal = self.binds.fetch_add(1, Ordering::SeqCst) + 1;
            let command = self.command.clone().lock_owned().await;
            if ordinal == 2 && self.hold_rebind.load(Ordering::SeqCst) {
                self.rebind_entered.add_permits(1);
                self.release_rebind.acquire().await.unwrap().forget();
            }
            Ok(GoalSessionLease::new(self.inner.binding.clone(), command))
        }

        async fn acquire_execution(
            &self,
            ingress: &GoalIngressContext,
            scope: &GoalExecutionScope,
            checkpoint: &GoalParentTurnCheckpoint,
        ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
            Ok(Box::new(HeldLease {
                inner: self
                    .inner
                    .acquire_execution(ingress, scope, checkpoint)
                    .await?,
                store: self.inner.store.clone(),
                turn: self.turn,
                entered: self.entered.clone(),
                release: self.release.clone(),
                accounting: self.accounting,
            }))
        }
    }

    #[async_trait]
    impl GoalSessionExecutionLease for HeldLease {
        fn session_key(&self) -> &GoalSessionKey {
            self.inner.session_key()
        }

        fn canonical_history(&self) -> anyhow::Result<Vec<ChatMessage>> {
            self.inner.canonical_history()
        }

        async fn append_resume_response(&mut self, response: String) -> anyhow::Result<()> {
            self.inner.append_resume_response(response).await
        }

        async fn append_parent_candidate(&mut self, candidate: String) -> anyhow::Result<()> {
            self.inner.append_parent_candidate(candidate).await
        }

        async fn run_parent_turn(
            &mut self,
            scope: &GoalExecutionScope,
            turn: GoalParentTurn,
        ) -> anyhow::Result<String> {
            if self.accounting.is_some() {
                assert_eq!(
                    self.store
                        .admit_pending_operation(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            "held-operation",
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
            }
            if matches!(self.turn, HeldTurn::Parent) {
                self.entered.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            if let Some(state) = self.accounting
                && state != GoalAccountingState::OutcomeUnknown
            {
                assert_eq!(
                    self.store
                        .settle_pending_operation(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            "held-operation",
                            state,
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
            }
            let result = self.inner.run_parent_turn(scope, turn).await;
            if matches!(self.turn, HeldTurn::AfterParent) {
                self.entered.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            result
        }

        async fn run_verifier(
            &mut self,
            scope: &GoalExecutionScope,
            turn: GoalVerifierTurn,
        ) -> anyhow::Result<GoalVerifierResult> {
            if matches!(self.turn, HeldTurn::Verifier) {
                self.entered.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            self.inner.run_verifier(scope, turn).await
        }
    }

    struct Fixture {
        config: Config,
        source: GoalHostSettingsSource,
        store: Arc<SqliteTaskStore>,
        driver: Arc<HeldDriver>,
        supervisor: Arc<GoalExecutionSupervisor>,
        scope: GoalExecutionScope,
        started: Option<zeroclaw_goal::GoalExecutionSubmission>,
        _directory: TempDir,
    }

    impl Fixture {
        async fn new(
            turn: HeldTurn,
            fault: ScriptedFault,
            accounting: Option<GoalAccountingState>,
        ) -> Self {
            Self::new_with_real_accounting(turn, fault, accounting, false).await
        }

        async fn new_with_real_accounting(
            turn: HeldTurn,
            fault: ScriptedFault,
            accounting: Option<GoalAccountingState>,
            real_accounting: bool,
        ) -> Self {
            let directory = TempDir::new().unwrap();
            let store = Arc::new(SqliteTaskStore::new(&directory.path().join("control")).unwrap());
            let config = valid_goal_config(true);
            let ingress = matrix_ingress();
            let inner = Arc::new(ScriptedDriver {
                binding: GoalSessionBinding::new(ingress.session_key().clone()),
                store: store.clone(),
                sessions: Arc::new(
                    SqliteSessionBackend::new(&directory.path().join("history")).unwrap(),
                ),
                script: Arc::new(StdMutex::new(ExecutionScript {
                    parents: VecDeque::from(["candidate".to_owned()]),
                    verifiers: VecDeque::from([SCRIPTED_COMPLETE.to_owned()]),
                    steps: Vec::new(),
                    events: Vec::new(),
                    fault: Some(fault),
                })),
                control_db: directory.path().join("control/control_plane.db"),
                ledger_file: directory.path().join("state/costs.jsonl"),
            });
            let driver = Arc::new(HeldDriver {
                inner,
                turn,
                entered: Arc::new(Semaphore::new(0)),
                release: Arc::new(Semaphore::new(0)),
                accounting,
                command: Arc::new(tokio::sync::Mutex::new(())),
                binds: AtomicUsize::new(0),
                hold_rebind: AtomicBool::new(false),
                rebind_entered: Semaphore::new(0),
                release_rebind: Semaphore::new(0),
            });
            let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
            let factory: Arc<dyn GoalOperationAccountingFactory> = if real_accounting {
                let tracker = Arc::new(
                    zeroclaw_config::cost::CostTracker::new(
                        zeroclaw_config::schema::CostConfig::default(),
                        directory.path(),
                    )
                    .unwrap(),
                );
                let rates = std::collections::HashMap::from([
                    ("model.input".to_owned(), 1.0),
                    ("model.output".to_owned(), 2.0),
                ]);
                let pricer = Arc::new(
                    zeroclaw_runtime::goal_accounting::ModelProviderGoalUsagePricer::new(Arc::new(
                        std::collections::HashMap::from([("primary".to_owned(), rates)]),
                    )),
                );
                Arc::new(
                    zeroclaw_runtime::goal_accounting::RuntimeGoalOperationAccountingFactory::new(
                        store.clone() as Arc<dyn GoalTaskRegistry>,
                        tracker,
                        "main",
                        pricer,
                    )
                    .unwrap(),
                )
            } else {
                Arc::new(ScriptedAccountingFactory)
            };
            let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(
                runtime.execution_engine(factory),
            )));
            let live = LiveConfig::new(config.clone());
            let source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
            let started = supervisor
                .submit(
                    &host_settings(&config),
                    ingress,
                    driver.clone(),
                    GoalCommand::Start {
                        budget: zeroclaw_commands::goal::GoalBudgetSelection::Limits(
                            zeroclaw_commands::goal::GoalBudgetLimits {
                                token_limit: Some(100),
                                cost_limit_usd: None,
                            },
                        ),
                        objective: "finish the bounded task".into(),
                    },
                )
                .await
                .unwrap();
            let task = store
                .current_goal_for_session(&driver.session_key().durable_id())
                .await
                .unwrap()
                .unwrap();
            let scope =
                GoalExecutionScope::new(task.id, task.session_id.unwrap(), task.execution_epoch)
                    .unwrap();
            Self {
                config,
                source,
                store,
                driver,
                supervisor,
                scope,
                started: Some(started),
                _directory: directory,
            }
        }

        async fn start(&mut self) {
            self.supervisor
                .launch_after_response(&self.source, &mut self.started.take().unwrap())
                .await
                .unwrap();
            timeout(Duration::from_secs(1), self.driver.entered.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
        }

        async fn task(&self) -> TaskRecord {
            self.store
                .current_goal_for_session(self.scope.session_id())
                .await
                .unwrap()
                .unwrap()
        }

        async fn pause(&self) -> zeroclaw_goal::GoalExecutionSubmission {
            let pause = self
                .supervisor
                .submit(
                    &host_settings(&self.config),
                    matrix_ingress(),
                    self.driver.clone(),
                    GoalCommand::Pause { immediate: true },
                )
                .await
                .unwrap();
            assert!(matches!(
                pause.response(),
                GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Pause)
            ));
            pause
        }

        async fn reason(&self) -> Option<String> {
            self.store
                .terminal_reason_for_session_goal(self.scope.task_id(), self.scope.session_id())
                .await
                .unwrap()
        }

        async fn goal(&self) -> GoalTaskRecord {
            self.store
                .get_goal_task(self.scope.task_id())
                .await
                .unwrap()
                .unwrap()
        }

        async fn enable_cost_limit(&self) {
            assert_eq!(
                self.store
                    .update_session_goal_limits(
                        self.scope.task_id(),
                        self.scope.session_id(),
                        self.scope.execution_epoch(),
                        Some(100),
                        Some(1.0),
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
        }

        async fn start_replacement(&self) -> zeroclaw_goal::GoalExecutionSubmission {
            self.supervisor
                .submit(
                    &host_settings(&self.config),
                    matrix_ingress(),
                    self.driver.clone(),
                    GoalCommand::Start {
                        budget: zeroclaw_commands::goal::GoalBudgetSelection::Limits(
                            zeroclaw_commands::goal::GoalBudgetLimits {
                                token_limit: Some(100),
                                cost_limit_usd: None,
                            },
                        ),
                        objective: "replacement goal".into(),
                    },
                )
                .await
                .unwrap()
        }

        fn intent_states(&self) -> Vec<(String, Option<String>)> {
            let connection = rusqlite::Connection::open_with_flags(
                self._directory.path().join("control/control_plane.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let mut statement = connection
                .prepare(
                    "SELECT state, outcome_reason FROM goal_control_intents
                 WHERE task_id = ?1 AND session_id = ?2 ORDER BY intent_id",
                )
                .unwrap();
            statement
                .query_map(
                    rusqlite::params![self.scope.task_id(), self.scope.session_id()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        }
    }

    #[tokio::test]
    async fn supervisor_settled_parent_and_verifier_errors_preserve_operator_pause_and_original_error()
     {
        for (turn, fault, cause) in [
            (
                HeldTurn::Parent,
                ScriptedFault::ParentError,
                "fixture provider failed",
            ),
            (
                HeldTurn::Verifier,
                ScriptedFault::VerifierError,
                "fixture verifier failed",
            ),
        ] {
            let mut fixture = Fixture::new(turn, fault, None).await;
            fixture.start().await;
            let mut pause = fixture.pause().await;
            fixture.driver.release.add_permits(1);
            let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
            assert!(format!("{error:#}").contains(cause));
            assert!(matches!(
                fixture
                    .supervisor
                    .launch_after_response(&fixture.source, &mut pause)
                    .await
                    .unwrap(),
                Some(GoalResponse::Paused(_))
            ));
            let task = fixture.task().await;
            assert_eq!(task.status, TaskStatus::Paused);
            assert_eq!(task.execution_epoch, fixture.scope.execution_epoch() + 1);
            let goal = fixture.goal().await;
            assert_eq!(goal.pause_reason, Some(GoalPauseReason::OperatorPaused));
            assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
            assert!(goal.pending_call_id.is_none());
            assert!(goal.pending_call_epoch.is_none());
            assert_eq!(fixture.reason().await, None);
            assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
            let history = fixture
                .driver
                .inner
                .sessions
                .try_load(fixture.scope.session_id())
                .unwrap();
            assert_eq!(
                history.len(),
                usize::from(matches!(turn, HeldTurn::Verifier))
            );
            if matches!(turn, HeldTurn::Verifier) {
                assert_eq!(history[0].content, "candidate");
            }
            let steps = &fixture.driver.inner.script.lock().unwrap().steps;
            assert_eq!(
                steps
                    .iter()
                    .filter(|step| matches!(step, ScriptedStep::Parent(_)))
                    .count(),
                1
            );
            assert_eq!(
                steps
                    .iter()
                    .filter(|step| matches!(step, ScriptedStep::Verifier(_)))
                    .count(),
                usize::from(matches!(turn, HeldTurn::Verifier))
            );
        }
    }

    #[tokio::test]
    async fn supervisor_settled_parent_and_verifier_errors_preserve_restart_pause() {
        for (turn, fault) in [
            (HeldTurn::Parent, ScriptedFault::ParentError),
            (HeldTurn::Verifier, ScriptedFault::VerifierError),
        ] {
            let mut fixture = Fixture::new(turn, fault, None).await;
            fixture.start().await;
            let supervisor = fixture.supervisor.clone();
            let restart =
                zeroclaw_spawn::spawn!(async move { supervisor.pause_for_restart().await });
            timeout(Duration::from_secs(1), async {
                loop {
                    if fixture.task().await.status == TaskStatus::Paused {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            fixture.driver.release.add_permits(1);
            timeout(Duration::from_secs(1), restart)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let task = fixture.task().await;
            assert_eq!(task.status, TaskStatus::Paused);
            let goal = fixture.goal().await;
            assert_eq!(goal.pause_reason, Some(GoalPauseReason::DaemonRestart));
            assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
            assert_eq!(fixture.reason().await, None);
            assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
        }
    }

    #[tokio::test]
    async fn supervisor_incomplete_accounting_still_fails_after_operator_pause() {
        for state in [
            GoalAccountingState::Missing,
            GoalAccountingState::Invalid,
            GoalAccountingState::OutcomeUnknown,
        ] {
            let mut fixture =
                Fixture::new(HeldTurn::Parent, ScriptedFault::ParentError, Some(state)).await;
            fixture.start().await;
            let _pause = fixture.pause().await;
            fixture.driver.release.add_permits(1);
            let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
            assert!(format!("{error:#}").contains("fixture provider failed"));
            let task = fixture.task().await;
            assert_eq!(task.status, TaskStatus::Failed);
            let goal = fixture.goal().await;
            assert_eq!(goal.accounting_state, state);
            assert!(goal.pending_call_id.is_none());
            assert!(goal.pending_call_epoch.is_none());
            assert_eq!(
                fixture.reason().await.as_deref(),
                Some(if state == GoalAccountingState::OutcomeUnknown {
                    "accounting_outcome_unknown"
                } else {
                    "accounting_missing_or_invalid"
                })
            );
            assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
        }
    }

    #[tokio::test]
    async fn supervisor_strict_ledger_failure_after_pause_is_terminal_even_when_goal_record_is_complete()
     {
        let mut fixture = Fixture::new_with_real_accounting(
            HeldTurn::AfterParent,
            ScriptedFault::CorruptLedgerAfterParent,
            None,
            true,
        )
        .await;
        fixture.enable_cost_limit().await;
        fixture.start().await;
        let goal = fixture.goal().await;
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        drop(fixture.pause().await);
        fixture.driver.release.add_permits(1);

        let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
        assert!(format!("{error:#}").contains("failed to read Goal accounting"));
        assert_eq!(fixture.task().await.status, TaskStatus::Failed);
        assert_eq!(
            fixture.reason().await.as_deref(),
            Some("accounting_missing_or_invalid")
        );
        let ledger = std::fs::read_to_string(&fixture.driver.inner.ledger_file).unwrap();
        let mut lines = ledger.lines();
        let record: zeroclaw_config::cost::types::CostRecord =
            serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(record.usage.total_tokens, 12);
        assert_eq!(record.task_id.as_deref(), Some(fixture.scope.task_id()));
        assert_eq!(lines.next(), Some("{\"partial\":"));
        assert_eq!(lines.next(), None);
        assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
    }

    #[tokio::test]
    async fn supervisor_strict_ledger_failure_does_not_replace_cancel_or_damage_replacement() {
        for replace_before_drain in [false, true] {
            let mut fixture = Fixture::new_with_real_accounting(
                HeldTurn::AfterParent,
                ScriptedFault::CorruptLedgerAfterParent,
                None,
                true,
            )
            .await;
            fixture.enable_cost_limit().await;
            fixture.start().await;
            let cancel = fixture
                .supervisor
                .submit(
                    &host_settings(&fixture.config),
                    matrix_ingress(),
                    fixture.driver.clone(),
                    GoalCommand::Cancel,
                )
                .await
                .unwrap();
            assert!(matches!(
                cancel.response(),
                GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Cancel)
            ));
            drop(cancel);
            assert_eq!(fixture.task().await.status, TaskStatus::Cancelled);

            let replacement = if replace_before_drain {
                Some(fixture.start_replacement().await)
            } else {
                None
            };
            fixture.driver.release.add_permits(1);
            let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
            let error = format!("{error:#}");
            if replace_before_drain {
                assert!(
                    error.contains("Goal extension disappeared while settling accounting"),
                    "unexpected old-worker error: {error}"
                );
            } else {
                assert!(
                    error.contains("failed to read Goal accounting"),
                    "unexpected old-worker error: {error}"
                );
                assert_eq!(fixture.task().await.status, TaskStatus::Cancelled);
            }
            let mut replacement = match replacement {
                Some(replacement) => replacement,
                None => fixture.start_replacement().await,
            };
            assert!(matches!(replacement.response(), GoalResponse::Started(_)));
            let current = fixture.task().await;
            assert_ne!(current.id, fixture.scope.task_id());
            assert_eq!(current.status, TaskStatus::Running);
            fixture
                .supervisor
                .pause_before_launch(&mut replacement, GoalPauseReason::OperatorPaused)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn resume_waits_for_predecessor_and_preserves_terminal_accounting_failure() {
        let mut fixture = Fixture::new_with_real_accounting(
            HeldTurn::AfterParent,
            ScriptedFault::CorruptLedgerAfterParent,
            None,
            true,
        )
        .await;
        fixture.enable_cost_limit().await;
        fixture.start().await;
        drop(fixture.pause().await);
        let zeroclaw_goal::GoalControlAdmission::Queued { response, deferred } = fixture
            .supervisor
            .admit_control(
                &host_settings(&fixture.config),
                matrix_ingress(),
                fixture.driver.clone(),
                GoalCommand::Resume { response: None },
            )
            .await
            .unwrap()
        else {
            panic!("resume must queue while the paused predecessor is owned");
        };
        assert!(matches!(response, GoalResponse::Queued(_)));
        let direct = fixture
            .supervisor
            .submit(
                &host_settings(&fixture.config),
                matrix_ingress(),
                fixture.driver.clone(),
                GoalCommand::Resume { response: None },
            )
            .await
            .unwrap();
        assert!(matches!(direct.response(), GoalResponse::Stale));
        drop(direct);
        let wait = deferred.wait();
        tokio::pin!(wait);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(wait.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(fixture.task().await.status, TaskStatus::Paused);
        fixture.driver.release.add_permits(1);
        let ready = timeout(Duration::from_secs(1), wait)
            .await
            .unwrap()
            .unwrap();
        let commit = ready.commit(&host_settings(&fixture.config)).await.unwrap();
        assert!(matches!(commit.response(), GoalResponse::Terminal(_)));
        drop(commit.release());
        assert_eq!(fixture.task().await.status, TaskStatus::Failed);
        assert_eq!(
            fixture.reason().await.as_deref(),
            Some("accounting_missing_or_invalid")
        );
        assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
    }

    #[tokio::test]
    async fn resume_waits_for_settled_predecessor_then_can_launch() {
        let mut fixture = Fixture::new(HeldTurn::Parent, ScriptedFault::ParentError, None).await;
        fixture.start().await;
        drop(fixture.pause().await);
        let zeroclaw_goal::GoalControlAdmission::Queued { response, deferred } = fixture
            .supervisor
            .admit_control(
                &host_settings(&fixture.config),
                matrix_ingress(),
                fixture.driver.clone(),
                GoalCommand::Resume { response: None },
            )
            .await
            .unwrap()
        else {
            panic!("resume must queue while the paused predecessor is owned");
        };
        assert!(matches!(response, GoalResponse::Queued(_)));
        let wait = deferred.wait();
        tokio::pin!(wait);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(wait.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(fixture.task().await.status, TaskStatus::Paused);
        fixture.driver.release.add_permits(1);
        let ready = timeout(Duration::from_secs(1), wait)
            .await
            .unwrap()
            .unwrap();
        let commit = ready.commit(&host_settings(&fixture.config)).await.unwrap();
        assert!(matches!(commit.response(), GoalResponse::Resumed(_)));
        let resumed_task = fixture.task().await;
        assert_eq!(resumed_task.status, TaskStatus::Running);
        assert_eq!(
            fixture.driver.inner.script.lock().unwrap().steps.len(),
            1,
            "the successor must not run before its resume notice"
        );
        let mut submission = commit.release();
        fixture
            .supervisor
            .launch_after_response(&fixture.source, &mut submission)
            .await
            .unwrap();
        timeout(Duration::from_secs(1), fixture.driver.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        fixture.driver.release.add_permits(1);
        let resumed_scope = GoalExecutionScope::new(
            resumed_task.id,
            resumed_task.session_id.unwrap(),
            resumed_task.execution_epoch,
        )
        .unwrap();
        let error = fixture.supervisor.drain(&resumed_scope).await.unwrap_err();
        assert!(format!("{error:#}").contains("fixture provider failed"));
    }

    #[tokio::test]
    async fn restart_closes_resume_wait_without_losing_predecessor_custody() {
        let mut fixture = Fixture::new(HeldTurn::Parent, ScriptedFault::ParentError, None).await;
        fixture.start().await;
        drop(fixture.pause().await);
        let zeroclaw_goal::GoalControlAdmission::Queued { deferred, .. } = fixture
            .supervisor
            .admit_control(
                &host_settings(&fixture.config),
                matrix_ingress(),
                fixture.driver.clone(),
                GoalCommand::Resume { response: None },
            )
            .await
            .unwrap()
        else {
            panic!("resume must queue while the paused predecessor is owned");
        };
        let wait = deferred.wait();
        tokio::pin!(wait);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(wait.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let supervisor = fixture.supervisor.clone();
        let restart = zeroclaw_spawn::spawn!(async move { supervisor.pause_for_restart().await });
        let closed = timeout(Duration::from_secs(1), wait)
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            std::error::Error::source(&closed)
                .unwrap()
                .is::<zeroclaw_goal::GoalCheckpointClosed>()
        );
        assert!(fixture.supervisor.owns_scope(&fixture.scope).await);
        fixture.driver.release.add_permits(1);
        timeout(Duration::from_secs(1), restart)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(fixture.task().await.status, TaskStatus::Paused);
        assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
        assert_eq!(
            fixture.intent_states(),
            vec![("abandoned".to_owned(), Some("session_closed".to_owned()))]
        );
    }

    #[tokio::test]
    async fn supervisor_unpaused_parent_error_remains_terminal() {
        let mut fixture = Fixture::new(HeldTurn::Parent, ScriptedFault::ParentError, None).await;
        fixture.start().await;
        fixture.driver.release.add_permits(1);
        let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
        assert!(format!("{error:#}").contains("fixture provider failed"));
        assert_eq!(fixture.task().await.status, TaskStatus::Failed);
        assert_eq!(
            fixture.reason().await.as_deref(),
            Some("parent_operation_failed")
        );
    }

    #[tokio::test]
    async fn supervisor_cancel_observes_worker_registered_while_waiting_for_command_lease() {
        let mut fixture = Fixture::new(HeldTurn::Parent, ScriptedFault::ParentError, None).await;
        fixture.driver.hold_rebind.store(true, Ordering::SeqCst);
        let supervisor = fixture.supervisor.clone();
        let source = fixture.source.clone();
        let mut started = fixture.started.take().unwrap();
        let launch = zeroclaw_spawn::spawn!(async move {
            supervisor
                .launch_after_response(&source, &mut started)
                .await
        });
        timeout(
            Duration::from_secs(1),
            fixture.driver.rebind_entered.acquire(),
        )
        .await
        .unwrap()
        .unwrap()
        .forget();
        let supervisor = fixture.supervisor.clone();
        let config = fixture.config.clone();
        let driver = fixture.driver.clone();
        let cancel = zeroclaw_spawn::spawn!(async move {
            supervisor
                .submit(
                    &host_settings(&config),
                    matrix_ingress(),
                    driver,
                    GoalCommand::Cancel,
                )
                .await
        });
        timeout(Duration::from_secs(1), async {
            while fixture.driver.binds.load(Ordering::SeqCst) < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fixture.driver.release_rebind.add_permits(1);
        launch.await.unwrap().unwrap();
        let mut cancel = timeout(Duration::from_secs(1), cancel)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            cancel.response(),
            GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Cancel)
        ));
        fixture.driver.release.add_permits(1);
        assert!(matches!(
            fixture
                .supervisor
                .launch_after_response(&fixture.source, &mut cancel)
                .await
                .unwrap(),
            Some(GoalResponse::Cancelled(_))
        ));
        assert_eq!(fixture.task().await.status, TaskStatus::Cancelled);
        assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
    }

    #[tokio::test]
    async fn supervisor_worker_panic_closes_queued_checkpoint_and_normal_drain_fails_missing_completion()
     {
        for accounting in [None, Some(GoalAccountingState::OutcomeUnknown)] {
            let mut fixture =
                Fixture::new(HeldTurn::Parent, ScriptedFault::ParentPanic, accounting).await;
            fixture.start().await;
            let zeroclaw_goal::GoalControlAdmission::Queued { deferred, .. } = fixture
                .supervisor
                .admit_control(
                    &host_settings(&fixture.config),
                    matrix_ingress(),
                    fixture.driver.clone(),
                    GoalCommand::Pause { immediate: false },
                )
                .await
                .unwrap()
            else {
                panic!("active parent must queue the control");
            };
            assert_eq!(fixture.intent_states(), vec![("pending".to_owned(), None)]);
            fixture.driver.release.add_permits(1);
            let closed = match timeout(Duration::from_secs(1), deferred.wait())
                .await
                .unwrap()
            {
                Err(closed) => closed,
                Ok(_) => panic!("a panicked worker must not admit the queued control"),
            };
            assert!(
                std::error::Error::source(&closed)
                    .unwrap()
                    .is::<zeroclaw_goal::GoalCheckpointClosed>()
            );
            let settled_intents = vec![(
                "superseded".to_owned(),
                Some("checkpoint_closed".to_owned()),
            )];
            assert_eq!(fixture.intent_states(), settled_intents);
            let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
            assert!(format!("{error:#}").contains("stopped without reporting its completion"));
            assert_eq!(fixture.task().await.status, TaskStatus::Failed);
            let goal = fixture.goal().await;
            assert_eq!(
                goal.accounting_state,
                accounting.unwrap_or(GoalAccountingState::Complete)
            );
            assert!(goal.pending_call_id.is_none());
            assert!(goal.pending_call_epoch.is_none());
            assert_eq!(
                fixture.reason().await.as_deref(),
                Some(if accounting.is_some() {
                    "accounting_outcome_unknown"
                } else {
                    "executor_failed"
                })
            );
            assert_eq!(fixture.intent_states(), settled_intents);
            assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
        }
    }

    #[tokio::test]
    async fn supervisor_missing_completion_is_terminal_even_after_operator_pause() {
        let mut fixture = Fixture::new(HeldTurn::Parent, ScriptedFault::ParentPanic, None).await;
        fixture.start().await;
        let _pause = fixture.pause().await;
        fixture.driver.release.add_permits(1);
        let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
        assert!(format!("{error:#}").contains("stopped without reporting its completion"));
        assert_eq!(fixture.task().await.status, TaskStatus::Failed);
        assert_eq!(fixture.reason().await.as_deref(), Some("executor_failed"));
        assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
    }
}

const SCRIPTED_COMPLETE: &str =
    r#"{"decision":"complete","reason":"all criteria met","blockers":[]}"#;

fn real_goal_accounting(
    fixture: &EngineFixture,
) -> (
    Arc<zeroclaw_config::cost::CostTracker>,
    Arc<zeroclaw_runtime::goal_accounting::RuntimeGoalOperationAccountingFactory>,
) {
    let tracker = Arc::new(
        zeroclaw_config::cost::CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            fixture._directory.path(),
        )
        .unwrap(),
    );
    let pricer = Arc::new(
        zeroclaw_runtime::goal_accounting::ModelProviderGoalUsagePricer::new(Arc::new(
            std::collections::HashMap::new(),
        )),
    );
    let factory = Arc::new(
        zeroclaw_runtime::goal_accounting::RuntimeGoalOperationAccountingFactory::new(
            fixture.store.clone() as Arc<dyn GoalTaskRegistry>,
            Arc::clone(&tracker),
            "main",
            pricer,
        )
        .unwrap(),
    );
    (tracker, factory)
}

#[tokio::test]
async fn engine_surfaces_abandoned_real_accountant_ledger_failure_before_verifier() {
    let mut fixture = EngineFixture::new(
        &["candidate"],
        &[],
        Some(ScriptedFault::AbandonWithBlockedLedger),
    )
    .await;
    let (_tracker, factory) = real_goal_accounting(&fixture);
    let error = fixture.run_with_factory(factory).await.unwrap_err();
    assert!(format!("{error:#}").contains("cost storage"));
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
    assert!(
        !fixture
            .steps()
            .iter()
            .any(|step| matches!(step, ScriptedStep::Verifier(_)))
    );
}

#[tokio::test]
async fn engine_waits_for_dropped_real_settlement_before_verifying() {
    let mut fixture = EngineFixture::new(
        &["candidate"],
        &[SCRIPTED_COMPLETE],
        Some(ScriptedFault::DropSettlement),
    )
    .await;
    let (tracker, factory) = real_goal_accounting(&fixture);
    let outcome = fixture.run_with_factory(factory).await.unwrap();
    assert_eq!(outcome, zeroclaw_goal::GoalExecutionOutcome::Completed);
    assert!(
        fixture
            .steps()
            .iter()
            .any(|step| matches!(step, ScriptedStep::Verifier(_)))
    );
    assert_eq!(
        tracker
            .get_strict_usage_totals_for_task_with_pricing(fixture.scope.task_id())
            .unwrap()
            .0,
        12
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.driver.ledger_file)
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn engine_persists_a_verified_candidate_before_completing() {
    // Completion follows the single durable candidate append and verifier.
    let mut fixture = EngineFixture::new(&["final answer"], &[SCRIPTED_COMPLETE], None).await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Completed
    );
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "final answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
            ScriptedStep::Verifier("final answer".into()),
        ]
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Completed);
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "final answer".into())]
    );
}

#[tokio::test]
async fn engine_task_read_failure_after_append_fails_the_epoch() {
    let mut fixture = EngineFixture::new(
        &["answer"],
        &[],
        Some(ScriptedFault::CorruptTaskAfterAppend),
    )
    .await;
    let error = fixture.run().await.unwrap_err();
    assert!(format!("{error:#}").contains("failed to read Goal lifecycle after an operation"));
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
        ]
    );
    assert_eq!(
        fixture.raw_status_and_reason(),
        ("failed".into(), Some("lifecycle_persistence_failed".into()))
    );
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("lifecycle_persistence_failed")
    );
}

#[tokio::test]
async fn engine_task_read_failure_after_continue_fails_before_another_parent() {
    let feedback = r#"{"decision":"continue","reason":"work remains","blockers":[]}"#;
    let mut fixture = EngineFixture::new(
        &["answer"],
        &[feedback],
        Some(ScriptedFault::CorruptTaskAfterContinue),
    )
    .await;
    let error = fixture.run().await.unwrap_err();
    assert!(format!("{error:#}").contains("failed to read Goal lifecycle after an operation"));
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
            ScriptedStep::Verifier("answer".into()),
        ]
    );
    assert_eq!(
        fixture.raw_status_and_reason(),
        ("failed".into(), Some("lifecycle_persistence_failed".into()))
    );
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("lifecycle_persistence_failed")
    );
}

#[tokio::test]
async fn engine_completion_write_failure_fails_the_epoch() {
    let mut fixture = EngineFixture::new(&["answer"], &[SCRIPTED_COMPLETE], None).await;
    fixture.reject_status_write("completed");
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("failed to record Goal completion"));
    assert!(rendered.contains("fixture status write failed"));
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("lifecycle_persistence_failed")
    );
}

#[tokio::test]
async fn engine_verifier_pause_write_failure_fails_the_epoch() {
    let blocked = r#"{"decision":"blocked","reason":"choice required","blockers":[{"kind":"needs_user_input","message":"Choose one"}]}"#;
    let mut fixture = EngineFixture::new(&["Choose one"], &[blocked], None).await;
    fixture.reject_status_write("paused");
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("failed to record Goal verifier pause"));
    assert!(rendered.contains("fixture status write failed"));
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("lifecycle_persistence_failed")
    );
}

#[tokio::test]
async fn engine_blocked_verifier_does_not_replace_winning_operator_pause() {
    let blocked = r#"{"decision":"blocked","reason":"choice required","blockers":[{"kind":"needs_user_input","message":"Choose one"}]}"#;
    let mut fixture = EngineFixture::new(
        &["Choose one"],
        &[blocked],
        Some(ScriptedFault::PauseDuringVerifier),
    )
    .await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Fenced
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
    assert_eq!(
        fixture
            .store
            .get_goal_task(fixture.scope.task_id())
            .await
            .unwrap()
            .unwrap()
            .pause_reason,
        Some(GoalPauseReason::OperatorPaused)
    );
    assert!(fixture.reason().await.is_none());
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "Choose one".into())]
    );
}

#[tokio::test]
async fn engine_candidate_persistence_failure_fails_without_verifying_or_completing() {
    // An append failure ends the exact Goal without admitting its verifier.
    let mut fixture = EngineFixture::new(&["answer"], &[], Some(ScriptedFault::FailAppend)).await;
    let error = fixture.run().await.unwrap_err();
    assert!(format!("{error:#}").contains("fixture append failed"));
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("candidate_persistence_failed")
    );
    assert!(fixture.history().is_empty());
}

#[tokio::test]
async fn engine_empty_candidate_keeps_its_error_when_pause_wins() {
    let mut fixture =
        EngineFixture::new(&[""], &[], Some(ScriptedFault::PauseThenEmptyParent)).await;
    let error = fixture.run().await.unwrap_err();
    let chain = format!("{error:#}");
    assert!(chain.contains("Goal parent returned an empty candidate"));
    assert!(chain.contains("Goal failure lost its execution fence"));
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
}

#[tokio::test]
async fn engine_invalid_verifier_keeps_its_error_when_pause_wins() {
    let mut fixture = EngineFixture::new(
        &["answer"],
        &["not valid JSON"],
        Some(ScriptedFault::PauseDuringVerifier),
    )
    .await;
    let error = fixture.run().await.unwrap_err();
    let chain = format!("{error:#}");
    assert!(chain.contains("Goal verifier response is invalid"));
    assert!(chain.contains("Goal failure lost its execution fence"));
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
}

#[tokio::test]
async fn engine_unavailable_pricing_keeps_its_error_when_pause_wins() {
    let mut fixture = EngineFixture::new(&["answer"], &[], None).await;
    assert_eq!(
        fixture
            .store
            .update_session_goal_limits(
                fixture.scope.task_id(),
                fixture.scope.session_id(),
                fixture.scope.execution_epoch(),
                Some(100),
                Some(1.0),
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    let factory = Arc::new(PauseDuringPricingFactory(Arc::clone(&fixture.store)));
    let error = fixture.run_with_factory(factory).await.unwrap_err();
    let chain = format!("{error:#}");
    assert!(chain.contains("Goal cost budget lacks complete pricing for actual provider usage"));
    assert!(chain.contains("Goal failure lost its execution fence"));
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
}

#[tokio::test]
async fn engine_pause_during_candidate_persistence_keeps_the_candidate_and_never_completes() {
    // A winning pause keeps the appended answer and prevents verifier admission.
    let mut fixture =
        EngineFixture::new(&["answer"], &[], Some(ScriptedFault::PauseDuringAppend)).await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Fenced
    );
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
        ]
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "answer".into())]
    );
}

#[tokio::test]
async fn engine_pause_during_verification_keeps_the_persisted_candidate_and_never_completes() {
    // A pause committed during verification cannot be overwritten by completion.
    let mut fixture = EngineFixture::new(
        &["answer"],
        &[SCRIPTED_COMPLETE],
        Some(ScriptedFault::PauseDuringVerifier),
    )
    .await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Fenced
    );
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
            ScriptedStep::Verifier("answer".into()),
        ]
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "answer".into())]
    );
}

#[tokio::test]
async fn engine_blocked_question_is_persisted_before_pause_and_retained_on_resume() {
    // Resume reads the durable question and appends its response before new work.
    let payload = serde_json::json!({"options": ["A", "B"]});
    let blocked = serde_json::json!({"decision": "blocked", "reason": "choice required", "blockers": [{"kind": "needs_user_input", "message": "Choose A or B?", "payload": payload}]}).to_string();
    let mut fixture = EngineFixture::new(
        &["Choose A or B?", "Done with B"],
        &[&blocked, SCRIPTED_COMPLETE],
        None,
    )
    .await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::VerifierBlocked
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
    let goal = fixture
        .store
        .get_goal_task(fixture.scope.task_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.pause_reason, Some(GoalPauseReason::NeedsUserInput));
    assert_eq!(goal.blockers[0].payload.as_ref(), Some(&payload));
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "Choose A or B?".into())]
    );
    let (_, request) = fixture
        .runtime
        .submit(
            &host_settings(&fixture.config),
            matrix_ingress(),
            fixture.driver.clone(),
            GoalCommand::Resume {
                response: Some("B".into()),
            },
        )
        .await
        .unwrap()
        .into_parts();
    let request = request.unwrap();
    fixture.scope = request.scope().clone();
    fixture.request = Some(request);
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Completed
    );
    let steps = fixture.steps();
    assert!(
        matches!(&steps[1], ScriptedStep::Append(text, TaskStatus::Running, _) if text == "Choose A or B?")
    );
    assert_eq!(steps[3], ScriptedStep::Resume("B".into()));
    assert_eq!(
        steps[4],
        ScriptedStep::Parent(vec![
            ("assistant".into(), "Choose A or B?".into()),
            ("user".into(), "B".into())
        ])
    );
    assert!(
        matches!(&steps[5], ScriptedStep::Append(text, TaskStatus::Running, _) if text == "Done with B")
    );
    assert_eq!(
        fixture.history(),
        vec![
            ("assistant".into(), "Choose A or B?".into()),
            ("user".into(), "B".into()),
            ("assistant".into(), "Done with B".into())
        ]
    );
}

#[tokio::test]
async fn engine_persists_each_continued_candidate_in_order() {
    // Continued answers persist once while feedback remains only working context.
    let feedback =
        r#"{"decision":"continue","reason":"finish the remaining criterion","blockers":[]}"#;
    let mut fixture = EngineFixture::new(
        &["draft one", "final answer"],
        &[feedback, SCRIPTED_COMPLETE],
        None,
    )
    .await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Completed
    );
    let steps = fixture.steps();
    let appended: Vec<_> = steps
        .iter()
        .filter_map(|step| match step {
            ScriptedStep::Append(text, _, _) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(appended, vec!["draft one", "final answer"]);
    let ScriptedStep::Parent(history) = &steps[3] else {
        panic!("expected the second parent turn");
    };
    assert_eq!(
        history[history.len() - 2],
        ("assistant".into(), "draft one".into())
    );
    assert_eq!(history.last().unwrap().0, "user");
    assert!(
        history
            .last()
            .unwrap()
            .1
            .contains("finish the remaining criterion")
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Completed);
    assert_eq!(
        fixture.history(),
        vec![
            ("assistant".into(), "draft one".into()),
            ("assistant".into(), "final answer".into())
        ]
    );
}

#[tokio::test]
async fn engine_parent_accounting_failure_fails_as_accounting_with_its_diagnostic() {
    // Context layers preserve typed classification and the original diagnostic.
    let mut fixture =
        EngineFixture::new(&[], &[], Some(ScriptedFault::ParentAccountingFailure)).await;
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("fixture pricing failed"));
    assert!(!rendered.contains("Goal parent operation failed"));
    assert!(
        error
            .chain()
            .any(|cause| cause.is::<GoalAccountingFailure>())
    );
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
}

#[tokio::test]
async fn engine_incomplete_accounting_names_the_durable_condition() {
    // A successful parent over durable Invalid accounting cannot append or verify.
    let mut fixture =
        EngineFixture::new(&["candidate"], &[], Some(ScriptedFault::SettleIncomplete)).await;
    let error = fixture.run().await.unwrap_err();
    assert!(error.to_string().contains("Goal accounting is incomplete"));
    assert!(error.to_string().contains("invalid or could not be priced"));
    assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
}

#[tokio::test]
async fn engine_empty_candidate_over_incomplete_accounting_fails_as_accounting() {
    // Accounting failure takes precedence over an empty parent response.
    let mut fixture =
        EngineFixture::new(&["   "], &[], Some(ScriptedFault::SettleIncomplete)).await;
    let error = fixture.run().await.unwrap_err();
    assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert!(fixture.history().is_empty());
}

#[tokio::test]
async fn engine_empty_candidate_fails_without_append_or_verifier() {
    // Complete accounting does not make an empty response a candidate.
    let mut fixture = EngineFixture::new(&["   "], &[], None).await;
    let error = fixture.run().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Goal parent returned an empty candidate")
    );
    assert_eq!(fixture.reason().await.as_deref(), Some("candidate_empty"));
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert!(fixture.history().is_empty());
}

#[tokio::test]
async fn engine_torn_ledger_after_settlement_is_an_accounting_failure() {
    let mut fixture = EngineFixture::new(
        &["candidate"],
        &[],
        Some(ScriptedFault::CorruptLedgerAfterParent),
    )
    .await;
    assert_eq!(
        fixture
            .store
            .update_session_goal_limits(
                fixture.scope.task_id(),
                fixture.scope.session_id(),
                fixture.scope.execution_epoch(),
                Some(100),
                Some(1.0),
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    let tracker = Arc::new(
        zeroclaw_config::cost::CostTracker::new(
            zeroclaw_config::schema::CostConfig::default(),
            fixture._directory.path(),
        )
        .unwrap(),
    );
    let rates = std::collections::HashMap::from([
        ("model.input".to_owned(), 1.0),
        ("model.output".to_owned(), 2.0),
    ]);
    let pricer = Arc::new(
        zeroclaw_runtime::goal_accounting::ModelProviderGoalUsagePricer::new(Arc::new(
            std::collections::HashMap::from([("primary".to_owned(), rates)]),
        )),
    );
    let factory = Arc::new(
        zeroclaw_runtime::goal_accounting::RuntimeGoalOperationAccountingFactory::new(
            fixture.store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            pricer,
        )
        .unwrap(),
    );
    let error = fixture.run_with_factory(factory).await.unwrap_err();
    assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
    assert!(
        error
            .to_string()
            .contains("failed to read Goal accounting after an operation")
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    let ledger = std::fs::read_to_string(&fixture.driver.ledger_file).unwrap();
    let mut lines = ledger.lines();
    let record: zeroclaw_config::cost::types::CostRecord =
        serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(record.usage.total_tokens, 12);
    assert_eq!(record.task_id.as_deref(), Some(fixture.scope.task_id()));
    assert_eq!(lines.next(), Some("{\"partial\":"));
    assert_eq!(lines.next(), None);
}

async fn assert_pricing_read_failure_settles_epoch(fail_on_call: usize) {
    let mut fixture = EngineFixture::new(&["answer"], &[SCRIPTED_COMPLETE], None).await;
    assert_eq!(
        fixture
            .store
            .update_session_goal_limits(
                fixture.scope.task_id(),
                fixture.scope.session_id(),
                fixture.scope.execution_epoch(),
                Some(100),
                Some(1.0),
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    let factory = Arc::new(FailingPricingFactory {
        fail_on_call,
        pause_before_failure: None,
    });
    let error = fixture.run_with_factory(factory).await.unwrap_err();
    assert!(format!("{error:#}").contains("fixture pricing ledger read failed"));
    assert!(
        error
            .to_string()
            .contains("fixture pricing ledger read failed")
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("pricing_unavailable")
    );
    let steps = fixture.steps();
    if fail_on_call == 1 {
        assert_eq!(steps, vec![ScriptedStep::Parent(Vec::new())]);
    } else {
        assert!(matches!(steps.last(), Some(ScriptedStep::Verifier(_))));
        assert_eq!(
            fixture.history(),
            vec![("assistant".into(), "answer".into())]
        );
    }
}

#[tokio::test]
async fn engine_parent_pricing_read_failure_settles_epoch() {
    assert_pricing_read_failure_settles_epoch(1).await;
}

#[tokio::test]
async fn engine_verifier_pricing_read_failure_settles_epoch() {
    assert_pricing_read_failure_settles_epoch(2).await;
}

#[tokio::test]
async fn engine_pricing_read_failure_preserves_winning_pause() {
    let mut fixture = EngineFixture::new(&["answer"], &[], None).await;
    assert_eq!(
        fixture
            .store
            .update_session_goal_limits(
                fixture.scope.task_id(),
                fixture.scope.session_id(),
                fixture.scope.execution_epoch(),
                Some(100),
                Some(1.0),
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    let factory = Arc::new(FailingPricingFactory {
        fail_on_call: 1,
        pause_before_failure: Some(Arc::clone(&fixture.store)),
    });
    let error = fixture.run_with_factory(factory).await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("fixture pricing ledger read failed"));
    assert!(rendered.contains("Goal failure lost its execution fence"));
    assert!(
        error
            .to_string()
            .contains("fixture pricing ledger read failed")
    );
    assert!(
        error
            .to_string()
            .contains("Goal failure lost its execution fence")
    );
    assert_eq!(
        error
            .to_string()
            .matches("fixture pricing ledger read failed")
            .count(),
        1
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
}

async fn assert_goal_record_read_failure_settles_epoch(fault: ScriptedFault) {
    let mut fixture = EngineFixture::new(&["answer"], &[SCRIPTED_COMPLETE], Some(fault)).await;
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("failed to read Goal accounting after an operation"));
    assert!(rendered.contains("get goal task"));
    assert!(error.to_string().contains("get goal task"));
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
}

#[tokio::test]
async fn engine_parent_goal_record_read_failure_settles_epoch() {
    assert_goal_record_read_failure_settles_epoch(ScriptedFault::CorruptGoalAfterParent).await;
}

#[tokio::test]
async fn engine_verifier_goal_record_read_failure_settles_epoch() {
    assert_goal_record_read_failure_settles_epoch(ScriptedFault::CorruptGoalAfterVerifier).await;
}

#[tokio::test]
async fn engine_parent_unknown_outcome_preserves_error_and_accounting_reason() {
    let mut fixture = EngineFixture::new(&[], &[], Some(ScriptedFault::ParentOutcomeUnknown)).await;
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("fixture parent interrupted"));
    assert!(rendered.contains("outcome of an earlier Goal model call is unknown"));
    assert!(error.to_string().contains("fixture parent interrupted"));
    assert!(
        error
            .to_string()
            .contains("outcome of an earlier Goal model call is unknown")
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
}

#[tokio::test]
async fn engine_fails_after_fallback_candidate_with_unknown_attempt_accounting() {
    let mut fixture = EngineFixture::new(
        &["fallback candidate"],
        &[SCRIPTED_COMPLETE],
        Some(ScriptedFault::ParentFallbackOutcomeUnknown),
    )
    .await;
    let (tracker, factory) = real_goal_accounting(&fixture);
    let result = fixture.run_with_factory(factory).await;
    assert!(result.is_err());
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
    assert!(
        !fixture
            .steps()
            .iter()
            .any(|step| matches!(step, ScriptedStep::Verifier(_)))
    );
    assert_eq!(
        tracker
            .get_strict_usage_totals_for_task_with_pricing(fixture.scope.task_id())
            .unwrap()
            .0,
        30
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.driver.ledger_file)
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[tokio::test]
async fn engine_verifier_unknown_outcome_preserves_error_and_accounting_reason() {
    let mut fixture = EngineFixture::new(
        &["answer"],
        &[],
        Some(ScriptedFault::VerifierOutcomeUnknown),
    )
    .await;
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("fixture verifier interrupted"));
    assert!(rendered.contains("outcome of an earlier Goal model call is unknown"));
    assert!(error.to_string().contains("fixture verifier interrupted"));
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "answer".into())]
    );
}

#[tokio::test]
async fn engine_unknown_outcome_does_not_overwrite_winning_pause() {
    // This direct-engine observation is intermediate: the supervisor test
    // `supervisor_incomplete_accounting_still_fails_after_operator_pause`
    // covers the later terminal classification.
    let mut fixture = EngineFixture::new(
        &[],
        &[],
        Some(ScriptedFault::PauseAfterParentOutcomeUnknown),
    )
    .await;
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("fixture parent interrupted"));
    assert!(rendered.contains("outcome of an earlier Goal model call is unknown"));
    assert!(rendered.contains("Goal failure lost its execution fence"));
    assert!(error.to_string().contains("fixture parent interrupted"));
    assert!(
        error
            .to_string()
            .contains("outcome of an earlier Goal model call is unknown")
    );
    assert_eq!(
        error
            .to_string()
            .matches("fixture parent interrupted")
            .count(),
        1
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
}

#[tokio::test]
async fn engine_parent_operation_error_keeps_its_classification() {
    // This error occurs before any physical provider attempt is recorded.
    let mut fixture = EngineFixture::new(&[], &[], Some(ScriptedFault::ParentError)).await;
    let error = fixture.run().await.unwrap_err();
    assert!(format!("{error:#}").contains("Goal parent operation failed"));
    assert!(format!("{error:#}").contains("fixture provider failed"));
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("parent_operation_failed")
    );
}

#[tokio::test]
async fn engine_classifies_real_anthropic_fallback_accounting() {
    use std::collections::HashMap;

    const CHILD_ENV: &str = "ZEROCLAW_GOAL_ANTHROPIC_FALLBACK_TEST_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        // The provider's HTTP-client cache is process-wide and must not reuse
        // connections owned by another test's Tokio runtime.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "engine_classifies_real_anthropic_fallback_accounting",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("run isolated Anthropic Goal test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "isolated test failed: {}\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    for case in [
        AnthropicCase::Handoff,
        AnthropicCase::HandoffRefusal,
        AnthropicCase::NoHandoff,
        AnthropicCase::MissingTerminalModel,
        AnthropicCase::CompleteRefusal,
        AnthropicCase::CompleteHandoff,
        AnthropicCase::CompleteHandoffStreamed,
        AnthropicCase::UnattributedTerminal,
        AnthropicCase::CostLimitedUnpricedHandoff,
    ] {
        let cost_limited_unpriced = matches!(case, AnthropicCase::CostLimitedUnpricedHandoff);
        let mut fixture = EngineFixture::new(
            &[],
            &[SCRIPTED_COMPLETE],
            Some(ScriptedFault::ParentAnthropic(case)),
        )
        .await;
        assert_eq!(
            fixture
                .store
                .update_session_goal_limits(
                    fixture.scope.task_id(),
                    fixture.scope.session_id(),
                    fixture.scope.execution_epoch(),
                    Some(1_000),
                    cost_limited_unpriced.then_some(1.0),
                )
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        let (tracker, _) = real_goal_accounting(&fixture);
        let mut rates = HashMap::from([
            ("model-a.input".to_owned(), 1.0),
            ("model-a.output".to_owned(), 2.0),
            ("model-a.cached_input".to_owned(), 0.5),
            ("model-a.cache_write".to_owned(), 1.5),
            ("model-b.input".to_owned(), 3.0),
            ("model-b.output".to_owned(), 4.0),
            ("(unattributed).input".to_owned(), 99.0),
            ("(unattributed).output".to_owned(), 99.0),
        ]);
        if cost_limited_unpriced {
            rates.remove("model-b.input");
            rates.remove("model-b.output");
        }
        let pricing = Arc::new(HashMap::from([("primary".to_owned(), rates)]));
        let factory = Arc::new(
            zeroclaw_runtime::goal_accounting::RuntimeGoalOperationAccountingFactory::new(
                fixture.store.clone() as Arc<dyn GoalTaskRegistry>,
                Arc::clone(&tracker),
                "main",
                Arc::new(
                    zeroclaw_runtime::goal_accounting::ModelProviderGoalUsagePricer::new(
                        Arc::clone(&pricing),
                    ),
                ),
            )
            .unwrap(),
        );
        let streamed = matches!(case, AnthropicCase::CompleteHandoffStreamed);
        let result = if streamed {
            use zeroclaw_runtime::agent::cost::{
                TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext,
            };
            let context = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), pricing);
            TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(Some(context), fixture.run_with_factory(factory))
                .await
        } else {
            fixture.run_with_factory(factory).await
        };
        let steps = fixture.steps();
        assert_eq!(
            steps
                .iter()
                .filter(|step| matches!(step, ScriptedStep::Parent(_)))
                .count(),
            1
        );
        let complete_refusal = matches!(case, AnthropicCase::CompleteRefusal);
        let no_handoff = matches!(case, AnthropicCase::NoHandoff);
        let complete_handoff = matches!(
            case,
            AnthropicCase::CompleteHandoff | AnthropicCase::CompleteHandoffStreamed
        );
        let unattributed = matches!(case, AnthropicCase::UnattributedTerminal);
        let chain = matches!(
            case,
            AnthropicCase::MissingTerminalModel
                | AnthropicCase::CompleteRefusal
                | AnthropicCase::CompleteHandoff
                | AnthropicCase::CompleteHandoffStreamed
                | AnthropicCase::UnattributedTerminal
                | AnthropicCase::CostLimitedUnpricedHandoff
        );
        if cost_limited_unpriced {
            assert!(result.is_err(), "{case:?}");
            assert_eq!(fixture.task().await.status, TaskStatus::Failed);
            assert_eq!(
                fixture.reason().await.as_deref(),
                Some("pricing_unavailable")
            );
            assert!(matches!(
                steps.as_slice(),
                [ScriptedStep::Parent(_), ScriptedStep::AdmissionRefused(message)]
                    if message.contains("Goal cost budget lacks complete pricing")
            ));
        } else if !no_handoff && !complete_handoff {
            assert!(result.is_err(), "{case:?}");
            if complete_refusal {
                assert!(
                    result
                        .as_ref()
                        .unwrap_err()
                        .downcast_ref::<zeroclaw_api::model_provider::ModelRefusalError>()
                        .is_some()
                );
            }
            assert_eq!(fixture.task().await.status, TaskStatus::Failed);
            assert_eq!(
                fixture.reason().await.as_deref(),
                Some(if complete_refusal {
                    "parent_operation_failed"
                } else {
                    "accounting_missing_or_invalid"
                }),
                "{case:?}"
            );
            assert!(
                !steps
                    .iter()
                    .any(|step| matches!(step, ScriptedStep::Verifier(_)))
            );
        } else {
            assert!(result.is_ok(), "{case:?}: {result:?}");
            assert_eq!(fixture.task().await.status, TaskStatus::Completed);
            assert!(
                steps
                    .iter()
                    .any(|step| matches!(step, ScriptedStep::Verifier(_)))
            );
        }
        let goal = fixture
            .store
            .get_goal_task(fixture.scope.task_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            goal.accounting_state,
            if no_handoff || complete_handoff || complete_refusal || cost_limited_unpriced {
                GoalAccountingState::Complete
            } else {
                GoalAccountingState::OutcomeUnknown
            }
        );
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(fixture.scope.task_id())
                .unwrap()
                .0,
            if streamed {
                34
            } else if chain {
                165
            } else {
                55
            }
        );
        let ledger = std::fs::read_to_string(&fixture.driver.ledger_file).unwrap();
        let rows: Vec<zeroclaw_config::cost::types::CostRecord> = ledger
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), if chain { 2 } else { 1 }, "{case:?}");
        assert!(
            rows[..rows.len() - 1]
                .iter()
                .all(|row| row.usage.pricing_available)
        );
        if chain {
            assert_eq!(rows[0].usage.model, "model-a");
            assert_eq!(rows[0].usage.total_tokens, if streamed { 17 } else { 110 });
        }
        let terminal = rows.last().unwrap();
        assert_eq!(terminal.usage.total_tokens, if streamed { 17 } else { 55 });
        assert_eq!(
            terminal.usage.model,
            if no_handoff {
                "model-a"
            } else if unattributed {
                "(unattributed)"
            } else {
                "model-b"
            }
        );
        if unattributed || cost_limited_unpriced {
            assert!(!terminal.usage.pricing_available);
            assert_eq!(terminal.usage.unpriced_tokens, 55);
            assert_eq!(terminal.usage.cost_usd, 0.0);
        } else {
            assert!(terminal.usage.pricing_available);
        }
        if complete_handoff && !streamed {
            assert!((rows[0].usage.cost_usd - 0.00012).abs() < 1e-12);
            assert!((terminal.usage.cost_usd - 0.00017).abs() < 1e-12);
        }
        if streamed {
            assert!(rows.iter().all(|row| {
                row.task_id.as_deref() == Some(fixture.scope.task_id())
                    && row.provider_ref.as_deref() == Some("primary")
            }));
            assert_eq!(rows[0].usage.input_tokens, 15);
            assert_eq!(rows[0].usage.cached_input_tokens, 4);
            assert_eq!(rows[0].usage.cache_creation_input_tokens, 1);
            assert!((rows[0].usage.cost_usd - 0.000_017_5).abs() < 1e-12);
            assert!((terminal.usage.cost_usd - 0.000_056).abs() < 1e-12);

            let script = fixture.driver.script.lock().unwrap();
            let usage: Vec<_> = script
                .events
                .iter()
                .filter_map(|event| match event {
                    zeroclaw_api::agent::TurnEvent::Usage {
                        provider_ref,
                        model,
                        input_tokens,
                        output_tokens,
                        cost_usd,
                        accepted,
                        ..
                    } => Some((
                        provider_ref,
                        model,
                        *input_tokens,
                        *output_tokens,
                        *cost_usd,
                        *accepted,
                    )),
                    _ => None,
                })
                .collect();
            assert_eq!(usage.len(), 2);
            assert_eq!(
                (
                    usage[0].0.as_str(),
                    usage[0].1.as_str(),
                    usage[0].2,
                    usage[0].3,
                    usage[0].5
                ),
                ("primary", "model-a", Some(15), Some(2), false)
            );
            assert_eq!(
                (
                    usage[1].0.as_str(),
                    usage[1].1.as_str(),
                    usage[1].2,
                    usage[1].3,
                    usage[1].5
                ),
                ("primary", "model-b", Some(12), Some(5), true)
            );
            assert!((usage[0].4.unwrap() - rows[0].usage.cost_usd).abs() < 1e-12);
            assert!((usage[1].4.unwrap() - terminal.usage.cost_usd).abs() < 1e-12);
        }
    }
}

#[tokio::test]
async fn engine_verifier_accounting_failure_fails_as_accounting_after_the_candidate_is_durable() {
    // Verifier accounting failure leaves the already appended parent response.
    let mut fixture = EngineFixture::new(
        &["final answer"],
        &[],
        Some(ScriptedFault::VerifierAccountingFailure),
    )
    .await;
    let error = fixture.run().await.unwrap_err();
    assert!(format!("{error:#}").contains("fixture pricing failed"));
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "final answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
            ScriptedStep::Verifier("final answer".into()),
        ]
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "final answer".into())]
    );
}

#[tokio::test]
async fn engine_verifier_operation_error_keeps_its_classification() {
    // Ordinary verifier errors remain distinct from typed accounting failures.
    let mut fixture =
        EngineFixture::new(&["answer"], &[], Some(ScriptedFault::VerifierError)).await;
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("Goal verifier operation failed"));
    assert!(rendered.contains("fixture verifier failed"));
    assert!(
        !error
            .chain()
            .any(|cause| cause.is::<GoalAccountingFailure>())
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("verifier_operation_failed")
    );
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
            ScriptedStep::Verifier("answer".into()),
        ]
    );
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "answer".into())]
    );
}

#[tokio::test]
async fn engine_verifier_error_after_a_winning_pause_keeps_both_errors_and_the_pause() {
    // A verifier failure cannot overwrite a committed pause or lose its cause.
    let mut fixture = EngineFixture::new(
        &["answer"],
        &[],
        Some(ScriptedFault::PauseThenVerifierError),
    )
    .await;
    let epoch = fixture.scope.execution_epoch();
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("fixture verifier failed"));
    assert!(rendered.contains("Goal verifier operation failed"));
    assert!(rendered.contains("Goal failure lost its execution fence"));
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append("answer".into(), TaskStatus::Running, epoch),
            ScriptedStep::Verifier("answer".into()),
        ]
    );
    let task = fixture.task().await;
    assert_eq!(task.status, TaskStatus::Paused);
    assert_eq!(task.execution_epoch, epoch + 1);
    assert_eq!(
        fixture
            .store
            .get_goal_task(fixture.scope.task_id())
            .await
            .unwrap()
            .unwrap()
            .pause_reason,
        Some(GoalPauseReason::OperatorPaused)
    );
    assert!(fixture.reason().await.is_none());
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "answer".into())]
    );
}

#[tokio::test]
async fn engine_parent_error_after_a_winning_pause_keeps_both_errors_and_the_pause() {
    // A failed stale-epoch terminal write preserves both errors and the pause.
    let mut fixture = EngineFixture::new(&[], &[], Some(ScriptedFault::PauseThenParentError)).await;
    let epoch = fixture.scope.execution_epoch();
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("fixture provider failed"));
    assert!(rendered.contains("Goal parent operation failed"));
    assert!(rendered.contains("Goal failure lost its execution fence"));
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    let task = fixture.task().await;
    assert_eq!(task.status, TaskStatus::Paused);
    assert_eq!(task.execution_epoch, epoch + 1);
    assert_eq!(
        fixture
            .store
            .get_goal_task(fixture.scope.task_id())
            .await
            .unwrap()
            .unwrap()
            .pause_reason,
        Some(GoalPauseReason::OperatorPaused)
    );
    assert!(fixture.reason().await.is_none());
    assert!(fixture.history().is_empty());
}

#[tokio::test]
async fn execution_lease_must_match_trusted_ingress_after_revalidation() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = Arc::new(WrongExecutionLeaseDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        returned_session_key: GoalSessionKey::zero_code("wrong-session").unwrap(),
        execution_acquires: AtomicUsize::new(0),
    });
    let scope = GoalExecutionScope::new("goal-1", ingress.session_key().durable_id(), 1).unwrap();
    let host = GoalExecutionHost::new();
    let submission = host
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    let checkpoint = GoalParentTurnCheckpoint::new();
    let error = match host
        .acquire_execution(&host_settings(&config), submission, &scope, &checkpoint)
        .await
    {
        Ok(_) => panic!("mismatched execution lease must be rejected"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("lease session key"));
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn disabled_goal_mode_cannot_acquire_an_execution_lease() {
    let config = valid_goal_config(false);
    let enabled_config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let scope = GoalExecutionScope::new("goal-1", ingress.session_key().durable_id(), 1).unwrap();
    let host = GoalExecutionHost::new();
    let submission = host
        .submit(
            &host_settings(&enabled_config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    let checkpoint = GoalParentTurnCheckpoint::new();
    let error = match host
        .acquire_execution(&host_settings(&config), submission, &scope, &checkpoint)
        .await
    {
        Ok(_) => panic!("disabled Goal Mode must not acquire an execution lease"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("disabled"));
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn exact_driver_binding_and_typed_command_are_preserved() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    });
    let command = GoalCommand::Pause { immediate: false };

    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            command.clone(),
        )
        .await
        .unwrap();

    assert_eq!(submission.ingress(), &ingress);
    assert_eq!(submission.command(), &command);
    assert_eq!(driver.binds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn submission_debug_does_not_expose_goal_text_or_raw_principals() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress.clone(),
            recording_driver(&ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "private stop condition".into(),
            },
        )
        .await
        .unwrap();

    let debug = format!("{submission:?}");
    assert!(debug.contains("surface: Matrix"));
    assert!(debug.contains("command_kind: \"start\""));
    assert!(!debug.contains("private stop condition"));
    assert!(!debug.contains("@alice:example.org"));

    let ingress_debug = format!("{ingress:?}");
    assert!(!ingress_debug.contains("@alice:example.org"));
    let scope_debug = format!(
        "{:?}",
        GoalExecutionScope::new(
            "goal-1",
            "matrix_room__room_example_org__alice_example_org",
            1
        )
        .unwrap()
    );
    assert!(!scope_debug.contains("alice_example_org"));
    let parent_debug = format!(
        "{:?}",
        GoalParentTurn {
            objective: "private stop condition".into(),
            working_history: Vec::new(),
        }
    );
    assert!(!parent_debug.contains("private stop condition"));
    let verifier_debug = format!(
        "{:?}",
        GoalVerifierTurn {
            objective: "private stop condition".into(),
            candidate: "private candidate".into(),
        }
    );
    assert!(!verifier_debug.contains("private stop condition"));
    assert!(!verifier_debug.contains("private candidate"));
}

#[test]
fn identical_raw_ids_from_matrix_and_zerocode_are_not_the_same_session() {
    let matrix = GoalSessionKey::matrix("matrix_shared-id").unwrap();
    let zerocode = GoalSessionKey::zero_code("shared-id").unwrap();

    assert_ne!(matrix, zerocode);
    assert_ne!(matrix.durable_id(), zerocode.durable_id());
}

#[test]
fn matrix_history_key_must_already_use_the_canonical_session_form() {
    let raw = "matrix_room:!room:example.org:@alice:example.org";

    assert!(GoalSessionKey::matrix(raw).is_err());
    assert!(GoalSessionKey::matrix(" matrix_room").is_err());
    assert!(GoalSessionKey::zero_code(" same-session").is_err());
}

#[test]
fn trusted_ingress_rejects_invalid_or_cross_surface_authority() {
    let matrix_key =
        GoalSessionKey::matrix("matrix_room__room_example_org__alice_example_org").unwrap();
    let matrix_principal = GoalIngressPrincipal::Matrix {
        raw_mxid: "@alice:example.org".into(),
    };

    assert!(
        GoalIngressContext::trusted(
            matrix_key.clone(),
            "",
            "matrix:primary",
            matrix_principal.clone(),
        )
        .is_err()
    );
    assert!(
        GoalIngressContext::trusted(matrix_key.clone(), "main", "", matrix_principal.clone(),)
            .is_err()
    );
    assert!(
        GoalIngressContext::trusted(
            matrix_key.clone(),
            "main",
            "matrix:primary",
            GoalIngressPrincipal::Matrix {
                raw_mxid: " ".into(),
            },
        )
        .is_err()
    );
    assert!(
        GoalIngressContext::trusted(
            matrix_key,
            "main",
            "matrix:primary",
            GoalIngressPrincipal::ZeroCode {
                tui_id: "connection".into(),
            },
        )
        .is_err()
    );
}

#[test]
fn trusted_ingress_rejects_noncanonical_principal_identity() {
    let session_key =
        GoalSessionKey::matrix("matrix_room__room_example_org__alice_example_org").unwrap();

    let error = GoalIngressContext::trusted(
        session_key,
        "main",
        "matrix:primary",
        GoalIngressPrincipal::Matrix {
            raw_mxid: " @alice:example.org ".into(),
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("not canonical"));
}

#[test]
fn host_settings_do_not_cache_config_debug_content() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let settings = GoalHostSettings::from_config(&config, 42, "test-boot").unwrap();
    let rendered = format!("{settings:?}");
    assert!(!rendered.contains("gpt-test"));
    assert!(!rendered.contains("openai.default"));
}

#[test]
fn host_settings_rejects_a_blank_boot_id() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    assert!(GoalHostSettings::from_config(&config, 42, " \t\n").is_err());
}

#[test]
fn execution_scope_rejects_blank_ids_and_nonpositive_epochs() {
    assert!(GoalExecutionScope::new("", "matrix_session", 1).is_err());
    assert!(GoalExecutionScope::new("goal-1", "", 1).is_err());
    assert!(GoalExecutionScope::new(" goal-1", "matrix_session", 1).is_err());
    assert!(GoalExecutionScope::new("goal-1", "matrix_session ", 1).is_err());
    assert!(GoalExecutionScope::new("goal-1", "matrix_session", 0).is_err());
}

#[tokio::test]
async fn controller_submission_future_is_send() {
    fn assert_send<T: Send>(_: T) {}

    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress.clone(),
            recording_driver(&ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    assert_send(controller.submit(&host_settings(&config), &submission));
}

#[tokio::test]
async fn controller_uses_only_a_host_validated_submission_for_lifecycle_transitions() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    });
    let host = GoalExecutionHost::new();

    let start = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "stop when the implementation is complete".into(),
            },
        )
        .await
        .unwrap();
    let (response, _start) = controller
        .submit_for_execution(&host_settings(&config), start)
        .await
        .unwrap();
    let GoalResponse::Started(started) = response else {
        panic!("expected a started Goal");
    };
    assert_eq!(started.execution_epoch, 1);
    assert_eq!(started.token_limit, Some(100));
    assert_eq!(started.cost_limit_usd, Some(1.0));
    let stored_task = store
        .current_goal_for_session(&ingress.session_key().durable_id())
        .await
        .unwrap()
        .expect("the started Goal must be current");
    let stored_goal = store
        .get_goal_task(&started.task_id)
        .await
        .unwrap()
        .expect("the started Goal extension must exist");
    assert_eq!(
        stored_task.principal_id.as_deref(),
        Some("@alice:example.org")
    );
    assert_eq!(
        stored_goal.objective,
        "stop when the implementation is complete"
    );

    let pause = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap();
    let GoalResponse::Paused(paused) = controller
        .submit(&host_settings(&config), &pause)
        .await
        .unwrap()
    else {
        panic!("expected a paused Goal");
    };
    assert_eq!(paused.execution_epoch, 2);

    let resume = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    let GoalResponse::Resumed(resumed) = controller
        .submit(&host_settings(&config), &resume)
        .await
        .unwrap()
    else {
        panic!("expected a resumed Goal");
    };
    assert_eq!(resumed.execution_epoch, 3);

    let cancel = host
        .submit(
            &host_settings(&config),
            ingress,
            driver,
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));
}

#[tokio::test]
async fn controller_distinguishes_active_goals_from_terminal_goals_with_unsettled_operations() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();

    let start_command = || GoalCommand::Start {
        budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
        objective: "finish the assigned task".into(),
    };
    let start = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            start_command(),
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&config), &start)
        .await
        .unwrap()
    else {
        panic!("expected a started Goal");
    };

    let active_start = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            start_command(),
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &active_start)
            .await
            .unwrap(),
        GoalResponse::AlreadyActive
    ));

    assert_eq!(
        store
            .admit_pending_operation(
                &started.task_id,
                &ingress.session_key().durable_id(),
                started.execution_epoch,
                "operation-under-settlement",
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    let cancel = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));

    let terminal_start = host
        .submit(&host_settings(&config), ingress, driver, start_command())
        .await
        .unwrap();
    let GoalResponse::Terminal(terminal) = controller
        .submit(&host_settings(&config), &terminal_start)
        .await
        .unwrap()
    else {
        panic!("a terminal Goal with an unsettled operation must not be called active");
    };
    assert_eq!(terminal.status, TaskStatus::Cancelled);
    assert!(!terminal.resumable);
}

#[tokio::test]
async fn resume_projects_a_paused_goal_that_is_not_yet_resumable() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();

    let start = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the assigned task".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&config), &start)
        .await
        .unwrap()
    else {
        panic!("expected a started Goal");
    };
    assert_eq!(
        store
            .admit_pending_operation(
                &started.task_id,
                &ingress.session_key().durable_id(),
                started.execution_epoch,
                "operation-still-settling",
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );

    // Only an immediate operator pause may fence this unsettled operation;
    // plain pause belongs to the supervisor's settled-turn checkpoint.
    let pause = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &pause)
            .await
            .unwrap(),
        GoalResponse::Paused(_)
    ));

    let resume = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    let GoalResponse::Status(status) = controller
        .submit(&host_settings(&config), &resume)
        .await
        .unwrap()
    else {
        panic!("resume must project the durable non-resumable paused state");
    };
    assert_eq!(status.status, TaskStatus::Paused);
    assert!(!status.resumable);

    assert_eq!(
        store
            .settle_pending_operation(
                &started.task_id,
                &ingress.session_key().durable_id(),
                started.execution_epoch,
                "operation-still-settling",
                GoalAccountingState::Invalid,
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );

    let resume = host
        .submit(
            &host_settings(&config),
            ingress,
            driver,
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    let GoalResponse::Status(status) = controller
        .submit(&host_settings(&config), &resume)
        .await
        .unwrap()
    else {
        panic!("incomplete accounting must project a durable non-resumable paused state");
    };
    assert_eq!(status.status, TaskStatus::Paused);
    assert_eq!(status.accounting_state, GoalAccountingState::Invalid);
    assert!(!status.resumable);
}

#[tokio::test]
async fn controller_projects_each_command_state_without_reinterpreting_the_store() {
    let enabled_config = valid_goal_config(true);
    let disabled_config = valid_goal_config(false);
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();

    let help = host
        .submit(
            &host_settings(&disabled_config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Help,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&disabled_config), &help)
            .await
            .unwrap(),
        GoalResponse::Disabled
    ));
    let disabled_status = host
        .submit(
            &host_settings(&disabled_config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&disabled_config), &disabled_status)
            .await
            .unwrap(),
        GoalResponse::Disabled
    ));

    let absent = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &absent)
            .await
            .unwrap(),
        GoalResponse::NoCurrentGoal
    ));

    let start = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the assigned task".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &start)
            .await
            .unwrap(),
        GoalResponse::Started(_)
    ));

    for (command, expected) in [
        (GoalCommand::Status, "status"),
        (GoalCommand::Budget, "budget"),
    ] {
        let submission = host
            .submit(
                &host_settings(&enabled_config),
                ingress.clone(),
                driver.clone(),
                command,
            )
            .await
            .unwrap();
        let response = controller
            .submit(&host_settings(&enabled_config), &submission)
            .await
            .unwrap();
        assert!(
            matches!(
                (&response, expected),
                (GoalResponse::Status(_), "status") | (GoalResponse::Budget(_), "budget")
            ),
            "unexpected response for {expected}: {response:?}"
        );
    }

    let update = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::SetBudget(zeroclaw_commands::goal::GoalBudgetSelection::Limits(
                zeroclaw_commands::goal::GoalBudgetLimits {
                    token_limit: Some(7),
                    cost_limit_usd: None,
                },
            )),
        )
        .await
        .unwrap();
    let GoalResponse::BudgetUpdated(updated) = controller
        .submit(&host_settings(&enabled_config), &update)
        .await
        .unwrap()
    else {
        panic!("expected the guarded budget update projection");
    };
    assert_eq!(updated.token_limit, Some(7));
    assert_eq!(updated.cost_limit_usd, None);

    let pause = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &pause)
            .await
            .unwrap(),
        GoalResponse::Paused(_)
    ));
    let pause_again = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &pause_again)
            .await
            .unwrap(),
        GoalResponse::AlreadyPaused(_)
    ));

    let cancel = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));
    let cancel_again = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &cancel_again)
            .await
            .unwrap(),
        GoalResponse::AlreadyCancelled(_)
    ));
    let terminal_status = host
        .submit(
            &host_settings(&config),
            ingress,
            driver,
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &terminal_status)
            .await
            .unwrap(),
        GoalResponse::Terminal(_)
    ));
}

#[tokio::test]
async fn local_help_does_not_bind_a_live_session_when_goal_mode_is_enabled() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);

    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Help,
        )
        .await
        .unwrap();

    assert_eq!(submission.command(), &GoalCommand::Help);
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &submission)
            .await
            .unwrap(),
        GoalResponse::Help
    ));
}

#[tokio::test]
async fn runtime_does_not_create_execution_for_read_only_goal_commands() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store as Arc<dyn GoalTaskRegistry>);
    let settings = host_settings(&config);
    let ingress = matrix_ingress();
    let driver = Arc::new(ExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        delivered: Arc::new(AtomicUsize::new(0)),
    });

    let (response, execution) = runtime
        .submit(&settings, ingress, driver.clone(), GoalCommand::Status)
        .await
        .unwrap()
        .into_parts();
    assert!(matches!(response, GoalResponse::NoCurrentGoal));
    assert!(execution.is_none());
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn runtime_start_without_budget_defaults_returns_budget_required_without_execution() {
    use zeroclaw_commands::goal::{GoalBudgetLimits, GoalBudgetSelection};

    // Missing defaults stop admission without handing off execution; explicit limits admit it.
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let session_id = ingress.session_key().durable_id();
        let mut config = valid_goal_config(true);
        config.goal.default_token_limit = None;
        config.goal.default_cost_limit_usd = None;
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let settings = host_settings(&config);
        let driver = Arc::new(ExecutionDriver {
            binding: GoalSessionBinding::new(ingress.session_key().clone()),
            execution_acquires: AtomicUsize::new(0),
            delivered: Arc::new(AtomicUsize::new(0)),
        });

        let (response, execution) = runtime
            .submit(
                &settings,
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "start without configured budget defaults".into(),
                },
            )
            .await
            .unwrap()
            .into_parts();
        assert!(matches!(response, GoalResponse::BudgetRequired));
        assert!(execution.is_none());
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
        assert!(
            store
                .current_goal_for_session(&session_id)
                .await
                .unwrap()
                .is_none()
        );

        let (response, execution) = runtime
            .submit(
                &settings,
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Limits(GoalBudgetLimits {
                        token_limit: Some(10),
                        cost_limit_usd: None,
                    }),
                    objective: "start with explicit budget limits".into(),
                },
            )
            .await
            .unwrap()
            .into_parts();
        assert!(matches!(response, GoalResponse::Started(_)));
        assert!(execution.is_some());
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn runtime_rejects_dangling_verifier_for_start_and_resume_but_allows_status() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let invalid = goal_config(
        true,
        "openai.missing",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );

    for command in [
        GoalCommand::Start {
            budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
            objective: "must not start".into(),
        },
        GoalCommand::Resume { response: None },
    ] {
        let error = match runtime
            .submit(
                &host_settings(&invalid),
                ingress.clone(),
                driver.clone(),
                command,
            )
            .await
        {
            Ok(_) => panic!("invalid verifier must reject Start and Resume"),
            Err(error) => error,
        };
        assert_admission_error(
            error,
            ConfigApiCode::DanglingReference,
            "goal.verifier.model_provider",
        );
    }

    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    assert!(
        store
            .current_goal_for_session(&ingress.session_key().durable_id())
            .await
            .unwrap()
            .is_none(),
        "rejected Start and Resume must not create a goal row"
    );

    let (response, execution) = runtime
        .submit(
            &host_settings(&invalid),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap()
        .into_parts();
    assert!(matches!(response, GoalResponse::NoCurrentGoal));
    assert!(execution.is_none());
    assert_eq!(driver.binds.load(Ordering::SeqCst), 1);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn runtime_rechecks_dangling_verifier_before_execution_acquisition() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = Arc::new(ExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        delivered: Arc::new(AtomicUsize::new(0)),
    });
    let valid = valid_goal_config(true);
    let (response, request) = runtime
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "acquire only while the verifier remains configured".into(),
            },
        )
        .await
        .unwrap()
        .into_parts();
    assert!(matches!(response, GoalResponse::Started(_)));
    let request = match request {
        Some(request) => request,
        None => panic!("a valid Start must return its execution request"),
    };

    let invalid = goal_config(
        true,
        "openai.missing",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let error = match runtime
        .acquire_execution(
            &host_settings(&invalid),
            request,
            &GoalParentTurnCheckpoint::new(),
        )
        .await
    {
        Ok(_) => panic!("dangling verifier must refuse execution acquisition"),
        Err(error) => error,
    };
    assert_admission_error(
        error,
        ConfigApiCode::DanglingReference,
        "goal.verifier.model_provider",
    );
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    // Settlement after a refused acquisition belongs to the later executor.

    let fresh_store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let fresh_runtime = GoalRuntime::new(fresh_store as Arc<dyn GoalTaskRegistry>);
    let fresh_ingress = matrix_ingress();
    let fresh_driver = Arc::new(ExecutionDriver {
        binding: GoalSessionBinding::new(fresh_ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        delivered: Arc::new(AtomicUsize::new(0)),
    });
    let fresh_valid = valid_goal_config(true);
    let (response, request) = fresh_runtime
        .submit(
            &host_settings(&fresh_valid),
            fresh_ingress,
            fresh_driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "acquire with fresh valid settings".into(),
            },
        )
        .await
        .unwrap()
        .into_parts();
    assert!(matches!(response, GoalResponse::Started(_)));
    let request = match request {
        Some(request) => request,
        None => panic!("a valid Start must return its execution request"),
    };
    let acquisition_settings = host_settings(&fresh_valid);
    let lease = fresh_runtime
        .acquire_execution(
            &acquisition_settings,
            request,
            &GoalParentTurnCheckpoint::new(),
        )
        .await
        .unwrap();
    drop(lease);
    assert_eq!(fresh_driver.execution_acquires.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn same_zerocode_session_retains_goal_control_after_agent_alias_refresh() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let host = GoalExecutionHost::new();
    let first_ingress = zerocode_ingress("alpha");
    let first_driver = recording_driver(&first_ingress);

    let start = host
        .submit(
            &host_settings(&config),
            first_ingress,
            first_driver,
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the assigned task".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&config), &start)
        .await
        .unwrap()
    else {
        panic!("expected a started Goal");
    };
    let stored_task = store
        .current_goal_for_session("rpc_same-live-session")
        .await
        .unwrap()
        .expect("the started Goal must be current");
    assert_eq!(stored_task.id, started.task_id);
    assert_eq!(stored_task.principal_id, None);

    let refreshed_ingress = zerocode_ingress("beta");
    let refreshed_driver = recording_driver(&refreshed_ingress);
    let status = host
        .submit(
            &host_settings(&config),
            refreshed_ingress.clone(),
            refreshed_driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &status)
            .await
            .unwrap(),
        GoalResponse::Status(_)
    ));

    let cancel = host
        .submit(
            &host_settings(&config),
            refreshed_ingress.clone(),
            refreshed_driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));

    let replacement = host
        .submit(
            &host_settings(&config),
            refreshed_ingress,
            refreshed_driver,
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the reassigned task".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &replacement)
            .await
            .unwrap(),
        GoalResponse::Started(_)
    ));
}

#[tokio::test]
async fn matrix_goal_control_requires_the_exact_raw_principal_after_key_normalization() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let shared_key = "matrix__r_example_org__a_b_example_org";
    let first_ingress = GoalIngressContext::trusted(
        GoalSessionKey::matrix(shared_key).unwrap(),
        "main",
        "matrix:primary",
        GoalIngressPrincipal::Matrix {
            raw_mxid: "@a.b:example.org".into(),
        },
    )
    .unwrap();
    let colliding_ingress = GoalIngressContext::trusted(
        GoalSessionKey::matrix(shared_key).unwrap(),
        "main",
        "matrix:primary",
        GoalIngressPrincipal::Matrix {
            raw_mxid: "@a_b:example.org".into(),
        },
    )
    .unwrap();
    let host = GoalExecutionHost::new();

    let first_start = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the assigned task".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&config), &first_start)
        .await
        .unwrap()
    else {
        panic!("owner should be able to start the Goal");
    };

    for command in [
        GoalCommand::Status,
        GoalCommand::Budget,
        GoalCommand::SetBudget(zeroclaw_commands::goal::GoalBudgetSelection::Unlimited),
        GoalCommand::Pause { immediate: false },
        GoalCommand::Pause { immediate: true },
        GoalCommand::Resume {
            response: Some("a response from another principal".into()),
        },
        GoalCommand::Cancel,
    ] {
        let submission = host
            .submit(
                &host_settings(&config),
                colliding_ingress.clone(),
                recording_driver(&colliding_ingress),
                command,
            )
            .await
            .unwrap();
        assert_eq!(
            controller
                .submit(&host_settings(&config), &submission)
                .await
                .unwrap(),
            GoalResponse::SessionUnavailable
        );
    }
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(
        runtime.execution_engine(Arc::new(ScriptedAccountingFactory)),
    )));
    for command in [
        GoalCommand::Start {
            budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
            objective: "finish a different task".into(),
        },
        GoalCommand::Pause { immediate: false },
        GoalCommand::Resume { response: None },
        GoalCommand::SetBudget(zeroclaw_commands::goal::GoalBudgetSelection::Unlimited),
    ] {
        let admission = supervisor
            .admit_control(
                &host_settings(&config),
                colliding_ingress.clone(),
                recording_driver(&colliding_ingress),
                command,
            )
            .await
            .unwrap();
        let zeroclaw_goal::GoalControlAdmission::Direct(submission) = admission else {
            panic!("a colliding principal must not queue a Goal control");
        };
        assert!(matches!(
            submission.response(),
            GoalResponse::SessionUnavailable
        ));
    }
    let colliding_start = host
        .submit(
            &host_settings(&config),
            colliding_ingress.clone(),
            recording_driver(&colliding_ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish a different task".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &colliding_start)
            .await
            .unwrap(),
        GoalResponse::SessionUnavailable
    ));

    let first_status = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert_eq!(
        controller
            .submit(&host_settings(&config), &first_status)
            .await
            .unwrap(),
        GoalResponse::Status(started)
    );

    let first_cancel = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &first_cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));

    let colliding_terminal_replacement = host
        .submit(
            &host_settings(&config),
            colliding_ingress.clone(),
            recording_driver(&colliding_ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "replace another principal's terminal goal".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &colliding_terminal_replacement)
            .await
            .unwrap(),
        GoalResponse::SessionUnavailable
    ));

    let first_terminal_status = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &first_terminal_status)
            .await
            .unwrap(),
        GoalResponse::Terminal(_)
    ));

    let first_terminal_replacement = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "replace the owner's terminal goal".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &first_terminal_replacement)
            .await
            .unwrap(),
        GoalResponse::Started(_)
    ));

    let first_replacement_status = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &first_replacement_status)
            .await
            .unwrap(),
        GoalResponse::Status(_)
    ));
}

#[tokio::test]
async fn typed_start_objective_is_rejected_before_durable_write() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress.clone(),
            recording_driver(&ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "x".repeat(zeroclaw_commands::goal::MAX_GOAL_OBJECTIVE_CHARS + 1),
            },
        )
        .await
        .unwrap();

    assert!(
        controller
            .submit(&host_settings(&config), &submission)
            .await
            .is_err()
    );
    assert!(
        store
            .current_goal_for_session(&ingress.session_key().durable_id())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn disabled_goal_submission_does_not_acquire_a_session_lease() {
    let config = valid_goal_config(false);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = Arc::new(LeaseDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        available_for_reconnect: Arc::new(AtomicUsize::new(1)),
    });

    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    assert!(
        driver.reconnect_is_allowed(),
        "disabled Goal Mode must not contend the driver's session lease"
    );
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &submission)
            .await
            .unwrap(),
        GoalResponse::Disabled
    ));
    assert!(
        driver.reconnect_is_allowed(),
        "a disabled submission carries no live-session lease"
    );
    drop(submission);
    assert!(
        driver.reconnect_is_allowed(),
        "the host never acquired a lease for disabled Goal Mode"
    );
}

#[tokio::test]
async fn session_lease_stays_held_during_a_guarded_lifecycle_mutation() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let reconnect_available = Arc::new(AtomicUsize::new(1));
    let registry = Arc::new(LeaseObservingRegistry::running_for(
        &ingress,
        reconnect_available.clone(),
    ));
    let controller = GoalController::new(registry.clone() as Arc<dyn GoalTaskRegistry>);
    let driver = Arc::new(LeaseDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        available_for_reconnect: reconnect_available,
    });

    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap();

    let GoalResponse::Paused(_) = controller
        .submit(&host_settings(&config), &submission)
        .await
        .unwrap()
    else {
        panic!("expected a paused Goal");
    };
    assert_eq!(registry.pause_observed.load(Ordering::SeqCst), 1);
    assert!(
        !driver.reconnect_is_allowed(),
        "the borrowed submission must retain its exact driver proof"
    );
    drop(submission);
    assert!(
        driver.reconnect_is_allowed(),
        "the lease may release after the guarded lifecycle submission is settled"
    );
}

async fn status_for_registry(
    config: &GoalHostSettings<'_>,
    ingress: &GoalIngressContext,
    registry: Arc<LeaseObservingRegistry>,
) -> GoalResponse {
    let controller = GoalController::new(registry as Arc<dyn GoalTaskRegistry>);
    let submission = GoalExecutionHost::new()
        .submit(
            config,
            ingress.clone(),
            recording_driver(ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    controller.submit(config, &submission).await.unwrap()
}

#[tokio::test]
async fn status_projection_is_stale_after_same_id_resume_changes_status_and_epoch() {
    let config = valid_goal_config(true);
    let settings = host_settings(&config);
    let ingress = matrix_ingress();
    let registry = Arc::new(LeaseObservingRegistry::paused_with_change_after_goal_read(
        &ingress,
        TaskStatus::Running,
        2,
    ));

    assert!(matches!(
        status_for_registry(&settings, &ingress, registry).await,
        GoalResponse::Stale
    ));
}

#[tokio::test]
async fn status_projection_is_stale_after_same_id_terminal_status_change() {
    let config = valid_goal_config(true);
    let settings = host_settings(&config);
    let ingress = matrix_ingress();
    let registry = Arc::new(LeaseObservingRegistry::paused_with_change_after_goal_read(
        &ingress,
        TaskStatus::Cancelled,
        1,
    ));

    assert!(matches!(
        status_for_registry(&settings, &ingress, registry).await,
        GoalResponse::Stale
    ));
}

#[tokio::test]
async fn status_projection_is_stale_after_same_id_epoch_only_change() {
    let config = valid_goal_config(true);
    let settings = host_settings(&config);
    let ingress = matrix_ingress();
    let registry = Arc::new(LeaseObservingRegistry::paused_with_change_after_goal_read(
        &ingress,
        TaskStatus::Paused,
        2,
    ));

    assert!(matches!(
        status_for_registry(&settings, &ingress, registry).await,
        GoalResponse::Stale
    ));
}

#[tokio::test]
async fn status_projection_preserves_stable_running_and_paused_snapshots() {
    let config = valid_goal_config(true);
    let settings = host_settings(&config);
    let ingress = matrix_ingress();
    let mut running_fixture =
        LeaseObservingRegistry::running_for(&ingress, Arc::new(AtomicUsize::new(1)));
    running_fixture.goal.effective_token_limit = Some(100);
    running_fixture.goal.effective_cost_limit_usd = Some(0.75);
    let running = Arc::new(running_fixture);

    let GoalResponse::Status(running) = status_for_registry(&settings, &ingress, running).await
    else {
        panic!("stable running task should project as status");
    };
    assert_eq!(running.status, TaskStatus::Running);
    assert_eq!(running.execution_epoch, 1);
    assert_eq!(running.pause_reason, None);
    assert_eq!(running.token_limit, Some(100));
    assert_eq!(running.cost_limit_usd, Some(0.75));
    assert_eq!(running.accounting_state, GoalAccountingState::Complete);
    assert!(!running.resumable);

    let mut paused = LeaseObservingRegistry::running_for(&ingress, Arc::new(AtomicUsize::new(1)));
    paused.task.status = TaskStatus::Paused;
    paused.goal.pause_reason = Some(GoalPauseReason::OperatorPaused);
    paused.goal.pause_description = Some("paused by operator".to_owned());
    let GoalResponse::Status(paused) =
        status_for_registry(&settings, &ingress, Arc::new(paused)).await
    else {
        panic!("stable paused task should project as status");
    };
    assert_eq!(paused.status, TaskStatus::Paused);
    assert_eq!(paused.execution_epoch, 1);
    assert_eq!(paused.pause_reason, Some(GoalPauseReason::OperatorPaused));
    assert!(paused.resumable);
}

#[tokio::test]
async fn projection_after_terminal_replacement_is_stale_not_corrupt() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let registry = Arc::new(LeaseObservingRegistry::replaced_before_goal_read(&ingress));
    let controller = GoalController::new(registry as Arc<dyn GoalTaskRegistry>);
    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress.clone(),
            recording_driver(&ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    assert!(matches!(
        controller
            .submit(&host_settings(&config), &submission)
            .await
            .unwrap(),
        GoalResponse::Stale
    ));
}

#[tokio::test]
async fn execution_acquisition_releases_the_admission_lease_first() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = Arc::new(LeaseDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        available_for_reconnect: Arc::new(AtomicUsize::new(1)),
    });
    let host = GoalExecutionHost::new();
    let submission = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    let scope = GoalExecutionScope::new("goal-1", ingress.session_key().durable_id(), 1).unwrap();
    let checkpoint = GoalParentTurnCheckpoint::new();

    let execution = host
        .acquire_execution(&host_settings(&config), submission, &scope, &checkpoint)
        .await
        .unwrap();
    assert!(
        !driver.reconnect_is_allowed(),
        "the execution lease must replace the released admission lease"
    );
    drop(execution);
    assert!(driver.reconnect_is_allowed());
}

#[tokio::test]
async fn supervisor_rechecks_live_config_at_acquisition_and_settles_refusal() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
    let ingress = matrix_ingress();
    let session_id = ingress.session_key().durable_id();
    let parent_started = Arc::new(Notify::new());
    let release_parent = Arc::new(Notify::new());
    let verifier_calls = Arc::new(AtomicUsize::new(0));
    let driver = Arc::new(PausingExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        parent_started: Arc::clone(&parent_started),
        release_parent: Arc::clone(&release_parent),
        verifier_calls: Arc::clone(&verifier_calls),
    });
    let directory = TempDir::new().unwrap();
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
    let accounting_factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor =
        GoalExecutionSupervisor::new(Arc::new(runtime.execution_engine(accounting_factory)));
    let snapshot = live.handle().snapshot();
    let mut started = supervisor
        .submit(
            &host_settings(&snapshot),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "refuse acquisition after the verifier is removed".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started_projection) = started.response() else {
        panic!("valid intake must admit Start");
    };
    let failed_task_id = started_projection.task_id.clone();
    let failed_scope = GoalExecutionScope::new(
        failed_task_id.as_str(),
        session_id.clone(),
        started_projection.execution_epoch,
    )
    .unwrap();
    let mut invalid = live.handle().snapshot();
    invalid.goal.verifier.model_provider = ModelProviderRef::new("openai.missing");
    live.publish(live.next_revision().unwrap(), invalid)
        .unwrap();
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut started)
            .await
            .unwrap()
            .is_none()
    );
    let failed = timeout(Duration::from_secs(1), async {
        loop {
            let current = store
                .current_goal_for_session(&session_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(current.id, failed_task_id);
            if current.status.is_terminal() {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("refused acquisition must settle the admitted epoch");
    assert_eq!(failed.status, TaskStatus::Failed);
    assert_eq!(
        store
            .terminal_reason_for_session_goal(&failed_task_id, &session_id)
            .await
            .unwrap()
            .as_deref(),
        Some("executor_failed")
    );
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    assert!(
        timeout(Duration::from_millis(20), parent_started.notified())
            .await
            .is_err(),
        "refused acquisition must not start a parent turn"
    );
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
    let goal = store.get_goal_task(&failed_task_id).await.unwrap().unwrap();
    assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
    assert!(goal.pending_call_id.is_none());
    assert!(goal.pending_call_epoch.is_none());
    assert!(
        timeout(Duration::from_secs(1), supervisor.drain(&failed_scope))
            .await
            .unwrap()
            .is_err(),
        "the exact refused worker must report its acquisition error"
    );
    drop(started);

    live.publish(live.next_revision().unwrap(), valid_goal_config(true))
        .unwrap();
    let snapshot = live.handle().snapshot();
    let mut restored = supervisor
        .submit(
            &host_settings(&snapshot),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "execute a new goal after restoring valid config".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(restored_projection) = restored.response() else {
        panic!("restored valid config must admit a new Goal");
    };
    assert_ne!(restored_projection.task_id, failed_task_id);
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut restored)
            .await
            .unwrap()
            .is_none()
    );
    timeout(Duration::from_secs(1), parent_started.notified())
        .await
        .expect("the restored valid config must allow the new parent to start");
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 1);
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
    let snapshot = live.handle().snapshot();
    let mut cancelled = supervisor
        .submit(
            &host_settings(&snapshot),
            ingress,
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        cancelled.response(),
        GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Cancel)
    ));
    release_parent.notify_one();
    assert!(matches!(
        timeout(
            Duration::from_secs(1),
            supervisor.launch_after_response(&settings_source, &mut cancelled),
        )
        .await
        .expect("cleanup must drain the admitted parent after cancellation")
        .unwrap(),
        Some(GoalResponse::Cancelled(_))
    ));
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn supervisor_immediate_pause_acknowledges_before_draining_parent() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
    let ingress = matrix_ingress();
    let parent_started = Arc::new(Notify::new());
    let release_parent = Arc::new(Notify::new());
    let verifier_calls = Arc::new(AtomicUsize::new(0));
    let driver = Arc::new(PausingExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        parent_started: Arc::clone(&parent_started),
        release_parent: Arc::clone(&release_parent),
        verifier_calls: Arc::clone(&verifier_calls),
    });
    let directory = TempDir::new().unwrap();
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
    let pricer = Arc::new(ModelProviderGoalUsagePricer::new(Arc::default()));
    let accounting_factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            pricer,
        )
        .unwrap(),
    );
    let engine = Arc::new(runtime.execution_engine(accounting_factory));
    let supervisor = Arc::new(GoalExecutionSupervisor::new(engine));

    let mut started = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(started.response(), GoalResponse::Started(_)));
    assert!(
        timeout(Duration::from_millis(20), parent_started.notified())
            .await
            .is_err(),
        "the admitted worker must wait until its surface delivers the start acknowledgement"
    );
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut started)
            .await
            .unwrap()
            .is_none()
    );
    parent_started.notified().await;

    let mut pause = timeout(
        Duration::from_millis(20),
        supervisor.submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        ),
    )
    .await
    .expect("immediate pause must return before the admitted parent settles")
    .unwrap();
    assert!(matches!(
        pause.response(),
        GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Pause)
    ));

    let drain_supervisor = Arc::clone(&supervisor);
    let drain_source = settings_source.clone();
    let mut draining = zeroclaw_spawn::spawn!(async move {
        drain_supervisor
            .launch_after_response(&drain_source, &mut pause)
            .await
    });
    assert!(
        timeout(Duration::from_millis(20), &mut draining)
            .await
            .is_err(),
        "post-response drain must wait for the admitted parent operation"
    );

    let mut status = timeout(
        Duration::from_millis(20),
        supervisor.submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        ),
    )
    .await
    .expect("draining a fenced parent must not hold the Goal command lease")
    .unwrap();
    assert!(matches!(status.response(), GoalResponse::Status(_)));
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut status)
            .await
            .unwrap()
            .is_none()
    );

    release_parent.notify_one();
    assert!(matches!(
        draining.await.unwrap().unwrap(),
        Some(GoalResponse::Paused(_))
    ));
    assert_eq!(
        verifier_calls.load(Ordering::SeqCst),
        0,
        "a pause may drain an admitted parent operation but cannot admit a verifier"
    );
}

#[tokio::test]
async fn verifier_block_publishes_the_parent_request_before_goal_pause() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
    let ingress = matrix_ingress();
    let events = Arc::new(Mutex::new(Vec::new()));
    let candidates = Arc::new(Mutex::new(Vec::new()));
    let driver = Arc::new(UserInputExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        events: Arc::clone(&events),
        candidates: Arc::clone(&candidates),
    });
    let directory = TempDir::new().unwrap();
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
    let accounting_factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let engine = Arc::new(runtime.execution_engine(accounting_factory));
    let supervisor = GoalExecutionSupervisor::new(engine);

    let mut started = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver,
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "choose the appropriate option".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(started.response(), GoalResponse::Started(_)));
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut started)
            .await
            .unwrap()
            .is_none()
    );

    let paused = timeout(Duration::from_secs(1), async {
        loop {
            let current = store
                .current_goal_for_session(&ingress.session_key().durable_id())
                .await
                .unwrap()
                .unwrap();
            if current.status == TaskStatus::Paused {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a validated user-input blocker must pause the Goal");

    assert_eq!(paused.status, TaskStatus::Paused);
    assert_eq!(
        candidates.lock().unwrap().as_slice(),
        &["Which option should I use?".to_owned()]
    );
    assert_eq!(
        events.lock().unwrap().as_slice(),
        &["parent", "candidate", "verifier"]
    );
}

#[tokio::test]
async fn supervisor_pauses_without_starting_when_the_surface_cannot_acknowledge() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let ingress = matrix_ingress();
    let parent_started = Arc::new(Notify::new());
    let release_parent = Arc::new(Notify::new());
    let verifier_calls = Arc::new(AtomicUsize::new(0));
    let driver = Arc::new(PausingExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        parent_started: Arc::clone(&parent_started),
        release_parent,
        verifier_calls: Arc::clone(&verifier_calls),
    });
    let directory = TempDir::new().unwrap();
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
    let accounting_factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor =
        GoalExecutionSupervisor::new(Arc::new(runtime.execution_engine(accounting_factory)));

    let mut started = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver,
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            },
        )
        .await
        .unwrap();
    let task_id = match started.response() {
        GoalResponse::Started(projection) => projection.task_id.clone(),
        response => panic!("expected start response, got {response:?}"),
    };
    supervisor
        .pause_before_launch(&mut started, GoalPauseReason::ExternalDependency)
        .await
        .unwrap();

    assert!(
        timeout(Duration::from_millis(20), parent_started.notified())
            .await
            .is_err(),
        "a Goal whose lifecycle acknowledgement was not delivered must not start"
    );
    let task = store
        .current_goal_for_session(&ingress.session_key().durable_id())
        .await
        .unwrap()
        .unwrap();
    let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Paused);
    assert_eq!(goal.pause_reason, Some(GoalPauseReason::ExternalDependency));
    assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
    assert!(goal.pending_call_id.is_none());
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
}

async fn assert_launch_rebind_failure_outcome(cancel_before_launch: bool) {
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let live = LiveConfig::new(valid_goal_config(true));
        let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
        let directory = TempDir::new().unwrap();
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
        let accounting_factory = Arc::new(
            RuntimeGoalOperationAccountingFactory::new(
                store.clone() as Arc<dyn GoalTaskRegistry>,
                tracker,
                "main",
                Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
            )
            .unwrap(),
        );
        let supervisor =
            GoalExecutionSupervisor::new(Arc::new(runtime.execution_engine(accounting_factory)));
        let inner = recording_driver(&ingress);
        let driver = Arc::new(LaunchRebindFailureDriver {
            inner: Arc::clone(&inner),
            refuse_bind: AtomicBool::new(false),
        });
        let mut started = supervisor
            .submit(
                &host_settings(&live.handle().snapshot()),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "finish the admitted task".into(),
                },
            )
            .await
            .unwrap();
        let task_id = match started.response() {
            GoalResponse::Started(projection) => projection.task_id.clone(),
            response => panic!("expected start response, got {response:?}"),
        };
        if cancel_before_launch {
            let cancelled = supervisor
                .submit(
                    &host_settings(&live.handle().snapshot()),
                    ingress.clone(),
                    recording_driver(&ingress),
                    GoalCommand::Cancel,
                )
                .await
                .unwrap();
            assert!(matches!(cancelled.response(), GoalResponse::Cancelled(_)));
        }
        driver.refuse_bind.store(true, Ordering::SeqCst);
        let error = supervisor
            .launch_after_response(&settings_source, &mut started)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("session binding revoked after acknowledgement"),
            "the original bind error must be retained: {error:#}"
        );
        let session_id = ingress.session_key().durable_id();
        let current = store
            .current_goal_for_session(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.id, task_id);
        let expected_status = if cancel_before_launch {
            TaskStatus::Cancelled
        } else {
            TaskStatus::Failed
        };
        assert_eq!(current.status, expected_status);
        let reason = store
            .terminal_reason_for_session_goal(&task_id, &session_id)
            .await
            .unwrap();
        if !cancel_before_launch {
            assert_eq!(reason.as_deref(), Some("executor_start_failed"));
        } else {
            assert_ne!(reason.as_deref(), Some("executor_start_failed"));
        }
        assert_eq!(inner.execution_acquires.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn launch_rebind_failure_settles_the_admitted_goal_before_returning() {
    assert_launch_rebind_failure_outcome(false).await;
}

async fn assert_post_commit_projection_failure_is_settled(resume: bool, cancelled: bool) {
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let directory = TempDir::new().unwrap();
        let store = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
        let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let live = LiveConfig::new(valid_goal_config(true));
        let session_id = ingress.session_key().durable_id();
        let driver = recording_driver(&ingress);
        let start = GoalCommand::Start {
            budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
            objective: "finish the bounded task".into(),
        };
        if resume {
            let initial = runtime
                .submit(
                    &host_settings(&live.handle().snapshot()),
                    ingress.clone(),
                    driver.clone(),
                    start.clone(),
                )
                .await
                .unwrap();
            let GoalResponse::Started(projection) = initial.response() else {
                panic!("expected initial start");
            };
            assert_eq!(
                store
                    .pause_session_goal(
                        &projection.task_id,
                        &session_id,
                        projection.execution_epoch,
                        GoalPauseState {
                            reason: GoalPauseReason::OperatorPaused,
                            description: Some("operator pause".into()),
                            blockers: Vec::new(),
                        },
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
        }
        let connection =
            rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
        // Corrupt only the projection payload after the real transition.
        // Neither injection substitutes the registry or introduces an await.
        let transition = if cancelled {
            "UPDATE tasks SET status = 'cancelled', execution_epoch = execution_epoch + 1,
             finished_at = 'fixture-finished', error = 'fixture-cancelled'
             WHERE id = NEW.task_id;"
        } else {
            ""
        };
        let trigger = if resume {
            format!(
                "CREATE TRIGGER fail_projection AFTER UPDATE OF pause_reason ON goal_tasks
             WHEN NEW.pause_reason IS NULL
             BEGIN UPDATE goal_tasks SET blockers_json = 'invalid-json'
                   WHERE task_id = NEW.task_id; {transition} END;"
            )
        } else {
            format!(
                "CREATE TRIGGER fail_projection AFTER INSERT ON goal_tasks
             BEGIN UPDATE goal_tasks SET blockers_json = 'invalid-json'
                   WHERE task_id = NEW.task_id; {transition} END;"
            )
        };
        connection.execute_batch(&trigger).unwrap();
        let command = if resume {
            GoalCommand::Resume { response: None }
        } else {
            start
        };
        let snapshot = live.handle().snapshot();
        let settings = host_settings(&snapshot);
        let mut submission =
            Box::pin(runtime.submit(&settings, ingress.clone(), driver.clone(), command));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let std::task::Poll::Ready(result) =
            std::future::Future::poll(submission.as_mut(), &mut context)
        else {
            panic!("SQLite admission and projection must settle within one poll");
        };
        let error = result
            .err()
            .expect("post-commit projection must report its original read error");
        assert!(format!("{error:#}").contains("get goal task"));
        let current = store
            .current_goal_for_session(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            current.status,
            if cancelled {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Failed
            },
            "post-commit projection error must not leave an unowned running epoch"
        );
        assert_eq!(current.execution_epoch, if resume { 4 } else { 2 });
        assert_eq!(
            store
                .terminal_reason_for_session_goal(&current.id, &session_id)
                .await
                .unwrap()
                .as_deref(),
            Some(if cancelled {
                "fixture-cancelled"
            } else {
                "executor_start_failed"
            })
        );
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn start_projection_failure_settles_the_committed_epoch() {
    assert_post_commit_projection_failure_is_settled(false, false).await;
}

#[tokio::test]
async fn resume_projection_failure_settles_the_committed_epoch() {
    assert_post_commit_projection_failure_is_settled(true, false).await;
}

#[tokio::test]
async fn projection_failure_preserves_a_competing_cancellation() {
    assert_post_commit_projection_failure_is_settled(false, true).await;
    assert_post_commit_projection_failure_is_settled(true, true).await;
}

#[tokio::test]
async fn launch_rebind_failure_preserves_a_competing_cancellation() {
    assert_launch_rebind_failure_outcome(true).await;
}

#[tokio::test]
async fn daemon_restart_fences_goal_until_explicit_resume_in_the_next_generation() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
    let ingress = matrix_ingress();
    let parent_started = Arc::new(Notify::new());
    let release_parent = Arc::new(Notify::new());
    let verifier_calls = Arc::new(AtomicUsize::new(0));
    let driver = Arc::new(PausingExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        parent_started: Arc::clone(&parent_started),
        release_parent: Arc::clone(&release_parent),
        verifier_calls: Arc::clone(&verifier_calls),
    });
    let directory = TempDir::new().unwrap();
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
    let pricer = Arc::new(ModelProviderGoalUsagePricer::new(Arc::default()));
    let accounting_factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            pricer,
        )
        .unwrap(),
    );
    let engine = Arc::new(runtime.execution_engine(accounting_factory));
    let coordinator = Arc::new(GoalExecutionRestartCoordinator::new());
    let supervisor = coordinator.new_supervisor(engine).await;

    let mut started = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            },
        )
        .await
        .unwrap();
    let task_id = match started.response() {
        GoalResponse::Started(projection) => projection.task_id.clone(),
        response => panic!("expected start response, got {response:?}"),
    };
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut started)
            .await
            .unwrap()
            .is_none()
    );
    parent_started.notified().await;

    let restarting = Arc::clone(&coordinator);
    let mut quiesce = zeroclaw_spawn::spawn!(async move { restarting.quiesce_for_restart().await });
    let paused_task = timeout(Duration::from_secs(1), async {
        loop {
            let current = store
                .current_goal_for_session(&ingress.session_key().durable_id())
                .await
                .unwrap()
                .unwrap();
            if current.status == TaskStatus::Paused {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("restart must durably fence the active Goal");
    assert_eq!(paused_task.id, task_id);
    let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
    assert_eq!(goal.pause_reason, Some(GoalPauseReason::DaemonRestart));
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
    assert!(
        timeout(Duration::from_millis(20), &mut quiesce)
            .await
            .is_err(),
        "restart must drain the already-admitted parent operation"
    );

    release_parent.notify_one();
    quiesce.await.unwrap().unwrap();
    assert_eq!(
        store
            .current_goal_for_session(&ingress.session_key().durable_id())
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Paused,
        "restart quiescence must not auto-resume the Goal"
    );

    let rejected_resume = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Resume { response: None },
        )
        .await;
    assert!(
        rejected_resume.is_err(),
        "the retiring generation must not admit a Goal resume"
    );

    coordinator.reopen_for_generation().await;
    let mut resumed = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver,
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    assert!(matches!(resumed.response(), GoalResponse::Resumed(_)));
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut resumed)
            .await
            .unwrap()
            .is_none()
    );
    parent_started.notified().await;
    release_parent.notify_one();
    let completed = timeout(Duration::from_secs(1), async {
        loop {
            let current = store
                .current_goal_for_session(&ingress.session_key().durable_id())
                .await
                .unwrap()
                .unwrap();
            if current.status.is_terminal() {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("explicit resume must execute in the next generation");
    assert_eq!(
        completed.status,
        TaskStatus::Completed,
        "resumed Goal terminal record: {completed:?}"
    );
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 1);
}
