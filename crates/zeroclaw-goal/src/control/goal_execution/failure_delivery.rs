//! Failure output custody after classification releases lifecycle locks.

use super::*;
use tokio::sync::OwnedMutexGuard;

/// Owns one exact worker's outstanding publication until delivery or withdrawal.
/// The held latch prevents another drainer from publishing or retiring its owner.
#[must_use = "deliver or explicitly withdraw the failure publication"]
pub(super) struct GoalFailureNoticeDelivery {
    engine: Arc<GoalExecutionEngine>,
    scope: GoalExecutionScope,
    driver: Arc<dyn GoalSessionDriver>,
    ingress: GoalIngressContext,
    publication: Option<OwnedMutexGuard<GoalFailurePublication>>,
    withdrawal: GoalParentTurnCheckpoint,
    worker: Arc<Mutex<GoalWorker>>,
    workers: Arc<Mutex<HashMap<String, Arc<Mutex<GoalWorker>>>>>,
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
        }
    }

    pub(super) async fn deliver(mut self) {
        if let Some(publication) = self.publication.as_mut()
            && publish_failure_notice_body(
                self.engine.as_ref(),
                &self.scope,
                self.driver.as_ref(),
                &self.ingress,
                &self.withdrawal,
                publication,
            )
            .await
            .is_err()
        {
            log_failure_notice_delivery_failure(&self.scope);
        }
        drop(self.publication.take());
        self.remove_worker().await;
    }

    pub(super) async fn withdraw(mut self) {
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
        remove_exact(&mut workers, self.scope.session_id(), &self.worker);
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
            remove_exact(&mut workers, self.scope.session_id(), &self.worker);
        }
    }
}

fn remove_exact(
    workers: &mut HashMap<String, Arc<Mutex<GoalWorker>>>,
    session: &str,
    expected: &Arc<Mutex<GoalWorker>>,
) {
    if workers
        .get(session)
        .is_some_and(|worker| Arc::ptr_eq(worker, expected))
    {
        workers.remove(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{NoModelAccounting, Registry};
    use crate::{GoalIngressPrincipal, GoalSessionKey};

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
        supervisor: GoalExecutionSupervisor,
        registry: Arc<Registry>,
        publication: Arc<Mutex<GoalFailurePublication>>,
        withdrawal: GoalParentTurnCheckpoint,
        worker: Arc<Mutex<GoalWorker>>,
        driver: Arc<dyn GoalSessionDriver>,
        ingress: GoalIngressContext,
    }

    async fn failed_worker_fixture() -> FailedWorkerFixture {
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
        let supervisor = GoalExecutionSupervisor::new(Arc::new(
            GoalRuntime::new(registry.clone()).execution_engine(Arc::new(NoModelAccounting)),
        ));
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
}
