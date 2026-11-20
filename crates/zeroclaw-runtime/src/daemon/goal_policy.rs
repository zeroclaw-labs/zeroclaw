//! Retained config-job ownership of transactional Goal-policy cutovers.

use std::sync::Arc;

use anyhow::Result;
use zeroclaw_config::schema::Config;
use zeroclaw_goal::{
    GoalDiagnosticTextGuard, GoalExecutionRestartCoordinator, GoalPolicyCutover, GoalPolicyFence,
    GoalTaskRegistry, task::GoalPolicyTarget,
};

use crate::{
    goal_accounting::RuntimeGoalDiagnosticTextGuard,
    live_config_authority::{ConfigCommit, ConfigWriteLease},
};

/// A config job's exact cancellation, checkpoint fence and admission closure.
///
/// Prepare while holding its `ConfigCommit`; retain through the writer's normal
/// save/publication and serialized side effects, then release the writer through
/// [`Self::release_config_commit`] before [`Self::finish`]. An active cutover
/// retains the same generation lease through its drain. Drop never spawns
/// cleanup or reports application success: the existing supervisors retain
/// their duties and admission stays closed.
#[must_use = "finish after releasing config serialization, including on save failure"]
pub struct LiveGoalPolicyCutover {
    generation_write: Option<ConfigWriteLease>,
    // Strong discovery owner; durable state remains in its canonical registry.
    coordinator: Option<Arc<GoalExecutionRestartCoordinator>>,
    // Only this retained job may release this admission token.
    closure: Option<GoalPolicyCutover>,
    // Every selected owner is fenced before publication or any drain wait.
    fence: Option<GoalPolicyFence>,
    // Exact committed identities are operation custody, not cached policy.
    targets: Vec<GoalPolicyTarget>,
    // Operation-local comparison: it becomes a closure only on publication.
    expands: bool,
    // Actual first/secondary causes survive cancellation of a later await.
    error: Option<anyhow::Error>,
    // Diagnostic disposition only; domain token Drop owns fail-closed transfer.
    finished: bool,
}

impl LiveGoalPolicyCutover {
    fn inert() -> Self {
        Self {
            generation_write: None,
            coordinator: None,
            closure: None,
            fence: None,
            targets: Vec::new(),
            expands: false,
            error: None,
            finished: false,
        }
    }

    /// Release config serialization while keeping an active cutover visible
    /// to generation drain. Inert writers release their lease immediately.
    pub fn release_config_commit(&mut self, commit: ConfigCommit) {
        let lease = commit.release_writer();
        if self.coordinator.is_some() {
            self.generation_write = Some(lease);
        }
    }

    /// Exact committed cancellation count, or `None` for an unchanged policy
    /// (or a nonparticipating process without an installed control plane).
    pub fn cancelled_goal_count(&self) -> Option<usize> {
        self.coordinator.as_ref().map(|_| self.targets.len())
    }

    /// Retain a serialized side-effect failure for an active Goal cutover.
    /// Inert callers retain and report their ordinary errors themselves.
    pub(crate) fn retain_primary_error(&mut self, error: anyhow::Error) {
        if self.coordinator.is_some() {
            retain_error(&mut self.error, error);
        }
    }

    /// Observe all additional Goal drains, then release only this job's token.
    ///
    /// `published` describes actual config publication, not save success. A
    /// published expansion requires runtime replacement even if another side
    /// effect failed. With successful drains, a failed save may release this
    /// token under unchanged old policy; already-cancelled tasks stay terminal.
    /// The first actual native error remains the source; real drain/release
    /// errors are retained as secondary causes and logged through the guard.
    pub async fn finish(self, published: bool, primary: Option<anyhow::Error>) -> Result<()> {
        self.finish_with_logger(published, primary, log_failure)
            .await
    }

