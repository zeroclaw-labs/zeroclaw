//! Integration tests for observability broadcast hook + FlushGuard.

use super::*;
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct CountingObserver {
    flushes: AtomicUsize,
}

impl Observer for CountingObserver {
    fn record_event(&self, _: &zeroclaw_api::observability_traits::ObserverEvent) {}
    fn record_metric(&self, _: &zeroclaw_api::observability_traits::ObserverMetric) {}
    fn flush(&self) { self.flushes.fetch_add(1, Ordering::SeqCst); }
    fn name(&self) -> &str { "counting" }
    fn as_any(&self) -> &dyn std::any::Any { self }
}

#[test]
fn tee_observer_flush_drives_broadcast_hook() {
    let _guard = HOOK_TEST_LOCK.lock();
    clear_broadcast_hook();
    let hook = Arc::new(CountingObserver::default());
    set_broadcast_hook(hook.clone());
    let cfg = ObservabilityConfig { backend: ObservabilityBackend::None, ..Default::default() };
    let observer = create_observer(&cfg);
    assert_eq!(hook.flushes.load(Ordering::SeqCst), 0);
    observer.flush();
    assert_eq!(hook.flushes.load(Ordering::SeqCst), 1);
    clear_broadcast_hook();
}

#[test]
fn flush_guard_drains_broadcast_hook_on_drop() {
    let _guard = HOOK_TEST_LOCK.lock();
    clear_broadcast_hook();
    let hook = Arc::new(CountingObserver::default());
    let _hook_guard = set_scoped_broadcast_hook(hook.clone());
    let cfg = ObservabilityConfig { backend: ObservabilityBackend::None, ..Default::default() };
    let observer: Arc<dyn Observer> = Arc::from(create_observer(&cfg));
    assert_eq!(hook.flushes.load(Ordering::SeqCst), 0);
    { let _fg = FlushGuard::new(observer.clone()); }
    assert_eq!(hook.flushes.load(Ordering::SeqCst), 1);
    drop(_hook_guard);
    observer.flush();
    assert_eq!(hook.flushes.load(Ordering::SeqCst), 1);
    clear_broadcast_hook();
}
