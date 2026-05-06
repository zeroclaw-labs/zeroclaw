use super::traits::{Observer, ObserverEvent, ObserverMetric};
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
    heartbeat_ticks: prometheus::IntCounter,
    errors: IntCounterVec,
    cache_hits: IntCounterVec,
    cache_misses: IntCounterVec,
    cache_tokens_saved: IntCounterVec,

    // Histograms
    agent_duration: HistogramVec,
    tool_duration: HistogramVec,
    request_latency: Histogram,

    // Gauges
    tokens_used: prometheus::IntGauge,
    active_sessions: GaugeVec,
    queue_depth: GaugeVec,

    // Hands
    hand_runs: IntCounterVec,
    hand_duration: HistogramVec,
    hand_findings: IntCounterVec,

    // DORA
    deployments_total: IntCounterVec,
    deployment_lead_time: Histogram,
    deployment_failure_rate: prometheus::Gauge,
    recovery_time: Histogram,
    mttr: prometheus::Gauge,
    deploy_success_count: std::sync::atomic::AtomicU64,
    deploy_failure_count: std::sync::atomic::AtomicU64,
}

impl Default for PrometheusObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl PrometheusObserver {
    pub fn new() -> Self {
        let registry = Registry::new();

        let agent_starts = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_agent_starts_total", "Total agent invocations"),
            &["model_provider", "model"],
        )
        .expect("valid metric");

        let llm_requests = IntCounterVec::new(
            prometheus::Opts::new(
                "zeroclaw_llm_requests_total",
                "Total LLM model_provider requests",
            ),
            &["model_provider", "model", "success"],
        )
        .expect("valid metric");

