//! Deferred controls retain command intent, never a snapshot of live authority.

use std::{fmt, sync::Weak};

use chrono::Utc;
use uuid::Uuid;

use super::super::{GoalControlClass, classify_goal_control};
use super::*;
use crate::task::{GoalControlIntent, GoalControlIntentKind, GoalControlIntentSettlement};

/// Either an ordinary response, or an acknowledgement with an owned continuation.
#[derive(Debug)]
#[must_use = "publish the direct response or run the owned queued continuation"]
pub enum GoalControlAdmission {
    Direct(GoalExecutionSubmission),
    Queued {
        response: GoalResponse,
        deferred: GoalDeferredControl,
    },
}

/// Audit-only abandonment; none of these reasons mutates Goal lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalControlAbandonReason {
    ControlDropped,
    DeliveryFailed,
    ConnectionClosed,
}

impl GoalControlAbandonReason {
    const fn token(self) -> &'static str {
        match self {
            Self::ControlDropped => "control_dropped",
            Self::DeliveryFailed => "delivery_failed",
            Self::ConnectionClosed => "connection_closed",
        }
    }
}

enum WaitResource {
    Worker(GoalDeferredControlTicket),
    Boundary(GoalParentTurnCheckpoint),
}

/// A queued control waiting without command serialization. The surface owns
/// this value in its tracked command task; dropping it abandons the audit intent.
#[must_use = "acknowledge and wait, or explicitly abandon this queued control"]
pub struct GoalDeferredControl {
    supervisor: Arc<GoalExecutionSupervisor>,
    driver: Arc<dyn GoalSessionDriver>,
    ingress: GoalIngressContext,
    command: Option<GoalCommand>,
    guard: IntentGuard,
    resource: Option<WaitResource>,
    custody: Option<OwnedRwLockReadGuard<()>>,
}

impl fmt::Debug for GoalDeferredControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoalDeferredControl")
            .field("kind", &self.command.as_ref().map(classify_goal_control))
            .finish_non_exhaustive()
    }
}

/// Boundary reached with a freshly validated command lease. Commit takes live
/// settings again; waiting never turns an earlier grant into durable authority.
#[must_use = "commit using live settings or abandon the owned control"]
pub struct GoalDeferredReady {
    control: GoalDeferredControl,
    lease: Option<GoalSessionLease>,
    boundary: Option<GoalTurnBoundary>,
}

impl fmt::Debug for GoalDeferredReady {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoalDeferredReady")
            .field("control", &self.control)
            .finish_non_exhaustive()
    }
}

/// Transition-only result. The surface must publish Started/Resumed before
/// release, then drain and launch through the supervisor's existing handoff.
#[must_use = "publish the required lifecycle response, then release and finish the handoff"]
pub struct GoalDeferredCommit {
    control: GoalDeferredControl,
    submission: GoalExecutionSubmission,
    boundary: Option<GoalTurnBoundary>,
}

impl fmt::Debug for GoalDeferredCommit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoalDeferredCommit")
            .field("response", self.submission.response())
            .finish_non_exhaustive()
    }
}

/// A wait ended before it could obtain live command authority. `response` is
/// the current durable outcome, not an inferred lifecycle transition.
#[derive(Debug)]
pub struct GoalDeferredClosed {
    response: GoalResponse,
    cause: anyhow::Error,
}

impl GoalDeferredClosed {
    pub fn response(&self) -> &GoalResponse {
        &self.response
    }
}

impl fmt::Display for GoalDeferredClosed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Goal deferred control closed before admission")
    }
}

impl std::error::Error for GoalDeferredClosed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

/// Only audit settlement is owned here; canonical task state is never inferred
/// from this guard or read back from the intent table.
struct IntentGuard {
    registry: Arc<dyn GoalTaskRegistry>,
    intent: GoalControlIntent,
    open: bool,
    cleanup: Option<(Weak<GoalExecutionSupervisor>, String, Uuid)>,
    drop_reason: GoalControlAbandonReason,
}

