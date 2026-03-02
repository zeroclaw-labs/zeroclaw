use super::traits::{Observer, ObserverEvent, ObserverMetric};
use anyhow::Context as _;
use prometheus::{
    Encoder, GaugeVec, Histogram, HistogramOpts, HistogramVec, IntCounterVec, Registry, TextEncoder,
};

/// Prometheus-backed observer — exposes metrics for scraping via `/metrics`.
pub struct PrometheusObserver {
    registry: Registry,

    // Counters
    agent_starts: IntCounterVec,
    llm_requests: IntCounterVec,
    tokens_input_total: IntCounterVec,
    tokens_output_total: IntCounterVec,
    tool_calls: IntCounterVec,
    channel_messages: IntCounterVec,
    webhook_auth_failures: IntCounterVec,
    heartbeat_ticks: prometheus::IntCounter,
    errors: IntCounterVec,

    // Histograms
    agent_duration: HistogramVec,
    tool_duration: HistogramVec,
    request_latency: Histogram,

    // Gauges
    tokens_used: prometheus::IntGauge,
    active_sessions: GaugeVec,
    queue_depth: GaugeVec,
}

impl PrometheusObserver {
    pub fn new() -> anyhow::Result<Self> {
        let registry = Registry::new();

        let agent_starts = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_agent_starts_total", "Total agent invocations"),
            &["provider", "model"],
        )
        .context("failed to create zeroclaw_agent_starts_total counter")?;

