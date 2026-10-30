use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use zeroclaw_commands::goal::GoalCommand;
use zeroclaw_config::{
    api_error::{ConfigApiCode, ConfigApiError},
    goal::{GoalBudgetLimits as ConfigGoalBudgetLimits, GoalConfig},
    providers::ModelProviderRef,
    schema::{Config, OpenAIModelProviderConfig},
};
use zeroclaw_goal::{
    GoalController, GoalExecutionHost, GoalExecutionScope, GoalHostSettings, GoalIngressContext,
    GoalIngressPrincipal, GoalParentTurn, GoalResponse, GoalSessionBinding, GoalSessionDriver,
    GoalSessionExecutionLease, GoalSessionKey, GoalSessionLease, GoalVerifierTurn,
};
use zeroclaw_runtime::control_plane::{
    GoalAccountingState, GoalPauseReason, GoalPauseState, GoalTaskRecord, GoalTaskRegistry,
    GoalTransitionResult, SqliteTaskStore, TaskContinuationContext, TaskRecord, TaskStatus,
};

struct RecordingDriver {
    binding: GoalSessionBinding,
    binds: AtomicUsize,
    execution_acquires: AtomicUsize,
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
    ) -> anyhow::Result<String> {
        Ok(format!("verifier:{}", turn.candidate))
    }

    async fn append_verified_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
        self.delivered.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct ExecutionDriver {
    binding: GoalSessionBinding,
    execution_acquires: AtomicUsize,
    delivered: Arc<AtomicUsize>,
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
        _scope: &GoalExecutionScope,
        _turn: GoalParentTurn,
    ) -> anyhow::Result<String> {
        anyhow::bail!("lease execution test does not run parent turns")
    }

    async fn run_verifier(
        &mut self,
        _scope: &GoalExecutionScope,
        _turn: GoalVerifierTurn,
    ) -> anyhow::Result<String> {
        anyhow::bail!("lease execution test does not run verifier turns")
    }

    async fn append_verified_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
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
        match host.acquire_execution(&invalid, start, &scope).await {
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
        host.acquire_execution(&host_settings(&restored), fresh_submission, &scope)
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

    let error = match host
        .acquire_execution(&host_settings(&config), submission, &scope)
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

    let mut lease = host
        .acquire_execution(&host_settings(&config), submission, &scope)
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
        "verifier:candidate"
    );
    lease
        .append_verified_candidate("candidate".into())
        .await
        .unwrap();

    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 1);
    assert_eq!(delivered.load(Ordering::SeqCst), 1);
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

    let error = match host
        .acquire_execution(&host_settings(&config), submission, &scope)
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

    let error = match host
        .acquire_execution(&host_settings(&config), submission, &scope)
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
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&config), &start)
        .await
        .unwrap()
    else {
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
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
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
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &first_start)
            .await
            .unwrap(),
        GoalResponse::Started(_)
    ));

    let colliding_status = host
        .submit(
            &host_settings(&config),
            colliding_ingress.clone(),
            recording_driver(&colliding_ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &colliding_status)
            .await
            .unwrap(),
        GoalResponse::NoCurrentGoal
    ));
    let colliding_cancel = host
        .submit(
            &host_settings(&config),
            colliding_ingress.clone(),
            recording_driver(&colliding_ingress),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &colliding_cancel)
            .await
            .unwrap(),
        GoalResponse::NoCurrentGoal
    ));
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
        GoalResponse::NoCurrentGoal
    ));

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
        GoalResponse::NoCurrentGoal
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

    let execution = host
        .acquire_execution(&host_settings(&config), submission, &scope)
        .await
        .unwrap();
    assert!(
        !driver.reconnect_is_allowed(),
        "the execution lease must replace the released admission lease"
    );
    drop(execution);
    assert!(driver.reconnect_is_allowed());
}