impl IntentGuard {
    async fn settle(&mut self, settlement: GoalControlIntentSettlement) -> Result<()> {
        self.registry
            .settle_control_intent(&self.intent.intent_id, settlement)
            .await?;
        // Stale/Missing can mean another owner already settled/deleted the row.
        // Neither permits overwriting it or claiming a lifecycle transition.
        self.open = false;
        Ok(())
    }
}

impl Drop for IntentGuard {
    fn drop(&mut self) {
        if !self.open && self.cleanup.is_none() {
            return;
        }
        let registry = self.registry.clone();
        let intent_id = self.intent.intent_id.clone();
        let open = self.open;
        let reason = self.drop_reason.token();
        let cleanup = self.cleanup.take();
        zeroclaw_spawn::spawn!(async move {
            if open
                && registry
                    .settle_control_intent(
                        &intent_id,
                        GoalControlIntentSettlement::Abandoned {
                            reason: reason.into(),
                        },
                    )
                    .await
                    .is_err()
            {
                // Keep the row unresolved for boot reconciliation. Drop cannot
                // report success or safely reveal arbitrary backend error text.
                zeroclaw_log::record!(ERROR,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                        .with_attrs(serde_json::json!({"error_key": "goal_control_intent_settlement_failed"})),
                    "Goal control intent abandonment could not be persisted");
            }
            if let Some((supervisor, session, generation)) = cleanup
                && let Some(supervisor) = supervisor.upgrade()
            {
                supervisor
                    .remove_pending_boundary(&session, generation)
                    .await;
            }
        });
    }
}