        let llm_requests = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_llm_requests_total", "Total LLM provider requests"),
            &["provider", "model", "success"],
        )
        .context("failed to create zeroclaw_llm_requests_total counter")?;

        let tokens_input_total = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_tokens_input_total", "Total input tokens consumed"),
            &["provider", "model"],
        )
        .context("failed to create zeroclaw_tokens_input_total counter")?;

        let tokens_output_total = IntCounterVec::new(
            prometheus::Opts::new(
                "zeroclaw_tokens_output_total",
                "Total output tokens consumed",
            ),
            &["provider", "model"],
        )
        .context("failed to create zeroclaw_tokens_output_total counter")?;

        let tool_calls = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_tool_calls_total", "Total tool calls"),
            &["tool", "success"],
        )
        .context("failed to create zeroclaw_tool_calls_total counter")?;

        let channel_messages = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_channel_messages_total", "Total channel messages"),
            &["channel", "direction"],
        )
        .context("failed to create zeroclaw_channel_messages_total counter")?;

        let webhook_auth_failures = IntCounterVec::new(
            prometheus::Opts::new(
                "zeroclaw_webhook_auth_failures_total",
                "Total webhook authentication failures",
            ),
            &["channel", "signature", "bearer"],
        )
        .context("failed to create zeroclaw_webhook_auth_failures_total counter")?;

        let heartbeat_ticks =
            prometheus::IntCounter::new("zeroclaw_heartbeat_ticks_total", "Total heartbeat ticks")
                .context("failed to create zeroclaw_heartbeat_ticks_total counter")?;

        let errors = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_errors_total", "Total errors by component"),
            &["component"],
        )
        .context("failed to create zeroclaw_errors_total counter")?;

        let agent_duration = HistogramVec::new(
            HistogramOpts::new(
                "zeroclaw_agent_duration_seconds",
                "Agent invocation duration in seconds",
            )
            .buckets(vec![0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0]),
            &["provider", "model"],
        )
        .context("failed to create zeroclaw_agent_duration_seconds histogram")?;

        let tool_duration = HistogramVec::new(
            HistogramOpts::new(
                "zeroclaw_tool_duration_seconds",
                "Tool execution duration in seconds",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0]),
            &["tool"],
        )
        .context("failed to create zeroclaw_tool_duration_seconds histogram")?;

        let request_latency = Histogram::with_opts(
            HistogramOpts::new(
                "zeroclaw_request_latency_seconds",
                "Request latency in seconds",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
        )
        .context("failed to create zeroclaw_request_latency_seconds histogram")?;

        let tokens_used = prometheus::IntGauge::new(
            "zeroclaw_tokens_used_last",
            "Tokens used in the last request",
        )
        .context("failed to create zeroclaw_tokens_used_last gauge")?;

        let active_sessions = GaugeVec::new(
            prometheus::Opts::new("zeroclaw_active_sessions", "Number of active sessions"),
            &[],
        )
        .context("failed to create zeroclaw_active_sessions gauge")?;

        let queue_depth = GaugeVec::new(
            prometheus::Opts::new("zeroclaw_queue_depth", "Message queue depth"),
            &[],
        )
        .context("failed to create zeroclaw_queue_depth gauge")?;

        // Register all metrics
        registry
            .register(Box::new(agent_starts.clone()))
            .context("failed to register zeroclaw_agent_starts_total counter")?;
        registry
            .register(Box::new(llm_requests.clone()))
            .context("failed to register zeroclaw_llm_requests_total counter")?;
        registry
            .register(Box::new(tokens_input_total.clone()))
            .context("failed to register zeroclaw_tokens_input_total counter")?;
        registry
            .register(Box::new(tokens_output_total.clone()))
            .context("failed to register zeroclaw_tokens_output_total counter")?;
        registry
            .register(Box::new(tool_calls.clone()))
            .context("failed to register zeroclaw_tool_calls_total counter")?;
        registry
            .register(Box::new(channel_messages.clone()))
            .context("failed to register zeroclaw_channel_messages_total counter")?;
        registry
            .register(Box::new(webhook_auth_failures.clone()))
            .context("failed to register zeroclaw_webhook_auth_failures_total counter")?;
        registry
            .register(Box::new(heartbeat_ticks.clone()))
            .context("failed to register zeroclaw_heartbeat_ticks_total counter")?;
        registry
            .register(Box::new(errors.clone()))
            .context("failed to register zeroclaw_errors_total counter")?;
        registry
            .register(Box::new(agent_duration.clone()))
            .context("failed to register zeroclaw_agent_duration_seconds histogram")?;
        registry
            .register(Box::new(tool_duration.clone()))
            .context("failed to register zeroclaw_tool_duration_seconds histogram")?;
        registry
            .register(Box::new(request_latency.clone()))
            .context("failed to register zeroclaw_request_latency_seconds histogram")?;
        registry
            .register(Box::new(tokens_used.clone()))
            .context("failed to register zeroclaw_tokens_used_last gauge")?;
        registry
            .register(Box::new(active_sessions.clone()))
            .context("failed to register zeroclaw_active_sessions gauge")?;
        registry
            .register(Box::new(queue_depth.clone()))
            .context("failed to register zeroclaw_queue_depth gauge")?;

        Ok(Self {
            registry,
            agent_starts,
            llm_requests,
            tokens_input_total,
            tokens_output_total,
            tool_calls,
            channel_messages,
            webhook_auth_failures,
            heartbeat_ticks,
            errors,
            agent_duration,
            tool_duration,
            request_latency,
            tokens_used,
            active_sessions,
            queue_depth,
        })
    }

    /// Encode all registered metrics into Prometheus text exposition format.
    pub fn encode(&self) -> String {
        let encoder = TextEncoder::new();
        let families = self.registry.gather();
        let mut buf = Vec::new();
        encoder.encode(&families, &mut buf).unwrap_or_default();
        String::from_utf8(buf).unwrap_or_default()
    }
}

