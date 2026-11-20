//! Exact external-interruption custody, separate from transport authority.

use super::*;

#[derive(Debug)]
pub(super) struct GoalExternalStopClosed;

impl std::fmt::Display for GoalExternalStopClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Goal launch is closed by an exact external-stop fence")
    }
}

impl std::error::Error for GoalExternalStopClosed {}

/// Canonical disposition observed for one captured external-stop scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalExternalStopDisposition {
    /// The captured Goal is canonically paused; opaque pause state is preserved.
    Paused,
    /// The canonical terminal winner is preserved, including genuine failure.
    Terminal(TaskStatus),
    /// A newer Running scope has its actual exact worker or held launch owner.
    SuccessorOwned,
    /// The old Goal no longer has a canonical session row.
    Absent,
    /// A different nonrunning Goal has replaced the captured one.
    Superseded,
    /// Continuation is closed, but its canonical lifecycle duty is unresolved.
    Unresolved,
}

impl GoalExternalStopDisposition {
    pub(super) fn settled(self) -> bool {
        self != Self::Unresolved
    }
}

/// An observed disposition, never a new lifecycle or authorization authority.
#[derive(Debug)]
pub struct GoalExternalCancellationScope {
    pub scope: GoalExecutionScope,
    pub disposition: GoalExternalStopDisposition,
}

/// External-stop outcome after unlocked admitted-operation drain.
/// Inspect all independent facts before signalling ordinary cancellation, then
/// deliver or explicitly withdraw any owed notice after the request response.
#[derive(Debug)]
#[must_use = "inspect drain/error facts and settle any owed publication"]
pub struct GoalExternalCancellation {
    pub scopes: Vec<GoalExternalCancellationScope>,
    /// False when an admitted operation's completion cannot be established.
    pub operation_drained: bool,
    /// Actual control/fence/drain error with independently owned secondary causes.
    /// A worker's ordinary operation error remains with its existing native
    /// publisher/completion owner; it is not a second control failure here.
    pub error: Option<anyhow::Error>,
    /// At most one worker exists per session; unlaunched publication stays with
    /// its existing launch holder/publication slot, never a synthetic worker.
    pub failure_notice: Option<GoalFailureNoticeDelivery>,
}

#[derive(Default)]
struct GoalExternalStopState {
    active: bool,
    retired: bool,
    error: Option<Arc<anyhow::Error>>,
    next_order: u64,
    error_order: Option<u64>,
}

/// Created on the existing worker/launch entry: retained custody and errors,
/// not copied TaskStatus, policy, or a second lookup table.
#[derive(Default)]
pub(super) struct GoalExternalStopDuty(std::sync::Mutex<GoalExternalStopState>);

#[derive(Debug)]
struct SharedExternalStopError(Arc<anyhow::Error>);

impl std::fmt::Display for SharedExternalStopError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("retained Goal operation failure")
    }
}

impl std::error::Error for SharedExternalStopError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}

pub(super) fn shared_external_error(error: Arc<anyhow::Error>) -> anyhow::Error {
    anyhow::Error::new(SharedExternalStopError(error))
}

impl GoalExternalStopDuty {
    fn state(&self) -> std::sync::MutexGuard<'_, GoalExternalStopState> {
        self.0.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(super) fn active(&self) -> bool {
        self.state().active
    }

    fn activate(&self) -> bool {
        let mut state = self.state();
        if state.retired {
            return false;
        }
        state.active = true;
        true
    }

    /// Called with the existing exact-owner map locked, without any await.
    /// A captured-but-not-fenced handle cannot resurrect a removed duty.
    pub(super) fn try_retire(&self) -> bool {
        let mut state = self.state();
        if state.active {
            return false;
        }
        state.retired = true;
        true
    }

    pub(super) fn stamp(&self) -> u64 {
        let mut state = self.state();
        let order = state.next_order;
        state.next_order = state.next_order.saturating_add(1);
        order
    }

    pub(super) fn retain(&self, error: anyhow::Error) {
        let mut state = self.state();
        if state.error.is_none() {
            state.error_order = Some(state.next_order);
            state.next_order = state.next_order.saturating_add(1);
        }
        let mut first = state.error.take().map(shared_external_error);
        retain_lifecycle_error(&mut first, error);
        state.error = first.map(Arc::new);
    }

