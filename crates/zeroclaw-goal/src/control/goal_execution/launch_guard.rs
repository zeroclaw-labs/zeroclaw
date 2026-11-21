//! Cancellation ownership between a durable admission and worker registration.

use super::*;

/// Process-local responsibility for one unlaunched epoch, not lifecycle state.
/// Only the registry's exact task/session/epoch predicates authorize cleanup.
pub(super) struct GoalLaunchGuard {
    engine: Arc<GoalExecutionEngine>,
    scope: Option<GoalExecutionScope>,
    failure: bool,
    pause_reason: GoalPauseReason,
    admission: Option<GoalExecutionAdmission>,
    custody: Option<OwnedRwLockReadGuard<()>>,
}

impl GoalLaunchGuard {
    pub(super) fn new(
        engine: Arc<GoalExecutionEngine>,
        execution: Option<&GoalExecutionRequest>,
        admission: Option<GoalExecutionAdmission>,
        custody: Option<OwnedRwLockReadGuard<()>>,
    ) -> Self {
        Self {
            engine,
            scope: execution.map(|request| request.scope().clone()),
            failure: false,
            pause_reason: GoalPauseReason::ExternalDependency,
            admission,
            custody,
        }
    }

    pub(super) fn scope(&self) -> Option<&GoalExecutionScope> {
        self.scope.as_ref()
    }

    /// Move responsibility into an awaited handoff so cancelling that future
    /// settles it even when its caller retains the surrounding submission.
    pub(super) fn take(&mut self) -> Self {
        Self {
            engine: self.engine.clone(),
            scope: self.scope.take(),
            failure: self.failure,
            pause_reason: self.pause_reason,
            admission: self.admission.take(),
            custody: self.custody.take(),
        }
    }

    pub(super) fn failed(&mut self) {
        self.failure = true;
    }

    pub(super) fn pausing(&mut self, reason: GoalPauseReason) {
        self.pause_reason = reason;
    }

    pub(super) fn settled(&mut self) {
        self.scope.take();
        self.admission.take();
        self.custody.take();
    }
}

impl Drop for GoalLaunchGuard {
    fn drop(&mut self) {
        let Some(scope) = self.scope.take() else {
            return;
        };
        let engine = self.engine.clone();
        let admission = self.admission.take();
        let custody = self.custody.take();
        let failure = self.failure;
        let pause_reason = self.pause_reason;
        zeroclaw_spawn::spawn!(async move {
            // Keep restart admission until this unlaunched epoch is settled.
            // A competing fence is authoritative; never overwrite its winner.
            let result = async {
                let current = engine
                    .registry
                    .current_goal_for_session(scope.session_id())
                    .await?;
                if !current.is_some_and(|task| {
                    task.id == scope.task_id()
                        && task.execution_epoch == scope.execution_epoch()
                        && task.status == TaskStatus::Running
                }) {
                    return Ok::<_, anyhow::Error>(());
                }
                if failure {
                    engine
                        .registry
                        .finish_session_goal(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            TaskStatus::Failed,
                            Some("executor_start_failed".into()),
                        )
                        .await?;
                } else {
                    engine
                        .registry
                        .pause_session_goal(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            GoalPauseState {
                                reason: pause_reason,
                                description: None,
                                blockers: Vec::new(),
                            },
                        )
                        .await?;
                }
                // Applied settles our obligation. Stale/Missing means another
                // exact guarded transition won; neither authorizes a retry.
                Ok(())
            }
            .await;
            if result.is_err() {
                zeroclaw_log::record!(
                    ERROR,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                        .with_attrs(serde_json::json!({"error_key": "goal_launch_cleanup_failed"})),
                    "Goal unlaunched epoch cleanup could not be persisted"
                );
            }
            drop(admission);
            drop(custody);
        });
    }
}