impl GoalExecutionSupervisor {
    /// Admit a normal control at an operation/ordinary-turn boundary, or keep
    /// existing immediate and read-only behavior. Queue acknowledgement follows
    /// durable intent recording and registration, never precedes them.
    ///
    /// A retired supervisor returns [`super::GoalSupervisorRetired`] before
    /// admission has any effects. The adapter must select a current supervisor
    /// before retrying the command.
    /// Build settings from the current canonical config for this command;
    /// queued controls retain intent, not these resolved settings.
    pub async fn admit_control(
        self: &Arc<Self>,
        settings: &GoalHostSettings<'_>,
        ingress: GoalIngressContext,
        driver: Arc<dyn GoalSessionDriver>,
        command: GoalCommand,
    ) -> Result<GoalControlAdmission> {
        let custody = self.enter_custody().await?;
        let GoalControlClass::Normal(kind) = classify_goal_control(&command) else {
            return self
                .submit_with_custody(settings, ingress, driver, command, custody)
                .await
                .map(GoalControlAdmission::Direct);
        };
        let mut custody = Some(custody);
        // Restart closes admission before collecting pending waiters. Keep its
        // read guard only through registration, never through the queued wait;
        // commit obtains a fresh guard after the live boundary is reached.
        let intake_admission = match &self.restart_gate {
            Some(gate) => gate.admit(&command).await?,
            None => None,
        };
        let submission = self
            .engine
            .runtime
            .host
            .submit(settings, ingress.clone(), driver.clone(), command.clone())
            .await?;
        let (rejection, current) = self
            .engine
            .runtime
            .controller
            .preflight_normal(settings, &submission)
            .await?;
        if let Some(response) = rejection {
            let result = GoalRuntimeSubmission {
                response,
                execution: None,
                lease: submission.into_admission_lease(),
            };
            return Ok(GoalControlAdmission::Direct(self.execution_submission(
                result,
                None,
                driver,
                ingress,
                None,
                None,
                &mut custody,
            )));
        }

        let mut guard = None;
        if let Some(task) = current
            .as_ref()
            .filter(|task| task.status == TaskStatus::Running)
        {
            let scope = GoalExecutionScope::new(
                task.id.clone(),
                ingress.session_key().durable_id(),
                task.execution_epoch,
            )?;
            if self.owns_scope(&scope).await {
                guard = Some(
                    self.record_intent(settings, &ingress, &command, current.as_ref())
                        .await?,
                );
                if let Some(ticket) = self.register_deferred_control(&scope).await? {
                    drop(submission);
                    let control = GoalDeferredControl {
                        supervisor: self.clone(),
                        driver,
                        ingress,
                        command: Some(command),
                        guard: guard
                            .take()
                            .context("Goal intent missing after registration")?,
                        resource: Some(WaitResource::Worker(ticket)),
                        custody,
                    };
                    return Ok(GoalControlAdmission::Queued {
                        response: GoalResponse::Queued(kind),
                        deferred: control,
                    });
                }
            }
        }

        let boundary = driver.try_turn_boundary(&ingress).await?;
        if let Some(boundary) = boundary {
            if let Some(guard) = guard {
                // Worker closure raced registration. Reuse the exact audit
                // binding rather than create another intent or apply blindly.
                let lease = submission
                    .into_admission_lease()
                    .context("Goal command lease missing")?;
                let ready = GoalDeferredReady {
                    control: GoalDeferredControl {
                        supervisor: self.clone(),
                        driver,
                        ingress,
                        command: Some(command),
                        guard,
                        resource: None,
                        custody,
                    },
                    lease: Some(lease),
                    boundary: Some(boundary),
                };
                drop(intake_admission);
                return ready
                    .commit(settings)
                    .await
                    .map(|commit| GoalControlAdmission::Direct(commit.into_direct()));
            }
            let previous = self.scope_for_session(&ingress).await?;
            let result = self
                .engine
                .runtime
                .apply_submission(settings, submission)
                .await?;
            return Ok(GoalControlAdmission::Direct(self.execution_submission(
                result,
                previous,
                driver,
                ingress,
                intake_admission,
                Some(boundary),
                &mut custody,
            )));
        }

        let mut guard = match guard {
            Some(guard) => guard,
            None => {
                self.record_intent(settings, &ingress, &command, current.as_ref())
                    .await?
            }
        };
        let checkpoint = GoalParentTurnCheckpoint::new();
        let session = ingress.session_key().durable_id();
        self.pending_boundaries
            .lock()
            .await
            .entry(session.clone())
            .or_default()
            .push(checkpoint.clone());
        guard.cleanup = Some((Arc::downgrade(self), session, checkpoint.generation()));
        drop(submission);
        Ok(GoalControlAdmission::Queued {
            response: GoalResponse::Queued(kind),
            deferred: GoalDeferredControl {
                supervisor: self.clone(),
                driver,
                ingress,
                command: Some(command),
                guard,
                resource: Some(WaitResource::Boundary(checkpoint)),
                custody,
            },
        })
    }

    async fn record_intent(
        &self,
        settings: &GoalHostSettings<'_>,
        ingress: &GoalIngressContext,
        command: &GoalCommand,
        current: Option<&TaskRecord>,
    ) -> Result<IntentGuard> {
        let kind = match command {
            GoalCommand::Start { budget, .. } => GoalControlIntentKind::Start { budget: *budget },
            GoalCommand::Resume { response } => GoalControlIntentKind::Resume {
                has_response: response.is_some(),
            },
            GoalCommand::Pause { immediate: false } => GoalControlIntentKind::Pause,
            GoalCommand::SetBudget(budget) => GoalControlIntentKind::SetBudget { budget: *budget },
            _ => bail!("non-normal Goal control cannot create an intent"),
        };
        let start = matches!(kind, GoalControlIntentKind::Start { .. });
        let intent = GoalControlIntent {
            intent_id: Uuid::new_v4().to_string(),
            session_id: ingress.session_key().durable_id(),
            task_id: if start {
                None
            } else {
                current.map(|task| task.id.clone())
            },
            expected_epoch: if start {
                None
            } else {
                current.map(|task| task.execution_epoch)
            },
            predecessor_task_id: if start {
                current.map(|task| task.id.clone())
            } else {
                None
            },
            kind,
            owner_pid: settings.owner_pid,
            owner_boot_id: settings.owner_boot_id.clone(),
            created_at: Utc::now().to_rfc3339(),
        };
        self.engine.registry.record_control_intent(&intent).await?;
        Ok(IntentGuard {
            registry: self.engine.registry.clone(),
            intent,
            open: true,
            cleanup: None,
            drop_reason: GoalControlAbandonReason::ControlDropped,
        })
    }