        let tokens_input_total = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_tokens_input_total", "Total input tokens consumed"),
            &["model_provider", "model"],
        )
        .expect("valid metric");

        let tokens_output_total = IntCounterVec::new(
            prometheus::Opts::new(
                "zeroclaw_tokens_output_total",
                "Total output tokens consumed",
            ),
            &["model_provider", "model"],
        )
        .expect("valid metric");

        let tool_calls = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_tool_calls_total", "Total tool calls"),
            &["tool", "success"],
        )
        .expect("valid metric");

        let channel_messages = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_channel_messages_total", "Total channel messages"),
            &["channel", "direction"],
        )
        .expect("valid metric");

        let heartbeat_ticks =
            prometheus::IntCounter::new("zeroclaw_heartbeat_ticks_total", "Total heartbeat ticks")
                .expect("valid metric");

        let errors = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_errors_total", "Total errors by component"),
            &["component"],
        )
        .expect("valid metric");

        let cache_hits = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_cache_hits_total", "Total response cache hits"),
            &["cache_type"],
        )
        .expect("valid metric");

        let cache_misses = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_cache_misses_total", "Total response cache misses"),
            &["cache_type"],
        )
        .expect("valid metric");

        let cache_tokens_saved = IntCounterVec::new(
            prometheus::Opts::new(
                "zeroclaw_cache_tokens_saved_total",
                "Total tokens saved by response cache",
            ),
            &["cache_type"],
        )
        .expect("valid metric");

        let agent_duration = HistogramVec::new(
            HistogramOpts::new(
                "zeroclaw_agent_duration_seconds",
                "Agent invocation duration in seconds",
            )
            .buckets(vec![0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0]),
            &["model_provider", "model"],
        )
        .expect("valid metric");

        let tool_duration = HistogramVec::new(
            HistogramOpts::new(
                "zeroclaw_tool_duration_seconds",
                "Tool execution duration in seconds",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0]),
            &["tool"],
        )
        .expect("valid metric");

        let request_latency = Histogram::with_opts(
            HistogramOpts::new(
                "zeroclaw_request_latency_seconds",
                "Request latency in seconds",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
        )
        .expect("valid metric");

        let tokens_used = prometheus::IntGauge::new(
            "zeroclaw_tokens_used_last",
            "Tokens used in the last request",
        )
        .expect("valid metric");

        let active_sessions = GaugeVec::new(
            prometheus::Opts::new("zeroclaw_active_sessions", "Number of active sessions"),
            &[],
        )
        .expect("valid metric");

        let queue_depth = GaugeVec::new(
            prometheus::Opts::new("zeroclaw_queue_depth", "Message queue depth"),
            &[],
        )
        .expect("valid metric");

        let hand_runs = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_hand_runs_total", "Total hand runs by outcome"),
            &["hand", "success"],
        )
        .expect("valid metric");

        let hand_duration = HistogramVec::new(
            HistogramOpts::new(
                "zeroclaw_hand_duration_seconds",
                "Hand run duration in seconds",
            )
            .buckets(vec![0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0]),
            &["hand"],
        )
        .expect("valid metric");

        let hand_findings = IntCounterVec::new(
            prometheus::Opts::new(
                "zeroclaw_hand_findings_total",
                "Total findings produced by hand runs",
            ),
            &["hand"],
        )
        .expect("valid metric");

        let deployments_total = IntCounterVec::new(
            prometheus::Opts::new("zeroclaw_deployments_total", "Total deployments by status"),
            &["status"],
        )
        .expect("valid metric");

        let deployment_lead_time = Histogram::with_opts(
            HistogramOpts::new(
                "zeroclaw_deployment_lead_time_seconds",
                "Deployment lead time from commit to deploy in seconds",
            )
            .buckets(vec![
                60.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0, 14400.0, 43200.0, 86400.0,
            ]),
        )
        .expect("valid metric");

        let deployment_failure_rate = prometheus::Gauge::new(
            "zeroclaw_deployment_failure_rate",
            "Ratio of failed deployments to total deployments",
        )
        .expect("valid metric");

        let recovery_time = Histogram::with_opts(
            HistogramOpts::new(
                "zeroclaw_recovery_time_seconds",
                "Time to recover from a failed deployment in seconds",
            )
            .buckets(vec![
                60.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0, 14400.0, 43200.0, 86400.0,
            ]),
        )
        .expect("valid metric");

        let mttr =
            prometheus::Gauge::new("zeroclaw_mttr_seconds", "Mean time to recovery in seconds")
                .expect("valid metric");

        // Register all metrics
        registry.register(Box::new(agent_starts.clone())).ok();
        registry.register(Box::new(llm_requests.clone())).ok();
        registry.register(Box::new(tokens_input_total.clone())).ok();
        registry
            .register(Box::new(tokens_output_total.clone()))
            .ok();
        registry.register(Box::new(tool_calls.clone())).ok();
        registry.register(Box::new(channel_messages.clone())).ok();
        registry.register(Box::new(heartbeat_ticks.clone())).ok();
        registry.register(Box::new(errors.clone())).ok();
        registry.register(Box::new(cache_hits.clone())).ok();
        registry.register(Box::new(cache_misses.clone())).ok();
        registry.register(Box::new(cache_tokens_saved.clone())).ok();
        registry.register(Box::new(agent_duration.clone())).ok();
        registry.register(Box::new(tool_duration.clone())).ok();
        registry.register(Box::new(request_latency.clone())).ok();
        registry.register(Box::new(tokens_used.clone())).ok();
        registry.register(Box::new(active_sessions.clone())).ok();
        registry.register(Box::new(queue_depth.clone())).ok();
        registry.register(Box::new(hand_runs.clone())).ok();
        registry.register(Box::new(hand_duration.clone())).ok();
        registry.register(Box::new(hand_findings.clone())).ok();
        registry.register(Box::new(deployments_total.clone())).ok();
        registry
            .register(Box::new(deployment_lead_time.clone()))
            .ok();
        registry
            .register(Box::new(deployment_failure_rate.clone()))
            .ok();
        registry.register(Box::new(recovery_time.clone())).ok();
        registry.register(Box::new(mttr.clone())).ok();

        Self {
            registry,
            agent_starts,
            llm_requests,
            tokens_input_total,
            tokens_output_total,
            tool_calls,
            channel_messages,
            heartbeat_ticks,
            errors,
            cache_hits,
            cache_misses,
            cache_tokens_saved,
            agent_duration,
            tool_duration,
            request_latency,
            tokens_used,
            active_sessions,
            queue_depth,
            hand_runs,
            hand_duration,
            hand_findings,
            deployments_total,
            deployment_lead_time,
            deployment_failure_rate,
            recovery_time,
            mttr,
            deploy_success_count: std::sync::atomic::AtomicU64::new(0),
            deploy_failure_count: std::sync::atomic::AtomicU64::new(0),
        }
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
            ObserverEvent::AgentStart {
                model_provider,
                model,
            } => {
                self.agent_starts
                    .with_label_values(&[model_provider, model])
                    .inc();
            }
            ObserverEvent::AgentEnd {
                model_provider,
                model,
                duration,
                tokens_used,
                cost_usd: _,
            } => {
                // Agent duration is recorded via the histogram with model_provider/model labels
                self.agent_duration
                    .with_label_values(&[model_provider, model])
                    .observe(duration.as_secs_f64());
                if let Some(t) = tokens_used {
                    self.tokens_used.set(i64::try_from(*t).unwrap_or(i64::MAX));
                }
            }
            ObserverEvent::LlmResponse {
                model_provider,
                model,
                success,
                input_tokens,
                output_tokens,
                ..
            } => {
                let success_str = if *success { "true" } else { "false" };
                self.llm_requests
                    .with_label_values(&[model_provider.as_str(), model.as_str(), success_str])
                    .inc();
                if let Some(input) = input_tokens {
                    self.tokens_input_total
                        .with_label_values(&[model_provider.as_str(), model.as_str()])
                        .inc_by(*input);
                }
                if let Some(output) = output_tokens {
                    self.tokens_output_total
                        .with_label_values(&[model_provider.as_str(), model.as_str()])
                        .inc_by(*output);
                }
            }
            ObserverEvent::ToolCallStart { .. }
            | ObserverEvent::TurnComplete
            | ObserverEvent::LlmRequest { .. }
            | ObserverEvent::DeploymentStarted { .. }
            | ObserverEvent::RecoveryCompleted { .. } => {}
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
            ObserverEvent::HeartbeatTick => {
                self.heartbeat_ticks.inc();
            }
            ObserverEvent::CacheHit {
                cache_type,
                tokens_saved,
            } => {
                self.cache_hits.with_label_values(&[cache_type]).inc();
                self.cache_tokens_saved
                    .with_label_values(&[cache_type])
                    .inc_by(*tokens_saved);
            }
            ObserverEvent::CacheMiss { cache_type } => {
                self.cache_misses.with_label_values(&[cache_type]).inc();
            }
            ObserverEvent::Error {
                component,
                message: _,
            } => {
                self.errors.with_label_values(&[component]).inc();
            }
            ObserverEvent::HandStarted { hand_name } => {
                self.hand_runs
                    .with_label_values(&[hand_name.as_str(), "true"])
                    .inc_by(0); // touch the series so it appears in output
            }
            ObserverEvent::HandCompleted {
                hand_name,
                duration_ms,
                findings_count,
            } => {
                self.hand_runs
                    .with_label_values(&[hand_name.as_str(), "true"])
                    .inc();
                self.hand_duration
                    .with_label_values(&[hand_name.as_str()])
                    .observe(*duration_ms as f64 / 1000.0);
                self.hand_findings
                    .with_label_values(&[hand_name.as_str()])
                    .inc_by(*findings_count as u64);
            }
            ObserverEvent::HandFailed {
                hand_name,
                duration_ms,
                ..
            } => {
                self.hand_runs
                    .with_label_values(&[hand_name.as_str(), "false"])
                    .inc();
                self.hand_duration
                    .with_label_values(&[hand_name.as_str()])
                    .observe(*duration_ms as f64 / 1000.0);
            }
            ObserverEvent::DeploymentCompleted { .. } => {
                self.deployments_total.with_label_values(&["success"]).inc();
                let s = self
                    .deploy_success_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1;
                let f = self
                    .deploy_failure_count
                    .load(std::sync::atomic::Ordering::Relaxed);
                let total = s + f;
                if total > 0 {
                    self.deployment_failure_rate.set(f as f64 / total as f64);
                }
            }
            ObserverEvent::DeploymentFailed { .. } => {
                self.deployments_total.with_label_values(&["failure"]).inc();
                let f = self
                    .deploy_failure_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1;
                let s = self
                    .deploy_success_count
                    .load(std::sync::atomic::Ordering::Relaxed);
                let total = s + f;
                if total > 0 {
                    self.deployment_failure_rate.set(f as f64 / total as f64);
                }
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
            ObserverMetric::HandRunDuration {
                hand_name,
                duration,
            } => {
                self.hand_duration
                    .with_label_values(&[hand_name.as_str()])
                    .observe(duration.as_secs_f64());
            }
            ObserverMetric::HandFindingsCount { hand_name, count } => {
                self.hand_findings
                    .with_label_values(&[hand_name.as_str()])
                    .inc_by(*count);
            }
            ObserverMetric::HandSuccessRate { hand_name, success } => {
                let success_str = if *success { "true" } else { "false" };
                self.hand_runs
                    .with_label_values(&[hand_name.as_str(), success_str])
                    .inc();
            }
            ObserverMetric::DeploymentLeadTime(d) => {
                self.deployment_lead_time.observe(d.as_secs_f64());
            }
            ObserverMetric::RecoveryTime(d) => {
                self.recovery_time.observe(d.as_secs_f64());
                self.mttr.set(d.as_secs_f64());
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

    #[test]
    fn prometheus_observer_name() {
        assert_eq!(PrometheusObserver::new().name(), "prometheus");
    }

    #[test]
    fn records_all_events_without_panic() {
        let obs = PrometheusObserver::new();
        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(500),
            tokens_used: Some(100),
            cost_usd: None,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openrouter".into(),
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
        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_event(&ObserverEvent::Error {
            component: "model_provider".into(),
            message: "timeout".into(),
        });
    }

    #[test]
    fn records_all_metrics_without_panic() {
        let obs = PrometheusObserver::new();
        obs.record_metric(&ObserverMetric::RequestLatency(Duration::from_secs(2)));
        obs.record_metric(&ObserverMetric::TokensUsed(500));
        obs.record_metric(&ObserverMetric::TokensUsed(0));
        obs.record_metric(&ObserverMetric::ActiveSessions(3));
        obs.record_metric(&ObserverMetric::QueueDepth(42));
    }

    #[test]
    fn encode_produces_prometheus_text_format() {
        let obs = PrometheusObserver::new();
        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(100),
            success: true,
        });
        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_metric(&ObserverMetric::RequestLatency(Duration::from_millis(250)));

        let output = obs.encode();
        assert!(output.contains("zeroclaw_agent_starts_total"));
        assert!(output.contains("zeroclaw_tool_calls_total"));
        assert!(output.contains("zeroclaw_heartbeat_ticks_total"));
        assert!(output.contains("zeroclaw_request_latency_seconds"));
    }

    #[test]
    fn counters_increment_correctly() {
        let obs = PrometheusObserver::new();

        for _ in 0..3 {
            obs.record_event(&ObserverEvent::HeartbeatTick);
        }

        let output = obs.encode();
        assert!(output.contains("zeroclaw_heartbeat_ticks_total 3"));
    }

    #[test]
    fn tool_calls_track_success_and_failure_separately() {
        let obs = PrometheusObserver::new();

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
        let obs = PrometheusObserver::new();
        obs.record_event(&ObserverEvent::Error {
            component: "model_provider".into(),
            message: "timeout".into(),
        });
        obs.record_event(&ObserverEvent::Error {
            component: "model_provider".into(),
            message: "rate limit".into(),
        });
        obs.record_event(&ObserverEvent::Error {
            component: "channels".into(),
            message: "disconnected".into(),
        });

        let output = obs.encode();
        assert!(output.contains(r#"zeroclaw_errors_total{component="model_provider"} 2"#));
        assert!(output.contains(r#"zeroclaw_errors_total{component="channels"} 1"#));
    }

    #[test]
    fn gauge_reflects_latest_value() {
        let obs = PrometheusObserver::new();
        obs.record_metric(&ObserverMetric::TokensUsed(100));
        obs.record_metric(&ObserverMetric::TokensUsed(200));

        let output = obs.encode();
        assert!(output.contains("zeroclaw_tokens_used_last 200"));
    }

    #[test]
    fn llm_response_tracks_request_count_and_tokens() {
        let obs = PrometheusObserver::new();

        obs.record_event(&ObserverEvent::LlmResponse {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(200),
            success: true,
            error_message: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(300),
            success: true,
            error_message: None,
            input_tokens: Some(200),
            output_tokens: Some(80),
        });

        let output = obs.encode();
        assert!(output.contains(
            r#"zeroclaw_llm_requests_total{model="claude-sonnet",model_provider="openrouter",success="true"} 2"#
        ));
        assert!(output.contains(
            r#"zeroclaw_tokens_input_total{model="claude-sonnet",model_provider="openrouter"} 300"#
        ));
        assert!(output.contains(
            r#"zeroclaw_tokens_output_total{model="claude-sonnet",model_provider="openrouter"} 130"#
        ));
    }

    #[test]
    fn hand_events_track_runs_and_duration() {
        let obs = PrometheusObserver::new();

        obs.record_event(&ObserverEvent::HandCompleted {
            hand_name: "review".into(),
            duration_ms: 1500,
            findings_count: 3,
        });
        obs.record_event(&ObserverEvent::HandCompleted {
            hand_name: "review".into(),
            duration_ms: 2000,
            findings_count: 1,
        });
        obs.record_event(&ObserverEvent::HandFailed {
            hand_name: "review".into(),
            error: "timeout".into(),
            duration_ms: 5000,
        });

        let output = obs.encode();
        assert!(output.contains(r#"zeroclaw_hand_runs_total{hand="review",success="true"} 2"#));
        assert!(output.contains(r#"zeroclaw_hand_runs_total{hand="review",success="false"} 1"#));
        assert!(output.contains(r#"zeroclaw_hand_findings_total{hand="review"} 4"#));
        assert!(output.contains("zeroclaw_hand_duration_seconds"));
    }

    #[test]
    fn hand_metrics_record_duration_and_findings() {
        let obs = PrometheusObserver::new();

        obs.record_metric(&ObserverMetric::HandRunDuration {
            hand_name: "scan".into(),
            duration: Duration::from_millis(800),
        });
        obs.record_metric(&ObserverMetric::HandFindingsCount {
            hand_name: "scan".into(),
            count: 5,
        });
        obs.record_metric(&ObserverMetric::HandSuccessRate {
            hand_name: "scan".into(),
            success: true,
        });
        obs.record_metric(&ObserverMetric::HandSuccessRate {
            hand_name: "scan".into(),
            success: false,
        });

        let output = obs.encode();
        assert!(output.contains("zeroclaw_hand_duration_seconds"));
        assert!(output.contains(r#"zeroclaw_hand_findings_total{hand="scan"} 5"#));
        assert!(output.contains(r#"zeroclaw_hand_runs_total{hand="scan",success="true"} 1"#));
        assert!(output.contains(r#"zeroclaw_hand_runs_total{hand="scan",success="false"} 1"#));
    }

    #[test]
    fn llm_response_without_tokens_increments_request_only() {
        let obs = PrometheusObserver::new();

        obs.record_event(&ObserverEvent::LlmResponse {
            model_provider: "ollama".into(),
            model: "llama3".into(),
            duration: Duration::from_millis(100),
            success: false,
            error_message: Some("timeout".into()),
            input_tokens: None,
            output_tokens: None,
        });

        let output = obs.encode();
        assert!(output.contains(
            r#"zeroclaw_llm_requests_total{model="llama3",model_provider="ollama",success="false"} 1"#
        ));
        // Token counters should not appear (no data recorded)
        assert!(!output.contains("zeroclaw_tokens_input_total{"));
        assert!(!output.contains("zeroclaw_tokens_output_total{"));
    }

    #[test]
    fn dora_deployment_events_track_counters() {
        let obs = PrometheusObserver::new();

        obs.record_event(&ObserverEvent::DeploymentCompleted {
            deploy_id: "d1".into(),
            commit_sha: "abc123".into(),
        });
        obs.record_event(&ObserverEvent::DeploymentCompleted {
            deploy_id: "d2".into(),
            commit_sha: "def456".into(),
        });
        obs.record_event(&ObserverEvent::DeploymentFailed {
            deploy_id: "d3".into(),
            reason: "timeout".into(),
        });

        let output = obs.encode();
        assert!(output.contains(r#"zeroclaw_deployments_total{status="success"} 2"#));
        assert!(output.contains(r#"zeroclaw_deployments_total{status="failure"} 1"#));
    }

    #[test]
    fn dora_failure_rate_gauge_updates() {
        let obs = PrometheusObserver::new();

        obs.record_event(&ObserverEvent::DeploymentCompleted {
            deploy_id: "d1".into(),
            commit_sha: "abc".into(),
        });
        obs.record_event(&ObserverEvent::DeploymentFailed {
            deploy_id: "d2".into(),
            reason: "error".into(),
        });

        let output = obs.encode();
        // 1 failure out of 2 total = 0.5
        assert!(output.contains("zeroclaw_deployment_failure_rate 0.5"));
    }

    #[test]
    fn dora_lead_time_and_recovery_metrics() {
        let obs = PrometheusObserver::new();

        obs.record_metric(&ObserverMetric::DeploymentLeadTime(Duration::from_secs(
            3600,
        )));
        obs.record_metric(&ObserverMetric::RecoveryTime(Duration::from_secs(600)));

        let output = obs.encode();
        assert!(output.contains("zeroclaw_deployment_lead_time_seconds"));
        assert!(output.contains("zeroclaw_recovery_time_seconds"));
        assert!(output.contains("zeroclaw_mttr_seconds 600"));
    }

    #[test]
    fn dora_started_and_recovery_events_no_panic() {
        let obs = PrometheusObserver::new();

        obs.record_event(&ObserverEvent::DeploymentStarted {
            deploy_id: "d1".into(),
        });
        obs.record_event(&ObserverEvent::RecoveryCompleted {
            deploy_id: "d1".into(),
        });
    }
}