impl Observer for PrometheusObserver {
    fn record_event(&self, event: &ObserverEvent) {
        match event {
            ObserverEvent::AgentStart { provider, model } => {
                self.agent_starts
                    .with_label_values(&[provider, model])
                    .inc();
            }
            ObserverEvent::AgentEnd {
                provider,
                model,
                duration,
                tokens_used,
                cost_usd: _,
            } => {
                // Agent duration is recorded via the histogram with provider/model labels
                self.agent_duration
                    .with_label_values(&[provider, model])
                    .observe(duration.as_secs_f64());
                if let Some(t) = tokens_used {
                    self.tokens_used.set(i64::try_from(*t).unwrap_or(i64::MAX));
                }
            }
            ObserverEvent::LlmResponse {
                provider,
                model,
                success,
                input_tokens,
                output_tokens,
                ..
            } => {
                let success_str = if *success { "true" } else { "false" };
                self.llm_requests
                    .with_label_values(&[provider.as_str(), model.as_str(), success_str])
                    .inc();
                if let Some(input) = input_tokens {
                    self.tokens_input_total
                        .with_label_values(&[provider.as_str(), model.as_str()])
                        .inc_by(*input);
                }
                if let Some(output) = output_tokens {
                    self.tokens_output_total
                        .with_label_values(&[provider.as_str(), model.as_str()])
                        .inc_by(*output);
                }
            }
            ObserverEvent::ToolCallStart { tool: _ }
            | ObserverEvent::TurnComplete
            | ObserverEvent::LlmRequest { .. } => {}
            ObserverEvent::ToolCall {
                tool,
                duration,
                success,
            } => {
                let success_str = if *success { "true" } else { "false" };
                self.tool_calls
                    .with_label_values(&[tool.as_str(), success_str])
                    .inc();
                self.tool_duration
                    .with_label_values(&[tool.as_str()])
                    .observe(duration.as_secs_f64());
            }
            ObserverEvent::ChannelMessage { channel, direction } => {
                self.channel_messages
                    .with_label_values(&[channel, direction])
                    .inc();
            }
            ObserverEvent::WebhookAuthFailure {
                channel,
                signature,
                bearer,
            } => {
                self.webhook_auth_failures
                    .with_label_values(&[channel, signature, bearer])
                    .inc();
            }
            ObserverEvent::HeartbeatTick => {
                self.heartbeat_ticks.inc();
            }
            ObserverEvent::Error {
                component,
                message: _,
            } => {
                self.errors.with_label_values(&[component]).inc();
            }
        }
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        match metric {
            ObserverMetric::RequestLatency(d) => {
                self.request_latency.observe(d.as_secs_f64());
            }
            ObserverMetric::TokensUsed(t) => {
                self.tokens_used.set(i64::try_from(*t).unwrap_or(i64::MAX));
            }
            ObserverMetric::ActiveSessions(s) => {
                self.active_sessions
                    .with_label_values(&[] as &[&str])
                    .set(*s as f64);
            }
            ObserverMetric::QueueDepth(d) => {
                self.queue_depth
                    .with_label_values(&[] as &[&str])
                    .set(*d as f64);
            }
        }
    }

    fn name(&self) -> &str {
        "prometheus"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_observer() -> PrometheusObserver {
        PrometheusObserver::new().expect("prometheus observer should initialize in tests")
    }

    #[test]
    fn prometheus_observer_name() {
        assert_eq!(test_observer().name(), "prometheus");
    }

    #[test]
    fn records_all_events_without_panic() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(500),
            tokens_used: Some(100),
            cost_usd: None,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::ZERO,
            tokens_used: None,
            cost_usd: None,
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
        obs.record_event(&ObserverEvent::WebhookAuthFailure {
            channel: "wati".into(),
            signature: "invalid".into(),
            bearer: "missing".into(),
        });
        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_event(&ObserverEvent::Error {
            component: "provider".into(),
            message: "timeout".into(),
        });
    }

    #[test]
    fn records_all_metrics_without_panic() {
        let obs = test_observer();
        obs.record_metric(&ObserverMetric::RequestLatency(Duration::from_secs(2)));
        obs.record_metric(&ObserverMetric::TokensUsed(500));
        obs.record_metric(&ObserverMetric::TokensUsed(0));
        obs.record_metric(&ObserverMetric::ActiveSessions(3));
        obs.record_metric(&ObserverMetric::QueueDepth(42));
    }