    pub(super) async fn close_pending_boundaries(&self, session: Option<&str>) {
        let mut pending = self.pending_boundaries.lock().await;
        if let Some(session) = session {
            if let Some(checkpoints) = pending.remove(session) {
                for checkpoint in checkpoints {
                    checkpoint.close();
                }
            }
        } else {
            for (_, checkpoints) in pending.drain() {
                for checkpoint in checkpoints {
                    checkpoint.close();
                }
            }
        }
    }

    async fn remove_pending_boundary(&self, session: &str, generation: Uuid) {
        let mut pending = self.pending_boundaries.lock().await;
        if let Some(checkpoints) = pending.get_mut(session) {
            checkpoints.retain(|checkpoint| checkpoint.generation() != generation);
            if checkpoints.is_empty() {
                pending.remove(session);
            }
        }
    }
}

impl GoalDeferredControl {
    /// Label task-abort abandonment at the owning surface before its tracked
    /// wait. This neither snapshots authority nor cancels a running Goal.
    pub fn with_abandon_reason(mut self, reason: GoalControlAbandonReason) -> Self {
        self.guard.drop_reason = reason;
        self
    }

    /// Abandon an uncommitted control after acknowledgement/connection failure.
    /// Dropping this value releases its registered ticket or ordinary wait.
    pub async fn abandon(mut self, reason: GoalControlAbandonReason) -> Result<()> {
        self.guard.drop_reason = reason;
        self.guard
            .settle(GoalControlIntentSettlement::Abandoned {
                reason: reason.token().into(),
            })
            .await
    }

    /// Wait without retaining command serialization across ordinary-turn waits.
    /// A Worker ticket holds the worker at its boundary; a Boundary waiter never
    /// waits on the command lock while owning the ordinary foreground permit.
    pub async fn wait(mut self) -> std::result::Result<GoalDeferredReady, GoalDeferredClosed> {
        let result = self.wait_for_lease().await;
        match result {
            Ok((lease, boundary)) => Ok(GoalDeferredReady {
                control: self,
                lease: Some(lease),
                boundary,
            }),
            Err(error) => {
                // A denied live bind must not reveal state through an obsolete
                // ingress grant. Only a known checkpoint fence reloads state.
                let closed = error.is::<GoalCheckpointClosed>();
                let response = if closed {
                    match self.current_response().await {
                        Ok(response) => response,
                        Err(cause) => {
                            return Err(GoalDeferredClosed {
                                response: GoalResponse::Stale,
                                cause,
                            });
                        }
                    }
                } else {
                    GoalResponse::Stale
                };
                let settlement = match (&self.resource, closed) {
                    (Some(WaitResource::Worker(_)), true) => {
                        GoalControlIntentSettlement::Superseded {
                            reason: "checkpoint_closed".into(),
                        }
                    }
                    _ => GoalControlIntentSettlement::Abandoned {
                        reason: "session_closed".into(),
                    },
                };
                if let Err(cause) = self.guard.settle(settlement).await {
                    return Err(GoalDeferredClosed {
                        response: GoalResponse::Stale,
                        cause,
                    });
                }
                Err(GoalDeferredClosed {
                    response,
                    cause: error,
                })
            }
        }
    }

