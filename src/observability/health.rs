use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
use parking_lot::Mutex;
use std::time::{Duration, Instant};

const ERROR_WINDOW: Duration = Duration::from_secs(60);
const ERROR_THRESHOLD: u64 = 5;
const CONSECUTIVE_FAILURE_THRESHOLD: u64 = 3;
const QUEUE_DEPTH_SPIKE_THRESHOLD: u64 = 1000;
const LATENCY_SPIKE_THRESHOLD: Duration = Duration::from_secs(30);

#[derive(Default)]
struct HealthState {
    error_count: u64,
    last_error_time: Option<Instant>,
    window_start: Option<Instant>,
    consecutive_failures: u64,
    repair_triggers: u64,
    repair_needed: bool,
}

pub struct SelfHealthObserver {
    state: Mutex<HealthState>,
}

impl SelfHealthObserver {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HealthState::default()),
        }
    }

    pub fn repair_needed(&self) -> bool {
        self.state.lock().repair_needed
    }

    pub fn repair_triggers(&self) -> u64 {
        self.state.lock().repair_triggers
    }

    fn check_thresholds(state: &mut HealthState) {
        let errors_in_window = if let Some(start) = state.window_start {
            if start.elapsed() <= ERROR_WINDOW {
                state.error_count >= ERROR_THRESHOLD
            } else {
                state.error_count = 1;
                state.window_start = Some(Instant::now());
                false
            }
        } else {
            false
        };

        let consecutive = state.consecutive_failures >= CONSECUTIVE_FAILURE_THRESHOLD;

        if (errors_in_window || consecutive) && !state.repair_needed {
            state.repair_needed = true;
            state.repair_triggers += 1;
        }
    }
}

impl Observer for SelfHealthObserver {
    fn record_event(&self, event: &ObserverEvent) {
        let mut state = self.state.lock();

        match event {
            ObserverEvent::LlmResponse { success: true, .. }
            | ObserverEvent::ToolCall { success: true, .. } => {
                state.consecutive_failures = 0;
            }
            ObserverEvent::Error { .. }
            | ObserverEvent::LlmResponse { success: false, .. }
            | ObserverEvent::ToolCall { success: false, .. } => {
                let now = Instant::now();
                if state.window_start.is_none() {
                    state.window_start = Some(now);
                }
                state.error_count += 1;
                state.last_error_time = Some(now);
                state.consecutive_failures += 1;

                Self::check_thresholds(&mut state);
            }
            _ => {}
        }
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        let mut state = self.state.lock();

        match metric {
            ObserverMetric::QueueDepth(depth) if *depth >= QUEUE_DEPTH_SPIKE_THRESHOLD => {
                if !state.repair_needed {
                    state.repair_needed = true;
                    state.repair_triggers += 1;
                }
            }
            ObserverMetric::RequestLatency(latency) if *latency >= LATENCY_SPIKE_THRESHOLD => {
                if !state.repair_needed {
                    state.repair_needed = true;
                    state.repair_triggers += 1;
                }
            }
            _ => {}
        }
    }

    fn flush(&self) {
        let state = self.state.lock();
        if state.repair_needed {
            tracing::warn!(
                error_count = state.error_count,
                consecutive_failures = state.consecutive_failures,
                repair_triggers = state.repair_triggers,
                "SelfHealthObserver: repair suggested — anomaly thresholds exceeded"
            );
        }
    }

    fn name(&self) -> &str {
        "self_health"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn no_repair_on_few_errors() {
        let obs = SelfHealthObserver::new();

        for _ in 0..2 {
            obs.record_event(&ObserverEvent::Error {
                component: "test".into(),
                message: "err".into(),
            });
        }

        assert!(!obs.repair_needed());
    }

    #[test]
    fn repair_triggered_on_error_threshold() {
        let obs = SelfHealthObserver::new();

        for _ in 0..5 {
            obs.record_event(&ObserverEvent::Error {
                component: "test".into(),
                message: "err".into(),
            });
        }

        assert!(obs.repair_needed());
        assert_eq!(obs.repair_triggers(), 1);
    }

    #[test]
    fn repair_triggered_on_consecutive_failures() {
        let obs = SelfHealthObserver::new();

        for _ in 0..3 {
            obs.record_event(&ObserverEvent::ToolCall {
                tool: "shell".into(),
                duration: Duration::from_millis(10),
                success: false,
            });
        }

        assert!(obs.repair_needed());
    }

    #[test]
    fn success_resets_consecutive_failures() {
        let obs = SelfHealthObserver::new();

        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(10),
            success: false,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(10),
            success: false,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(10),
            success: true,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(10),
            success: false,
        });

        assert!(!obs.repair_needed());
    }

    #[test]
    fn queue_depth_spike_triggers_repair() {
        let obs = SelfHealthObserver::new();

        obs.record_metric(&ObserverMetric::QueueDepth(999));
        assert!(!obs.repair_needed());

        obs.record_metric(&ObserverMetric::QueueDepth(1000));
        assert!(obs.repair_needed());
    }

    #[test]
    fn high_latency_triggers_repair() {
        let obs = SelfHealthObserver::new();

        obs.record_metric(&ObserverMetric::RequestLatency(Duration::from_secs(29)));
        assert!(!obs.repair_needed());

        obs.record_metric(&ObserverMetric::RequestLatency(Duration::from_secs(30)));
        assert!(obs.repair_needed());
    }

    #[test]
    fn flush_does_not_panic() {
        let obs = SelfHealthObserver::new();
        obs.flush();

        for _ in 0..6 {
            obs.record_event(&ObserverEvent::Error {
                component: "test".into(),
                message: "err".into(),
            });
        }
        obs.flush();
    }

    #[test]
    fn name_returns_self_health() {
        let obs = SelfHealthObserver::new();
        assert_eq!(obs.name(), "self_health");
    }

    #[test]
    fn as_any_downcasts() {
        let obs = SelfHealthObserver::new();
        assert!(obs.as_any().downcast_ref::<SelfHealthObserver>().is_some());
    }
}
