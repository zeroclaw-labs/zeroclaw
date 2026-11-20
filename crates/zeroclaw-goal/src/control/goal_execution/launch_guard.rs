//! Continuous process-local custody between Applied admission and worker handoff.

use super::*;

type LaunchKey = (String, String, i64);

fn launch_key(scope: &GoalExecutionScope) -> LaunchKey {
    (
        scope.task_id().to_owned(),
        scope.session_id().to_owned(),
        scope.execution_epoch(),
    )
}

/// The table creates cleanup custody, never a copy of durable task status.
pub(super) struct GoalLaunchOwner {
    engine: Arc<GoalExecutionEngine>,
    workers: GoalWorkerMap,
    entries: std::sync::Mutex<HashMap<LaunchKey, Arc<GoalLaunchEntry>>>,
}

struct GoalLaunchEntry {
    scope: GoalExecutionScope,
    state: std::sync::Mutex<GoalLaunchState>,
    attempt: Mutex<()>,
    external_stop: Arc<GoalExternalStopDuty>,
}

/// Exact existing launch-entry custody captured without registry or model I/O.
pub(super) struct GoalLaunchExternalStop {
    owner: Arc<GoalLaunchOwner>,
    entry: Arc<GoalLaunchEntry>,
}

impl GoalLaunchExternalStop {
    pub(super) fn scope(&self) -> &GoalExecutionScope {
        &self.entry.scope
    }
    pub(super) fn duty(&self) -> Arc<GoalExternalStopDuty> {
        self.entry.external_stop.clone()
    }
    pub(super) fn held(&self) -> bool {
        self.entry.state().held
    }
    pub(super) fn fence(&self) {
        self.entry.state().pause_reason = GoalPauseReason::OperatorPaused;
    }
    pub(super) fn genuine_failure(&self) -> Option<anyhow::Error> {
        let failed = self.entry.state().failure;
        failed.then(|| {
            self.entry.fault().unwrap_or_else(|| {
                zeroclaw_log::record!(
                    ERROR,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                        .with_attrs(serde_json::json!({"error_key": "goal_launch_genuine_failure_missing_cause"})),
                    "Goal launch retains a genuine failure requiring its original owner"
                );
                anyhow::Error::msg(
                    "Goal launch retains a genuine failure requiring its original owner"
                )
            })
        })
    }
    pub(super) async fn retry(&self) -> Result<()> {
        self.owner.retry_scope(&self.entry.scope, false).await
    }
}

struct GoalLaunchState {
    held: bool,
    settled: bool,
    failure: bool,
    pause_reason: GoalPauseReason,
    primary: Option<Arc<anyhow::Error>>,
    first_fault: Option<(u64, Arc<anyhow::Error>)>,
    initial: Option<Arc<Mutex<GoalInitialAttempt>>>,
}

struct GoalInitialAttempt {
    handle: JoinHandle<()>,
    observed: bool,
}

/// A projection of an owned cause. Standard source traversal reaches the actual
/// native error; formatting is not the ownership or identity of that error.
#[derive(Debug)]
struct SharedLaunchError(Arc<anyhow::Error>);

impl std::fmt::Display for SharedLaunchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("retained Goal launch failure")
    }
}

impl std::error::Error for SharedLaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}

fn shared_error(error: Arc<anyhow::Error>) -> anyhow::Error {
    anyhow::Error::new(SharedLaunchError(error))
}

// The operation primary, or otherwise the earliest retained cleanup fault,
// stays the source head. Later typed secondaries wrap outward, so {:#} renders
// the newest secondary first without changing the global lifecycle format.
fn compose_cleanup(
    primary: Option<Arc<anyhow::Error>>,
    local: Option<(u64, Arc<anyhow::Error>)>,
    external: Option<(u64, anyhow::Error)>,
) -> Option<anyhow::Error> {
    let mut fault = primary.map(shared_error);
    let local = local.map(|(order, error)| (order, shared_error(error)));
    let (first, second) = match (local, external) {
        (Some(local), Some(external)) if external.0 < local.0 => (Some(external), Some(local)),
        (local, external) => (local, external),
    };
    for (_, error) in first.into_iter().chain(second) {
        retain_lifecycle_error(&mut fault, error);
    }
    fault
}

impl GoalLaunchEntry {
    fn state(&self) -> std::sync::MutexGuard<'_, GoalLaunchState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn record_fault(&self, error: anyhow::Error) {
        let order = self.external_stop.stamp();
        self.state()
            .first_fault
            .get_or_insert_with(|| (order, Arc::new(error)));
    }

    fn fault(&self) -> Option<anyhow::Error> {
        let (primary, local) = {
            let state = self.state();
            (state.primary.clone(), state.first_fault.clone())
        };
        let external = self.external_stop.error_ordered();
        compose_cleanup(primary, local, external)
    }
}