    async fn wait_for_lease(&mut self) -> Result<(GoalSessionLease, Option<GoalTurnBoundary>)> {
        match self
            .resource
            .as_mut()
            .context("Goal deferred wait has no registration")?
        {
            WaitResource::Worker(ticket) => {
                ticket.wait_until_turn().await?;
                let lease = tokio::select! {
                    biased;
                    () = ticket.wait_closed() => return Err(GoalCheckpointClosed.into()),
                    lease = self.driver.bind(&self.ingress) => lease?,
                };
                self.validate_lease(&lease)?;
                Ok((lease, None))
            }
            WaitResource::Boundary(checkpoint) => loop {
                let lease = tokio::select! {
                    biased;
                    () = checkpoint.wait_closed() => return Err(GoalCheckpointClosed.into()),
                    lease = self.driver.bind(&self.ingress) => lease?,
                };
                if lease.binding().session_key() != self.ingress.session_key()
                    || self.driver.session_key() != self.ingress.session_key()
                {
                    bail!("Goal deferred binding does not match trusted ingress");
                }
                if let Some(boundary) = self.driver.try_turn_boundary(&self.ingress).await? {
                    // Custody and both leases are still held. Finish this
                    // registration now rather than scheduling its removal on
                    // Drop, which would make an idle supervisor look busy.
                    if let Some((_, session, generation)) = self.guard.cleanup.as_ref() {
                        self.supervisor
                            .remove_pending_boundary(session, *generation)
                            .await;
                    }
                    self.guard.cleanup = None;
                    return Ok((lease, Some(boundary)));
                }
                drop(lease);
                let boundary = self
                    .driver
                    .await_turn_boundary(&self.ingress, checkpoint)
                    .await?;
                drop(boundary);
            },
        }
    }

    fn validate_lease(&self, lease: &GoalSessionLease) -> Result<()> {
        ensure!(
            lease.binding().session_key() == self.ingress.session_key()
                && self.driver.session_key() == self.ingress.session_key(),
            "Goal deferred binding does not match trusted ingress"
        );
        Ok(())
    }

    async fn binding_matches(&self) -> Result<bool> {
        let current = self
            .supervisor
            .engine
            .runtime
            .controller
            .current(&self.ingress, &self.guard.intent.session_id)
            .await?;
        Ok(match (&self.guard.intent.kind, current) {
            (GoalControlIntentKind::Start { .. }, None) => {
                self.guard.intent.predecessor_task_id.is_none()
                    && self
                        .supervisor
                        .engine
                        .registry
                        .current_goal_for_session(&self.guard.intent.session_id)
                        .await?
                        .is_none()
            }
            (GoalControlIntentKind::Start { .. }, Some(task)) => {
                task.status.is_terminal()
                    && self.guard.intent.predecessor_task_id.as_deref() == Some(task.id.as_str())
            }
            (kind, Some(task)) => {
                self.guard.intent.task_id.as_deref() == Some(task.id.as_str())
                    && self.guard.intent.expected_epoch == Some(task.execution_epoch)
                    && match kind {
                        GoalControlIntentKind::Pause => task.status == TaskStatus::Running,
                        GoalControlIntentKind::Resume { .. } => task.status == TaskStatus::Paused,
                        GoalControlIntentKind::SetBudget { .. } => !task.status.is_terminal(),
                        GoalControlIntentKind::Start { .. } => false,
                    }
            }
            _ => false,
        })
    }

    async fn current_response(&self) -> Result<GoalResponse> {
        let controller = &self.supervisor.engine.runtime.controller;
        let session = &self.guard.intent.session_id;
        let Some(task) = controller.current(&self.ingress, session).await? else {
            return Ok(GoalResponse::NoCurrentGoal);
        };
        if task.status.is_terminal() {
            controller
                .projection_response_for_current(
                    &self.ingress,
                    session,
                    &task,
                    GoalResponse::Terminal,
                )
                .await
        } else if task.status == TaskStatus::Paused {
            controller
                .projection_response_for_current(
                    &self.ingress,
                    session,
                    &task,
                    GoalResponse::AlreadyPaused,
                )
                .await
        } else if task.status == TaskStatus::Running {
            Ok(GoalResponse::AlreadyActive)
        } else {
            Ok(GoalResponse::Stale)
        }
    }
}

impl GoalDeferredReady {
    /// Set the audit-only abandonment reason after boundary acquisition.
    pub fn with_abandon_reason(mut self, reason: GoalControlAbandonReason) -> Self {
        self.control.guard.drop_reason = reason;
        self
    }

