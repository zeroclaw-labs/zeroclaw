//! Process-local ZeroCode ownership for Goal execution workers.
//!
//! Durable lifecycle and accounting state remain in the control plane. This
//! registry only retains the exact join handles needed to drain a session's
//! fenced execution epoch when an RPC lifecycle operation replaces or removes
//! that session.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context as _, Result, bail, ensure};
use async_trait::async_trait;
use tokio::sync::Mutex;
use zeroclaw_api::{
    jsonrpc::JsonRpcNotification,
    model_provider::{ChatMessage, ChatRequest, TokenUsage},
};

use crate::{
    agent::agent::{
        Agent, IsolatedTranscriptSource, PreparedIsolatedTurn, TurnEvent,
        build_session_model_provider,
    },
    agent::cost::{
        TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext, build_model_provider_pricing,
    },
    control_plane::{ControlPlaneHandle, control_plane},
    cost::CostTracker,
    goal_accounting::{
        GoalForegroundContext, ModelProviderGoalUsagePricer, RuntimeGoalDiagnosticTextGuard,
        RuntimeGoalOperationAccountingFactory,
    },
    rpc::{
        context::RpcContext,
        session::{GoalFrameClass, GoalFrameDelivery, GoalRecipient, GoalSessionSnapshot},
    },
};

use zeroclaw_commands::goal::GoalCommand;
use zeroclaw_goal::accounting::{GoalUsagePrice, GoalUsagePricer};
use zeroclaw_goal::control::{
    GoalControlAbandonReason, GoalControlAdmission, GoalExecutionNotice, GoalExecutionScope,
    GoalExecutionSupervisor, GoalHostSettings, GoalHostSettingsSource, GoalIngressContext,
    GoalIngressPrincipal, GoalLaunchFailureReported, GoalOperationScope, GoalParentTurn,
    GoalParentTurnInput, GoalParentTurnKind, GoalParentTurnResult, GoalResponse, GoalRuntime,
    GoalSessionBinding, GoalSessionDriver, GoalSessionExecutionLease, GoalSessionKey,
    GoalSessionLease, GoalSessionQuiescence, GoalSupervisorRetired, GoalSurface, GoalTurnBoundary,
    GoalVerifierAttribution, GoalVerifierResult, GoalVerifierTurn, goal_parent_turn_input,
};
use zeroclaw_goal::{GoalParentTurnCheckpoint, GoalPauseReason};
#[cfg(test)]
use zeroclaw_goal::{GoalTransitionResult, control::dispose_unowned_session_goal};
use zeroclaw_rpc_proto::notification::SESSION_GOAL_UPDATE;

/// Resolve each priced attempt from the ordinary ZeroCode pricing source.
/// Retain live config rather than RpcContext so a resident supervisor neither
/// caches pricing policy nor forms a context-to-supervisor ownership cycle.
struct ZeroCodeGoalUsagePricer {
    config: zeroclaw_config::live::LiveConfigHandle,
}

impl GoalUsagePricer for ZeroCodeGoalUsagePricer {
    fn price(&self, provider_ref: &str, model: &str, usage: &TokenUsage) -> Result<GoalUsagePrice> {
        let pricing = {
            let config = self.config.read();
            Arc::new(build_model_provider_pricing(&config))
        };
        ModelProviderGoalUsagePricer::new(pricing).price(provider_ref, model, usage)
    }
}

async fn acquire_zero_code_goal_session_guard(
    session_queue: Arc<zeroclaw_infra::session_queue::SessionActorQueue>,
    session_id: &str,
    checkpoint: &GoalParentTurnCheckpoint,
) -> Result<zeroclaw_infra::session_queue::SessionGuard> {
    tokio::select! {
        biased;
        _ = checkpoint.wait_closed() => bail!("ZeroCode Goal execution was fenced before session admission"),
        result = session_queue.acquire(session_id) => {
            result.context("ZeroCode Goal session is busy")
        }
    }
}

/// Retains a ZeroCode Goal admission until its JSON-RPC response has been
/// queued ahead of any ordinary parent-turn events.
pub(super) struct RpcGoalCommandSubmission {
    response: GoalResponse,
    issuer: GoalCommandIssuer,
    post_response: Option<RpcGoalPostResponse>,
}

/// A command belongs to its original connection, even if another connection
/// later becomes the observer of the ongoing Goal execution.
#[derive(Clone)]
struct GoalCommandIssuer {
    authorization: Arc<super::dispatch::GoalSessionAuthorization>,
    generation: u64,
}

impl std::fmt::Debug for RpcGoalCommandSubmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RpcGoalCommandSubmission")
            .field("has_post_response_action", &self.post_response.is_some())
            .finish_non_exhaustive()
    }
}

struct RpcGoalPostResponse {
    runtime: Arc<RpcGoalRuntime>,
    context: Arc<RpcContext>,
    control_plane: ControlPlaneHandle,
    session_id: String,
    supervisor: Arc<GoalExecutionSupervisor>,
    settings_source: GoalHostSettingsSource,
    admission: GoalControlAdmission,
    issuer: GoalCommandIssuer,
}

async fn publish_goal_lifecycle(
    issuer: &GoalCommandIssuer,
    session_id: &str,
    response: &GoalResponse,
) -> Result<()> {
    publish_goal_update(
        issuer,
        crate::rpc::types::SessionGoalUpdate::Lifecycle {
            session_id: session_id.to_owned(),
            response: crate::rpc::types::goal_response_wire(response),
        },
    )
    .await
}

async fn publish_goal_update(
    issuer: &GoalCommandIssuer,
    update: crate::rpc::types::SessionGoalUpdate,
) -> Result<()> {
    let notification = encode_goal_update(update)?;
    ensure!(
        issuer
            .authorization
            .send_goal_command_notification(issuer.generation, notification)
            .await,
        "ZeroCode Goal lifecycle response could not be queued"
    );
    Ok(())
}

fn encode_goal_update(update: crate::rpc::types::SessionGoalUpdate) -> Result<String> {
    let params = serde_json::to_value(update)?;
    Ok(serde_json::to_string(&JsonRpcNotification::new(
        SESSION_GOAL_UPDATE,
        params,
    ))?)
}

async fn report_goal_command_failure(
    issuer: &GoalCommandIssuer,
    session_id: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    match publish_goal_update(
        issuer,
        crate::rpc::types::SessionGoalUpdate::CommandFailed {
            session_id: session_id.to_owned(),
        },
    )
    .await
    {
        Ok(()) => error,
        Err(delivery) => error.context(delivery),
    }
}

impl RpcGoalCommandSubmission {
    pub(super) fn response(&self) -> &GoalResponse {
        &self.response
    }

    pub(super) async fn send_issuer_result(
        &self,
        id: serde_json::Value,
        value: serde_json::Value,
    ) -> bool {
        self.issuer
            .authorization
            .send_goal_command_result(self.issuer.generation, &self.response, id, value)
            .await
    }

    /// Launch only after the response is in the shared outbound queue. The
    /// same queue preserves response-before-agent-event ordering for both
    /// local IPC and WSS.
    pub(super) async fn after_response(self) -> Result<()> {
        self.finish(true).await
    }

    /// Without the first acknowledgement, an unseen Goal must not launch.
    pub(super) async fn response_delivery_failed(self) -> Result<()> {
        self.finish(false).await
    }