    async fn finish_with_logger(
        mut self,
        published: bool,
        primary: Option<anyhow::Error>,
        log: impl FnOnce(&anyhow::Error),
    ) -> Result<()> {
        let _generation_write = self.generation_write.take();
        // Store before awaiting: cancellation must not discard a known cause.
        if let Some(error) = primary {
            retain_error(&mut self.error, error);
        }
        let mut drained = true;
        if let Some(fence) = self.fence.take()
            && let Err(error) = fence.drain().await
        {
            drained = false;
            retain_error(&mut self.error, error);
        }
        if drained
            && let (Some(coordinator), Some(closure)) = (&self.coordinator, self.closure.take())
            && let Err(error) = coordinator
                .finish_policy_cutover(closure, published && self.expands)
                .await
        {
            retain_error(&mut self.error, error);
        }
        self.finished = true;
        if let Some(error) = self.error.take() {
            let error = if let Some(count) = self.cancelled_goal_count() {
                let error = error.context(format!(
                    "Goal policy cutover cancelled {count} goal(s); config published={published}"
                ));
                log(&error);
                error
            } else {
                error
            };
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for LiveGoalPolicyCutover {
    fn drop(&mut self) {
        if !self.finished && self.coordinator.is_some() {
            let detail = self.error.as_ref().map_or_else(
                || "Goal policy cutover abandoned before required completion".to_owned(),
                |error| format!("Goal policy cutover abandoned: {error:#}"),
            );
            log_failure_text(&detail);
        }
        // Owned domain tokens/fences synchronously transfer to fail-closed
        // recovery. No task or join handle is created or discarded here.
    }
}

/// Prepare the prospective policy under the retained canonical config writer.
/// Classification and one exact cancellation batch precede every checkpoint
/// fence, save, publication and alias-generation effect.
pub async fn begin_live_goal_policy_cutover(
    commit: &ConfigCommit,
    prospective: &Config,
) -> Result<LiveGoalPolicyCutover> {
    prepare_installed(commit, prospective, false).await
}

/// Reconcile an already-published canonical policy without a second write.
/// Used by resumed destructive transactions that legitimately skip persistence;
/// it cannot clear another job's closure or a pending runtime replacement.
pub async fn reconcile_live_goal_policy_cutover(
    commit: &ConfigCommit,
) -> Result<LiveGoalPolicyCutover> {
    prepare_installed(commit, &commit.current_config(), true).await
}

async fn prepare_installed(
    commit: &ConfigCommit,
    prospective: &Config,
    force: bool,
) -> Result<LiveGoalPolicyCutover> {
    let Some(control_plane) = crate::control_plane::control_plane() else {
        return Ok(LiveGoalPolicyCutover::inert());
    };
    let current = commit.current_config();
    // An unrelated writer must not depend on Goal-store availability.
    if !force && super::goal_policy_snapshot(&current) == super::goal_policy_snapshot(prospective) {
        return Ok(LiveGoalPolicyCutover::inert());
    }
    let registry = control_plane.goal_store().map_err(|error| {
        log_failure(&error);
        error
    })?;
    prepare(
        control_plane.goal_execution_restart(),
        registry,
        &current,
        prospective,
        force,
    )
    .await
}

pub(super) async fn reconcile_candidate_with(
    commit: &ConfigCommit,
    prospective: &Config,
    control_plane: &crate::control_plane::ControlPlaneHandle,
) -> Result<LiveGoalPolicyCutover> {
    let registry = control_plane.goal_store().map_err(|error| {
        log_failure(&error);
        error
    })?;
    prepare(
        control_plane.goal_execution_restart(),
        registry,
        &commit.current_config(),
        prospective,
        true,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn begin_live_goal_policy_cutover_with(
    coordinator: Arc<GoalExecutionRestartCoordinator>,
    registry: Arc<dyn GoalTaskRegistry>,
    live: &zeroclaw_config::live::LiveConfigHandle,
    prospective: &Config,
) -> Result<LiveGoalPolicyCutover> {
    let current = live.snapshot();
    prepare(coordinator, registry, &current, prospective, false).await
}

async fn prepare(
    coordinator: Arc<GoalExecutionRestartCoordinator>,
    registry: Arc<dyn GoalTaskRegistry>,
    current: &Config,
    prospective: &Config,
    force: bool,
) -> Result<LiveGoalPolicyCutover> {
    let old_policy = super::goal_policy_snapshot(current);
    let policy = super::goal_policy_snapshot(prospective);
    if !force && old_policy == policy {
        return Ok(LiveGoalPolicyCutover::inert());
    }
    let closure = coordinator.begin_policy_cutover().await;
    let mut cutover = LiveGoalPolicyCutover {
        generation_write: None,
        coordinator: Some(coordinator.clone()),
        closure: Some(closure),
        fence: None,
        targets: Vec::new(),
        expands: policy.expands_from(&old_policy),
        error: None,
        finished: false,
    };
    let revoked =
        match zeroclaw_goal::control::revoke_goals_under_policy(registry.as_ref(), &policy).await {
            Ok(revoked) => revoked,
            Err(error) => {
                // No cancellation committed, so no policy-driven worker fence is
                // permitted. Releasing this token cannot reopen another closure.
                // Retain the original before that await, including on cancellation.
                cutover.error = Some(error);
                if let Some(closure) = cutover.closure.take()
                    && let Err(secondary) = coordinator.finish_policy_cutover(closure, false).await
                {
                    retain_error(&mut cutover.error, secondary);
                }
                cutover.finished = true;
                let error = cutover.error.take().unwrap_or_else(|| {
                    anyhow::Error::msg("Goal policy preparation failed without a retained cause")
                });
                log_failure(&error);
                return Err(error);
            }
        };
    cutover.targets = revoked.into_iter().map(|(target, _)| target).collect();
    // Arm the job before waiting on metadata/command locks. The domain owner
    // fences every registered/held target before any admitted operation drain.
    cutover.fence = Some(coordinator.fence_revoked_in_place(&cutover.targets).await);
    Ok(cutover)
}

#[derive(Debug)]
struct SecondaryError {
    primary: anyhow::Error,
    secondary: anyhow::Error,
}

impl std::fmt::Display for SecondaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Goal policy cleanup also failed: {:#}",
            self.secondary
        )
    }
}

impl std::error::Error for SecondaryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.primary.as_ref())
    }
}

