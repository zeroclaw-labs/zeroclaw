//! Failure output custody after classification releases lifecycle locks.

use super::*;
use std::future::Future;
use tokio::sync::OwnedMutexGuard;

/// Owns one exact worker's outstanding publication until delivery or withdrawal.
/// The held latch prevents another drainer from publishing or retiring its owner.
#[must_use = "deliver or explicitly withdraw the failure publication"]
pub struct GoalFailureNoticeDelivery {
    engine: Arc<GoalExecutionEngine>,
    scope: GoalExecutionScope,
    driver: Arc<dyn GoalSessionDriver>,
    ingress: GoalIngressContext,
    publication: Option<OwnedMutexGuard<GoalFailurePublication>>,
    withdrawal: GoalParentTurnCheckpoint,
    worker: Arc<Mutex<GoalWorker>>,
    workers: Arc<Mutex<HashMap<String, Arc<Mutex<GoalWorker>>>>>,
    external_stop: Arc<GoalExternalStopDuty>,
}

impl std::fmt::Debug for GoalFailureNoticeDelivery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoalFailureNoticeDelivery")
            .finish_non_exhaustive()
    }
}

impl GoalFailureNoticeDelivery {
    pub(super) fn new(
        supervisor: &GoalExecutionSupervisor,
        scope: GoalExecutionScope,
        worker: Arc<Mutex<GoalWorker>>,
        driver: Arc<dyn GoalSessionDriver>,
        ingress: GoalIngressContext,
        withdrawal: GoalParentTurnCheckpoint,
        publication: OwnedMutexGuard<GoalFailurePublication>,
        external_stop: Arc<GoalExternalStopDuty>,
    ) -> Self {
        Self {
            engine: Arc::clone(&supervisor.engine),
            scope,
            driver,
            ingress,
            publication: Some(publication),
            withdrawal,
            worker,
            workers: Arc::clone(&supervisor.workers),
            external_stop,
        }
    }

    /// Attempt publication once, then release the latch and exact worker.
    /// Dropping an unsettled obligation is only the abort/panic fallback.
    pub async fn deliver(self) -> Result<()> {
        self.deliver_or_withdraw(std::future::pending()).await
    }

    /// Attempt publication, or explicitly withdraw if cancellation wins.
    /// Cancellation has priority; dropping this future is only the abort/panic
    /// fallback, not the normal connection shutdown path.
    pub async fn deliver_or_withdraw<F: Future<Output = ()>>(mut self, cancelled: F) -> Result<()> {
        let result = if let Some(publication) = self.publication.as_mut() {
            let body = publish_failure_notice_body(
                self.engine.as_ref(),
                &self.scope,
                self.driver.as_ref(),
                &self.ingress,
                &self.withdrawal,
                publication,
            );
            tokio::select! {
                biased;
                () = cancelled => None,
                result = body => Some(result),
            }
        } else {
            Some(Ok(()))
        };
        let Some(result) = result else {
            self.withdraw().await;
            return Ok(());
        };
        if let Err(error) = &result {
            log_failure_notice_delivery_error(self.engine.as_ref(), &self.scope, error);
        }
        drop(self.publication.take());
        self.remove_worker().await;
        result
    }

    /// Withdraw publication explicitly and retire its exact worker.
    /// Dropping an unsettled obligation is only the abort/panic fallback.
    pub async fn withdraw(mut self) {
        self.withdrawal.close();
        if let Some(publication) = self.publication.as_mut() {
            publication.notice_withdrawn = true;
            log_failure_notice_delivery_failure(&self.scope);
        }
        drop(self.publication.take());
        self.remove_worker().await;
    }

    async fn remove_worker(&self) {
        let mut workers = self.workers.lock().await;
        remove_exact(
            &mut workers,
            self.scope.session_id(),
            &self.worker,
            &self.external_stop,
        );
    }
}