    async fn finish(self, acknowledged: bool) -> Result<()> {
        if let Some(post_response) = self.post_response {
            post_response.finish(acknowledged).await?;
        }
        Ok(())
    }
}

impl RpcGoalPostResponse {
    async fn finish(self, acknowledged: bool) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
        let supervisor = Arc::clone(&self.supervisor);
        let session_id = self.session_id.clone();
        let result = if acknowledged {
            self.after_acknowledgement().await
        } else {
            self.abandon_unacknowledged().await
        };
        // No command custody remains here. Even read-only and refused
        // controls can leave a registration that has never owned a worker.
        runtime
            .retire_supervisor_if_idle(&session_id, &supervisor)
            .await;
        result
    }

    async fn after_acknowledgement(self) -> Result<()> {
        #[cfg(test)]
        let runtime = Arc::clone(&self.runtime);
        let RpcGoalPostResponse {
            context,
            control_plane,
            session_id,
            supervisor,
            settings_source,
            admission,
            issuer,
            ..
        } = self;
        let (mut submission, publish_initial) = match admission {
            GoalControlAdmission::Direct(submission) => (submission, false),
            GoalControlAdmission::Queued { deferred, .. } => {
                let ready = match deferred.wait().await {
                    Ok(ready) => ready,
                    Err(closed) => {
                        let delivery =
                            publish_goal_lifecycle(&issuer, &session_id, closed.response()).await;
                        let error = anyhow::Error::from(closed);
                        return Err(match delivery {
                            Ok(()) => error,
                            Err(delivery) => error.context(delivery),
                        });
                    }
                };
                let ready = ready.with_abandon_reason(GoalControlAbandonReason::ControlDropped);
                #[cfg(test)]
                runtime.wait_test_deferred_commit_pause(&session_id).await;
                // Config is live authority; the queued command's earlier
                // settings cannot authorize its eventual transition.
                let config = context.config.snapshot();
                let settings = match GoalHostSettings::from_config(
                    &config,
                    std::process::id(),
                    control_plane.boot_id.clone(),
                ) {
                    Ok(settings) => settings,
                    Err(error) => {
                        return Err(report_goal_command_failure(&issuer, &session_id, error).await);
                    }
                };
                let commit = match ready.commit(&settings).await {
                    Ok(commit) => commit,
                    Err(error) if error.is::<GoalLaunchFailureReported>() => return Err(error),
                    Err(error) => {
                        return Err(report_goal_command_failure(&issuer, &session_id, error).await);
                    }
                };
                let before_release = matches!(
                    commit.response(),
                    GoalResponse::Started(_) | GoalResponse::Resumed(_)
                );
                if before_release {
                    let delivery =
                        publish_goal_lifecycle(&issuer, &session_id, commit.response()).await;
                    let mut submission = commit.release();
                    if let Err(error) = delivery {
                        supervisor
                            .pause_before_launch(
                                &mut submission,
                                GoalPauseReason::ExternalDependency,
                            )
                            .await?;
                        return Err(error);
                    }
                    (submission, false)
                } else {
                    // A fenced worker can drain only after its checkpoint
                    // and command lease are released by this control task.
                    (commit.release(), true)
                }
            }
        };
        let initial_response = submission.response().clone();
        let late_response = match supervisor
            .launch_after_response(&settings_source, &mut submission)
            .await
        {
            Ok(response) => response,
            Err(error) if error.is::<GoalLaunchFailureReported>() => return Err(error),
            Err(error) => {
                return Err(report_goal_command_failure(&issuer, &session_id, error).await);
            }
        };
        if let Some(response) = late_response
            .as_ref()
            .or_else(|| publish_initial.then_some(&initial_response))
        {
            publish_goal_lifecycle(&issuer, &session_id, response).await?;
        }
        Ok(())
    }

    async fn abandon_unacknowledged(self) -> Result<()> {
        match self.admission {
            GoalControlAdmission::Queued { deferred, .. } => {
                deferred
                    .abandon(GoalControlAbandonReason::DeliveryFailed)
                    .await?;
            }
            GoalControlAdmission::Direct(mut submission) => {
                // Exactly these direct responses may carry execution or a resident predecessor.
                if matches!(
                    submission.response(),
                    GoalResponse::Started(_)
                        | GoalResponse::Resumed(_)
                        | GoalResponse::TransitionPending(_)
                        | GoalResponse::Terminal(_)
                ) {
                    self.supervisor
                        .pause_before_launch(&mut submission, GoalPauseReason::ExternalDependency)
                        .await?;
                }
            }
        }
        Ok(())
    }
}

/// The immutable, trusted ZeroCode binding for one Goal admission.
pub struct ZeroCodeGoalSessionDriver {
    context: Arc<RpcContext>,
    control_plane: Option<ControlPlaneHandle>,
    authorization: Arc<super::dispatch::GoalSessionAuthorization>,
    // Held until launch transfers it into the worker future.
    connection_activity: std::sync::Mutex<Option<crate::rpc::ConnectionActivity>>,
    // Once launch has selected its presentation path, a later failure must never reattach the issuer.
    launch_attach_reached: AtomicBool,
    session_key: GoalSessionKey,
    agent_alias: String,
    session_generation: u64,
    recipient: Arc<GoalRecipient>,
    tui_id: String,
    command_lock: Arc<Mutex<()>>,
    // Query-only input from the admitted command, never execution authority.
    recall_query: String,
}

enum OwnerCheck {
    Command,
    Continuation,
}

impl ZeroCodeGoalSessionDriver {
    /// Capture the live session facts that command text cannot control.
    pub(super) async fn new(
        context: Arc<RpcContext>,
        connection_activity: Option<crate::rpc::ConnectionActivity>,
        session_id: String,
        tui_id: String,
        authorization: super::dispatch::GoalSessionAuthorization,
        recall_query: String,
    ) -> Result<Self> {
        let session_key = GoalSessionKey::zero_code(session_id)?;
        let raw_session_id = match &session_key {
            GoalSessionKey::ZeroCode { raw_session_id } => raw_session_id,
            GoalSessionKey::Matrix { .. } => unreachable!("ZeroCode key constructor is typed"),
        };
        let snapshot = context
            .sessions
            .goal_session_snapshot(raw_session_id)
            .await
            .context("ZeroCode Goal session is absent")?;
        ensure!(
            snapshot.owner_tui_id.as_deref() == Some(tui_id.as_str()),
            "ZeroCode Goal caller does not own the session"
        );
        ensure!(
            matches!(snapshot.chat_mode, crate::rpc::types::ChatMode::Chat),
            "Goal Mode is unavailable for ACP sessions"
        );
        let control_plane = context.goal_runtime.current_control_plane();
        Ok(Self {
            context,
            control_plane,
            authorization: Arc::new(authorization),
            connection_activity: std::sync::Mutex::new(connection_activity),
            launch_attach_reached: AtomicBool::new(false),
            session_key,
            agent_alias: snapshot.agent_alias,
            session_generation: snapshot.generation,
            recipient: snapshot.goal_recipient,
            tui_id,
            command_lock: snapshot.goal_command,
            recall_query,
        })
    }

    fn raw_session_id(&self) -> &str {
        match &self.session_key {
            GoalSessionKey::ZeroCode { raw_session_id } => raw_session_id,
            GoalSessionKey::Matrix { .. } => unreachable!("ZeroCode driver has a typed key"),
        }
    }

