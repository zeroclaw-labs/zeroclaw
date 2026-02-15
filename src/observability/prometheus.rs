use super::traits::{Observer, ObserverEvent, ObserverMetric};
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;
use std::sync::Mutex;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct AgentLabels {
    provider: String,
    model: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ToolLabels {
    tool: String,
    success: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ChannelLabels {
    channel: String,
    direction: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ErrorLabels {
    component: String,
}

pub struct PrometheusObserver {
    registry: Mutex<Registry>,
    agent_starts: Family<AgentLabels, Counter>,
    agent_completions: Counter,
    agent_duration_seconds: Histogram,
    tool_calls: Family<ToolLabels, Counter>,
    tool_duration_seconds: Histogram,
    channel_messages: Family<ChannelLabels, Counter>,
    heartbeat_ticks: Counter,
    errors: Family<ErrorLabels, Counter>,
    request_latency_seconds: Histogram,
    tokens_used_total: Counter,
    active_sessions: Gauge,
    queue_depth: Gauge,
}

const LATENCY_BUCKETS: [f64; 11] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];
const DURATION_BUCKETS: [f64; 11] = [0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0];
const TOOL_BUCKETS: [f64; 9] = [0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0];

impl PrometheusObserver {
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let agent_starts = Family::<AgentLabels, Counter>::default();
        registry.register("zeroclaw_agent_starts", "Agent start events", agent_starts.clone());

        let agent_completions = Counter::default();
        registry.register("zeroclaw_agent_completions", "Agent completions", agent_completions.clone());

        let agent_duration_seconds = Histogram::new(DURATION_BUCKETS.into_iter());
        registry.register(
            "zeroclaw_agent_duration_seconds",
            "Agent call duration in seconds",
            agent_duration_seconds.clone(),
        );

        let tool_calls = Family::<ToolLabels, Counter>::default();
        registry.register("zeroclaw_tool_calls", "Tool call events", tool_calls.clone());

        let tool_duration_seconds = Histogram::new(TOOL_BUCKETS.into_iter());
        registry.register(
            "zeroclaw_tool_duration_seconds",
            "Tool call duration in seconds",
            tool_duration_seconds.clone(),
        );

        let channel_messages = Family::<ChannelLabels, Counter>::default();
        registry.register("zeroclaw_channel_messages", "Channel message events", channel_messages.clone());

        let heartbeat_ticks = Counter::default();
        registry.register("zeroclaw_heartbeat_ticks", "Heartbeat tick count", heartbeat_ticks.clone());

        let errors = Family::<ErrorLabels, Counter>::default();
        registry.register("zeroclaw_errors", "Error events by component", errors.clone());

        let request_latency_seconds = Histogram::new(LATENCY_BUCKETS.into_iter());
        registry.register(
            "zeroclaw_request_latency_seconds",
            "Request latency in seconds",
            request_latency_seconds.clone(),
        );

        let tokens_used_total = Counter::default();
        registry.register("zeroclaw_tokens_used_total", "Total tokens consumed", tokens_used_total.clone());

        let active_sessions: Gauge = Gauge::default();
        registry.register("zeroclaw_active_sessions", "Active session count", active_sessions.clone());

        let queue_depth: Gauge = Gauge::default();
        registry.register("zeroclaw_queue_depth", "Current queue depth", queue_depth.clone());

        Self {
            registry: Mutex::new(registry),
            agent_starts,
            agent_completions,
            agent_duration_seconds,
            tool_calls,
            tool_duration_seconds,
            channel_messages,
            heartbeat_ticks,
            errors,
            request_latency_seconds,
            tokens_used_total,
            active_sessions,
            queue_depth,
        }
    }

    pub fn encode(&self) -> String {
        let mut buf = String::new();
        let reg = self.registry.lock().expect("prometheus registry poisoned");
        encode(&mut buf, &reg).expect("prometheus text encoding failed");
        buf
    }
}

impl Observer for PrometheusObserver {
    fn record_event(&self, event: &ObserverEvent) {
        match event {
            ObserverEvent::AgentStart { provider, model } => {
                self.agent_starts
                    .get_or_create(&AgentLabels {
                        provider: provider.clone(),
                        model: model.clone(),
                    })
                    .inc();
            }
            ObserverEvent::AgentEnd {
                duration,
                tokens_used,
            } => {
                self.agent_completions.inc();
                self.agent_duration_seconds.observe(duration.as_secs_f64());
                if let Some(t) = tokens_used {
                    self.tokens_used_total.inc_by(*t);
                }
            }
            ObserverEvent::ToolCall {
                tool,
                duration,
                success,
            } => {
                self.tool_calls
                    .get_or_create(&ToolLabels {
                        tool: tool.clone(),
                        success: success.to_string(),
                    })
                    .inc();
                self.tool_duration_seconds.observe(duration.as_secs_f64());
            }
            ObserverEvent::ChannelMessage { channel, direction } => {
                self.channel_messages
                    .get_or_create(&ChannelLabels {
                        channel: channel.clone(),
                        direction: direction.clone(),
                    })
                    .inc();
            }
            ObserverEvent::HeartbeatTick => {
                self.heartbeat_ticks.inc();
            }
            ObserverEvent::Error {
                component,
                message: _,
            } => {
                self.errors
                    .get_or_create(&ErrorLabels {
                        component: component.clone(),
                    })
                    .inc();
            }
        }
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        match metric {
            ObserverMetric::RequestLatency(d) => {
                self.request_latency_seconds.observe(d.as_secs_f64());
            }
            ObserverMetric::TokensUsed(t) => {
                self.tokens_used_total.inc_by(*t);
            }
            ObserverMetric::ActiveSessions(s) => {
                self.active_sessions
                    .set(i64::try_from(*s).unwrap_or(i64::MAX));
            }
            ObserverMetric::QueueDepth(d) => {
                self.queue_depth
                    .set(i64::try_from(*d).unwrap_or(i64::MAX));
            }
        }
    }

    fn name(&self) -> &str {
        "prometheus"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn name() {
        assert_eq!(PrometheusObserver::new().name(), "prometheus");
    }

    #[test]
    fn records_all_events_without_panic() {
        let obs = PrometheusObserver::new();
        obs.record_event(&ObserverEvent::AgentStart {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            duration: Duration::from_millis(500),
            tokens_used: Some(100),
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            duration: Duration::ZERO,
            tokens_used: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(10),
            success: true,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "file_read".into(),
            duration: Duration::from_millis(5),
            success: false,
        });
        obs.record_event(&ObserverEvent::ChannelMessage {
            channel: "telegram".into(),
            direction: "inbound".into(),
        });
        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_event(&ObserverEvent::Error {
            component: "provider".into(),
            message: "timeout".into(),
        });
    }

    #[test]
    fn records_all_metrics_without_panic() {
        let obs = PrometheusObserver::new();
        obs.record_metric(&ObserverMetric::RequestLatency(Duration::from_secs(2)));
        obs.record_metric(&ObserverMetric::TokensUsed(500));
        obs.record_metric(&ObserverMetric::ActiveSessions(3));
        obs.record_metric(&ObserverMetric::QueueDepth(10));
    }

    #[test]
    fn encode_produces_valid_output() {
        let obs = PrometheusObserver::new();
        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_metric(&ObserverMetric::TokensUsed(42));

        let output = obs.encode();
        assert!(!output.is_empty());
        assert!(output.contains("zeroclaw_heartbeat_ticks"));
        assert!(output.contains("zeroclaw_tokens_used_total"));
    }

    #[test]
    fn flush_does_not_panic() {
        PrometheusObserver::new().flush();
    }
}
