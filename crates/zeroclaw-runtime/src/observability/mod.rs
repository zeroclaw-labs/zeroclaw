pub mod dora;
pub mod log;
pub mod multi;
pub mod noop;
#[cfg(feature = "observability-otel")]
pub mod otel;
#[cfg(feature = "observability-prometheus")]
pub mod prometheus;
pub mod runtime_trace;
pub mod traits;
pub mod verbose;

#[allow(unused_imports)]
pub use self::log::LogObserver;
#[allow(unused_imports)]
pub use self::multi::MultiObserver;
pub use noop::NoopObserver;
#[cfg(feature = "observability-otel")]
pub use otel::OtelObserver;
#[cfg(feature = "observability-prometheus")]
pub use prometheus::PrometheusObserver;
pub use traits::{Observer, ObserverEvent};
#[allow(unused_imports)]
pub use verbose::VerboseObserver;

use std::any::Any;
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;
use traits::ObserverMetric;
use zeroclaw_config::schema::{ObservabilityBackend, ObservabilityConfig};

/// Process-wide broadcast hook installed by long-running subsystems (today: the
/// gateway) so that events emitted by observers built in *other* subsystems —
/// notably the agent loop's `process_message` — also fan out to the SSE
/// broadcast channel. Without this, observers created per call site stay
/// isolated and `/api/events` only sees the gateway's own direct emissions.
///
/// Uses `parking_lot::RwLock` so the event-recording path never has to handle
/// lock poisoning: a panic inside a hook would not silently disable the entire
/// observability channel on subsequent calls.
static BROADCAST_HOOK: OnceLock<RwLock<BroadcastHookState>> = OnceLock::new();

struct BroadcastHookEntry {
    scoped_id: Option<u64>,
    observer: Arc<dyn Observer>,
}

#[derive(Default)]
struct BroadcastHookState {
    next_scoped_id: u64,
    entries: Vec<BroadcastHookEntry>,
}

impl BroadcastHookState {
    fn current(&self) -> Option<Arc<dyn Observer>> {
        self.entries.last().map(|entry| entry.observer.clone())
    }
}

fn broadcast_hook_slot() -> &'static RwLock<BroadcastHookState> {
    BROADCAST_HOOK.get_or_init(|| RwLock::new(BroadcastHookState::default()))
}

/// Install a process-wide observer that will receive every event recorded
/// through observers built by [`create_observer`]. Calling this again replaces
/// the previous hook.
pub fn set_broadcast_hook(observer: Arc<dyn Observer>) {
    let mut slot = broadcast_hook_slot().write();
    slot.entries.clear();
    slot.entries.push(BroadcastHookEntry {
        scoped_id: None,
        observer,
    });
}

/// Guard returned by [`set_scoped_broadcast_hook`].
///
/// Dropping the guard removes the hook it installed, but only if a later caller
/// has not already replaced the process-wide hook. If multiple scoped hooks are
/// live at once, dropping the newest hook restores the previous still-live hook.
#[must_use = "hold the guard for as long as the broadcast hook should remain installed"]
pub struct BroadcastHookGuard {
    scoped_id: u64,
}

impl Drop for BroadcastHookGuard {
    fn drop(&mut self) {
        let mut slot = broadcast_hook_slot().write();
        slot.entries
            .retain(|entry| entry.scoped_id != Some(self.scoped_id));
    }
}

/// Install a process-wide observer and return a guard that clears it on drop.
#[must_use = "hold the guard for as long as the broadcast hook should remain installed"]
pub fn set_scoped_broadcast_hook(observer: Arc<dyn Observer>) -> BroadcastHookGuard {
    let mut slot = broadcast_hook_slot().write();
    let scoped_id = slot.next_scoped_id;
    slot.next_scoped_id = slot.next_scoped_id.wrapping_add(1);
    slot.entries.push(BroadcastHookEntry {
        scoped_id: Some(scoped_id),
        observer,
    });
    BroadcastHookGuard { scoped_id }
}

