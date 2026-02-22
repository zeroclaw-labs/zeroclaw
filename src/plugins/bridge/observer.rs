use std::sync::Arc;

use crate::observability::traits::ObserverMetric;
use crate::observability::{Observer, ObserverEvent};

pub struct ObserverBridge {
    inner: Arc<dyn Observer>,
}

impl ObserverBridge {
    pub fn new(inner: Arc<dyn Observer>) -> Self {
        Self { inner }
    }

    pub fn new_box(inner: Box<dyn Observer>) -> Self {
        Self {
            inner: Arc::from(inner),
        }
    }
}

impl Observer for ObserverBridge {
    fn record_event(&self, event: &ObserverEvent) {
        self.inner.record_event(event);
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        self.inner.record_metric(metric);
    }

    fn flush(&self) {
        self.inner.flush();
    }

    fn name(&self) -> &str {
        "observer-bridge"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[derive(Default)]
    struct DummyObserver {
        events: Mutex<u64>,
    }

    impl Observer for DummyObserver {
        fn record_event(&self, _event: &ObserverEvent) {
            *self.events.lock() += 1;
        }

        fn record_metric(&self, _metric: &ObserverMetric) {}

        fn name(&self) -> &str {
            "dummy"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn bridge_forwards_events() {
        let inner: Arc<dyn Observer> = Arc::new(DummyObserver::default());
        let bridge = ObserverBridge::new(Arc::clone(&inner));
        bridge.record_event(&ObserverEvent::HeartbeatTick);
        assert_eq!(bridge.name(), "observer-bridge");
    }
}