    pub(super) fn error(&self) -> Option<anyhow::Error> {
        self.state().error.clone().map(shared_external_error)
    }

    pub(super) fn error_ordered(&self) -> Option<(u64, anyhow::Error)> {
        let state = self.state();
        state
            .error_order
            .zip(state.error.clone())
            .map(|(order, error)| (order, shared_external_error(error)))
    }

    pub(super) fn report(&self, engine: &GoalExecutionEngine, scope: &GoalExecutionScope) {
        let Some(error) = self.error() else {
            return;
        };
        let text = format!("Goal external-stop settlement: {error:#}");
        let protected = engine.diagnostic_text_guard.as_ref().map_or_else(
            || "[Goal external-stop diagnostic withheld]".to_owned(),
            |guard| guard.guard_diagnostic_text(&text),
        );
        zeroclaw_log::record!(
            ERROR,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                serde_json::json!({
                    "error_key": "goal_external_stop_failed",
                    "task_id": scope.task_id(),
                    "execution_epoch": scope.execution_epoch(),
                })
            ),
            protected
        );
    }

    pub(super) fn complete(&self) -> Option<anyhow::Error> {
        self.complete_ordered().map(|(_, error)| error)
    }

    pub(super) fn complete_ordered(&self) -> Option<(u64, anyhow::Error)> {
        let mut state = self.state();
        state.active = false;
        let order = state.error_order.take();
        order
            .zip(state.error.take())
            .map(|(order, error)| (order, shared_external_error(error)))
    }
}

struct CapturedExternalScope {
    scope: GoalExecutionScope,
    worker: Option<Arc<Mutex<GoalWorker>>>,
    checkpoint: Option<GoalParentTurnCheckpoint>,
    launch: Option<launch_guard::GoalLaunchExternalStop>,
    duty: Arc<GoalExternalStopDuty>,
    phase_error: Option<Arc<anyhow::Error>>,
}

/// Read-only capture of exact execution owners under the caller's captured
/// session command authority. Revalidate that authority after capture awaits
/// and immediately before calling fence; capturing alone acquires no effect.
#[must_use = "revalidate captured authority before initiating the exact fence"]
pub struct GoalExternalCancellationTarget {
    supervisor: Arc<GoalExecutionSupervisor>,
    scopes: Vec<CapturedExternalScope>,
}

/// Acquired external-stop duty. Drain only after releasing command and ordinary
/// foreground serialization. Drop transfers no new work: the same worker or
/// exact launch entry already retains the closed continuation and actual cause.
#[must_use = "release command authority, then observe exact admitted-operation drain"]
pub struct GoalExternalCancellationFence {
    supervisor: Arc<GoalExecutionSupervisor>,
    scopes: Vec<CapturedExternalScope>,
}