    pub fn agent_alias(&self) -> &str {
        &self.agent_alias
    }

    fn assert_ingress(&self, ingress: &GoalIngressContext) -> Result<()> {
        ensure!(
            ingress.surface() == GoalSurface::ZeroCode,
            "Goal ingress is not ZeroCode"
        );
        ensure!(
            ingress.session_key() == &self.session_key,
            "Goal ingress session is stale"
        );
        ensure!(
            ingress.agent() == self.agent_alias,
            "Goal ingress agent is stale"
        );
        ensure!(
            ingress.route() == format!("zerocode:{}", self.raw_session_id()),
            "Goal ingress route is stale"
        );
        match ingress.principal() {
            GoalIngressPrincipal::ZeroCode { tui_id }
                if tui_id.as_str() == self.tui_id.as_str() =>
            {
                Ok(())
            }
            _ => bail!("Goal ingress principal is stale"),
        }
    }

    async fn revalidate_session(&self, owner_check: OwnerCheck) -> Result<GoalSessionSnapshot> {
        let snapshot = self
            .context
            .sessions
            .goal_session_snapshot(self.raw_session_id())
            .await
            .context("ZeroCode Goal session is absent")?;
        ensure!(
            snapshot.generation == self.session_generation,
            "ZeroCode Goal session was replaced"
        );
        ensure!(
            snapshot.agent_alias == self.agent_alias,
            "ZeroCode Goal session agent changed"
        );
        if matches!(owner_check, OwnerCheck::Command) {
            ensure!(
                snapshot.owner_tui_id.as_deref() == Some(self.tui_id.as_str()),
                "ZeroCode Goal caller no longer owns the session"
            );
        }
        ensure!(
            matches!(snapshot.chat_mode, crate::rpc::types::ChatMode::Chat),
            "Goal Mode is unavailable for ACP sessions"
        );
        Ok(snapshot)
    }

    async fn bind_goal_session(
        &self,
        ingress: &GoalIngressContext,
        pending_launch: bool,
    ) -> Result<GoalSessionLease> {
        self.assert_ingress(ingress)?;
        self.revalidate_session(OwnerCheck::Command).await?;
        let guard = self.command_lock.clone().lock_owned().await;
        let snapshot = self.revalidate_session(OwnerCheck::Command).await?;
        let grants = self.authorization.revalidate().await?;
        self.authorization
            .admit_execution(grants.as_ref(), &snapshot.reaction_channels)
            .await?;
        ensure!(
            pending_launch
                || self.authorization.is_read_only()
                || !snapshot.goal_admission_is_fenced(),
            "ZeroCode Goal session transition is pending"
        );
        ensure!(
            super::session_disposition::reconcile_session(&self.context, self.raw_session_id())
                .await?,
            "ZeroCode Goal session disposition is pending"
        );
        self.revalidate_session(OwnerCheck::Command).await?;
        #[cfg(test)]
        self.context
            .goal_runtime
            .check_test_launch_bind_failure(&self.session_key)
            .await?;
        if pending_launch {
            self.launch_attach_reached.store(true, Ordering::Release);
            if self.authorization.issuing_connection_open() {
                self.context
                    .sessions
                    .attach_goal_recipient(
                        self.raw_session_id(),
                        self.session_generation,
                        &self.tui_id,
                        self.authorization.frame_destination(),
                    )
                    .await;
            }
        }
        Ok(GoalSessionLease::new(
            GoalSessionBinding::new(self.session_key.clone()),
            guard,
        ))
    }
}

#[async_trait]
impl GoalSessionDriver for ZeroCodeGoalSessionDriver {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn take_execution_liveness(&self) -> Option<Box<dyn Send>> {
        self.connection_activity
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .map(|activity| Box::new(activity) as Box<dyn Send>)
    }

    async fn bind(&self, ingress: &GoalIngressContext) -> Result<GoalSessionLease> {
        self.bind_goal_session(ingress, false).await
    }

    async fn bind_for_launch(&self, ingress: &GoalIngressContext) -> Result<GoalSessionLease> {
        self.bind_goal_session(ingress, true).await
    }

    async fn try_turn_boundary(
        &self,
        ingress: &GoalIngressContext,
    ) -> Result<Option<GoalTurnBoundary>> {
        self.assert_ingress(ingress)?;
        self.revalidate_session(OwnerCheck::Command).await?;
        let guard = self
            .context
            .sessions
            .session_queue
            .try_acquire(self.raw_session_id())
            .await
            .context("ZeroCode Goal turn boundary could not be admitted")?;
        self.revalidate_session(OwnerCheck::Command).await?;
        Ok(guard.map(GoalTurnBoundary::new))
    }

    async fn await_turn_boundary(
        &self,
        ingress: &GoalIngressContext,
        checkpoint: &GoalParentTurnCheckpoint,
    ) -> Result<GoalTurnBoundary> {
        self.assert_ingress(ingress)?;
        loop {
            self.revalidate_session(OwnerCheck::Command).await?;
            let admission = tokio::select! {
                biased;
                () = checkpoint.wait_closed() => return Err(zeroclaw_goal::GoalCheckpointClosed.into()),
                admission = self.context.sessions.session_queue.reserve(self.raw_session_id()) => {
                    admission.context("ZeroCode Goal turn boundary could not be reserved")?
                }
            };
            let result = tokio::select! {
                biased;
                () = checkpoint.wait_closed() => return Err(zeroclaw_goal::GoalCheckpointClosed.into()),
                result = admission.acquire() => result,
            };
            match result {
                Ok(guard) => {
                    self.revalidate_session(OwnerCheck::Command).await?;
                    return Ok(GoalTurnBoundary::new(guard));
                }
                Err(zeroclaw_infra::session_queue::SessionQueueError::Timeout { .. }) => {
                    // Ordinary turns keep their configured queue timeout. A
                    // deferred control waits until its boundary or fence.
                }
                Err(error) => return Err(error).context("ZeroCode Goal turn boundary failed"),
            }
        }
    }

    async fn acquire_execution(
        &self,
        ingress: &GoalIngressContext,
        scope: &GoalExecutionScope,
        checkpoint: &GoalParentTurnCheckpoint,
    ) -> Result<Box<dyn GoalSessionExecutionLease>> {
        self.assert_ingress(ingress)?;
        ensure!(
            scope.session_id() == self.session_key.durable_id(),
            "Goal execution session is stale"
        );
        let queue_guard = acquire_zero_code_goal_session_guard(
            Arc::clone(&self.context.sessions.session_queue),
            self.raw_session_id(),
            checkpoint,
        )
        .await?;
        let snapshot = self.revalidate_session(OwnerCheck::Continuation).await?;
        if snapshot.goal_admission_is_fenced()
            && let Some(control_plane) = &self.control_plane
            && control_plane
                .goal_store()?
                .current_goal_for_session(scope.session_id())
                .await?
                .is_some_and(|task| {
                    task.id == scope.task_id()
                        && task.execution_epoch == scope.execution_epoch()
                        && task.status == zeroclaw_api::task::TaskStatus::Running
                })
        {
            checkpoint.close();
            return Err(zeroclaw_goal::GoalCheckpointClosed.into());
        }
        ensure!(
            super::session_disposition::reconcile_session(&self.context, self.raw_session_id())
                .await?,
            "ZeroCode Goal session disposition is pending"
        );
        let agent = snapshot.agent;
        Ok(Box::new(ZeroCodeGoalExecutionLease {
            _queue_guard: queue_guard,
            agent,
            prepared_parent: None,
            unlaunched_issuer: (!self.launch_attach_reached.load(Ordering::Acquire))
                .then(|| self.tui_id.clone()),
            control_plane: self.control_plane.clone(),
            agent_alias: self.agent_alias.clone(),
            session_generation: self.session_generation,
            history_has_trim_breadcrumb: false,
            recipient: Arc::clone(&self.recipient),
            context: Arc::clone(&self.context),
            session_key: self.session_key.clone(),
            authorization: Arc::clone(&self.authorization),
            recall_query: self.recall_query.clone(),
        }))
    }
}