impl GoalLaunchOwner {
    pub(super) fn new(engine: Arc<GoalExecutionEngine>, workers: GoalWorkerMap) -> Arc<Self> {
        Arc::new(Self {
            engine,
            workers,
            entries: std::sync::Mutex::new(HashMap::new()),
        })
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<LaunchKey, Arc<GoalLaunchEntry>>> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    pub(super) fn log_reopen_refusal(&self) {
        zeroclaw_log::record!(
            ERROR,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                .with_attrs(serde_json::json!({"error_key": "goal_launch_cleanup_failed"})),
            "Goal restart admission remains closed while launch cleanup is unresolved"
        );
    }

    pub(super) fn owns(&self, scope: &GoalExecutionScope) -> bool {
        self.entries().contains_key(&launch_key(scope))
    }

    pub(super) fn capture_external_stop(
        self: &Arc<Self>,
        session: &str,
    ) -> Vec<GoalLaunchExternalStop> {
        self.entries()
            .values()
            .filter(|entry| entry.scope.session_id() == session && !entry.state().settled)
            .map(|entry| GoalLaunchExternalStop {
                owner: self.clone(),
                entry: entry.clone(),
            })
            .collect()
    }

    pub(super) fn pending_scopes(&self, session: Option<&str>) -> Vec<GoalExecutionScope> {
        self.entries()
            .values()
            .filter(|entry| {
                session.is_none_or(|session| entry.scope.session_id() == session)
                    && !entry.state().held
            })
            .map(|entry| entry.scope.clone())
            .collect()
    }

    fn remove(&self, entry: &Arc<GoalLaunchEntry>) {
        let mut entries = self.entries();
        let key = launch_key(&entry.scope);
        if entries
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
            && entry.external_stop.try_retire()
        {
            entries.remove(&key);
        }
    }

    pub(super) fn launch_permitted(&self, scope: &GoalExecutionScope) -> Result<()> {
        if let Some(entry) = self.entries().get(&launch_key(scope)) {
            if entry.external_stop.active() {
                return Err(external_stop::GoalExternalStopClosed.into());
            }
            ensure!(
                entry.state().held,
                "Goal launch cleanup already owns this execution scope"
            );
        }
        Ok(())
    }

    fn diagnostic_text(&self, entry: &GoalLaunchEntry) -> Option<String> {
        let external = entry.external_stop.error();
        let text = {
            let state = entry.state();
            if state.primary.is_none() && state.first_fault.is_none() && external.is_none() {
                return None;
            }
            format!(
                "Goal unlaunched cleanup: primary={}; cleanup={}; external_stop={}",
                state
                    .primary
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), |error| format!("{error:#}")),
                state
                    .first_fault
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), |(_, error)| format!("{error:#}")),
                external
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), |error| format!("{error:#}"))
            )
        };
        Some(self.engine.diagnostic_text_guard.as_ref().map_or_else(
            || "[Goal cleanup diagnostic withheld]".to_owned(),
            |guard| guard.guard_diagnostic_text(&text),
        ))
    }

    fn log_causes(&self, entry: &GoalLaunchEntry) {
        let Some(protected) = self.diagnostic_text(entry) else {
            return;
        };
        zeroclaw_log::record!(
            ERROR,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                .with_attrs(serde_json::json!({"error_key": "goal_launch_cleanup_failed"})),
            protected
        );
    }

    /// Only a real worker or an exact held handoff can own a Running successor.
    pub(super) async fn successor_owned(&self, current: &TaskRecord, session: &str) -> bool {
        let key = (
            current.id.clone(),
            session.to_owned(),
            current.execution_epoch,
        );
        if self
            .entries()
            .get(&key)
            .is_some_and(|entry| entry.state().held)
        {
            return true;
        }
        let worker = self.workers.lock().await.get(session).cloned();
        if let Some(worker) = worker {
            let worker = worker.lock().await;
            return worker.task_id == current.id
                && worker.execution_epoch == current.execution_epoch;
        }
        false
    }

    async fn canonical_running(&self, entry: &GoalLaunchEntry) -> Result<bool> {
        let scope = &entry.scope;
        let Some(current) = self
            .engine
            .registry
            .current_goal_for_session(scope.session_id())
            .await?
        else {
            return Ok(false);
        };
        ensure!(
            current.session_id.as_deref() == Some(scope.session_id()),
            "Goal cleanup returned a mismatched canonical session binding"
        );
        ensure!(
            current.kind == zeroclaw_api::task::TaskKind::Goal && current.execution_epoch > 0,
            "Goal cleanup returned a malformed canonical task binding"
        );
        if current.id == scope.task_id() {
            ensure!(
                current.execution_epoch >= scope.execution_epoch(),
                "Goal cleanup observed a backwards execution epoch"
            );
        }
        if current.status != TaskStatus::Running {
            return Ok(false);
        }
        if current.id == scope.task_id() && current.execution_epoch == scope.execution_epoch() {
            return Ok(true);
        }
        ensure!(
            self.successor_owned(&current, scope.session_id()).await,
            "Goal cleanup found a Running successor without an exact process-local owner"
        );
        Ok(false)
    }

    async fn settle_canonical(&self, entry: &GoalLaunchEntry) -> Result<bool> {
        if !self.canonical_running(entry).await? {
            return Ok(false);
        }
        let scope = &entry.scope;
        let (failure, reason) = {
            let state = entry.state();
            (state.failure, state.pause_reason)
        };
        // Pause advances the fence. Only a proven exact saturated Running epoch
        // uses the existing overflow-safe terminal transition instead.
        let saturated = !failure && scope.execution_epoch() == i64::MAX;
        let transition = if failure || saturated {
            self.engine
                .registry
                .finish_session_goal(
                    scope.task_id(),
                    scope.session_id(),
                    scope.execution_epoch(),
                    TaskStatus::Failed,
                    Some("executor_start_failed".into()),
                )
                .await
        } else {
            self.engine
                .registry
                .pause_session_goal(
                    scope.task_id(),
                    scope.session_id(),
                    scope.execution_epoch(),
                    GoalPauseState {
                        reason,
                        description: None,
                        blockers: Vec::new(),
                    },
                )
                .await
        };
        let transition = if saturated {
            transition.context("Goal cleanup pause cannot advance a saturated execution epoch")?
        } else {
            transition?
        };
        if saturated {
            entry.record_fault(anyhow::Error::msg(
                "Goal cleanup pause cannot advance a saturated execution epoch",
            ));
        }
        if transition == GoalTransitionResult::Applied {
            return Ok(failure || saturated);
        }
        ensure!(
            !self.canonical_running(entry).await?,
            "Goal cleanup transition made no progress on its exact Running scope"
        );
        Ok(false)
    }

    async fn attempt(&self, entry: Arc<GoalLaunchEntry>, observe_initial: bool) -> Result<()> {
        // The handle remains in the entry even if this waiter is cancelled.
        // Observe it before taking the permit needed by that initial task.
        if observe_initial {
            let initial = entry.state().initial.clone();
            if let Some(initial) = initial {
                let mut initial = initial.lock().await;
                if !initial.observed {
                    let result = (&mut initial.handle).await;
                    initial.observed = true;
                    if let Err(error) = result {
                        entry.record_fault(
                            anyhow::Error::new(error).context("Goal initial cleanup task stopped"),
                        );
                    }
                }
            }
        }
        let _permit = entry.attempt.lock().await;
        if entry.state().settled {
            return entry.fault().map_or(Ok(()), Err);
        }
        if entry.state().held {
            return Ok(());
        }
        if let Err(error) = self.settle_canonical(&entry).await {
            entry.record_fault(error);
            return Err(entry.fault().unwrap_or_else(|| {
                anyhow::Error::msg("Goal cleanup failed without a retained cause")
            }));
        }
        let fault = entry.fault();
        entry.state().settled = true;
        self.log_causes(&entry);
        drop(entry.external_stop.complete());
        self.remove(&entry);
        match fault {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(super) async fn retry_scope(
        &self,
        scope: &GoalExecutionScope,
        restart: bool,
    ) -> Result<()> {
        let entry = self.entries().get(&launch_key(scope)).cloned();
        let Some(entry) = entry else {
            return Ok(());
        };
        {
            let mut state = entry.state();
            if state.held {
                return Ok(());
            }
            if restart {
                state.pause_reason = GoalPauseReason::DaemonRestart;
            }
        }
        self.attempt(entry, true).await
    }

    pub(super) async fn retry_session(&self, session: &str) -> Result<()> {
        let mut failure = None;
        if let Err(error) = external_stop::retry_completed_worker(
            self.engine.as_ref(),
            self,
            &self.workers,
            session,
        )
        .await
        {
            retain_lifecycle_error(&mut failure, error);
        }
        for scope in self.pending_scopes(Some(session)) {
            if let Err(error) = self.retry_scope(&scope, false).await {
                retain_lifecycle_error(&mut failure, error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// The guard is created unarmed before controller mutation. Applied arms it
/// synchronously, before projection/audit can suspend or fail.
pub(in crate::control) struct GoalLaunchGuard {
    owner: Arc<GoalLaunchOwner>,
    entry: Option<Arc<GoalLaunchEntry>>,
    admission: Option<GoalExecutionAdmission>,
    custody: Option<OwnedRwLockReadGuard<()>>,
}

impl GoalLaunchGuard {
    pub(super) fn new(
        owner: Arc<GoalLaunchOwner>,
        admission: Option<GoalExecutionAdmission>,
        custody: Option<OwnedRwLockReadGuard<()>>,
    ) -> Self {
        Self {
            owner,
            entry: None,
            admission,
            custody,
        }
    }

    pub(in crate::control) fn arm(&mut self, scope: GoalExecutionScope) -> Result<()> {
        let mut entries = self.owner.entries();
        let key = launch_key(&scope);
        ensure!(
            !entries.contains_key(&key),
            "Goal execution scope already has a launch owner"
        );
        let entry = Arc::new(GoalLaunchEntry {
            scope,
            attempt: Mutex::new(()),
            external_stop: Arc::new(GoalExternalStopDuty::default()),
            state: std::sync::Mutex::new(GoalLaunchState {
                held: true,
                settled: false,
                failure: false,
                pause_reason: GoalPauseReason::ExternalDependency,
                primary: None,
                first_fault: None,
                initial: None,
            }),
        });
        entries.insert(key, entry.clone());
        self.entry = Some(entry);
        Ok(())
    }

    pub(in crate::control) fn scope(&self) -> Option<&GoalExecutionScope> {
        self.entry.as_ref().map(|entry| &entry.scope)
    }

    pub(super) fn take(&mut self) -> Self {
        Self {
            owner: self.owner.clone(),
            entry: self.entry.take(),
            admission: self.admission.take(),
            custody: self.custody.take(),
        }
    }

    pub(in crate::control) fn retain_primary(&mut self, error: anyhow::Error) {
        if let Some(entry) = &self.entry {
            let mut state = entry.state();
            state.failure = true;
            state.primary.get_or_insert_with(|| Arc::new(error));
        }
    }

    pub(in crate::control) fn take_primary(&mut self) -> Option<anyhow::Error> {
        let entry = self.entry.as_ref()?;
        let mut state = entry.state();
        if state.settled {
            state
                .primary
                .take()
                .map(|error| Arc::try_unwrap(error).unwrap_or_else(shared_error))
        } else {
            state
                .primary
                .as_ref()
                .map(|error| shared_error(error.clone()))
        }
    }

    pub(in crate::control) async fn retry_session(&self, session: &str) -> Result<()> {
        self.owner.retry_session(session).await
    }

    pub(in crate::control) fn has_pending_session(&self, session: &str) -> bool {
        !self.owner.pending_scopes(Some(session)).is_empty()
    }

    /// Awaited failure cleanup retains the held entry and primary cause through
    /// cancellation. Only a canonical fence permits discharge. An unsettled
    /// error reports the same retained cleanup causes without consuming them.
    pub(in crate::control) async fn reconcile_held(&mut self) -> Result<bool> {
        let Some(entry) = self.entry.clone() else {
            return Ok(false);
        };
        let _permit = entry.attempt.lock().await;
        match self.owner.settle_canonical(&entry).await {
            Ok(failed) => {
                let external = entry.external_stop.complete_ordered();
                entry.state().settled = true;
                self.owner.remove(&entry);
                let local = entry.state().first_fault.clone();
                compose_cleanup(None, local, external).map_or(Ok(failed), Err)
            }
            Err(error) => {
                entry.record_fault(error);
                let local = entry.state().first_fault.clone();
                let external = entry.external_stop.error_ordered();
                compose_cleanup(None, local, external).map_or_else(
                    || Err(anyhow::Error::msg("Goal cleanup has no retained failure")),
                    Err,
                )
            }
        }
    }

    pub(super) fn failed(&mut self) {
        if let Some(entry) = &self.entry {
            entry.state().failure = true;
        }
    }

    pub(super) fn pausing(&mut self, reason: GoalPauseReason) {
        if let Some(entry) = &self.entry {
            entry.state().pause_reason = reason;
        }
    }

    /// Discharge exact lifecycle custody without releasing command custody.
    /// A classified no-execution response must still own its surface handoff.
    pub(in crate::control) fn discharge_entry(&mut self) {
        if let Some(entry) = self.entry.take() {
            self.owner.log_causes(&entry);
            self.owner.remove(&entry);
        }
    }

    pub(in crate::control) fn settled(&mut self) {
        self.discharge_entry();
        self.admission.take();
        self.custody.take();
    }
}

impl Drop for GoalLaunchGuard {
    fn drop(&mut self) {
        let Some(entry) = self.entry.take() else {
            return;
        };
        if entry.state().settled {
            self.owner.log_causes(&entry);
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            let order = entry.external_stop.stamp();
            {
                let mut state = entry.state();
                state.held = false;
                state.first_fault.get_or_insert_with(|| {
                    (
                        order,
                        Arc::new(anyhow::Error::msg(
                            "Goal cleanup scheduling unavailable without a Tokio runtime",
                        )),
                    )
                });
            }
            drop(self.admission.take());
            drop(self.custody.take());
            return;
        }
        let owner = self.owner.clone();
        let retained = entry.clone();
        let (start, ready) = tokio::sync::oneshot::channel();
        {
            let mut state = entry.state();
            state.held = false;
            let handle = zeroclaw_spawn::spawn!(async move {
                if ready.await.is_ok() && owner.attempt(retained.clone(), false).await.is_err() {
                    owner.log_causes(&retained);
                }
            });
            state.initial = Some(Arc::new(Mutex::new(GoalInitialAttempt {
                handle,
                observed: false,
            })));
        }
        // Install the handle as part of synchronous custody transfer. Its task
        // cannot touch the registry until both admission readers are released.
        drop(self.admission.take());
        drop(self.custody.take());
        let _ = start.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{CleanupFault, NoModelAccounting, Registry};

    fn owner(registry: Arc<Registry>) -> Arc<GoalLaunchOwner> {
        let engine =
            Arc::new(GoalRuntime::new(registry).execution_engine(Arc::new(NoModelAccounting)));
        GoalLaunchOwner::new(engine, Arc::new(Mutex::new(HashMap::new())))
    }

    fn armed(owner: &Arc<GoalLaunchOwner>, scope: &GoalExecutionScope) -> GoalLaunchGuard {
        let mut guard = GoalLaunchGuard::new(owner.clone(), None, None);
        guard.arm(scope.clone()).unwrap();
        guard
    }

    fn layers(mut error: &anyhow::Error) -> (Vec<&anyhow::Error>, &anyhow::Error) {
        let mut secondaries = Vec::new();
        while let Some(layer) = error.downcast_ref::<GoalLifecycleSecondaryError>() {
            secondaries.push(&layer.secondary);
            error = &layer.primary;
        }
        (secondaries, error)
    }

    fn has(error: &anyhow::Error, text: &str) -> bool {
        error.chain().any(|cause| {
            cause
                .downcast_ref::<CleanupFault>()
                .is_some_and(|fault| fault.0 == text)
        })
    }

    fn assert_cleanup_faults(error: &anyhow::Error, first: &str, latest_first: &[&str]) {
        let (secondaries, root) = layers(error);
        assert!(has(root, first), "{error:#}");
        assert_eq!(secondaries.len(), latest_first.len(), "{error:#}");
        for (secondary, expected) in secondaries.iter().zip(latest_first) {
            assert!(has(secondary, expected), "{error:#}");
            assert!(
                !has(error, expected),
                "secondary replaced the source: {error:#}"
            );
        }
        let rendered = format!("{error:#}");
        let mut previous = None;
        for text in latest_first.iter().copied().chain(std::iter::once(first)) {
            assert_eq!(rendered.matches(text).count(), 1, "{rendered}");
            let position = rendered.find(text).unwrap();
            if let Some(previous) = previous {
                assert!(previous < position, "{rendered}");
            }
            previous = Some(position);
        }
    }

    #[test]
    fn cleanup_fault_composition_preserves_local_first() {
        let registry = Arc::new(Registry::default());
        let owner = owner(registry);
        let scope = GoalExecutionScope::new("fixture-goal", "local-first", 1).unwrap();
        let mut launch = armed(&owner, &scope);
        let entry = launch.entry.as_ref().unwrap();
        entry.record_fault(anyhow::Error::new(CleanupFault("local-first")));
        entry
            .external_stop
            .retain(anyhow::Error::new(CleanupFault("external-later")));
        assert_cleanup_faults(&entry.fault().unwrap(), "local-first", &["external-later"]);
        launch.settled();
    }

    #[test]
    fn cleanup_fault_composition_preserves_operation_primary_and_external_first() {
        let registry = Arc::new(Registry::default());
        let owner = owner(registry);
        let scope = GoalExecutionScope::new("fixture-goal", "external-first", 1).unwrap();
        let mut launch = armed(&owner, &scope);
        let entry = launch.entry.as_ref().unwrap().clone();
        entry
            .external_stop
            .retain(anyhow::Error::new(CleanupFault("external-first")));
        entry.record_fault(anyhow::Error::new(CleanupFault("local-second")));
        launch.retain_primary(anyhow::Error::new(CleanupFault("operation-primary")));
        assert_cleanup_faults(
            &entry.fault().unwrap(),
            "operation-primary",
            &["local-second", "external-first"],
        );
        launch.settled();
    }

    #[test]
    fn cleanup_fault_composition_does_not_resurrect_completed_external_fault() {
        let registry = Arc::new(Registry::default());
        let owner = owner(registry);
        let scope = GoalExecutionScope::new("fixture-goal", "external-completed", 1).unwrap();
        let mut launch = armed(&owner, &scope);
        let entry = launch.entry.as_ref().unwrap();
        entry
            .external_stop
            .retain(anyhow::Error::new(CleanupFault("external-old")));
        entry.record_fault(anyhow::Error::new(CleanupFault("local-retained")));
        assert!(has(
            &entry.external_stop.complete().unwrap(),
            "external-old"
        ));
        entry
            .external_stop
            .retain(anyhow::Error::new(CleanupFault("external-new")));
        let error = entry.fault().unwrap();
        assert_cleanup_faults(&error, "local-retained", &["external-new"]);
        assert!(!format!("{error:#}").contains("external-old"));
        launch.settled();
    }

    #[tokio::test]
    async fn held_cleanup_settlement_orders_both_fault_sequences() {
        for external_first in [true, false] {
            let registry = Arc::new(Registry::default());
            registry.seed("settlement-order", TaskStatus::Running);
            let owner = owner(registry.clone());
            let scope = GoalExecutionScope::new("fixture-goal", "settlement-order", 1).unwrap();
            let mut launch = armed(&owner, &scope);
            let entry = launch.entry.as_ref().unwrap().clone();
            if external_first {
                entry
                    .external_stop
                    .retain(anyhow::Error::new(CleanupFault("external")));
                entry.record_fault(anyhow::Error::new(CleanupFault("local")));
            } else {
                entry.record_fault(anyhow::Error::new(CleanupFault("local")));
                entry
                    .external_stop
                    .retain(anyhow::Error::new(CleanupFault("external")));
            }
            let error = launch.reconcile_held().await.unwrap_err();
            let (first, later) = if external_first {
                ("external", "local")
            } else {
                ("local", "external")
            };
            assert_cleanup_faults(&error, first, &[later]);
            assert!(!entry.external_stop.active());
            assert!(entry.external_stop.error().is_none());
            assert!(!owner.owns(&scope));
            assert_eq!(registry.0.lock().unwrap().pause_calls, 1);
            launch.settled();
        }
    }

    #[tokio::test]
    async fn held_cleanup_errors_preserve_external_first_until_settlement() {
        let registry = Arc::new(Registry::default());
        registry.seed("held-external-first", TaskStatus::Running);
        registry.0.lock().unwrap().fail_pause = true;
        let owner = owner(registry.clone());
        let scope = GoalExecutionScope::new("fixture-goal", "held-external-first", 1).unwrap();
        let mut launch = armed(&owner, &scope);
        let entry = launch.entry.as_ref().unwrap().clone();
        entry
            .external_stop
            .retain(anyhow::Error::new(CleanupFault("external-first")));
        for attempts in 1..=2 {
            let error = launch.reconcile_held().await.unwrap_err();
            assert_cleanup_faults(
                &error,
                "external-first",
                &["fixture cleanup pause unavailable"],
            );
            assert!(owner.owns(&scope));
            assert!(entry.external_stop.error().is_some());
            {
                let state = entry.state();
                assert!(state.held);
                assert!(!state.settled);
            }
            assert_eq!(registry.0.lock().unwrap().pause_calls, attempts);
        }
        registry.0.lock().unwrap().fail_pause = false;
        let error = launch.reconcile_held().await.unwrap_err();
        assert_cleanup_faults(
            &error,
            "external-first",
            &["fixture cleanup pause unavailable"],
        );
        assert!(!owner.owns(&scope));
        assert!(!entry.external_stop.active());
        assert!(entry.external_stop.error().is_none());
        assert_eq!(registry.0.lock().unwrap().pause_calls, 3);
        launch.settled();
    }

    #[tokio::test]
    async fn held_cleanup_error_preserves_older_local_fault() {
        let registry = Arc::new(Registry::default());
        registry.seed("held-local-first", TaskStatus::Running);
        registry.0.lock().unwrap().fail_pause = true;
        let owner = owner(registry.clone());
        let scope = GoalExecutionScope::new("fixture-goal", "held-local-first", 1).unwrap();
        let mut launch = armed(&owner, &scope);
        let entry = launch.entry.as_ref().unwrap().clone();
        entry.record_fault(anyhow::Error::new(CleanupFault("local-first")));
        entry
            .external_stop
            .retain(anyhow::Error::new(CleanupFault("external-later")));
        let error = launch.reconcile_held().await.unwrap_err();
        assert_cleanup_faults(&error, "local-first", &["external-later"]);
        assert!(!format!("{error:#}").contains("fixture cleanup pause unavailable"));
        assert!(owner.owns(&scope));
        assert!(entry.state().held);
        assert!(!entry.state().settled);
        assert!(entry.external_stop.error().is_some());
        assert_eq!(registry.0.lock().unwrap().pause_calls, 1);
        registry.0.lock().unwrap().fail_pause = false;
        assert_cleanup_faults(
            &launch.reconcile_held().await.unwrap_err(),
            "local-first",
            &["external-later"],
        );
        assert_eq!(registry.0.lock().unwrap().pause_calls, 2);
        launch.settled();
    }

    #[test]
    fn drop_without_runtime_retains_custody_and_releases_admission_readers() {
        let registry = Arc::new(Registry::default());
        registry.seed("runtime-free", TaskStatus::Running);
        let owner = owner(registry.clone());
        let scope = GoalExecutionScope::new("fixture-goal", "runtime-free", 1).unwrap();
        let admission = Arc::new(RwLock::new(()));
        let custody = Arc::new(RwLock::new(()));
        let mut guard = GoalLaunchGuard::new(
            owner.clone(),
            Some(GoalExecutionAdmission {
                _guard: admission.clone().try_read_owned().unwrap(),
            }),
            Some(custody.clone().try_read_owned().unwrap()),
        );
        guard.arm(scope.clone()).unwrap();
        drop(guard);
        assert!(owner.owns(&scope));
        assert!(admission.try_write().is_ok());
        assert!(custody.try_write().is_ok());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let error = runtime
            .block_on(owner.retry_scope(&scope, false))
            .unwrap_err();
        assert!(format!("{error:#}").contains("scheduling unavailable"));
        assert_eq!(
            registry.0.lock().unwrap().task.as_ref().unwrap().status,
            TaskStatus::Paused
        );
        assert!(!owner.owns(&scope));
    }

    #[tokio::test]
    async fn aborted_initial_handle_is_observed_before_exact_retry() {
        let registry = Arc::new(Registry::default());
        registry.seed("aborted-initial", TaskStatus::Running);
        let entered = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        {
            let mut state = registry.0.lock().unwrap();
            state.lookup_wait_at = Some(1);
            state.lookup_wait = Some(release);
            state.lookup_entered = Some(entered.clone());
        }
        let owner = owner(registry.clone());
        let scope = GoalExecutionScope::new("fixture-goal", "aborted-initial", 1).unwrap();
        drop(armed(&owner, &scope));
        entered.acquire().await.unwrap().forget();
        let entry = owner.entries().get(&launch_key(&scope)).unwrap().clone();
        let initial = entry.state().initial.clone().unwrap();
        initial.lock().await.handle.abort();
        let error = owner.retry_scope(&scope, false).await.unwrap_err();
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<tokio::task::JoinError>()
                .is_some_and(|join| join.is_cancelled())
        }));
        assert_eq!(
            registry.0.lock().unwrap().task.as_ref().unwrap().status,
            TaskStatus::Paused
        );
        assert!(!owner.owns(&scope));
    }

    #[tokio::test]
    async fn newer_running_epoch_requires_its_exact_held_launch_owner() {
        let registry = Arc::new(Registry::default());
        registry.seed("held-successor", TaskStatus::Running);
        let owner = owner(registry.clone());
        let old = GoalExecutionScope::new("fixture-goal", "held-successor", 1).unwrap();
        let next = GoalExecutionScope::new("fixture-goal", "held-successor", 2).unwrap();
        let mut predecessor = armed(&owner, &old);
        registry
            .0
            .lock()
            .unwrap()
            .task
            .as_mut()
            .unwrap()
            .execution_epoch = 2;
        let mut successor = armed(&owner, &next);
        assert!(!predecessor.reconcile_held().await.unwrap());
        predecessor.settled();
        assert!(!owner.owns(&old));
        assert!(owner.owns(&next));
        {
            let state = registry.0.lock().unwrap();
            assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
            assert_eq!(state.task.as_ref().unwrap().execution_epoch, 2);
            assert_eq!(state.pause_calls, 0);
            assert_eq!(state.finish_calls, 0);
        }
        successor.reconcile_held().await.unwrap();
        successor.settled();
    }

    #[tokio::test]
    async fn newer_running_epoch_with_registered_worker_preserves_the_winner() {
        struct Driver(crate::GoalSessionKey);
        #[async_trait::async_trait]
        impl GoalSessionDriver for Driver {
            fn session_key(&self) -> &crate::GoalSessionKey {
                &self.0
            }
            async fn bind(&self, _: &GoalIngressContext) -> Result<GoalSessionLease> {
                bail!("private successor fixture must not bind")
            }
            async fn acquire_execution(
                &self,
                _: &GoalIngressContext,
                _: &GoalExecutionScope,
                _: &GoalParentTurnCheckpoint,
            ) -> Result<Box<dyn GoalSessionExecutionLease>> {
                bail!("private successor fixture must not execute")
            }
        }
        let key = crate::GoalSessionKey::zero_code("worker-successor").unwrap();
        let session = key.durable_id();
        let registry = Arc::new(Registry::default());
        registry.seed(&session, TaskStatus::Running);
        let owner = owner(registry.clone());
        let old = GoalExecutionScope::new("fixture-goal", &session, 1).unwrap();
        let mut predecessor = armed(&owner, &old);
        let ingress = GoalIngressContext::trusted(
            key.clone(),
            "fixture-agent",
            "fixture-route",
            crate::GoalIngressPrincipal::ZeroCode {
                tui_id: "fixture-client".into(),
            },
        )
        .unwrap();
        let (completed, completion) = watch::channel(Some(Ok(GoalExecutionOutcome::Fenced)));
        let worker = Arc::new(Mutex::new(GoalWorker {
            task_id: "fixture-goal".into(),
            execution_epoch: 2,
            driver: Arc::new(Driver(key)),
            ingress,
            failure_publication: Arc::new(Mutex::new(GoalFailurePublication::default())),
            checkpoint: GoalParentTurnCheckpoint::new(),
            notice_withdrawal: GoalParentTurnCheckpoint::new(),
            completion,
            lifecycle_finalization: Arc::new(Mutex::new(())),
            external_stop: Arc::new(GoalExternalStopDuty::default()),
            _handle: zeroclaw_spawn::spawn!(async move {
                drop(completed);
            }),
        }));
        owner.workers.lock().await.insert(session.clone(), worker);
        registry
            .0
            .lock()
            .unwrap()
            .task
            .as_mut()
            .unwrap()
            .execution_epoch = 2;
        assert!(!predecessor.reconcile_held().await.unwrap());
        predecessor.settled();
        assert!(!owner.owns(&old));
        {
            let state = registry.0.lock().unwrap();
            assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
            assert_eq!(state.task.as_ref().unwrap().execution_epoch, 2);
            assert_eq!(state.pause_calls, 0);
            assert_eq!(state.finish_calls, 0);
        }
        let worker = owner.workers.lock().await.remove(&session).unwrap();
        let mut worker = worker.lock().await;
        (&mut worker._handle).await.unwrap();
    }

    #[test]
    fn cleanup_diagnostic_guards_complete_causes_and_withholds_without_guard() {
        struct Guard(std::sync::Mutex<Vec<String>>);
        impl GoalDiagnosticTextGuard for Guard {
            fn guard_diagnostic_text(&self, text: &str) -> String {
                self.0.lock().unwrap().push(text.to_owned());
                "[protected cleanup causes]".into()
            }
        }
        let registry = Arc::new(Registry::default());
        let diagnostic_guard = Arc::new(Guard(std::sync::Mutex::new(Vec::new())));
        let engine = Arc::new(
            GoalRuntime::new(registry.clone())
                .execution_engine(Arc::new(NoModelAccounting))
                .with_diagnostic_text_guard(diagnostic_guard.clone()),
        );
        let owner = GoalLaunchOwner::new(engine, Arc::new(Mutex::new(HashMap::new())));
        let scope = GoalExecutionScope::new("diagnostic-goal", "diagnostic-session", 1).unwrap();
        let mut launch = armed(&owner, &scope);
        launch.retain_primary(
            anyhow::Error::new(CleanupFault("synthetic-primary-secret-shaped-value"))
                .context("projection-original-context"),
        );
        let entry = launch.entry.as_ref().unwrap();
        entry.record_fault(
            anyhow::Error::new(CleanupFault("synthetic-secondary-secret-shaped-value"))
                .context("write-secondary-context"),
        );
        assert_eq!(
            owner.diagnostic_text(entry).as_deref(),
            Some("[protected cleanup causes]")
        );
        let captured = diagnostic_guard.0.lock().unwrap();
        assert_eq!(captured.len(), 1);
        for text in [
            "projection-original-context",
            "synthetic-primary-secret-shaped-value",
            "write-secondary-context",
            "synthetic-secondary-secret-shaped-value",
        ] {
            assert!(captured[0].contains(text));
        }
        drop(captured);
        let unguarded = self::owner(registry);
        assert_eq!(
            unguarded.diagnostic_text(entry).as_deref(),
            Some("[Goal cleanup diagnostic withheld]")
        );
        let mut logs = zeroclaw_log::subscribe_or_install();
        zeroclaw_log::try_install_capture_subscriber();
        let marker = uuid::Uuid::new_v4().to_string();
        let other_marker = uuid::Uuid::new_v4().to_string();
        {
            let span = zeroclaw_log::info_span!(
                target: "zeroclaw_log_internal_scope",
                "cleanup_diagnostic_test",
                cleanup_diagnostic_test_id = %marker,
            );
            let _entered = span.enter();
            owner.log_causes(entry);
            unguarded.log_causes(entry);
        }
        {
            let span = zeroclaw_log::info_span!(
                target: "zeroclaw_log_internal_scope",
                "cleanup_diagnostic_negative_control",
                cleanup_diagnostic_test_id = %other_marker,
            );
            let _entered = span.enter();
            unguarded.log_causes(entry);
        }
        let mut observed = Vec::new();
        let mut negative_control_seen = false;
        loop {
            match logs.try_recv() {
                Ok(event) => {
                    assert!(
                        !event
                            .to_string()
                            .contains("synthetic-primary-secret-shaped-value")
                    );
                    assert!(
                        !event
                            .to_string()
                            .contains("synthetic-secondary-secret-shaped-value")
                    );
                    if event["attributes"]["cleanup_diagnostic_test_id"] == other_marker
                        && event["attributes"]["error_key"] == "goal_launch_cleanup_failed"
                        && event["message"] == "[Goal cleanup diagnostic withheld]"
                    {
                        negative_control_seen = true;
                    }
                    if event["attributes"]["cleanup_diagnostic_test_id"] == marker
                        && event["attributes"]["error_key"] == "goal_launch_cleanup_failed"
                        && let Some(message) = event["message"].as_str()
                        && matches!(
                            message,
                            "[protected cleanup causes]" | "[Goal cleanup diagnostic withheld]"
                        )
                    {
                        observed.push(message.to_owned());
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(error) => panic!("cleanup diagnostic capture lost evidence: {error}"),
            }
        }
        assert_eq!(
            observed,
            [
                "[protected cleanup causes]",
                "[Goal cleanup diagnostic withheld]"
            ]
        );
        assert!(
            negative_control_seen,
            "out-of-scope diagnostic was not captured"
        );
        let error = entry.fault().unwrap();
        assert!(error.chain().any(|cause| cause.is::<CleanupFault>()));
        launch.settled();
    }
}