impl GoalExecutionSupervisor {
    /// Capture executing workers and admitted launch duties only, without a
    /// task lookup, checkpoint closure, lifecycle write, or driver acquisition.
    /// A paused idle resident's publication/control custody stays unchanged.
    pub async fn capture_external_cancellation(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<Option<GoalExternalCancellationTarget>> {
        ensure!(
            !session_id.trim().is_empty(),
            "Goal external-stop session is blank"
        );
        let mut scopes = Vec::new();
        let worker = self.workers.lock().await.get(session_id).cloned();
        if let Some(worker) = worker {
            let resident = worker.lock().await;
            if resident.completion.borrow().is_none() || resident.external_stop.active() {
                scopes.push(CapturedExternalScope {
                    scope: GoalExecutionScope::new(
                        resident.task_id.clone(),
                        session_id,
                        resident.execution_epoch,
                    )?,
                    worker: Some(worker.clone()),
                    checkpoint: Some(resident.checkpoint.clone()),
                    launch: None,
                    duty: resident.external_stop.clone(),
                    phase_error: None,
                });
            }
        }
        for launch in self.launches.capture_external_stop(session_id) {
            let scope = launch.scope().clone();
            if let Some(existing) = scopes
                .iter_mut()
                .find(|candidate| same_external_scope(&candidate.scope, &scope))
            {
                existing.launch = Some(launch);
            } else {
                scopes.push(CapturedExternalScope {
                    scope,
                    worker: None,
                    checkpoint: None,
                    duty: launch.duty(),
                    launch: Some(launch),
                    phase_error: None,
                });
            }
        }
        Ok(
            (!scopes.is_empty()).then(|| GoalExternalCancellationTarget {
                supervisor: self.clone(),
                scopes,
            }),
        )
    }

    pub(super) async fn reconcile_external_scope(
        &self,
        scope: &GoalExecutionScope,
        duty: &GoalExternalStopDuty,
        allow_pause: bool,
    ) -> Result<GoalExternalStopDisposition> {
        reconcile_external_scope(
            self.engine.as_ref(),
            self.launches.as_ref(),
            scope,
            duty,
            allow_pause,
        )
        .await
    }
}

async fn reconcile_external_scope(
    engine: &GoalExecutionEngine,
    launches: &GoalLaunchOwner,
    scope: &GoalExecutionScope,
    duty: &GoalExternalStopDuty,
    allow_pause: bool,
) -> Result<GoalExternalStopDisposition> {
    let current = engine
        .registry
        .current_goal_for_session(scope.session_id())
        .await?;
    let disposition = external_winner(launches, scope, current.as_ref()).await?;
    if disposition.settled() || !allow_pause {
        return Ok(disposition);
    }
    // Only exact Running reaches this branch. Saturation uses the same
    // narrow terminal fence as launch cleanup, never wrapping its epoch.
    let saturated = scope.execution_epoch() == i64::MAX;
    let transition = if saturated {
        zeroclaw_log::record!(
            ERROR,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                .with_attrs(serde_json::json!({"error_key": "goal_external_stop_epoch_saturated"})),
            "Goal external pause cannot advance a saturated execution epoch"
        );
        duty.retain(anyhow::Error::msg(
            "Goal external pause cannot advance a saturated execution epoch",
        ));
        engine
            .registry
            .finish_session_goal(
                scope.task_id(),
                scope.session_id(),
                scope.execution_epoch(),
                TaskStatus::Failed,
                Some("executor_start_failed".into()),
            )
            .await?
    } else {
        engine
            .registry
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
            .await?
    };
    if transition == GoalTransitionResult::Applied {
        return Ok(if saturated {
            GoalExternalStopDisposition::Terminal(TaskStatus::Failed)
        } else {
            GoalExternalStopDisposition::Paused
        });
    }
    let winner = engine
        .registry
        .current_goal_for_session(scope.session_id())
        .await?;
    let disposition = external_winner(launches, scope, winner.as_ref()).await?;
    ensure!(
        disposition.settled(),
        "Goal external pause made no progress on its exact Running scope"
    );
    Ok(disposition)
}

async fn external_winner(
    launches: &GoalLaunchOwner,
    scope: &GoalExecutionScope,
    current: Option<&TaskRecord>,
) -> Result<GoalExternalStopDisposition> {
    let Some(current) = current else {
        return Ok(GoalExternalStopDisposition::Absent);
    };
    ensure!(
        current.kind == zeroclaw_api::task::TaskKind::Goal,
        "Goal external-stop winner is not a Goal"
    );
    ensure!(
        current.session_id.as_deref() == Some(scope.session_id()),
        "Goal external-stop winner has a mismatched session"
    );
    ensure!(
        current.execution_epoch > 0,
        "Goal external-stop winner has an invalid epoch"
    );
    if current.id == scope.task_id() {
        ensure!(
            current.execution_epoch >= scope.execution_epoch(),
            "Goal external-stop winner moved backwards"
        );
        if current.status.is_terminal() {
            return Ok(GoalExternalStopDisposition::Terminal(current.status));
        }
        if current.status == TaskStatus::Paused {
            return Ok(GoalExternalStopDisposition::Paused);
        }
        if current.status == TaskStatus::Running
            && current.execution_epoch == scope.execution_epoch()
        {
            return Ok(GoalExternalStopDisposition::Unresolved);
        }
    } else if current.status != TaskStatus::Running {
        return Ok(GoalExternalStopDisposition::Superseded);
    }
    ensure!(
        current.status == TaskStatus::Running,
        "Goal external-stop winner has an unsupported lifecycle state"
    );
    ensure!(
        launches.successor_owned(current, scope.session_id()).await,
        "Goal external-stop successor is Running without its exact owner"
    );
    Ok(GoalExternalStopDisposition::SuccessorOwned)
}

/// Existing authorized control retry: never wait for an admitted operation or
/// publication while its command lock is held. A completed, fully-accounted
/// worker can settle the exact retained fence and surface its first cause once.
pub(super) async fn retry_completed_worker(
    engine: &GoalExecutionEngine,
    launches: &GoalLaunchOwner,
    workers: &GoalWorkerMap,
    session: &str,
) -> Result<()> {
    let Some(worker) = workers.lock().await.get(session).cloned() else {
        return Ok(());
    };
    let (scope, duty, completed) = {
        let worker = worker.lock().await;
        if !worker.external_stop.active() {
            return Ok(());
        }
        let completed = worker.completion.borrow().is_some();
        (
            GoalExecutionScope::new(worker.task_id.clone(), session, worker.execution_epoch)?,
            worker.external_stop.clone(),
            completed,
        )
    };
    if !completed {
        return Err(duty.error().unwrap_or_else(|| {
            zeroclaw_log::record!(
                ERROR,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                    serde_json::json!({"error_key": "goal_external_stop_operation_not_drained"})
                ),
                "Goal external-stop admitted operation still requires unlocked drain"
            );
            anyhow::Error::msg(
                "Goal external-stop admitted operation still requires unlocked drain",
            )
        }));
    }
    let disposition = match reconcile_external_scope(engine, launches, &scope, &duty, true).await {
        Ok(disposition) => disposition,
        Err(error) => {
            duty.retain(error);
            GoalExternalStopDisposition::Unresolved
        }
    };
    let clean = match engine.registry.get_goal_task(scope.task_id()).await {
        Ok(Some(goal)) => {
            goal.pending_call_id.is_none()
                && goal.pending_call_epoch.is_none()
                && goal.accounting_state == GoalAccountingState::Complete
        }
        Ok(None)
            if matches!(
                disposition,
                GoalExternalStopDisposition::Absent
                    | GoalExternalStopDisposition::Superseded
                    | GoalExternalStopDisposition::SuccessorOwned
            ) =>
        {
            true
        }
        Ok(None) => {
            zeroclaw_log::record!(
                ERROR,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                    serde_json::json!({"error_key": "goal_external_stop_extension_missing"})
                ),
                "Goal extension missing while settling external stop"
            );
            duty.retain(anyhow::Error::msg(
                "Goal extension missing while settling external stop",
            ));
            false
        }
        Err(error) => {
            duty.retain(error);
            false
        }
    };
    let error = if disposition.settled() && clean {
        duty.report(engine, &scope);
        duty.complete()
    } else {
        duty.error().or_else(|| {
            zeroclaw_log::record!(
                ERROR,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                    serde_json::json!({"error_key": "goal_external_stop_settlement_unresolved"})
                ),
                "Goal external-stop accounting or lifecycle drain remains unresolved"
            );
            Some(anyhow::Error::msg(
                "Goal external-stop accounting or lifecycle drain remains unresolved",
            ))
        })
    };
    error.map_or(Ok(()), Err)
}