    #[test]
    fn encode_produces_prometheus_text_format() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(100),
            success: true,
        });
        obs.record_event(&ObserverEvent::WebhookAuthFailure {
            channel: "wati".into(),
            signature: "invalid".into(),
            bearer: "missing".into(),
        });
        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_metric(&ObserverMetric::RequestLatency(Duration::from_millis(250)));

        let output = obs.encode();
        assert!(output.contains("zeroclaw_agent_starts_total"));
        assert!(output.contains("zeroclaw_tool_calls_total"));
        assert!(output.contains("zeroclaw_webhook_auth_failures_total"));
        assert!(output.contains("zeroclaw_heartbeat_ticks_total"));
        assert!(output.contains("zeroclaw_request_latency_seconds"));
    }

    #[test]
    fn counters_increment_correctly() {
        let obs = test_observer();

        for _ in 0..3 {
            obs.record_event(&ObserverEvent::HeartbeatTick);
        }

        let output = obs.encode();
        assert!(output.contains("zeroclaw_heartbeat_ticks_total 3"));
    }

    #[test]
    fn tool_calls_track_success_and_failure_separately() {
        let obs = test_observer();

        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(10),
            success: true,
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

        let output = obs.encode();
        assert!(output.contains(r#"zeroclaw_tool_calls_total{success="true",tool="shell"} 2"#));
        assert!(output.contains(r#"zeroclaw_tool_calls_total{success="false",tool="shell"} 1"#));
    }

    #[test]
    fn errors_track_by_component() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::Error {
            component: "provider".into(),
            message: "timeout".into(),
        });
        obs.record_event(&ObserverEvent::Error {
            component: "provider".into(),
            message: "rate limit".into(),
        });
        obs.record_event(&ObserverEvent::Error {
            component: "channels".into(),
            message: "disconnected".into(),
        });

        let output = obs.encode();
        assert!(output.contains(r#"zeroclaw_errors_total{component="provider"} 2"#));
        assert!(output.contains(r#"zeroclaw_errors_total{component="channels"} 1"#));
    }

    #[test]
    fn gauge_reflects_latest_value() {
        let obs = test_observer();
        obs.record_metric(&ObserverMetric::TokensUsed(100));
        obs.record_metric(&ObserverMetric::TokensUsed(200));

        let output = obs.encode();
        assert!(output.contains("zeroclaw_tokens_used_last 200"));
    }

    #[test]
    fn llm_response_tracks_request_count_and_tokens() {
        let obs = test_observer();

        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(200),
            success: true,
            error_message: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(300),
            success: true,
            error_message: None,
            input_tokens: Some(200),
            output_tokens: Some(80),
        });

        let output = obs.encode();
        assert!(output.contains(
            r#"zeroclaw_llm_requests_total{model="claude-sonnet",provider="openrouter",success="true"} 2"#
        ));
        assert!(output.contains(
            r#"zeroclaw_tokens_input_total{model="claude-sonnet",provider="openrouter"} 300"#
        ));
        assert!(output.contains(
            r#"zeroclaw_tokens_output_total{model="claude-sonnet",provider="openrouter"} 130"#
        ));
    }

    #[test]
    fn llm_response_without_tokens_increments_request_only() {
        let obs = test_observer();

        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "ollama".into(),
            model: "llama3".into(),
            duration: Duration::from_millis(100),
            success: false,
            error_message: Some("timeout".into()),
            input_tokens: None,
            output_tokens: None,
        });

        let output = obs.encode();
        assert!(output.contains(
            r#"zeroclaw_llm_requests_total{model="llama3",provider="ollama",success="false"} 1"#
        ));
        // Token counters should not appear (no data recorded)
        assert!(!output.contains("zeroclaw_tokens_input_total{"));
        assert!(!output.contains("zeroclaw_tokens_output_total{"));
    }
}