struct ZeroCodeGoalExecutionLease {
    _queue_guard: zeroclaw_infra::session_queue::SessionGuard,
    agent: Arc<Mutex<Agent>>,
    // The Agent guard owns the exact preparation until Start/Resume consumes it.
    prepared_parent: Option<PreparedIsolatedTurn>,
    unlaunched_issuer: Option<String>,
    control_plane: Option<ControlPlaneHandle>,
    agent_alias: String,
    session_generation: u64,
    history_has_trim_breadcrumb: bool,
    recipient: Arc<GoalRecipient>,
    context: Arc<RpcContext>,
    session_key: GoalSessionKey,
    authorization: Arc<super::dispatch::GoalSessionAuthorization>,
    recall_query: String,
}

impl Drop for ZeroCodeGoalExecutionLease {
    fn drop(&mut self) {
        // Clear before relinquishing the session permit to another Goal turn.
        self.recipient.clear_unsaved_candidate();
    }
}

impl ZeroCodeGoalExecutionLease {
    fn raw_session_id(&self) -> Result<&str> {
        match &self.session_key {
            GoalSessionKey::ZeroCode { raw_session_id } => Ok(raw_session_id),
            GoalSessionKey::Matrix { .. } => bail!("ZeroCode Goal lease has a Matrix session key"),
        }
    }

    async fn admit_agent_effect(&self) -> Result<tokio::sync::OwnedMutexGuard<Agent>> {
        let mut agent = Arc::clone(&self.agent).lock_owned().await;
        self.revalidate_agent_effect(&mut agent).await?;
        Ok(agent)
    }

    async fn revalidate_agent_effect(&self, agent: &mut Agent) -> Result<()> {
        let result: Result<()> = async {
            // Epoch ownership does not confer continuing RPC authority.
            // Recheck at each new effect after waiting for the Agent.
            let grants = self.authorization.revalidate().await?;
            let snapshot = self
                .context
                .sessions
                .goal_session_snapshot(self.raw_session_id()?)
                .await
                .context("ZeroCode Goal session is absent")?;
            ensure!(
                snapshot.generation == self.session_generation
                    && snapshot.agent_alias == self.agent_alias
                    && matches!(snapshot.chat_mode, crate::rpc::types::ChatMode::Chat)
                    && Arc::ptr_eq(&snapshot.agent, &self.agent),
                "ZeroCode Goal session binding changed before Agent admission"
            );
            self.authorization
                .admit_execution(grants.as_ref(), &snapshot.reaction_channels)
                .await?;
            self.authorization.ensure_turn_capability(agent)?;
            self.authorization.apply_ceiling(grants.as_ref(), agent);
            Ok(())
        }
        .await;
        if let Err(error) = &result
            && let Some(refusal) = error.downcast_ref::<super::dispatch::GoalRpcRefusal>()
        {
            let _ = self
                .publish_turn_failure(format!("turn refused by daemon: {}", refusal.message))
                .await;
        }
        result
    }

    async fn publish_parent_turn_failure(&self, error: &anyhow::Error) -> Result<()> {
        if error.is::<super::dispatch::GoalRpcRefusal>() {
            return Ok(());
        }
        let owned_timeout = zeroclaw_api::turn_stop::turn_stop(error)
            .is_some_and(|stop| stop.code == zeroclaw_api::turn_stop::TurnStopCode::TurnTimeout);
        if crate::agent::loop_::is_tool_loop_cancelled(error) && !owned_timeout {
            return Ok(());
        }
        let heading = crate::agent::terminal_completion_error_message(error, None)
            .unwrap_or_else(|| "turn failed: Agent turn failed".to_owned());
        let diagnostic = format!("{heading}: {}", zeroclaw_log::display_chain(error));
        let diagnostic = zeroclaw_providers::scrub_secret_patterns(&diagnostic);
        let diagnostic = {
            let config = self.context.config.read();
            crate::security::scrub_with_config(&diagnostic, &config.security.leak_detection)
        };
        let diagnostic = crate::agent::turn::scrub_credentials(&diagnostic);
        self.publish_turn_failure(diagnostic).await
    }

    async fn publish_turn_failure(&self, diagnostic: String) -> Result<()> {
        let session_id = self.raw_session_id()?;
        let frame = super::dispatch::turn_completion_notification(
            session_id,
            crate::rpc::types::TurnCompletionOutcome::Failed,
            diagnostic,
            None,
            None,
            super::dispatch::TurnCompleteExtras::for_goal(),
        )
        .context("render ZeroCode Goal turn failure")?;
        ensure!(
            self.publish_frame(session_id, frame, GoalFrameClass::Notice)
                .await
                == GoalFrameDelivery::Delivered,
            "ZeroCode Goal turn failure could not be delivered"
        );
        Ok(())
    }

    async fn publish_frame(
        &self,
        session_id: &str,
        frame: String,
        class: GoalFrameClass,
    ) -> GoalFrameDelivery {
        self.context
            .sessions
            .publish_goal_frame(session_id, &self.recipient, frame, class)
            .await
    }

    async fn publish_parent_event(&self, session_id: &str, event: &TurnEvent) {
        if let Some(frame) = super::dispatch::notification_for_turn_event(session_id, event) {
            self.publish_frame(session_id, frame, GoalFrameClass::ParentEvent)
                .await;
        }
    }

    fn cost_context(&self) -> Option<ToolLoopCostTrackingContext> {
        self.context.cost_tracker.as_ref().map(|tracker| {
            let config = self.context.config.read();
            ToolLoopCostTrackingContext::new(
                Arc::clone(tracker),
                Arc::new(build_model_provider_pricing(&config)),
            )
            .with_agent_alias(&self.agent_alias)
        })
    }

    fn foreground_root(&self, operation: &GoalOperationScope) -> Result<GoalForegroundContext> {
        Ok(GoalForegroundContext::new_root(
            operation.execution().clone(),
            self.control_plane
                .clone()
                .context("Goal control plane is unavailable")?,
            tokio_util::sync::CancellationToken::new(),
            None,
        ))
    }
}

fn verifier_attribution_from_attempts<'a>(
    attempts: impl IntoIterator<Item = (bool, &'a str, &'a str)>,
) -> Option<GoalVerifierAttribution> {
    attempts
        .into_iter()
        .filter(|(accepted, _, _)| *accepted)
        .map(|(_, provider_ref, model)| GoalVerifierAttribution {
            provider_ref: provider_ref.to_owned(),
            model: model.to_owned(),
        })
        .last()
}

/// Let Tokio move other work off this worker without detaching an admitted
/// session write from the Goal lease. Current-thread runtimes run it inline.
fn run_session_write<T>(write: impl FnOnce() -> T) -> T {
    if tokio::runtime::Handle::current().runtime_flavor()
        == tokio::runtime::RuntimeFlavor::MultiThread
    {
        tokio::task::block_in_place(write)
    } else {
        write()
    }
}

