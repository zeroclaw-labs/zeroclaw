//! Parent-owned monitoring with a durable, single-attempt incident claim.
//! `data_dir/heartbeat/history.db` owns the watchdog baseline, tick generation,
//! and last delivery attempt; task history and live metrics are not substitutes.
//! Notification policy resolves from the canonical live daemon Config each check.
//! `deadman_timeout_minutes = 0` mutes only this notification.
//! Muting/restarting does not re-arm a claimed incident; only a completed tick
//! does. Channel failures are uncertain and never retried automatically. This
//! favors avoiding duplicate alerts over guaranteed delivery after a crash.
//! Optional daily quiet hours defer unclaimed incidents until the next allowed
//! check. Ordinary heartbeat tasks remain independent of this notification,
//! and recovery is logged internally only.
use super::store;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::{future::Future, path::Path, sync::Arc};
use tokio::time::Duration;
use zeroclaw_config::schema::Config;

const POLICY_COMPONENT: &str = "heartbeat-notification-policy";

/// Neither future is detached. Dropping/reloading the parent drops both, and
/// a worker error cannot leave a watcher with stale configuration/metrics.
pub(crate) async fn supervise(
    worker: impl Future<Output = Result<()>>,
    watcher: impl Future<Output = Result<()>>,
) -> Result<()> {
    tokio::select! {
        // Give the watcher its immediate durable-state check even when startup
        // fails synchronously. Otherwise repeated short worker generations can
        // starve the check forever despite an overdue persisted baseline.
        biased;
        result = watcher => result,
        result = worker => result,
    }
}

/// Settle one bounded startup check before polling the worker, so a fast
/// startup failure cannot systematically cancel a pending notification. The
/// owning daemon future still cancels this check on shutdown/reload.
pub(crate) async fn run<F, Fut>(
    worker: impl Future<Output = Result<()>>,
    data_dir: &Path,
    resolve: impl Fn() -> Result<Option<Arc<Config>>>,
    deliver: F,
) -> Result<()>
where
    F: FnMut(Arc<Config>, i64) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    run_with_clock(worker, data_dir, resolve, Utc::now, deliver).await
}

async fn run_with_clock<F, Fut>(
    worker: impl Future<Output = Result<()>>,
    data_dir: &Path,
    resolve: impl Fn() -> Result<Option<Arc<Config>>>,
    now: impl Fn() -> DateTime<Utc>,
    mut deliver: F,
) -> Result<()>
where
    F: FnMut(Arc<Config>, i64) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    check_resolved_once(data_dir, resolve(), now(), &mut deliver).await?;
    supervise(worker, watch_with_clock(data_dir, resolve, now, deliver)).await
}

async fn watch_with_clock<F, Fut>(
    data_dir: &Path,
    resolve: impl Fn() -> Result<Option<Arc<Config>>>,
    now: impl Fn() -> DateTime<Utc>,
    mut deliver: F,
) -> Result<()>
where
    F: FnMut(Arc<Config>, i64) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    loop {
        check_resolved_once(data_dir, resolve(), now(), &mut deliver).await?;
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

fn report_policy_error(error: &anyhow::Error) {
    crate::health::mark_component_error(POLICY_COMPONENT, error.to_string());
    ::zeroclaw_log::record!(
        ERROR,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({"error": error.to_string()})),
        "Heartbeat notification policy invalid; alerts suppressed until configuration is corrected"
    );
}

async fn check_resolved_once<F, Fut>(
    data_dir: &Path,
    resolved: Result<Option<Arc<Config>>>,
    now: DateTime<Utc>,
    deliver: &mut F,
) -> Result<()>
where
    F: FnMut(Arc<Config>, i64) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    match resolved {
        Ok(current) => check_once(data_dir, current, now, deliver).await,
        Err(error) => {
            // An invalid dedicated notification route must not stop the worker.
            report_policy_error(&error);
            Ok(())
        }
    }
}

