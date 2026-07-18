//! Integration tests for observability broadcast hook and FlushGuard.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use zeroclaw_runtime::observability::{
    BroadcastHookGuard, FlushGuard, ObservabilityBackend, ObservabilityConfig, Observer,
    clear_broadcast_hook, create_observer, current_broadcast_hook, set_broadcast_hook,
    set_scoped_broadcast_hook,
};

#[derive(Default)]
struct CountingObserver {
    flushes: AtomicUsize,
}

impl Observer for CountingObserver {
    fn record_event(&self, _event: &zeroclaw_api::observability_traits::ObserverEvent) {}

    fn record_metric(&self, _metric: &zeroclaw_api::observability_traits::ObserverMetric) {}

    fn flush(&self) {
        self.flushes.fetch_add(1, Ordering::SeqCst);
    }

    fn name(&self) -> &str {
        "counting"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

static HOOK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn tee_observer_flush_drives_broadcast_hook() {
    let _guard = HOOK_TEST_LOCK.lock();
    clear_broadcast_hook();

    let hook = Arc::new(CountingObserver::default());
    set_broadcast_hook(hook.clone());

    let cfg = ObservabilityConfig {
        backend: ObservabilityBackend::None,
        ..ObservabilityConfig::default()
    };
    let observer = create_observer(&cfg);

    assert_eq!(hook.flushes.load(Ordering::SeqCst), 0);
    observer.flush();
    assert_eq!(
        hook.flushes.load(Ordering::SeqCst),
        1,
        "TeeObserver::flush must fan to the broadcast hook"
    );

    clear_broadcast_hook();
}

#[test]
fn flush_guard_drains_broadcast_hook_on_drop() {
    let _guard = HOOK_TEST_LOCK.lock();
    clear_broadcast_hook();

    let hook = Arc::new(CountingObserver::default());
    let _hook_guard = set_scoped_broadcast_hook(hook.clone());

    let cfg = ObservabilityConfig {
        backend: ObservabilityBackend::None,
        ..ObservabilityConfig::default()
    };
    let observer: Arc<dyn Observer> = Arc::from(create_observer(&cfg));

    assert_eq!(hook.flushes.load(Ordering::SeqCst), 0);

    // Drop the FlushGuard first — this is the production drop order
    // (FlushGuard declared after BroadcastHookGuard, so drops first).
    // The hook must be flushed while still installed.
    {
        let _flush_guard = FlushGuard::new(observer.clone());
        assert_eq!(hook.flushes.load(Ordering::SeqCst), 0);
    }
    assert_eq!(
        hook.flushes.load(Ordering::SeqCst),
        1,
        "FlushGuard::drop must flush the broadcast hook while it is still installed"
    );

    // Now drop the hook guard; the hook is removed and subsequent flushes
    // must not reach it.
    drop(_hook_guard);
    observer.flush();
    assert_eq!(
        hook.flushes.load(Ordering::SeqCst),
        1,
        "no further flushes after the hook is uninstalled"
    );

    clear_broadcast_hook();
}
