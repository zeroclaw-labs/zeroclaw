pub use zeroclaw_api::observability_traits::*;

#[allow(unused_imports)]
pub use async_trait::async_trait;

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::time::Duration;

    #[derive(Default)]
    struct DummyObserver {
        events: Mutex<u64>,
        metrics: Mutex<u64>,
    }

    impl Observer for DummyObserver {
        fn record_event(&self, _event: &ObserverEvent) {
            let mut guard = self.events.lock();
            *guard += 1;
        }

        fn record_metric(&self, _metric: &ObserverMetric) {
            let mut guard = self.metrics.lock();
            *guard += 1;
        }

        fn name(&self) -> &str {
            "dummy-observer"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn observer_records_events_and_metrics() {
        let observer = DummyObserver::default();

        observer.record_event(&ObserverEvent::HeartbeatTick);
        observer.record_event(&ObserverEvent::Error {
            component: "test".into(),
            message: "boom".into(),
        });
        observer.record_metric(&ObserverMetric::TokensUsed(42));

        assert_eq!(*observer.events.lock(), 2);
        assert_eq!(*observer.metrics.lock(), 1);
    }

    #[test]
    fn observer_default_flush_and_as_any_work() {
        let observer = DummyObserver::default();

        observer.flush();
        assert_eq!(observer.name(), "dummy-observer");
        assert!(observer.as_any().downcast_ref::<DummyObserver>().is_some());
    }

    #[test]
    fn observer_event_and_metric_are_cloneable() {
        let event = ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(10),
            success: true,
        };
        let metric = ObserverMetric::RequestLatency(Duration::from_millis(8));

        let cloned_event = event.clone();
        let cloned_metric = metric.clone();

        assert!(matches!(cloned_event, ObserverEvent::ToolCall { .. }));
        assert!(matches!(cloned_metric, ObserverMetric::RequestLatency(_)));
    }

    #[test]
    fn hand_events_recordable() {
        let observer = DummyObserver::default();

        observer.record_event(&ObserverEvent::HandStarted {
            hand_name: "review".into(),
        });
        observer.record_event(&ObserverEvent::HandCompleted {
            hand_name: "review".into(),
            duration_ms: 1500,
            findings_count: 3,
        });
        observer.record_event(&ObserverEvent::HandFailed {
            hand_name: "review".into(),
            error: "timeout".into(),
            duration_ms: 5000,
        });

        assert_eq!(*observer.events.lock(), 3);
    }

    #[test]
    fn hand_metrics_recordable() {
        let observer = DummyObserver::default();

        observer.record_metric(&ObserverMetric::HandRunDuration {
            hand_name: "review".into(),
            duration: Duration::from_millis(1500),
        });
        observer.record_metric(&ObserverMetric::HandFindingsCount {
            hand_name: "review".into(),
            count: 3,
        });
        observer.record_metric(&ObserverMetric::HandSuccessRate {
            hand_name: "review".into(),
            success: true,
        });

        assert_eq!(*observer.metrics.lock(), 3);
    }

    #[test]
    fn hand_event_and_metric_are_cloneable() {
        let event = ObserverEvent::HandCompleted {
            hand_name: "review".into(),
            duration_ms: 500,
            findings_count: 2,
        };
        let metric = ObserverMetric::HandRunDuration {
            hand_name: "review".into(),
            duration: Duration::from_millis(500),
        };

        let cloned_event = event.clone();
        let cloned_metric = metric.clone();

        assert!(matches!(cloned_event, ObserverEvent::HandCompleted { .. }));
        assert!(matches!(
            cloned_metric,
            ObserverMetric::HandRunDuration { .. }
        ));
    }
}