/// Remove the broadcast hook, if any. Intended for tests and orderly shutdown.
pub fn clear_broadcast_hook() {
    broadcast_hook_slot().write().entries.clear();
}

fn current_broadcast_hook() -> Option<Arc<dyn Observer>> {
    broadcast_hook_slot().read().current()
}

/// Wrapper that forwards every event to a primary observer plus the
/// process-wide broadcast hook (when set). Metrics flow only to the primary.
struct TeeObserver {
    primary: Box<dyn Observer>,
}

impl Observer for TeeObserver {
    fn record_event(&self, event: &ObserverEvent) {
        self.primary.record_event(event);
        if let Some(hook) = current_broadcast_hook() {
            hook.record_event(event);
        }
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        self.primary.record_metric(metric);
    }

    fn flush(&self) {
        self.primary.flush();
    }

    fn name(&self) -> &str {
        // Delegate so callers (and tests) see the underlying backend name,
        // not the internal wrapper.
        self.primary.name()
    }

    fn as_any(&self) -> &dyn Any {
        // Expose the primary so downcasts (e.g. to PrometheusObserver in the
        // gateway's /metrics handler) keep working transparently.
        self.primary.as_any()
    }
}

/// Factory: create the right observer from config
pub fn create_observer(config: &ObservabilityConfig) -> Box<dyn Observer> {
    Box::new(TeeObserver {
        primary: create_primary_observer(config),
    })
}