fn same_external_scope(left: &GoalExecutionScope, right: &GoalExecutionScope) -> bool {
    left.task_id() == right.task_id()
        && left.session_id() == right.session_id()
        && left.execution_epoch() == right.execution_epoch()
}

impl GoalExternalCancellationTarget {
    /// First effect phase, called under revalidated captured command authority.
    /// Close every captured continuation before the first lifecycle I/O. A
    /// failed pause stays owned and cannot skip later safe operation drains.
    pub async fn fence(self) -> GoalExternalCancellationFence {
        let mut fence = GoalExternalCancellationFence {
            supervisor: self.supervisor,
            scopes: self.scopes,
        };
        fence.scopes.retain(|captured| captured.duty.activate());
        for captured in &fence.scopes {
            if let Some(checkpoint) = &captured.checkpoint {
                checkpoint.close();
            }
            if let Some(launch) = &captured.launch {
                launch.fence();
            }
        }
        for captured in &fence.scopes {
            fence
                .supervisor
                .close_pending_boundaries(Some(captured.scope.session_id()))
                .await;
        }
        for captured in &mut fence.scopes {
            if let Some(launch) = &captured.launch
                && let Some(error) = launch.genuine_failure()
            {
                // The original launch holder owns genuine failure and its
                // separate notice. External interruption cannot demote it.
                captured.duty.retain(error);
                captured.phase_error = captured.duty.error().map(Arc::new);
                continue;
            }
            if let Err(error) = fence
                .supervisor
                .reconcile_external_scope(&captured.scope, &captured.duty, true)
                .await
            {
                captured.duty.retain(error);
            }
            captured.phase_error = captured.duty.error().map(Arc::new);
        }
        fence
    }
}