async fn check_once<F, Fut>(
    data_dir: &Path,
    current: Option<Arc<Config>>,
    now: DateTime<Utc>,
    deliver: &mut F,
) -> Result<()>
where
    F: FnMut(Arc<Config>, i64) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let Some(current) = current else {
        crate::health::mark_component_ok(POLICY_COMPONENT);
        return Ok(());
    };
    let policy = &current.heartbeat;
    if !policy.enabled || policy.deadman_timeout_minutes == 0 {
        crate::health::mark_component_ok(POLICY_COMPONENT);
        return Ok(());
    }
    if let Some(quiet) = &policy.deadman_quiet_hours {
        match quiet.contains(now) {
            Ok(true) => {
                // Defer without claiming: only a still-overdue incident can
                // notify after quiet hours. Actual recovery remains authoritative.
                crate::health::mark_component_ok(POLICY_COMPONENT);
                return Ok(());
            }
            Ok(false) => {}
            Err(error) => {
                // Normal config saves reject this. A malformed file or direct
                // live-handle mutation must fail closed for notifications only,
                // without aborting ordinary heartbeat work through supervise.
                report_policy_error(&error);
                return Ok(());
            }
        }
    }
    crate::health::mark_component_ok(POLICY_COMPONENT);
    if let Some(sequence) =
        store::claim_deadman_alert(data_dir, now, policy.deadman_timeout_minutes)?
    {
        let delivered = matches!(
            tokio::time::timeout(Duration::from_secs(30), deliver(current, sequence)).await,
            Ok(Ok(()))
        );
        store::finish_deadman_alert(data_dir, sequence, delivered)?;
        if !delivered {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Deadman alert delivery unconfirmed; incident will not be retried"
            );
        }
    }
    Ok(())
}