pub(super) fn retain_error(first: &mut Option<anyhow::Error>, secondary: anyhow::Error) {
    *first = Some(match first.take() {
        Some(primary) => anyhow::Error::new(SecondaryError { primary, secondary }),
        None => secondary,
    });
}

pub(super) fn log_failure(error: &anyhow::Error) {
    log_failure_text(&format!("{error:#}"));
}

fn log_failure_text(text: &str) {
    let protected = RuntimeGoalDiagnosticTextGuard.guard_diagnostic_text(text);
    zeroclaw_log::record!(
        ERROR,
        zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
            .with_outcome(zeroclaw_log::EventOutcome::Failure)
            .with_attrs(serde_json::json!({"error_key": "goal_policy_cutover_failed"})),
        protected
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{control_plane::SqliteTaskStore, live_config_authority::LiveConfigAuthority};

    #[tokio::test]
    async fn inert_cutover_releases_generation_write_immediately() {
        let authority = LiveConfigAuthority::for_tests(Config::default());
        let commit = authority.begin_config_commit().await.unwrap();
        let mut cutover = LiveGoalPolicyCutover::inert();
        cutover.release_config_commit(commit);
        assert!(!authority.config_write_lock_is_held());
        assert_eq!(authority.agent_lifecycle().active_config_write_count(), 0);
        cutover.finish(false, None).await.unwrap();
    }

    #[tokio::test]
    async fn inert_cutover_preserves_ordinary_errors_without_goal_diagnostics() {
        let mut cutover = LiveGoalPolicyCutover::inert();
        cutover.retain_primary_error(anyhow::Error::msg("inert-retained-fixture"));
        assert!(cutover.error.is_none());
        cutover
            .finish_with_logger(false, None, |_| panic!("inert cutover must not log"))
            .await
            .unwrap();

        let error = LiveGoalPolicyCutover::inert()
            .finish_with_logger(
                false,
                Some(anyhow::Error::msg("inert-primary-fixture")),
                |_| panic!("ordinary error must not produce a Goal diagnostic"),
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "inert-primary-fixture");
    }

    #[tokio::test]
    async fn active_cutover_releases_generation_write_on_success_and_error() {
        for fail in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let registry = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
            let mut config = Config::default();
            config.goal.enabled = true;
            let authority = LiveConfigAuthority::for_tests(config.clone());
            let commit = authority.begin_config_commit().await.unwrap();
            let mut prospective = config.clone();
            prospective.goal.enabled = false;
            let coordinator = Arc::new(GoalExecutionRestartCoordinator::new());
            let mut cutover = prepare(coordinator, registry, &config, &prospective, false)
                .await
                .unwrap();
            assert_eq!(cutover.cancelled_goal_count(), Some(0));
            cutover.release_config_commit(commit);
            assert!(!authority.config_write_lock_is_held());
            assert_eq!(authority.agent_lifecycle().active_config_write_count(), 1);
            if fail {
                cutover.retain_primary_error(anyhow::Error::msg("active-retained-fixture"));
            }
            let primary = fail.then(|| anyhow::Error::msg("fixture config completion failure"));
            let logged = std::cell::Cell::new(false);
            let result = cutover
                .finish_with_logger(false, primary, |_| logged.set(true))
                .await;
            if fail {
                let chain = format!("{:#}", result.unwrap_err());
                assert!(chain.contains("active-retained-fixture"));
                assert!(chain.contains("fixture config completion failure"));
                assert!(
                    logged.get(),
                    "active failure must produce a Goal diagnostic"
                );
            } else {
                result.unwrap();
                assert!(!logged.get());
            }
            assert_eq!(authority.agent_lifecycle().active_config_write_count(), 0);
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                authority.drain_agent_lifecycle_retaining_ownership(),
            )
            .await
            .unwrap();
        }
    }
}