impl GoalExternalCancellationFence {
    /// Second effect phase. No command/foreground/config writer may be held.
    /// The returned error and publication are independent of physical drain.
    pub async fn drain(self) -> GoalExternalCancellation {
        let mut outcome = GoalExternalCancellation {
            scopes: Vec::new(),
            operation_drained: true,
            error: None,
            failure_notice: None,
        };
        for captured in &self.scopes {
            let mut lifecycle_drained = true;
            if let Some(worker) = &captured.worker {
                match self
                    .supervisor
                    .drain_lifecycle_fence_with_external_retry(&captured.scope, false)
                    .await
                {
                    Ok(mut drain) => {
                        if let Some(error) = drain.error.take() {
                            captured.duty.retain(error);
                        }
                        if let Some(delivery) = drain.failure_notice.take() {
                            // One actual resident worker per captured session.
                            debug_assert!(outcome.failure_notice.is_none());
                            outcome.failure_notice = Some(delivery);
                        }
                    }
                    Err(error) => {
                        captured.duty.retain(error);
                        lifecycle_drained = false;
                    }
                }
                outcome.operation_drained &= worker.lock().await.completion.borrow().is_some();
            } else if let Some(launch) = &captured.launch {
                // A held handoff has no admitted model operation to await.
                // Its holder retains release and unlaunched notice custody.
                if !launch.held()
                    && captured.duty.error().is_none()
                    && let Err(error) = launch.retry().await
                {
                    captured.duty.retain(error);
                }
            }
            let disposition = match self
                .supervisor
                .reconcile_external_scope(&captured.scope, &captured.duty, false)
                .await
            {
                Ok(disposition) => disposition,
                Err(error) => {
                    captured.duty.retain(error);
                    GoalExternalStopDisposition::Unresolved
                }
            };
            let scope_drained = match &captured.worker {
                Some(worker) => worker.lock().await.completion.borrow().is_some(),
                None => true,
            };
            if (!disposition.settled() || !scope_drained || !lifecycle_drained)
                && captured.duty.error().is_none()
            {
                zeroclaw_log::record!(
                    ERROR,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                        .with_attrs(
                            serde_json::json!({"error_key": "goal_external_stop_drain_unresolved"})
                        ),
                    "Goal external-stop fence or admitted-operation drain remains unresolved"
                );
                captured.duty.retain(anyhow::Error::msg(
                    "Goal external-stop fence or admitted-operation drain remains unresolved",
                ));
            }
            let error = if disposition.settled() && scope_drained && lifecycle_drained {
                // An interruption at the following map await cannot erase a
                // known cause before either native return or guarded diagnosis.
                captured
                    .duty
                    .report(self.supervisor.engine.as_ref(), &captured.scope);
                captured.duty.complete()
            } else {
                captured.duty.error()
            };
            if let Some(error) =
                error.or_else(|| captured.phase_error.clone().map(shared_external_error))
            {
                retain_lifecycle_error(&mut outcome.error, error);
            }
            if let Some(worker) = &captured.worker {
                self.supervisor
                    .remove_worker_if_settled(captured.scope.session_id(), worker)
                    .await;
            }
            outcome.scopes.push(GoalExternalCancellationScope {
                scope: captured.scope.clone(),
                disposition,
            });
        }
        outcome
    }
}

#[cfg(test)]
mod tests;