/// Persistence is required for re-arm. If it fails, the worker propagates the
/// error instead of pretending a volatile timestamp settled the incident.
pub(crate) fn completed_tick(data_dir: &Path) -> Result<()> {
    if store::record_completed_tick(data_dir, Utc::now())? {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Heartbeat recovered; deadman alert re-armed after completed tick"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn policy(timeout: u32) -> Result<Option<Arc<Config>>> {
        let mut config = Config::default();
        config.heartbeat.enabled = true;
        config.heartbeat.deadman_timeout_minutes = timeout;
        Ok(Some(Arc::new(config)))
    }

    #[tokio::test]
    async fn invalid_quiet_policy_does_not_stop_worker_or_claim_and_recovers() {
        // The health registry is process-global; isolate its status assertions
        // from other watchdog fixtures that concurrently publish valid policy.
        if std::env::var_os("ZEROCLAW_TEST_INVALID_QUIET_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "heartbeat::deadman::tests::invalid_quiet_policy_does_not_stop_worker_or_claim_and_recovers"])
                .env("ZEROCLAW_TEST_INVALID_QUIET_CHILD", "1")
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        store::start_deadman(tmp.path(), now - chrono::Duration::hours(1)).unwrap();
        let mut invalid = policy(45).unwrap().unwrap();
        Arc::make_mut(&mut invalid).heartbeat.deadman_quiet_hours = Some(Default::default());
        let worker_polls = AtomicUsize::new(0);
        run_with_clock(
            async {
                worker_polls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            tmp.path(),
            || Ok(Some(invalid.clone())),
            || now,
            |_, _| async { panic!("invalid policy must not notify") },
        )
        .await
        .unwrap();
        assert_eq!(worker_polls.load(Ordering::SeqCst), 1);
        assert_eq!(
            crate::health::snapshot().components[POLICY_COMPONENT].status,
            "error"
        );
        check_once(tmp.path(), None, now, &mut |_, _| async {
            panic!("removed policy must not notify")
        })
        .await
        .unwrap();
        assert_eq!(
            crate::health::snapshot().components[POLICY_COMPONENT].status,
            "ok"
        );
        run_with_clock(
            async { Ok(()) },
            tmp.path(),
            || anyhow::bail!("synthetic invalid notification route"),
            || now,
            |_, _| async { panic!("invalid route must not notify") },
        )
        .await
        .unwrap();
        assert_eq!(
            crate::health::snapshot().components[POLICY_COMPONENT].status,
            "error"
        );
        Arc::make_mut(&mut invalid)
            .heartbeat
            .deadman_timeout_minutes = 0;
        check_once(tmp.path(), Some(invalid), now, &mut |_, _| async {
            panic!("muted policy must not notify")
        })
        .await
        .unwrap();
        assert_eq!(
            crate::health::snapshot().components[POLICY_COMPONENT].status,
            "ok"
        );
        assert!(
            store::claim_deadman_alert(tmp.path(), now, 45)
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn startup_check_finishes_async_delivery_before_fast_worker_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        store::start_deadman(tmp.path(), now - chrono::Duration::hours(1)).unwrap();
        let worker_polls = AtomicUsize::new(0);
        let sends = AtomicUsize::new(0);
        let result = run_with_clock(
            async {
                worker_polls.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("synthetic startup failure")
            },
            tmp.path(),
            || policy(45),
            || now,
            |_, _| async {
                assert_eq!(worker_polls.load(Ordering::SeqCst), 0);
                tokio::time::sleep(Duration::from_secs(2)).await;
                assert_eq!(worker_polls.load(Ordering::SeqCst), 0);
                sends.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(worker_polls.load(Ordering::SeqCst), 1);
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        assert_eq!(
            store::claim_deadman_alert(tmp.path(), now, 45).unwrap(),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn startup_delivery_timeout_still_allows_worker_and_never_retries() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        store::start_deadman(tmp.path(), now - chrono::Duration::hours(1)).unwrap();
        let start = tokio::time::Instant::now();
        let result = run_with_clock(
            async { anyhow::bail!("synthetic worker failure after bounded check") },
            tmp.path(),
            || policy(45),
            || now,
            |_, _| std::future::pending::<Result<()>>(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(start.elapsed(), Duration::from_secs(30));
        assert_eq!(
            store::claim_deadman_alert(tmp.path(), now, 45).unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn quiet_hours_defer_only_still_overdue_incidents() {
        use zeroclaw_config::schema::HeartbeatQuietHoursConfig;
        let tmp = tempfile::tempdir().unwrap();
        let at = |text: &str| DateTime::parse_from_rfc3339(text).unwrap().to_utc();
        let mut config = policy(45).unwrap().unwrap();
        Arc::make_mut(&mut config).heartbeat.deadman_quiet_hours =
            Some(HeartbeatQuietHoursConfig {
                start: "22:00".into(),
                end: "07:00".into(),
                timezone: "UTC".into(),
            });
        store::start_deadman(tmp.path(), at("2026-09-27T21:00:00Z")).unwrap();
        let sends = AtomicUsize::new(0);
        let mut deliver = |_: Arc<Config>, _: i64| {
            sends.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(()))
        };
        for timestamp in ["2026-09-27T22:00:00Z", "2026-09-28T06:59:59Z"] {
            check_once(
                tmp.path(),
                Some(config.clone()),
                at(timestamp),
                &mut deliver,
            )
            .await
            .unwrap();
        }
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        check_once(
            tmp.path(),
            Some(config.clone()),
            at("2026-09-28T07:00:00Z"),
            &mut deliver,
        )
        .await
        .unwrap();
        check_once(
            tmp.path(),
            Some(config.clone()),
            at("2026-09-28T08:00:00Z"),
            &mut deliver,
        )
        .await
        .unwrap();
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        // The next absence recovers while quiet: it must not notify at dawn.
        store::record_completed_tick(tmp.path(), at("2026-09-28T21:00:00Z")).unwrap();
        check_once(
            tmp.path(),
            Some(config.clone()),
            at("2026-09-28T23:00:00Z"),
            &mut deliver,
        )
        .await
        .unwrap();
        store::record_completed_tick(tmp.path(), at("2026-09-29T06:59:00Z")).unwrap();
        check_once(
            tmp.path(),
            Some(config.clone()),
            at("2026-09-29T07:00:00Z"),
            &mut deliver,
        )
        .await
        .unwrap();
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        // A genuinely new prolonged absence can still notify.
        check_once(
            tmp.path(),
            Some(config),
            at("2026-09-29T07:45:00Z"),
            &mut deliver,
        )
        .await
        .unwrap();
        assert_eq!(sends.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn live_mute_and_quiet_policy_changes_reach_existing_watcher() {
        use zeroclaw_config::schema::HeartbeatQuietHoursConfig;
        let tmp = tempfile::tempdir().unwrap();
        let base = DateTime::parse_from_rfc3339("2026-09-28T12:00:00Z")
            .unwrap()
            .to_utc();
        store::start_deadman(tmp.path(), base - chrono::Duration::hours(1)).unwrap();
        let live = Arc::new(parking_lot::RwLock::new(
            policy(0).unwrap().unwrap().as_ref().clone(),
        ));
        let source = live.clone();
        let sends = Arc::new(AtomicUsize::new(0));
        let observed = sends.clone();
        let delivered_routes = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let routes = delivered_routes.clone();
        let root = tmp.path().to_owned();
        let start = tokio::time::Instant::now();
        let task = ::zeroclaw_spawn::spawn!(async move {
            run_with_clock(
                std::future::pending(),
                &root,
                || Ok(Some(Arc::new(source.read().clone()))),
                || base + chrono::Duration::from_std(start.elapsed()).unwrap(),
                |current, sequence| {
                    routes
                        .lock()
                        .push((current.heartbeat.deadman_to.clone(), sequence));
                    observed.fetch_add(1, Ordering::SeqCst);
                    std::future::ready(Ok(()))
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        {
            let mut current = live.write();
            current.heartbeat.deadman_timeout_minutes = 45;
            current.heartbeat.deadman_to = Some("first-destination".into());
        }
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        live.write().heartbeat.deadman_timeout_minutes = 0;
        store::record_completed_tick(tmp.path(), base).unwrap();
        tokio::time::advance(Duration::from_secs(60 * 60)).await;
        tokio::task::yield_now().await;
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        {
            let mut current = live.write();
            current.heartbeat.deadman_timeout_minutes = 45;
            current.heartbeat.deadman_quiet_hours = Some(HeartbeatQuietHoursConfig {
                start: "12:00".into(),
                end: "18:00".into(),
                timezone: "UTC".into(),
            });
        }
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        {
            let mut current = live.write();
            current.heartbeat.deadman_quiet_hours = None;
            current.heartbeat.deadman_to = Some("updated-destination".into());
        }
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        {
            let delivered = delivered_routes.lock();
            assert_eq!(delivered[0].0.as_deref(), Some("first-destination"));
            assert_eq!(delivered[1].0.as_deref(), Some("updated-destination"));
            assert_ne!(delivered[0].1, delivered[1].1);
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_immediate_worker_failures_do_not_starve_overdue_incident() {
        let tmp = tempfile::tempdir().unwrap();
        let base = Utc::now();
        store::start_deadman(tmp.path(), base).unwrap();
        let attempts = AtomicUsize::new(0);
        for minute in 0..70 {
            let now = base + chrono::Duration::minutes(minute);
            // Recreate the exact parent ownership boundary each generation:
            // startup fails before any minute-long timer could have fired.
            store::start_deadman(tmp.path(), now).unwrap();
            let result = supervise(
                async { anyhow::bail!("synthetic worker startup failure") },
                watch_with_clock(
                    tmp.path(),
                    || policy(45),
                    || now,
                    |_, _| async {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                ),
            )
            .await;
            assert!(result.is_err());
            assert_eq!(attempts.load(Ordering::SeqCst), usize::from(minute > 45));
        }
        assert!(
            store::record_completed_tick(tmp.path(), base + chrono::Duration::minutes(70)).unwrap()
        );
        let result = supervise(
            async { anyhow::bail!("synthetic later startup failure") },
            watch_with_clock(
                tmp.path(),
                || policy(45),
                || base + chrono::Duration::minutes(116),
                |_, _| async {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            ),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn immediate_worker_failure_cancels_pending_attempt_without_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        store::start_deadman(tmp.path(), now - chrono::Duration::hours(1)).unwrap();
        let attempts = AtomicUsize::new(0);
        let result = supervise(
            async { anyhow::bail!("synthetic immediate startup failure") },
            watch_with_clock(
                tmp.path(),
                || policy(45),
                || now,
                |_, _| {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<Result<()>>()
                },
            ),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        store::start_deadman(tmp.path(), now).unwrap();
        assert_eq!(
            store::claim_deadman_alert(tmp.path(), now + chrono::Duration::hours(1), 45).unwrap(),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_after_possible_delivery_is_not_retried_and_parent_drop_cancels() {
        let tmp = tempfile::tempdir().unwrap();
        let base = Utc::now();
        store::start_deadman(tmp.path(), base).unwrap();
        let start = tokio::time::Instant::now();
        let sends = Arc::new(AtomicUsize::new(0));
        let root = tmp.path().to_owned();
        let observed = sends.clone();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = ::zeroclaw_spawn::spawn!(async move {
            supervise(
                async {
                    let _ = stop_rx.await;
                    Ok(())
                },
                watch_with_clock(
                    &root,
                    || policy(45),
                    || base + chrono::Duration::from_std(start.elapsed()).unwrap(),
                    |_, _| {
                        observed.fetch_add(1, Ordering::SeqCst);
                        // The endpoint may accept delivery before its response hangs.
                        std::future::pending::<Result<()>>()
                    },
                ),
            )
            .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(46 * 60)).await;
        tokio::task::yield_now().await;
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(60 * 60)).await;
        tokio::task::yield_now().await;
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        stop_tx.send(()).unwrap();
        task.await.unwrap().unwrap();
        // A real tick allows a later incident, but the old watcher must be gone.
        store::record_completed_tick(tmp.path(), base).unwrap();
        tokio::time::advance(Duration::from_secs(60 * 60)).await;
        tokio::task::yield_now().await;
        assert_eq!(sends.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn abort_drops_inflight_delivery_and_restart_keeps_claim() {
        struct PendingDelivery(Arc<AtomicUsize>);
        impl Drop for PendingDelivery {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        store::start_deadman(tmp.path(), now - chrono::Duration::hours(1)).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let observed = drops.clone();
        let root = tmp.path().to_owned();
        let task = ::zeroclaw_spawn::spawn!(async move {
            supervise(
                std::future::pending(),
                watch_with_clock(
                    &root,
                    || policy(45),
                    || now,
                    |_, _| {
                        let pending = PendingDelivery(observed.clone());
                        async move {
                            let _pending = pending;
                            std::future::pending::<Result<()>>().await
                        }
                    },
                ),
            )
            .await
        });
        tokio::task::yield_now().await;
        // The overdue check now starts immediately. Abort while its delivery
        // is still pending, before the 30-second delivery timeout.
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        store::start_deadman(tmp.path(), now).unwrap();
        assert_eq!(
            store::claim_deadman_alert(tmp.path(), now + chrono::Duration::hours(1), 45).unwrap(),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn muted_watcher_never_calls_delivery_or_claims() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        store::start_deadman(tmp.path(), now - chrono::Duration::hours(1)).unwrap();
        let watcher = watch_with_clock(
            tmp.path(),
            || policy(0),
            || now,
            |_, _| async { panic!("muted watcher sent an alert") },
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(2 * 60 * 60), watcher)
                .await
                .is_err()
        );
        assert!(
            store::claim_deadman_alert(tmp.path(), now, 45)
                .unwrap()
                .is_some()
        );
    }
}