fn create_primary_observer(config: &ObservabilityConfig) -> Box<dyn Observer> {
    match config.backend {
        ObservabilityBackend::Log => Box::new(LogObserver::new()),
        ObservabilityBackend::Verbose => Box::new(VerboseObserver::new()),
        ObservabilityBackend::Prometheus => {
            #[cfg(feature = "observability-prometheus")]
            {
                Box::new(PrometheusObserver::shared())
            }
            #[cfg(not(feature = "observability-prometheus"))]
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "Prometheus backend requested but this build was compiled without `observability-prometheus`; falling back to noop."
                );
                Box::new(NoopObserver)
            }
        }
        ObservabilityBackend::Otel => {
            #[cfg(feature = "observability-otel")]
            match OtelObserver::new(
                config.otel_endpoint.as_deref(),
                config.otel_service_name.as_deref(),
                config.otel_headers.clone(),
            ) {
                Ok(obs) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"endpoint": config
                            .otel_endpoint
                            .as_deref()
                            .unwrap_or("http://localhost:4318")})),
                        "OpenTelemetry observer initialized"
                    );
                    Box::new(obs)
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "Failed to create OTel observer. Falling back to noop."
                    );
                    Box::new(NoopObserver)
                }
            }
            #[cfg(not(feature = "observability-otel"))]
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "OpenTelemetry backend requested but this build was compiled without `observability-otel`; falling back to noop."
                );
                Box::new(NoopObserver)
            }
        }
        ObservabilityBackend::None => Box::new(NoopObserver),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_none_returns_noop() {
        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::None,
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_noop_returns_noop() {
        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::None,
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "noop");
    }

    #[test]
    fn factory_log_returns_log() {
        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::Log,
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "log");
    }

    #[test]
    fn factory_verbose_returns_verbose() {
        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::Verbose,
            ..ObservabilityConfig::default()
        };
        assert_eq!(create_observer(&cfg).name(), "verbose");
    }

    #[test]
    fn factory_prometheus_returns_prometheus() {
        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::Prometheus,
            ..ObservabilityConfig::default()
        };
        let expected = if cfg!(feature = "observability-prometheus") {
            "prometheus"
        } else {
            "noop"
        };
        assert_eq!(create_observer(&cfg).name(), expected);
    }

    #[test]
    fn factory_otel_returns_otel() {
        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::Otel,
            otel_endpoint: Some("http://127.0.0.1:19999".into()),
            otel_service_name: Some("test".into()),
            ..ObservabilityConfig::default()
        };
        let expected = if cfg!(feature = "observability-otel") {
            "otel"
        } else {
            "noop"
        };
        assert_eq!(create_observer(&cfg).name(), expected);
    }

    #[test]
    fn factory_opentelemetry_alias() {
        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::Otel,
            otel_endpoint: Some("http://127.0.0.1:19999".into()),
            otel_service_name: Some("test".into()),
            ..ObservabilityConfig::default()
        };
        let expected = if cfg!(feature = "observability-otel") {
            "otel"
        } else {
            "noop"
        };
        assert_eq!(create_observer(&cfg).name(), expected);
    }

    #[test]
    fn factory_otlp_alias() {
        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::Otel,
            otel_endpoint: Some("http://127.0.0.1:19999".into()),
            otel_service_name: Some("test".into()),
            ..ObservabilityConfig::default()
        };
        let expected = if cfg!(feature = "observability-otel") {
            "otel"
        } else {
            "noop"
        };
        assert_eq!(create_observer(&cfg).name(), expected);
    }

    #[test]
    fn unknown_backend_falls_back_to_noop_at_load() {
        let bad: ObservabilityConfig = toml::from_str("backend = \"xyzzy_unknown\"").unwrap();
        assert_eq!(bad.backend, ObservabilityBackend::None);
        let empty: ObservabilityConfig = toml::from_str("backend = \"\"").unwrap();
        assert_eq!(empty.backend, ObservabilityBackend::None);
        assert_eq!(create_observer(&bad).name(), "noop");
    }

    use parking_lot::Mutex as PlMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test observer that counts events and metrics, used to verify the
    /// broadcast hook fan-out and that downcasts pass through `TeeObserver`.
    #[derive(Default)]
    struct CountingObserver {
        events: AtomicUsize,
        metrics: AtomicUsize,
    }

    impl Observer for CountingObserver {
        fn record_event(&self, _event: &ObserverEvent) {
            self.events.fetch_add(1, Ordering::SeqCst);
        }

        fn record_metric(&self, _metric: &ObserverMetric) {
            self.metrics.fetch_add(1, Ordering::SeqCst);
        }

        fn name(&self) -> &str {
            "counting"
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Serialize tests that touch the process-wide broadcast hook so they
    /// don't observe each other's installations.
    static HOOK_TEST_LOCK: PlMutex<()> = PlMutex::new(());

    #[test]
    fn broadcast_hook_receives_events_from_factory_observer() {
        let _guard = HOOK_TEST_LOCK.lock();
        clear_broadcast_hook();

        let hook = Arc::new(CountingObserver::default());
        set_broadcast_hook(hook.clone());

        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::None,
            ..ObservabilityConfig::default()
        };
        let observer = create_observer(&cfg);

        observer.record_event(&ObserverEvent::HeartbeatTick);
        observer.record_event(&ObserverEvent::Error {
            component: "x".into(),
            message: "y".into(),
        });

        assert_eq!(hook.events.load(Ordering::SeqCst), 2);

        clear_broadcast_hook();
    }

    #[test]
    fn broadcast_hook_does_not_receive_metrics() {
        let _guard = HOOK_TEST_LOCK.lock();
        clear_broadcast_hook();

        let hook = Arc::new(CountingObserver::default());
        set_broadcast_hook(hook.clone());

        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::None,
            ..ObservabilityConfig::default()
        };
        let observer = create_observer(&cfg);

        observer.record_metric(&ObserverMetric::TokensUsed(10));
        observer.record_metric(&ObserverMetric::TokensUsed(20));

        assert_eq!(hook.events.load(Ordering::SeqCst), 0);
        assert_eq!(hook.metrics.load(Ordering::SeqCst), 0);

        clear_broadcast_hook();
    }

    #[test]
    fn broadcast_hook_unset_means_only_primary_runs() {
        let _guard = HOOK_TEST_LOCK.lock();
        clear_broadcast_hook();

        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::None,
            ..ObservabilityConfig::default()
        };
        let observer = create_observer(&cfg);

        // No hook installed; recording must not panic and must be a no-op.
        observer.record_event(&ObserverEvent::HeartbeatTick);
        observer.record_metric(&ObserverMetric::TokensUsed(1));
    }

    #[test]
    fn scoped_broadcast_hook_guard_clears_installed_hook_on_drop() {
        let _guard = HOOK_TEST_LOCK.lock();
        clear_broadcast_hook();

        let hook = Arc::new(CountingObserver::default());
        let broadcast_guard = set_scoped_broadcast_hook(hook.clone());

        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::None,
            ..ObservabilityConfig::default()
        };
        let observer = create_observer(&cfg);
        observer.record_event(&ObserverEvent::HeartbeatTick);
        assert_eq!(hook.events.load(Ordering::SeqCst), 1);

        drop(broadcast_guard);
        observer.record_event(&ObserverEvent::HeartbeatTick);
        assert_eq!(hook.events.load(Ordering::SeqCst), 1);

        clear_broadcast_hook();
    }

    #[test]
    fn scoped_broadcast_hook_guard_preserves_replacement_hook() {
        let _guard = HOOK_TEST_LOCK.lock();
        clear_broadcast_hook();

        let old_hook = Arc::new(CountingObserver::default());
        let old_guard = set_scoped_broadcast_hook(old_hook.clone());

        let new_hook = Arc::new(CountingObserver::default());
        set_broadcast_hook(new_hook.clone());
        drop(old_guard);

        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::None,
            ..ObservabilityConfig::default()
        };
        let observer = create_observer(&cfg);
        observer.record_event(&ObserverEvent::HeartbeatTick);

        assert_eq!(old_hook.events.load(Ordering::SeqCst), 0);
        assert_eq!(new_hook.events.load(Ordering::SeqCst), 1);

        clear_broadcast_hook();
    }

    #[test]
    fn dropping_newer_scoped_broadcast_hook_restores_older_live_hook() {
        let _guard = HOOK_TEST_LOCK.lock();
        clear_broadcast_hook();

        let old_hook = Arc::new(CountingObserver::default());
        let old_guard = set_scoped_broadcast_hook(old_hook.clone());

        let new_hook = Arc::new(CountingObserver::default());
        let new_guard = set_scoped_broadcast_hook(new_hook.clone());

        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::None,
            ..ObservabilityConfig::default()
        };
        let observer = create_observer(&cfg);
        observer.record_event(&ObserverEvent::HeartbeatTick);
        assert_eq!(old_hook.events.load(Ordering::SeqCst), 0);
        assert_eq!(new_hook.events.load(Ordering::SeqCst), 1);

        drop(new_guard);
        observer.record_event(&ObserverEvent::HeartbeatTick);
        assert_eq!(old_hook.events.load(Ordering::SeqCst), 1);
        assert_eq!(new_hook.events.load(Ordering::SeqCst), 1);

        drop(old_guard);
        observer.record_event(&ObserverEvent::HeartbeatTick);
        assert_eq!(old_hook.events.load(Ordering::SeqCst), 1);
        assert_eq!(new_hook.events.load(Ordering::SeqCst), 1);

        clear_broadcast_hook();
    }

    #[test]
    fn factory_observer_downcasts_through_tee() {
        let _guard = HOOK_TEST_LOCK.lock();
        clear_broadcast_hook();

        let cfg = ObservabilityConfig {
            backend: ObservabilityBackend::Log,
            ..ObservabilityConfig::default()
        };
        let observer = create_observer(&cfg);

        // `as_any` must surface the primary observer so existing downcasts
        // (e.g. PrometheusObserver in /metrics) keep working through the tee.
        assert!(observer.as_any().downcast_ref::<LogObserver>().is_some());
    }
}