impl Drop for GoalFailureNoticeDelivery {
    fn drop(&mut self) {
        let Some(mut publication) = self.publication.take() else {
            return;
        };
        self.withdrawal.close();
        publication.notice_withdrawn = true;
        log_failure_notice_delivery_failure(&self.scope);
        drop(publication);
        if let Ok(mut workers) = self.workers.try_lock() {
            remove_exact(
                &mut workers,
                self.scope.session_id(),
                &self.worker,
                &self.external_stop,
            );
        }
    }
}

fn remove_exact(
    workers: &mut HashMap<String, Arc<Mutex<GoalWorker>>>,
    session: &str,
    expected: &Arc<Mutex<GoalWorker>>,
    external_stop: &GoalExternalStopDuty,
) {
    if workers
        .get(session)
        .is_some_and(|worker| Arc::ptr_eq(worker, expected))
        && external_stop.try_retire()
    {
        workers.remove(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::GoalParentTurnInput;
    use crate::test_support::{CleanupFault, NoModelAccounting, Registry};
    use crate::{
        GoalExecutionNotice, GoalIngressPrincipal, GoalOperationScope, GoalParentTurn,
        GoalParentTurnResult, GoalSessionKey, GoalVerifierResult, GoalVerifierTurn,
    };

    struct NoDeliveryDriver(GoalSessionKey);

    #[async_trait::async_trait]
    impl GoalSessionDriver for NoDeliveryDriver {
        fn session_key(&self) -> &GoalSessionKey {
            &self.0
        }

        async fn bind(&self, _: &GoalIngressContext) -> Result<GoalSessionLease> {
            bail!("private custody test must never acquire command authority")
        }

        async fn acquire_execution(
            &self,
            _: &GoalIngressContext,
            _: &GoalExecutionScope,
            _: &GoalParentTurnCheckpoint,
        ) -> Result<Box<dyn GoalSessionExecutionLease>> {
            bail!("private custody test must never deliver")
        }
    }

    struct FailedWorkerFixture {
        scope: GoalExecutionScope,
        supervisor: Arc<GoalExecutionSupervisor>,
        registry: Arc<Registry>,
        publication: Arc<Mutex<GoalFailurePublication>>,
        withdrawal: GoalParentTurnCheckpoint,
        worker: Arc<Mutex<GoalWorker>>,
        driver: Arc<dyn GoalSessionDriver>,
        ingress: GoalIngressContext,
    }

    async fn failed_worker_fixture() -> FailedWorkerFixture {
        failed_worker_fixture_with_guard(None).await
    }

    async fn failed_worker_fixture_with_guard(
        guard: Option<Arc<dyn GoalDiagnosticTextGuard>>,
    ) -> FailedWorkerFixture {
        let key = GoalSessionKey::matrix("matrix_private_custody_fixture").unwrap();
        let session = key.durable_id();
        let ingress = GoalIngressContext::trusted(
            key.clone(),
            "fixture-agent",
            "fixture-route",
            GoalIngressPrincipal::Matrix {
                raw_mxid: "@fixture:example.test".into(),
            },
        )
        .unwrap();
        let registry = Arc::new(Registry::default());
        registry.seed(&session, TaskStatus::Failed);
        let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
        let scope = GoalExecutionScope::new(task.id, &session, task.execution_epoch).unwrap();
        let mut engine =
            GoalRuntime::new(registry.clone()).execution_engine(Arc::new(NoModelAccounting));
        if let Some(guard) = guard {
            engine = engine.with_diagnostic_text_guard(guard);
        }
        let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(engine)));
        let publication = Arc::new(Mutex::new(GoalFailurePublication::default()));
        let withdrawal = GoalParentTurnCheckpoint::new();
        let driver: Arc<dyn GoalSessionDriver> = Arc::new(NoDeliveryDriver(key));
        let (_, completion) = watch::channel(Some(Ok(GoalExecutionOutcome::Fenced)));
        let worker = Arc::new(Mutex::new(GoalWorker {
            task_id: scope.task_id().into(),
            execution_epoch: scope.execution_epoch(),
            driver: Arc::clone(&driver),
            ingress: ingress.clone(),
            failure_publication: Arc::clone(&publication),
            checkpoint: GoalParentTurnCheckpoint::new(),
            notice_withdrawal: withdrawal.clone(),
            completion,
            lifecycle_finalization: Arc::new(Mutex::new(())),
            external_stop: Arc::new(GoalExternalStopDuty::default()),
            _handle: zeroclaw_spawn::spawn!(async {}),
        }));
        supervisor
            .workers
            .lock()
            .await
            .insert(session.clone(), Arc::clone(&worker));
        FailedWorkerFixture {
            scope,
            supervisor,
            registry,
            publication,
            withdrawal,
            worker,
            driver,
            ingress,
        }
    }

    async fn fixture_delivery(fixture: &FailedWorkerFixture) -> GoalFailureNoticeDelivery {
        GoalFailureNoticeDelivery::new(
            &fixture.supervisor,
            fixture.scope.clone(),
            Arc::clone(&fixture.worker),
            Arc::clone(&fixture.driver),
            fixture.ingress.clone(),
            fixture.withdrawal.clone(),
            Arc::clone(&fixture.publication).lock_owned().await,
            fixture.worker.lock().await.external_stop.clone(),
        )
    }

    async fn assert_explicitly_withdrawn(fixture: &FailedWorkerFixture) {
        let publication = fixture.publication.try_lock().unwrap();
        assert!(publication.notice_withdrawn);
        assert!(!publication.notice_published);
        drop(publication);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            fixture.withdrawal.wait_closed(),
        )
        .await
        .unwrap();
        assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
        assert!(
            fixture
                .supervisor
                .drain_lifecycle_fence(&fixture.scope)
                .await
                .unwrap()
                .failure_notice
                .is_none()
        );
    }

    #[tokio::test]
    async fn deliver_or_withdraw_withdraws_explicitly_when_cancelled_first() {
        let fixture = failed_worker_fixture().await;
        fixture_delivery(&fixture)
            .await
            .deliver_or_withdraw(std::future::ready(()))
            .await
            .unwrap();
        assert_explicitly_withdrawn(&fixture).await;
    }

    struct PendingDeliveryDriver {
        key: GoalSessionKey,
        entered: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl GoalSessionDriver for PendingDeliveryDriver {
        fn session_key(&self) -> &GoalSessionKey {
            &self.key
        }

        async fn bind(&self, _: &GoalIngressContext) -> Result<GoalSessionLease> {
            bail!("custody test must never acquire command authority")
        }

        async fn acquire_execution(
            &self,
            _: &GoalIngressContext,
            _: &GoalExecutionScope,
            _: &GoalParentTurnCheckpoint,
        ) -> Result<Box<dyn GoalSessionExecutionLease>> {
            self.entered.notify_one();
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn deliver_or_withdraw_withdraws_explicitly_when_cancelled_during_acquisition() {
        let mut fixture = failed_worker_fixture().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        fixture.driver = Arc::new(PendingDeliveryDriver {
            key: fixture.ingress.session_key().clone(),
            entered: Arc::clone(&entered),
        });
        let delivery = fixture_delivery(&fixture).await;
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let task = zeroclaw_spawn::spawn!(async move {
            delivery
                .deliver_or_withdraw(async { cancelled.await.unwrap() })
                .await
                .unwrap();
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
            .await
            .unwrap();
        cancel.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_explicitly_withdrawn(&fixture).await;
    }

    #[tokio::test]
    async fn deliver_or_withdraw_settles_like_deliver_when_not_cancelled() {
        let fixture = failed_worker_fixture().await;
        fixture_delivery(&fixture)
            .await
            .deliver_or_withdraw(std::future::pending())
            .await
            .unwrap_err();
        let publication = fixture.publication.try_lock().unwrap();
        assert!(!publication.notice_withdrawn);
        assert!(!publication.notice_published);
        drop(publication);
        assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
    }

    #[tokio::test]
    async fn foreign_task_and_epochs_form_no_failure_obligation_or_accounting_effect() {
        for foreign in 0..3 {
            let fixture = failed_worker_fixture().await;
            {
                let mut state = fixture.registry.0.lock().unwrap();
                match foreign {
                    0 => state.task.as_mut().unwrap().id = "foreign-fixture-task".into(),
                    1 => state.task.as_mut().unwrap().execution_epoch += 3,
                    2 => {
                        let goal = state.goal.as_mut().unwrap();
                        goal.pending_call_id = Some("foreign-fixture-operation".into());
                        goal.pending_call_epoch = Some(fixture.scope.execution_epoch() + 3);
                    }
                    _ => unreachable!(),
                }
            }
            let drain = fixture
                .supervisor
                .drain_lifecycle_fence(&fixture.scope)
                .await
                .unwrap();
            assert!(drain.failure_notice.is_none());
            assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
            let publication = fixture.publication.lock().await;
            assert!(!publication.notice_published);
            assert!(!publication.notice_withdrawn);
            drop(publication);
            let state = fixture.registry.0.lock().unwrap();
            assert!(state.pending_settlements.is_empty());
            assert_eq!(state.finish_calls, 0);
            if foreign == 2 {
                assert_eq!(
                    state.goal.as_ref().unwrap().pending_call_id.as_deref(),
                    Some("foreign-fixture-operation")
                );
                assert_eq!(
                    state.goal.as_ref().unwrap().pending_call_epoch,
                    Some(fixture.scope.execution_epoch() + 3)
                );
            }
        }
    }

    #[tokio::test]
    async fn held_latch_retains_worker_and_dropped_obligation_withdraws_exact_owner() {
        let FailedWorkerFixture {
            scope,
            supervisor,
            publication,
            withdrawal,
            worker,
            driver,
            ingress,
            ..
        } = failed_worker_fixture().await;
        let session = scope.session_id();
        let guard = Arc::clone(&publication).lock_owned().await;
        let drain = supervisor.drain_lifecycle_fence(&scope).await.unwrap();
        assert!(drain.failure_notice.is_none());
        assert!(supervisor.owns_scope(&scope).await);
        supervisor.remove_worker_if_settled(session, &worker).await;
        assert!(supervisor.owns_scope(&scope).await);
        let obligation = GoalFailureNoticeDelivery::new(
            &supervisor,
            scope.clone(),
            Arc::clone(&worker),
            driver,
            ingress,
            withdrawal.clone(),
            guard,
            worker.lock().await.external_stop.clone(),
        );
        // A busy map exercises Drop's nonblocking fallback. The next drainer
        // must retire the withdrawn worker without recreating its publication.
        let map = supervisor.workers.lock().await;
        drop(obligation);
        assert!(publication.lock().await.notice_withdrawn);
        tokio::time::timeout(std::time::Duration::from_secs(1), withdrawal.wait_closed())
            .await
            .unwrap();
        drop(map);
        assert!(supervisor.owns_scope(&scope).await);
        let drain = supervisor.drain_lifecycle_fence(&scope).await.unwrap();
        assert!(drain.failure_notice.is_none());
        assert!(!supervisor.owns_scope(&scope).await);
    }

    struct PublicationScript {
        events: std::sync::Mutex<Vec<&'static str>>,
        entered: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
        block: bool,
        fail: bool,
    }

    struct PublishingDriver {
        key: GoalSessionKey,
        script: Arc<PublicationScript>,
    }

    #[async_trait::async_trait]
    impl GoalSessionDriver for PublishingDriver {
        fn session_key(&self) -> &GoalSessionKey {
            &self.key
        }
        async fn bind(&self, _: &GoalIngressContext) -> Result<GoalSessionLease> {
            bail!("publication fixture must not bind command authority")
        }
        async fn acquire_execution(
            &self,
            _: &GoalIngressContext,
            _: &GoalExecutionScope,
            _: &GoalParentTurnCheckpoint,
        ) -> Result<Box<dyn GoalSessionExecutionLease>> {
            Ok(Box::new(PublishingDriver {
                key: self.key.clone(),
                script: self.script.clone(),
            }))
        }
    }

    #[async_trait::async_trait]
    impl GoalSessionExecutionLease for PublishingDriver {
        fn session_key(&self) -> &GoalSessionKey {
            &self.key
        }
        async fn canonical_history(
            &mut self,
            _: &GoalParentTurnInput,
        ) -> Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
            bail!("publication fixture must not read history")
        }
        async fn run_parent_turn(
            &mut self,
            _: &GoalOperationScope,
            _: GoalParentTurn,
        ) -> Result<GoalParentTurnResult> {
            bail!("publication fixture must not run a parent")
        }
        async fn run_verifier(
            &mut self,
            _: &GoalOperationScope,
            _: GoalVerifierTurn,
        ) -> Result<GoalVerifierResult> {
            bail!("publication fixture must not run a verifier")
        }
        async fn append_parent_candidate(&mut self, _: String) -> Result<()> {
            bail!("publication fixture must not append history")
        }
        async fn publish_goal_core_error(&mut self, error: &anyhow::Error) -> Result<()> {
            assert!(error.chain().any(|cause| cause.is::<CleanupFault>()));
            self.script.events.lock().unwrap().push("original");
            Ok(())
        }
        async fn publish_goal_notice(&mut self, _: GoalExecutionNotice) -> Result<()> {
            self.script.events.lock().unwrap().push("notice-attempt");
            self.script.entered.notify_one();
            if self.script.block {
                self.script.release.acquire().await.unwrap().forget();
            }
            if self.script.fail {
                return Err(
                    anyhow::Error::new(CleanupFault("distinct publication body failure")).context(
                        format!("safe-long-context:{}:safe-tail", "detail-".repeat(100)),
                    ),
                );
            }
            self.script.events.lock().unwrap().push("notice-published");
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingGuard {
        inputs: std::sync::Mutex<Vec<String>>,
        entered: tokio::sync::Notify,
    }
    impl GoalDiagnosticTextGuard for RecordingGuard {
        fn guard_diagnostic_text(&self, text: &str) -> String {
            self.inputs.lock().unwrap().push(text.into());
            self.entered.notify_one();
            "[protected publication diagnostic]".into()
        }
    }

    fn publishing_driver(
        fixture: &mut FailedWorkerFixture,
        block: bool,
        fail: bool,
    ) -> Arc<PublicationScript> {
        let script = Arc::new(PublicationScript {
            events: std::sync::Mutex::new(Vec::new()),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
            block,
            fail,
        });
        fixture.driver = Arc::new(PublishingDriver {
            key: fixture.ingress.session_key().clone(),
            script: script.clone(),
        });
        script
    }

    #[tokio::test]
    async fn publication_body_result_retains_typed_cause_and_guards_the_complete_chain() {
        for fail in [false, true] {
            let guard = Arc::new(RecordingGuard::default());
            let mut fixture = failed_worker_fixture_with_guard(Some(guard.clone())).await;
            let script = publishing_driver(&mut fixture, false, fail);
            fixture.publication.lock().await.core_error = GoalCoreErrorPublication::Parked(
                anyhow::Error::new(CleanupFault("distinct parked Goal-only original")),
            );
            let delivery = fixture_delivery(&fixture).await;
            let map = if fail {
                Some(fixture.supervisor.workers.lock().await)
            } else {
                None
            };
            let task = zeroclaw_spawn::spawn!(delivery.deliver());
            if fail {
                tokio::time::timeout(std::time::Duration::from_secs(1), guard.entered.notified())
                    .await
                    .expect("body error must reach the complete diagnostic guard");
                assert!(
                    !task.is_finished(),
                    "guard the actual cause before waiting for worker removal"
                );
            }
            drop(map);
            let result = task.await.unwrap();
            if fail {
                let error = result.unwrap_err();
                assert!(error.chain().any(|cause| {
                    cause
                        .downcast_ref::<CleanupFault>()
                        .is_some_and(|fault| fault.0 == "distinct publication body failure")
                }));
                let inputs = guard.inputs.lock().unwrap();
                assert_eq!(inputs.len(), 1);
                assert!(
                    inputs[0].contains("safe-long-context:") && inputs[0].contains(":safe-tail")
                );
                assert!(inputs[0].contains("distinct publication body failure"));
                assert!(inputs[0].len() > 500);
                assert_eq!(
                    *script.events.lock().unwrap(),
                    ["original", "notice-attempt"]
                );
            } else {
                result.unwrap();
                assert!(guard.inputs.lock().unwrap().is_empty());
                assert_eq!(
                    *script.events.lock().unwrap(),
                    ["original", "notice-attempt", "notice-published"]
                );
            }
            let publication = fixture.publication.lock().await;
            assert!(matches!(
                publication.core_error,
                GoalCoreErrorPublication::Delivered
            ));
            assert_eq!(publication.notice_published, !fail);
            assert!(!publication.notice_withdrawn);
            assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
        }
    }

    #[tokio::test]
    async fn cancellation_during_publication_withdraws_without_repeating_the_original() {
        let mut fixture = failed_worker_fixture().await;
        let script = publishing_driver(&mut fixture, true, false);
        fixture.publication.lock().await.core_error = GoalCoreErrorPublication::Parked(
            anyhow::Error::new(CleanupFault("distinct parked original")),
        );
        let delivery = fixture_delivery(&fixture).await;
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let task = zeroclaw_spawn::spawn!(async move {
            delivery
                .deliver_or_withdraw(async { cancelled.await.unwrap() })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), script.entered.notified())
            .await
            .expect("delivery must reach the actual publication body");
        cancel.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(
            *script.events.lock().unwrap(),
            ["original", "notice-attempt"]
        );
        assert_explicitly_withdrawn(&fixture).await;
    }

    #[tokio::test]
    async fn host_rendered_and_delivered_slots_never_fabricate_a_second_original() {
        for slot in 0..3 {
            let mut fixture = failed_worker_fixture().await;
            let script = publishing_driver(&mut fixture, false, false);
            fixture.publication.lock().await.core_error = match slot {
                0 => GoalCoreErrorPublication::RunErrorRendered,
                1 => GoalCoreErrorPublication::Delivered,
                _ => GoalCoreErrorPublication::Absent,
            };
            fixture_delivery(&fixture).await.deliver().await.unwrap();
            fixture_delivery(&fixture).await.deliver().await.unwrap();
            assert_eq!(
                *script.events.lock().unwrap(),
                ["notice-attempt", "notice-published"]
            );
            assert!(fixture.publication.lock().await.notice_published);
        }
    }

    #[tokio::test]
    async fn withdrawal_cannot_remove_a_replacement_worker_or_an_external_duty() {
        let fixture = failed_worker_fixture().await;
        let replacement = failed_worker_fixture().await;
        let delivery = fixture_delivery(&fixture).await;
        fixture.supervisor.workers.lock().await.insert(
            fixture.scope.session_id().into(),
            replacement.worker.clone(),
        );
        delivery.withdraw().await;
        assert!(Arc::ptr_eq(
            fixture
                .supervisor
                .workers
                .lock()
                .await
                .get(fixture.scope.session_id())
                .unwrap(),
            &replacement.worker
        ));

        let fixture = failed_worker_fixture().await;
        let (completed, completion) = watch::channel(None);
        fixture.worker.lock().await.completion = completion;
        let fence = fixture
            .supervisor
            .capture_external_cancellation(fixture.scope.session_id())
            .await
            .unwrap()
            .unwrap()
            .fence()
            .await;
        fixture_delivery(&fixture).await.withdraw().await;
        drop(fence);
        assert!(fixture.supervisor.owns_scope(&fixture.scope).await);
        assert!(!fixture.supervisor.try_retire().await);
        completed
            .send(Some(Ok(GoalExecutionOutcome::Fenced)))
            .unwrap();
        let drain = fixture
            .supervisor
            .drain_lifecycle_fence(&fixture.scope)
            .await
            .unwrap();
        assert!(drain.error.is_none() && drain.failure_notice.is_none());
        assert!(fixture.supervisor.try_retire().await);
    }
}