    /// Claim once and commit through the same driver lease. This performs no
    /// predecessor drain, successor launch, lifecycle publication or rebind.
    /// Build settings from the current canonical config after `wait` returns.
    pub async fn commit(mut self, settings: &GoalHostSettings<'_>) -> Result<GoalDeferredCommit> {
        let control = &mut self.control;
        let command = control
            .command
            .take()
            .context("Goal deferred command missing")?;
        let admission = match &control.supervisor.restart_gate {
            Some(gate) => match gate.admit(&command).await {
                Ok(admission) => admission,
                Err(error) => {
                    control
                        .guard
                        .settle(GoalControlIntentSettlement::Abandoned {
                            reason: "refused_admission".into(),
                        })
                        .await?;
                    return Err(error);
                }
            },
            None => None,
        };
        let claim = control
            .supervisor
            .engine
            .registry
            .claim_control_intent(&control.guard.intent.intent_id, &settings.owner_boot_id)
            .await?;
        let response = if claim != GoalTransitionResult::Applied {
            control
                .guard
                .settle(GoalControlIntentSettlement::Superseded {
                    reason: "claim_stale".into(),
                })
                .await?;
            Some(GoalResponse::Stale)
        } else if !control.binding_matches().await? {
            control
                .guard
                .settle(GoalControlIntentSettlement::Superseded {
                    reason: "binding_changed".into(),
                })
                .await?;
            Some(control.current_response().await?)
        } else {
            None
        };
        let lease = self
            .lease
            .take()
            .context("Goal deferred command lease missing")?;
        let previous = control
            .supervisor
            .scope_for_session(&control.ingress)
            .await?;
        let needs_audit = response.is_none();
        let runtime_submission = if let Some(response) = response {
            GoalRuntimeSubmission {
                response,
                execution: None,
                lease: Some(lease),
            }
        } else {
            control
                .supervisor
                .engine
                .runtime
                .submit_bound(
                    settings,
                    control.ingress.clone(),
                    control.driver.clone(),
                    command,
                    lease,
                )
                .await?
        };
        let applied = matches!(
            runtime_submission.response(),
            GoalResponse::Started(_)
                | GoalResponse::Resumed(_)
                | GoalResponse::Paused(_)
                | GoalResponse::BudgetUpdated(_)
        );
        // Own the durable admission before audit persistence can suspend.
        let mut submission = control.supervisor.execution_submission(
            runtime_submission,
            previous,
            control.driver.clone(),
            control.ingress.clone(),
            admission,
            None,
            &mut control.custody,
        );
        if needs_audit {
            let settlement = if applied {
                GoalControlIntentSettlement::Applied
            } else {
                GoalControlIntentSettlement::Superseded {
                    reason: "transition_not_applied".into(),
                }
            };
            if let Err(error) = control.guard.settle(settlement).await {
                // A durable Start/Resume is not yet supervisor-owned. An
                // audit error must not drop its only launch request and leave
                // Running behind without an owner. Use the existing launch
                // failure reconciliation; never admit model work on this path.
                submission.launch_guard.failed();
                if let Some(scope) = submission.launch_guard.scope() {
                    control
                        .supervisor
                        .reconcile_launch_failure(scope)
                        .await
                        .context("reconcile unlaunched Goal after control audit failure")?;
                }
                submission.launch_guard.settled();
                return Err(error).context("persist Goal control transition audit");
            }
        }
        Ok(GoalDeferredCommit {
            control: self.control,
            submission,
            boundary: self.boundary,
        })
    }
}

impl GoalDeferredCommit {
    pub fn response(&self) -> &GoalResponse {
        self.submission.response()
    }

    /// Release command and checkpoint/ordinary-turn ownership before any drain.
    pub fn release(mut self) -> GoalExecutionSubmission {
        drop(self.submission._lease.take());
        drop(self.boundary.take());
        drop(self.control.resource.take());
        self.submission
    }

    fn into_direct(mut self) -> GoalExecutionSubmission {
        self.submission.boundary = self.boundary.take();
        self.submission
    }
}