#[async_trait]
impl GoalSessionExecutionLease for ZeroCodeGoalExecutionLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    async fn canonical_history(&mut self, input: &GoalParentTurnInput) -> Result<Vec<ChatMessage>> {
        let cost_context = self.cost_context();
        TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(cost_context, async {
                ensure!(
                    self.prepared_parent.is_none(),
                    "ZeroCode Goal parent was already prepared"
                );
                let agent = self.admit_agent_effect().await?;
                let mut prepared = PreparedIsolatedTurn::new(agent, input.clone())?;
                let history = prepared.canonical_prefix().await?;
                self.prepared_parent = Some(prepared);
                Ok(history)
            })
            .await
    }

    async fn publish_goal_core_error(&mut self, error: &anyhow::Error) -> Result<()> {
        self.publish_parent_turn_failure(error).await
    }

    async fn append_resume_response(&mut self, response: String) -> Result<()> {
        let mut agent = self.admit_agent_effect().await?;
        let session_key = format!("rpc_{}", self.raw_session_id()?);
        let message = ChatMessage::user(&response);
        // Persist before changing the live agent. A successful resume must
        // never be visible only to this process and then vanish when the
        // same ZeroCode session is reconstructed after daemon restart.
        let backend = self
            .context
            .session_backend
            .as_ref()
            .context("cannot resume Goal response: ZeroCode session persistence is disabled")?;
        run_session_write(|| backend.append(&session_key, &message))
            .context("persist ZeroCode Goal resume response")?;
        agent.seed_history(std::slice::from_ref(&message));
        Ok(())
    }

    async fn run_parent_turn(
        &mut self,
        operation: &GoalOperationScope,
        turn: GoalParentTurn,
    ) -> Result<GoalParentTurnResult> {
        self.recipient.clear_unsaved_candidate();
        let cost_context = self.cost_context();
        let result = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(cost_context, async {
                let root = self.foreground_root(operation)?;
                let work = async {
                    let input = goal_parent_turn_input(&turn);
                    let prepared = match turn.kind {
                        GoalParentTurnKind::Start | GoalParentTurnKind::Resume => {
                            let mut prepared = self
                                .prepared_parent
                                .take()
                                .context("ZeroCode Goal parent has no captured preparation")?;
                            ensure!(
                                prepared.matches_input(&input),
                                "ZeroCode Goal parent input changed after preparation"
                            );
                            // Capture is read-only and can precede the parent checkpoint.
                            // Revalidate on the same held Agent after that admission wait.
                            self.revalidate_agent_effect(prepared.agent_mut()).await?;
                            prepared
                        }
                        GoalParentTurnKind::Continue => {
                            ensure!(
                                self.prepared_parent.is_none(),
                                "ZeroCode Goal continuation has an unused preparation"
                            );
                            PreparedIsolatedTurn::new(self.admit_agent_effect().await?, input)?
                        }
                    };
                    let source = match turn.kind {
                        GoalParentTurnKind::Start | GoalParentTurnKind::Resume => {
                            IsolatedTranscriptSource::Canonical {
                                prefix: turn.working_history,
                            }
                        }
                        GoalParentTurnKind::Continue => IsolatedTranscriptSource::Continuation {
                            history: turn.working_history,
                            history_has_trim_breadcrumb: self.history_has_trim_breadcrumb,
                        },
                    };
                    let session_id = self.raw_session_id()?.to_owned();
                    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
                    let recall_query = self.recall_query.clone();
                    let run = prepared.run_with_presentation(source, &recall_query, event_tx);
                    tokio::pin!(run);

                    let mut event_rx_open = true;
                    let result = loop {
                        tokio::select! {
                            outcome = &mut run => {
                                while let Some(event) = event_rx.recv().await {
                                    self.publish_parent_event(&session_id, &event).await;
                                }
                                break outcome;
                            }
                            event = event_rx.recv(), if event_rx_open => {
                                match event {
                                    Some(event) => {
                                        self.publish_parent_event(&session_id, &event).await;
                                    }
                                    None => event_rx_open = false,
                                }
                            }
                        }
                    };
                    let (outcome, presentation) = result?;
                    // Complete the native stream before publishing the clean candidate.
                    // A client that joined halfway through can now keep the marked tail.
                    for delta in presentation {
                        let frame = super::dispatch::presentation_notification_for_chunk(
                            &session_id,
                            delta,
                        )?;
                        self.publish_frame(&session_id, frame, GoalFrameClass::ParentEvent)
                            .await;
                    }
                    self.history_has_trim_breadcrumb = outcome.history_has_trim_breadcrumb;
                    #[cfg(test)]
                    let outcome = {
                        let mut outcome = outcome;
                        if self
                            .context
                            .goal_runtime
                            .take_test_parent_projection_failure(&session_id)
                        {
                            outcome.working_history.clear();
                        }
                        outcome
                    };
                    let result = GoalParentTurnResult::from_completed_agent_turn(
                        outcome.response,
                        outcome.working_history,
                    )?;
                    // This is presentation fallback, not verifier acceptance or durable append.
                    let candidate = zeroclaw_providers::scrub_secret_patterns(result.candidate());
                    let candidate = {
                        let config = self.context.config.read();
                        crate::security::scrub_with_config(
                            &candidate,
                            &config.security.leak_detection,
                        )
                    };
                    let candidate = crate::agent::turn::scrub_credentials(&candidate);
                    let notification = encode_goal_update(
                        crate::rpc::types::SessionGoalUpdate::VerifiedCandidate {
                            session_id: session_id.clone(),
                            candidate,
                        },
                    )?;
                    // Losing the observer after admission must not turn successful work
                    // into a parent failure or prevent the controller from saving it.
                    self.publish_frame(&session_id, notification, GoalFrameClass::Candidate)
                        .await;
                    Ok(result)
                };
                tokio::pin!(work);
                root.scope(work).await
            })
            .await;
        if let Err(error) = &result {
            let _ = self.publish_parent_turn_failure(error).await;
        }
        result
    }

    async fn run_verifier(
        &mut self,
        operation: &GoalOperationScope,
        turn: GoalVerifierTurn,
    ) -> Result<GoalVerifierResult> {
        let cost_context = self.cost_context();
        let result: Result<GoalVerifierResult> = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(cost_context, async {
                let root = self.foreground_root(operation)?;
                let work = async {
                    let agent = self.admit_agent_effect().await?;
                    drop(agent);
                    let config = Arc::new(self.context.config.read().clone());
                    // Alias-aware construction can synchronously refresh credentials.
                    let (provider, _provider_ref, model_selector, route_resolver) =
                        tokio::task::spawn_blocking(move || {
                            build_session_model_provider(
                                &config,
                                &config.goal.verifier.model_provider,
                                config.goal.verifier.model.as_deref(),
                            )
                        })
                        .await
                        .context("Goal verifier provider construction task failed")??;
                    let route = route_resolver.resolve(&model_selector);
                    let messages = turn.to_chat_messages();
                    let mut settled_attempts = Vec::new();
                    let response = crate::agent::loop_::ResolvedModelAccess {
                        model_provider: provider.as_ref(),
                        provider_name: &route.provider_name,
                        model: &route.model,
                        dispatch_model: &model_selector,
                        temperature: None,
                    }
                    .run_model_query(
                        ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        &mut settled_attempts,
                    )
                    .await?;
                    let response = response.text.context("Goal verifier returned no text")?;
                    let attribution = verifier_attribution_from_attempts(
                        settled_attempts.iter().map(|attempt| {
                            (
                                attempt.accepted,
                                attempt.provider_ref.as_str(),
                                attempt.model.as_str(),
                            )
                        }),
                    );
                    Ok(GoalVerifierResult {
                        response,
                        attribution,
                    })
                };
                tokio::pin!(work);
                root.scope(work).await
            })
            .await;
        if let Err(error) = &result {
            let _ = self.publish_parent_turn_failure(error).await;
        }
        result
    }

    async fn append_parent_candidate(&mut self, candidate: String) -> Result<()> {
        let session_id = self.raw_session_id()?;
        let backend = self.context.session_backend.as_ref().context(
            "cannot publish Goal parent response: ZeroCode session persistence is disabled",
        );
        let session_key = format!("rpc_{session_id}");
        let message = ChatMessage::assistant(&candidate);
        let persisted = match backend {
            Ok(backend) => {
                #[cfg(test)]
                self.context
                    .sessions
                    .wait_test_effect_pause("goal-before-candidate-save")
                    .await;
                run_session_write(|| backend.append(&session_key, &message))
                    .context("persist ZeroCode Goal parent response")
            }
            Err(error) => Err(error),
        };
        self.recipient.clear_unsaved_candidate();
        persisted?;
        self.agent.lock().await.seed_history(&[message]);
        Ok(())
    }

    async fn publish_goal_notice(&mut self, notice: GoalExecutionNotice) -> Result<()> {
        // A failed launch can be reconciled before its first recipient is attached.
        // Never replace a current or pending reader; delivery still checks live authority.
        if matches!(notice, GoalExecutionNotice::Failed(_))
            && let Some(issuer) = self.unlaunched_issuer.take()
        {
            self.context
                .sessions
                .attach_vacant_goal_recipient(
                    self.raw_session_id()?,
                    self.session_generation,
                    &issuer,
                    self.authorization.frame_destination(),
                )
                .await;
        }
        let session_id = self.raw_session_id()?;
        let (kind, update) = match notice {
            GoalExecutionNotice::Completed => (
                "completed",
                crate::rpc::types::SessionGoalUpdate::Completed {
                    session_id: session_id.to_owned(),
                },
            ),
            GoalExecutionNotice::PausedForBlocker => (
                "paused_for_blocker",
                crate::rpc::types::SessionGoalUpdate::PausedForBlocker {
                    session_id: session_id.to_owned(),
                },
            ),
            GoalExecutionNotice::Failed(reason) => (
                "failed",
                crate::rpc::types::SessionGoalUpdate::Failed {
                    session_id: session_id.to_owned(),
                    reason: crate::rpc::types::goal_failure_reason_wire(reason),
                },
            ),
        };
        let frame = encode_goal_update(update)?;
        if self
            .publish_frame(session_id, frame, GoalFrameClass::Notice)
            .await
            == GoalFrameDelivery::NotDelivered
        {
            zeroclaw_log::record!(
                WARN,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Fail)
                    .with_outcome(zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(serde_json::json!({
                        "session_id": session_id,
                        "notice_kind": kind,
                    })),
                "ZeroCode Goal lifecycle notice was not delivered"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod verifier_attribution_tests {
    use super::{GoalVerifierAttribution, verifier_attribution_from_attempts};

    #[test]
    fn reports_the_provider_and_model_of_the_accepted_attempt() {
        let attribution = verifier_attribution_from_attempts([
            (false, "openai.primary", "gpt-primary"),
            (true, "anthropic.fallback", "claude-fallback"),
        ]);

        assert_eq!(
            attribution,
            Some(GoalVerifierAttribution {
                provider_ref: "anthropic.fallback".to_owned(),
                model: "claude-fallback".to_owned(),
            })
        );
    }

    #[test]
    fn leaves_attribution_absent_when_no_attempt_was_accepted() {
        let attribution = verifier_attribution_from_attempts([
            (false, "openai.primary", "gpt-primary"),
            (false, "anthropic.fallback", "claude-fallback"),
        ]);

        assert_eq!(attribution, None);
    }
}

#[cfg(test)]
mod zero_code_session_admission_tests {
    use super::acquire_zero_code_goal_session_guard;
    use std::{sync::Arc, time::Duration};
    use zeroclaw_goal::GoalParentTurnCheckpoint;
    use zeroclaw_infra::session_queue::SessionActorQueue;

    #[tokio::test]
    async fn lifecycle_fence_withdraws_unadmitted_goal_behind_replacement_reservation() {
        let queue = Arc::new(SessionActorQueue::new(8, 5, 60));
        let active_turn = queue.acquire("goal-session").await.unwrap();
        let replacement = queue.reserve("goal-session").await.unwrap();
        let checkpoint = GoalParentTurnCheckpoint::new();
        let waiter_checkpoint = checkpoint.clone();
        let waiter_queue = Arc::clone(&queue);
        let mut goal_waiter = zeroclaw_spawn::spawn!(async move {
            acquire_zero_code_goal_session_guard(waiter_queue, "goal-session", &waiter_checkpoint)
                .await
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            while queue.queue_depth("goal-session").await < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Goal execution must queue behind the already-reserved replacement");

        checkpoint.close();
        assert!(
            tokio::time::timeout(Duration::from_millis(500), &mut goal_waiter)
                .await
                .expect("lifecycle fencing must wake an unadmitted Goal worker")
                .expect("Goal worker must not panic")
                .is_err()
        );
        assert_eq!(
            queue.queue_depth("goal-session").await,
            2,
            "fenced Goal waiter must release its FIFO registration"
        );

        drop(active_turn);
        let replacement_guard = tokio::time::timeout(Duration::from_secs(1), replacement.acquire())
            .await
            .expect("replacement must proceed after the admitted turn settles")
            .expect("reserved replacement admission must succeed");
        drop(replacement_guard);
        assert_eq!(queue.queue_depth("goal-session").await, 0);
    }
}

#[cfg(test)]
#[derive(Clone, PartialEq, Eq)]
enum GoalTestBindFailure {
    RunningEpoch { key: String, epoch: i64 },
    NextBind { key: String },
}

#[cfg(test)]
pub(super) struct GoalTestDeferredCommitPause {
    pub session_id: String,
    pub entered: Arc<tokio::sync::Notify>,
    pub release: Arc<tokio::sync::Notify>,
}

/// ZeroCode's process-local Goal worker registry.
#[derive(Default)]
pub struct RpcGoalRuntime {
    supervisors: Mutex<HashMap<String, Arc<GoalExecutionSupervisor>>>,
    // Keep exact proof of an executor drained by RPC when a transient task
    // write failure prevents pausing it. Durable state remains in the ledger.
    interrupted_workers: Mutex<HashMap<String, GoalExecutionScope>>,
    #[cfg(test)]
    test_control_plane: std::sync::OnceLock<crate::control_plane::ControlPlaneHandle>,
    #[cfg(test)]
    test_launch_bind_failure: std::sync::Mutex<Option<GoalTestBindFailure>>,
    #[cfg(test)]
    pub(super) test_deferred_commit_pause: std::sync::Mutex<Option<GoalTestDeferredCommitPause>>,
    #[cfg(test)]
    pub(super) test_parent_projection_failure: std::sync::Mutex<Option<String>>,
}

impl RpcGoalRuntime {
    #[cfg(test)]
    fn take_test_parent_projection_failure(&self, session_id: &str) -> bool {
        let mut pending = self.test_parent_projection_failure.lock().unwrap();
        if pending.as_deref() == Some(session_id) {
            pending.take();
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    async fn wait_test_deferred_commit_pause(&self, session_id: &str) {
        let pause = {
            let mut pending = self.test_deferred_commit_pause.lock().unwrap();
            if pending
                .as_ref()
                .is_some_and(|pause| pause.session_id == session_id)
            {
                pending.take()
            } else {
                None
            }
        };
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    /// Inject one launch-bind failure without revoking notice-delivery authority.
    #[cfg(test)]
    pub(super) fn fail_test_launch_bind(&self, key: String, epoch: i64) {
        *self.test_launch_bind_failure.lock().unwrap() =
            Some(GoalTestBindFailure::RunningEpoch { key, epoch });
    }

    #[cfg(test)]
    pub(super) fn fail_test_next_bind(&self, key: String) {
        *self.test_launch_bind_failure.lock().unwrap() =
            Some(GoalTestBindFailure::NextBind { key });
    }

    #[cfg(test)]
    async fn check_test_launch_bind_failure(&self, session: &GoalSessionKey) -> Result<()> {
        let expected = self.test_launch_bind_failure.lock().unwrap().clone();
        let Some(expected) = expected else {
            return Ok(());
        };
        let key = match &expected {
            GoalTestBindFailure::RunningEpoch { key, .. }
            | GoalTestBindFailure::NextBind { key } => key,
        };
        if key != &session.durable_id() {
            return Ok(());
        }
        let should_fail = match &expected {
            GoalTestBindFailure::RunningEpoch { epoch, .. } => {
                let registry = self.current_control_plane().unwrap().goal_store()?;
                registry
                    .current_goal_for_session(key)
                    .await?
                    .is_some_and(|task| {
                        task.status == zeroclaw_api::task::TaskStatus::Running
                            && task.execution_epoch == *epoch
                    })
            }
            GoalTestBindFailure::NextBind { .. } => true,
        };
        if should_fail {
            let mut fault = self.test_launch_bind_failure.lock().unwrap();
            if fault.as_ref() == Some(&expected) {
                fault.take();
                bail!("private fixture launch bind detail");
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn install_test_control_plane(
        &self,
        handle: crate::control_plane::ControlPlaneHandle,
    ) {
        assert!(self.test_control_plane.set(handle).is_ok());
    }

    #[cfg(test)]
    pub(super) fn ensure_test_control_plane(&self, handle: ControlPlaneHandle) {
        self.test_control_plane.get_or_init(|| handle);
    }

    pub(super) fn current_control_plane(&self) -> Option<crate::control_plane::ControlPlaneHandle> {
        #[cfg(test)]
        if let Some(handle) = self.test_control_plane.get() {
            return Some(handle.clone());
        }
        control_plane().cloned()
    }

    /// Opening another session must not dispose this session's Goal. The
    /// optional sibling sweep retains both live and durable Goal owners,
    /// including those that still expose a terminal result.
    pub(super) async fn can_evict_session_goal(&self, session_id: &str) -> Result<bool> {
        if self.supervisor(session_id).await.is_some() {
            return Ok(false);
        }
        let Some(control_plane) = self.current_control_plane() else {
            return Ok(true);
        };
        let durable_session_id = GoalSessionKey::zero_code(session_id)?.durable_id();
        Ok(control_plane
            .goal_store()?
            .current_goal_for_session(&durable_session_id)
            .await?
            .is_none())
    }

    /// Orphan retirement only detaches a live entry; it never settles a pending
    /// operation or deletes the durable Goal.
    pub(super) async fn can_retire_session_goal(&self, session_id: &str) -> Result<bool> {
        let durable_session_id = GoalSessionKey::zero_code(session_id)?.durable_id();
        if let Some(supervisor) = self.supervisor(session_id).await {
            if !supervisor
                .try_retire_terminal_session(&durable_session_id)
                .await?
            {
                return Ok(false);
            }
            self.remove_supervisor_if_current(session_id, &supervisor)
                .await;
            return Ok(true);
        }
        let Some(control_plane) = self.current_control_plane() else {
            return Ok(true);
        };
        let registry = control_plane.goal_store()?;
        let Some(task) = registry
            .current_goal_for_session(&durable_session_id)
            .await?
        else {
            return Ok(true);
        };

        if !task.status.is_terminal() {
            return Ok(false);
        }
        let goal = registry
            .get_goal_task(&task.id)
            .await?
            .context("Goal extension disappeared during session retirement")?;
        Ok(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none())
    }

    /// Withdraw new worker admission while preserving the still-bound Goal.
    /// The caller owns the generation-local fence and a tracked future.
    pub(super) async fn quiesce_for_session_transition(
        &self,
        session_id: &str,
    ) -> Result<Option<GoalSessionQuiescence>> {
        let Ok(key) = GoalSessionKey::zero_code(session_id) else {
            return Ok(None);
        };
        let durable_session_id = key.durable_id();
        let quiescence = if let Some(supervisor) = self.supervisor(session_id).await {
            supervisor
                .quiesce_session_worker(&durable_session_id)
                .await?
        } else {
            None
        };
        if quiescence
            .as_ref()
            .is_some_and(GoalSessionQuiescence::has_failure_notice)
        {
            // The caller must let go of any actor reservation before the
            // outstanding native failure notice can acquire that actor.
            return Ok(quiescence);
        }
        if let Some(quiescence) = &quiescence {
            self.interrupted_workers
                .lock()
                .await
                .insert(session_id.to_owned(), quiescence.scope().clone());
        }

        let Some(control_plane) = self.current_control_plane() else {
            ensure!(
                quiescence.is_none(),
                "Goal control plane unavailable during drain"
            );
            return Ok(None);
        };
        let registry = control_plane.goal_store()?;
        let exact_interruption = self
            .interrupted_workers
            .lock()
            .await
            .get(session_id)
            .cloned();
        if let Some(scope) = exact_interruption {
            control_plane
                .session_dispositions()?
                .pause_running_goal_for_session_interruption(
                    scope.task_id(),
                    &durable_session_id,
                    scope.execution_epoch(),
                )
                .await?;
            let current = registry
                .current_goal_for_session(&durable_session_id)
                .await?;
            ensure!(
                current
                    .as_ref()
                    .is_none_or(|task| task.status != zeroclaw_api::task::TaskStatus::Running),
                "Goal executor could not be settled for the session transition"
            );
            let mut interrupted = self.interrupted_workers.lock().await;
            if interrupted.get(session_id).is_some_and(|current| {
                current.task_id() == scope.task_id()
                    && current.execution_epoch() == scope.execution_epoch()
            }) {
                interrupted.remove(session_id);
            }
        } else {
            ensure!(
                registry
                    .current_goal_for_session(&durable_session_id)
                    .await?
                    .is_none_or(|task| task.status != zeroclaw_api::task::TaskStatus::Running),
                "Goal launch is still settling for this session"
            );
        }
        Ok(quiescence)
    }

    /// Dispose fixture Goal state without applying a native session effect.
    #[cfg(test)]
    pub(super) async fn dispose_session(&self, session_id: &str) -> Result<()> {
        let durable_session_id = GoalSessionKey::zero_code(session_id)?.durable_id();
        if let Some(supervisor) = self.supervisor(session_id).await {
            let result = supervisor.dispose_session(&durable_session_id).await?;
            self.confirm_goal_disposed(&durable_session_id, result)
                .await?;
            self.retire_supervisor_if_idle(session_id, &supervisor)
                .await;
        } else if let Some(control_plane) = self.current_control_plane() {
            let registry = control_plane.goal_store()?;
            let result =
                dispose_unowned_session_goal(registry.as_ref(), &durable_session_id).await?;
            self.confirm_goal_disposed(&durable_session_id, result)
                .await?;
        }
        Ok(())
    }

    #[cfg(test)]
    async fn confirm_goal_disposed(
        &self,
        durable_session_id: &str,
        result: GoalTransitionResult,
    ) -> Result<()> {
        match result {
            GoalTransitionResult::Applied => Ok(()),
            GoalTransitionResult::Stale => anyhow::bail!("Goal changed during session disposal"),
            GoalTransitionResult::Missing => {
                let control_plane = self
                    .current_control_plane()
                    .context("Goal control plane unavailable during session disposal")?;
                ensure!(
                    control_plane
                        .goal_store()?
                        .current_goal_for_session(durable_session_id)
                        .await?
                        .is_none(),
                    "Goal changed during session disposal"
                );
                Ok(())
            }
        }
    }

    /// Submit one parsed Goal command through the current ZeroCode session.
    pub(super) async fn submit(
        self: &Arc<Self>,
        context: Arc<RpcContext>,
        driver: Arc<ZeroCodeGoalSessionDriver>,
        command: GoalCommand,
    ) -> Result<RpcGoalCommandSubmission> {
        let config = context.config.read().clone();
        let issuer = GoalCommandIssuer {
            authorization: Arc::clone(&driver.authorization),
            generation: driver.session_generation,
        };
        if !config.goal.enabled {
            return Ok(RpcGoalCommandSubmission {
                response: GoalResponse::Disabled,
                issuer,
                post_response: None,
            });
        }
        let control_plane = driver
            .control_plane
            .clone()
            .context("Goal control plane is unavailable")?;
        let registry = control_plane.goal_store()?;
        let restart_coordinator = control_plane.goal_execution_restart();
        let settings_source = GoalHostSettingsSource::new(
            context.config.clone(),
            std::process::id(),
            control_plane.boot_id.clone(),
        )?;
        let session_id = driver.raw_session_id().to_owned();
        let ingress = GoalIngressContext::trusted(
            driver.session_key().clone(),
            driver.agent_alias().to_owned(),
            format!("zerocode:{session_id}"),
            GoalIngressPrincipal::ZeroCode {
                tui_id: driver.tui_id.clone(),
            },
        )?;
        let (supervisor, admission) = loop {
            let supervisor = match self.supervisor(&session_id).await {
                Some(supervisor) => supervisor,
                None => {
                    let runtime = GoalRuntime::new(Arc::clone(&registry));
                    let tracker = match context.cost_tracker.as_ref() {
                        Some(tracker) => Arc::clone(tracker),
                        None => CostTracker::get_or_init_global_required(
                            config.cost.clone(),
                            &config.data_dir,
                        )?,
                    };
                    let pricer = Arc::new(ZeroCodeGoalUsagePricer {
                        config: context.config.clone(),
                    });
                    let accounting = Arc::new(RuntimeGoalOperationAccountingFactory::new(
                        Arc::clone(&registry),
                        tracker,
                        driver.agent_alias().to_owned(),
                        pricer,
                    )?);
                    let engine = Arc::new(
                        runtime
                            .execution_engine(accounting)
                            .with_diagnostic_text_guard(Arc::new(RuntimeGoalDiagnosticTextGuard)),
                    );
                    // Register with restart only after winning the session's
                    // slot. The coordinator retains a strong registration.
                    let mut supervisors = self.supervisors.lock().await;
                    match supervisors.entry(session_id.clone()) {
                        std::collections::hash_map::Entry::Occupied(entry) => {
                            Arc::clone(entry.get())
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            let supervisor = restart_coordinator.new_supervisor(engine).await;
                            Arc::clone(entry.insert(supervisor))
                        }
                    }
                }
            };
            let current_config = context.config.snapshot();
            let settings = match GoalHostSettings::from_config(
                &current_config,
                std::process::id(),
                control_plane.boot_id.clone(),
            ) {
                Ok(settings) => settings,
                Err(error) => {
                    self.retire_supervisor_if_idle(&session_id, &supervisor)
                        .await;
                    return Err(error);
                }
            };
            match supervisor
                .admit_control(&settings, ingress.clone(), driver.clone(), command.clone())
                .await
            {
                Ok(admission) => break (supervisor, admission),
                Err(error) if error.is::<GoalSupervisorRetired>() => {
                    // Retirement preceded custody admission, so this command
                    // has made no effects and can select the resident owner again.
                    self.remove_supervisor_if_current(&session_id, &supervisor)
                        .await;
                }
                Err(error) => {
                    self.retire_supervisor_if_idle(&session_id, &supervisor)
                        .await;
                    return Err(error);
                }
            }
        };
        let (response, admission) = match admission {
            GoalControlAdmission::Direct(submission) => (
                submission.response().clone(),
                GoalControlAdmission::Direct(submission),
            ),
            GoalControlAdmission::Queued { response, deferred } => (
                response.clone(),
                GoalControlAdmission::Queued {
                    response,
                    deferred: deferred
                        .with_abandon_reason(GoalControlAbandonReason::ConnectionClosed),
                },
            ),
        };
        Ok(RpcGoalCommandSubmission {
            response,
            issuer: issuer.clone(),
            post_response: Some(RpcGoalPostResponse {
                runtime: Arc::clone(self),
                context,
                control_plane,
                session_id,
                supervisor,
                settings_source,
                admission,
                issuer,
            }),
        })
    }
    /// Return a resident supervisor for this session, if it has one.
    pub async fn supervisor(&self, session_id: &str) -> Option<Arc<GoalExecutionSupervisor>> {
        self.supervisors.lock().await.get(session_id).cloned()
    }

    async fn retire_supervisor_if_idle(
        &self,
        session_id: &str,
        supervisor: &Arc<GoalExecutionSupervisor>,
    ) {
        if supervisor.try_retire().await {
            self.remove_supervisor_if_current(session_id, supervisor)
                .await;
        }
    }

    /// Install a preconstructed supervisor for a test fixture.
    #[cfg(test)]
    pub(crate) async fn install_supervisor_if_absent(
        &self,
        session_id: String,
        supervisor: Arc<GoalExecutionSupervisor>,
    ) -> Arc<GoalExecutionSupervisor> {
        Arc::clone(
            self.supervisors
                .lock()
                .await
                .entry(session_id)
                .or_insert(supervisor),
        )
    }

    /// Remove a supervisor only if it is still the instance that completed the
    /// current lifecycle command.
    pub async fn remove_supervisor_if_current(
        &self,
        session_id: &str,
        supervisor: &Arc<GoalExecutionSupervisor>,
    ) {
        let mut supervisors = self.supervisors.lock().await;
        if supervisors
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, supervisor))
        {
            supervisors.remove(session_id);
        }
    }
}
