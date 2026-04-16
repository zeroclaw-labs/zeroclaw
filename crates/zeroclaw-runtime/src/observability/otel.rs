use super::traits::{Observer, ObserverEvent, ObserverMetric};
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::trace::{Span, SpanKind, Status, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue, global};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use parking_lot::Mutex;
use std::any::Any;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::SystemTime;

static TRACER_PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();
static METER_PROVIDER: OnceLock<SdkMeterProvider> = OnceLock::new();

/// OpenTelemetry-backed observer — exports traces and metrics via OTLP.
pub struct OtelObserver {
    // Span parenting state
    agent_context: Mutex<Option<Context>>,
    pending_messages_count: Mutex<Option<usize>>,
    pending_tool_args: Mutex<Option<String>>,
    pending_prompt_content: Mutex<Option<String>>,

    // Metrics instruments
    agent_starts: Counter<u64>,
    agent_duration: Histogram<f64>,
    llm_calls: Counter<u64>,
    llm_duration: Histogram<f64>,
    tool_calls: Counter<u64>,
    tool_duration: Histogram<f64>,
    channel_messages: Counter<u64>,
    heartbeat_ticks: Counter<u64>,
    errors: Counter<u64>,
    request_latency: Histogram<f64>,
    tokens_used: Counter<u64>,
    active_sessions: Gauge<u64>,
    queue_depth: Gauge<u64>,
    hand_runs: Counter<u64>,
    hand_duration: Histogram<f64>,
    hand_findings: Counter<u64>,
}

impl OtelObserver {
    /// Create a new OTel observer exporting to the given OTLP endpoint.
    ///
    /// Uses HTTP/protobuf transport (port 4318 by default).
    /// Falls back to `http://localhost:4318` if no endpoint is provided.
    pub fn new(
        endpoint: Option<&str>,
        service_name: Option<&str>,
        headers: Option<HashMap<String, String>>,
    ) -> Result<Self, String> {
        let base_endpoint = endpoint.unwrap_or("http://localhost:4318");
        let traces_endpoint = format!("{}/v1/traces", base_endpoint.trim_end_matches('/'));
        let metrics_endpoint = format!("{}/v1/metrics", base_endpoint.trim_end_matches('/'));
        let service_name = service_name.unwrap_or("zeroclaw");

        // ── Singleton tracer provider (OnceLock) ────────────────
        let _tp = TRACER_PROVIDER.get_or_init(|| {
            let mut span_builder = opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_endpoint(&traces_endpoint);
            if let Some(ref h) = headers {
                span_builder = span_builder.with_headers(h.clone());
            }
            let span_exporter = span_builder
                .build()
                .expect("Failed to create OTLP span exporter");

            let tp = SdkTracerProvider::builder()
                .with_batch_exporter(span_exporter)
                .with_resource(
                    opentelemetry_sdk::Resource::builder()
                        .with_service_name(service_name.to_string())
                        .build(),
                )
                .build();

            global::set_tracer_provider(tp.clone());
            tp
        });

        // ── Singleton meter provider (OnceLock) ─────────────────
        let _mp = METER_PROVIDER.get_or_init(|| {
            let mut metric_builder = opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_endpoint(&metrics_endpoint);
            if let Some(ref h) = headers {
                metric_builder = metric_builder.with_headers(h.clone());
            }
            let metric_exporter = metric_builder
                .build()
                .expect("Failed to create OTLP metric exporter");

            let metric_reader =
                opentelemetry_sdk::metrics::PeriodicReader::builder(metric_exporter).build();

            let mp = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
                .with_reader(metric_reader)
                .with_resource(
                    opentelemetry_sdk::Resource::builder()
                        .with_service_name(service_name.to_string())
                        .build(),
                )
                .build();

            global::set_meter_provider(mp.clone());
            mp
        });

        // ── Create metric instruments ────────────────────────────
        let meter = global::meter("zeroclaw");

        let agent_starts = meter
            .u64_counter("zeroclaw.agent.starts")
            .with_description("Total agent invocations")
            .build();

        let agent_duration = meter
            .f64_histogram("zeroclaw.agent.duration")
            .with_description("Agent invocation duration in seconds")
            .with_unit("s")
            .build();

        let llm_calls = meter
            .u64_counter("zeroclaw.llm.calls")
            .with_description("Total LLM provider calls")
            .build();

        let llm_duration = meter
            .f64_histogram("zeroclaw.llm.duration")
            .with_description("LLM provider call duration in seconds")
            .with_unit("s")
            .build();

        let tool_calls = meter
            .u64_counter("zeroclaw.tool.calls")
            .with_description("Total tool calls")
            .build();

        let tool_duration = meter
            .f64_histogram("zeroclaw.tool.duration")
            .with_description("Tool execution duration in seconds")
            .with_unit("s")
            .build();

        let channel_messages = meter
            .u64_counter("zeroclaw.channel.messages")
            .with_description("Total channel messages")
            .build();

        let heartbeat_ticks = meter
            .u64_counter("zeroclaw.heartbeat.ticks")
            .with_description("Total heartbeat ticks")
            .build();

        let errors = meter
            .u64_counter("zeroclaw.errors")
            .with_description("Total errors by component")
            .build();

        let request_latency = meter
            .f64_histogram("zeroclaw.request.latency")
            .with_description("Request latency in seconds")
            .with_unit("s")
            .build();

        let tokens_used = meter
            .u64_counter("zeroclaw.tokens.used")
            .with_description("Total tokens consumed (monotonic)")
            .build();

        let active_sessions = meter
            .u64_gauge("zeroclaw.sessions.active")
            .with_description("Current number of active sessions")
            .build();

        let queue_depth = meter
            .u64_gauge("zeroclaw.queue.depth")
            .with_description("Current message queue depth")
            .build();

        let hand_runs = meter
            .u64_counter("zeroclaw.hand.runs")
            .with_description("Total hand runs")
            .build();

        let hand_duration = meter
            .f64_histogram("zeroclaw.hand.duration")
            .with_description("Hand run duration in seconds")
            .with_unit("s")
            .build();

        let hand_findings = meter
            .u64_counter("zeroclaw.hand.findings")
            .with_description("Total findings produced by hand runs")
            .build();

        Ok(Self {
            agent_context: Mutex::new(None),
            pending_messages_count: Mutex::new(None),
            pending_tool_args: Mutex::new(None),
            pending_prompt_content: Mutex::new(None),
            agent_starts,
            agent_duration,
            llm_calls,
            llm_duration,
            tool_calls,
            tool_duration,
            channel_messages,
            heartbeat_ticks,
            errors,
            request_latency,
            tokens_used,
            active_sessions,
            queue_depth,
            hand_runs,
            hand_duration,
            hand_findings,
        })
    }
}

impl Observer for OtelObserver {
    fn record_event(&self, event: &ObserverEvent) {
        let tracer = global::tracer("zeroclaw");

        // Macro: build a span as child of the agent root context, or standalone.
        macro_rules! child_span {
            ($builder:expr, $ctx:expr) => {
                if let Some(parent_ctx) = $ctx.as_ref() {
                    tracer.build_with_context($builder, parent_ctx)
                } else {
                    tracer.build($builder)
                }
            };
        }

        match event {
            ObserverEvent::AgentStart { provider, model } => {
                self.agent_starts.add(
                    1,
                    &[
                        KeyValue::new("provider", provider.clone()),
                        KeyValue::new("model", model.clone()),
                    ],
                );

                // Create root span for the agent invocation
                let span = tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("agent.invocation")
                        .with_kind(SpanKind::Internal)
                        .with_attributes(vec![
                            KeyValue::new("provider", provider.clone()),
                            KeyValue::new("model", model.clone()),
                        ]),
                );
                let ctx = Context::current_with_span(span);
                *self.agent_context.lock() = Some(ctx);
            }
            ObserverEvent::LlmRequest {
                messages_count,
                prompt_content,
                ..
            } => {
                *self.pending_messages_count.lock() = Some(*messages_count);
                *self.pending_prompt_content.lock() = prompt_content.clone();
            }
            ObserverEvent::ToolCallStart { arguments, .. } => {
                *self.pending_tool_args.lock() = arguments.clone();
            }
            ObserverEvent::TurnComplete
            | ObserverEvent::CacheHit { .. }
            | ObserverEvent::CacheMiss { .. } => {}
            ObserverEvent::LlmResponse {
                provider,
                model,
                duration,
                success,
                error_message,
                input_tokens,
                output_tokens,
                response_content,
            } => {
                let secs = duration.as_secs_f64();
                let attrs = [
                    KeyValue::new("provider", provider.clone()),
                    KeyValue::new("model", model.clone()),
                    KeyValue::new("success", success.to_string()),
                ];
                self.llm_calls.add(1, &attrs);
                self.llm_duration.record(secs, &attrs);

                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                let messages_count = self.pending_messages_count.lock().take().unwrap_or(0);
                let prompt_content = self.pending_prompt_content.lock().take();

                let mut span_attrs = vec![
                    KeyValue::new("provider", provider.clone()),
                    KeyValue::new("model", model.clone()),
                    KeyValue::new("success", *success),
                    KeyValue::new("duration_s", secs),
                    KeyValue::new("messages_count", messages_count as i64),
                ];
                if let Some(input) = input_tokens {
                    span_attrs.push(KeyValue::new("gen_ai.usage.input_tokens", *input as i64));
                }
                if let Some(output) = output_tokens {
                    span_attrs.push(KeyValue::new("gen_ai.usage.output_tokens", *output as i64));
                }
                let total = input_tokens.unwrap_or(0) + output_tokens.unwrap_or(0);
                if total > 0 {
                    span_attrs.push(KeyValue::new("gen_ai.usage.total_tokens", total as i64));
                }
                if let Some(err) = error_message {
                    span_attrs.push(KeyValue::new("error.message", err.clone()));
                }
                if let Some(content) = prompt_content {
                    span_attrs.push(KeyValue::new("gen_ai.content.prompt", content));
                }
                if let Some(content) = response_content {
                    span_attrs.push(KeyValue::new("gen_ai.content.completion", content.clone()));
                }

                let builder = opentelemetry::trace::SpanBuilder::from_name("llm.call")
                    .with_kind(SpanKind::Client)
                    .with_start_time(start_time)
                    .with_attributes(span_attrs);

                let parent_ctx = self.agent_context.lock().clone();
                let mut span = child_span!(builder, parent_ctx);
                if *success {
                    span.set_status(Status::Ok);
                } else {
                    span.set_status(Status::error(error_message.clone().unwrap_or_default()));
                }
                span.end();
            }
            ObserverEvent::AgentEnd {
                provider,
                model,
                duration,
                tokens_used,
                cost_usd,
            } => {
                let secs = duration.as_secs_f64();

                // End the root agent span that was created in AgentStart
                if let Some(ctx) = self.agent_context.lock().take() {
                    let span = ctx.span();
                    span.set_attribute(KeyValue::new("duration_s", secs));
                    if let Some(t) = tokens_used {
                        span.set_attribute(KeyValue::new("gen_ai.usage.total_tokens", *t as i64));
                    }
                    if let Some(c) = cost_usd {
                        span.set_attribute(KeyValue::new("cost_usd", *c));
                    }
                    span.set_status(Status::Ok);
                    span.end();
                }

                // Clear pending state
                *self.pending_messages_count.lock() = None;
                *self.pending_tool_args.lock() = None;
                *self.pending_prompt_content.lock() = None;

                self.agent_duration.record(
                    secs,
                    &[
                        KeyValue::new("provider", provider.clone()),
                        KeyValue::new("model", model.clone()),
                    ],
                );
            }
            ObserverEvent::ToolCall {
                tool,
                duration,
                success,
                output,
            } => {
                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                let status = if *success {
                    Status::Ok
                } else {
                    Status::error("")
                };

                let mut span_attrs = vec![
                    KeyValue::new("tool.name", tool.clone()),
                    KeyValue::new("tool.success", *success),
                    KeyValue::new("duration_s", secs),
                ];
                if let Some(args) = self.pending_tool_args.lock().take() {
                    span_attrs.push(KeyValue::new("tool.arguments", args));
                }
                if let Some(out) = output {
                    let truncated = if out.len() > 4096 {
                        format!("{}...", &out[..out.floor_char_boundary(4096)])
                    } else {
                        out.clone()
                    };
                    span_attrs.push(KeyValue::new("tool.output", truncated));
                }

                let builder = opentelemetry::trace::SpanBuilder::from_name("tool.call")
                    .with_kind(SpanKind::Internal)
                    .with_start_time(start_time)
                    .with_attributes(span_attrs);

                let parent_ctx = self.agent_context.lock().clone();
                let mut span = child_span!(builder, parent_ctx);
                span.set_status(status);
                span.end();

                let attrs = [
                    KeyValue::new("tool", tool.clone()),
                    KeyValue::new("success", success.to_string()),
                ];
                self.tool_calls.add(1, &attrs);
                self.tool_duration
                    .record(secs, &[KeyValue::new("tool", tool.clone())]);
            }
            ObserverEvent::ChannelMessage { channel, direction } => {
                self.channel_messages.add(
                    1,
                    &[
                        KeyValue::new("channel", channel.clone()),
                        KeyValue::new("direction", direction.clone()),
                    ],
                );
            }
            ObserverEvent::HeartbeatTick => {
                self.heartbeat_ticks.add(1, &[]);
            }
            ObserverEvent::Error { component, message } => {
                let builder = opentelemetry::trace::SpanBuilder::from_name("error")
                    .with_kind(SpanKind::Internal)
                    .with_attributes(vec![
                        KeyValue::new("component", component.clone()),
                        KeyValue::new("error.message", message.clone()),
                    ]);

                let parent_ctx = self.agent_context.lock().clone();
                let mut span = child_span!(builder, parent_ctx);
                span.set_status(Status::error(message.clone()));
                span.end();

                self.errors
                    .add(1, &[KeyValue::new("component", component.clone())]);
            }
            ObserverEvent::HandStarted { .. } => {}
            ObserverEvent::HandCompleted {
                hand_name,
                duration_ms,
                findings_count,
            } => {
                let secs = *duration_ms as f64 / 1000.0;
                let duration = std::time::Duration::from_millis(*duration_ms);
                let start_time = SystemTime::now()
                    .checked_sub(duration)
                    .unwrap_or(SystemTime::now());

                let builder = opentelemetry::trace::SpanBuilder::from_name("hand.run")
                    .with_kind(SpanKind::Internal)
                    .with_start_time(start_time)
                    .with_attributes(vec![
                        KeyValue::new("hand.name", hand_name.clone()),
                        KeyValue::new("hand.success", true),
                        KeyValue::new("hand.findings", *findings_count as i64),
                        KeyValue::new("duration_s", secs),
                    ]);

                let parent_ctx = self.agent_context.lock().clone();
                let mut span = child_span!(builder, parent_ctx);
                span.set_status(Status::Ok);
                span.end();

                let attrs = [
                    KeyValue::new("hand", hand_name.clone()),
                    KeyValue::new("success", "true"),
                ];
                self.hand_runs.add(1, &attrs);
                self.hand_duration
                    .record(secs, &[KeyValue::new("hand", hand_name.clone())]);
                self.hand_findings.add(
                    *findings_count as u64,
                    &[KeyValue::new("hand", hand_name.clone())],
                );
            }
            ObserverEvent::HandFailed {
                hand_name,
                error,
                duration_ms,
            } => {
                let secs = *duration_ms as f64 / 1000.0;
                let duration = std::time::Duration::from_millis(*duration_ms);
                let start_time = SystemTime::now()
                    .checked_sub(duration)
                    .unwrap_or(SystemTime::now());

                let builder = opentelemetry::trace::SpanBuilder::from_name("hand.run")
                    .with_kind(SpanKind::Internal)
                    .with_start_time(start_time)
                    .with_attributes(vec![
                        KeyValue::new("hand.name", hand_name.clone()),
                        KeyValue::new("hand.success", false),
                        KeyValue::new("error.message", error.clone()),
                        KeyValue::new("duration_s", secs),
                    ]);

                let parent_ctx = self.agent_context.lock().clone();
                let mut span = child_span!(builder, parent_ctx);
                span.set_status(Status::error(error.clone()));
                span.end();

                let attrs = [
                    KeyValue::new("hand", hand_name.clone()),
                    KeyValue::new("success", "false"),
                ];
                self.hand_runs.add(1, &attrs);
                self.hand_duration
                    .record(secs, &[KeyValue::new("hand", hand_name.clone())]);
            }
            ObserverEvent::DeploymentStarted { .. }
            | ObserverEvent::DeploymentCompleted { .. }
            | ObserverEvent::DeploymentFailed { .. }
            | ObserverEvent::RecoveryCompleted { .. } => {
                // DORA deployment events: OTel pass-through not yet implemented.
            }
        }
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        match metric {
            ObserverMetric::RequestLatency(d) => {
                self.request_latency.record(d.as_secs_f64(), &[]);
            }
            ObserverMetric::TokensUsed(t) => {
                self.tokens_used.add(*t, &[]);
            }
            ObserverMetric::ActiveSessions(s) => {
                self.active_sessions.record(*s, &[]);
            }
            ObserverMetric::QueueDepth(d) => {
                self.queue_depth.record(*d, &[]);
            }
            ObserverMetric::HandRunDuration {
                hand_name,
                duration,
            } => {
                self.hand_duration.record(
                    duration.as_secs_f64(),
                    &[KeyValue::new("hand", hand_name.clone())],
                );
            }
            ObserverMetric::HandFindingsCount { hand_name, count } => {
                self.hand_findings
                    .add(*count, &[KeyValue::new("hand", hand_name.clone())]);
            }
            ObserverMetric::HandSuccessRate { hand_name, success } => {
                let success_str = if *success { "true" } else { "false" };
                self.hand_runs.add(
                    1,
                    &[
                        KeyValue::new("hand", hand_name.clone()),
                        KeyValue::new("success", success_str),
                    ],
                );
            }
            ObserverMetric::DeploymentLeadTime(_) | ObserverMetric::RecoveryTime(_) => {
                // DORA metrics: OTel pass-through not yet implemented.
            }
        }
    }

    fn flush(&self) {
        if let Some(tp) = TRACER_PROVIDER.get()
            && let Err(e) = tp.force_flush()
        {
            tracing::warn!("OTel trace flush failed: {e}");
        }
        if let Some(mp) = METER_PROVIDER.get()
            && let Err(e) = mp.force_flush()
        {
            tracing::warn!("OTel metric flush failed: {e}");
        }
    }

    fn name(&self) -> &str {
        "otel"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // Note: OtelObserver::new() requires an OTLP endpoint.
    // In tests we verify the struct creation fails gracefully
    // when no collector is available, and test the observer interface
    // by constructing with a known-unreachable endpoint (spans/metrics
    // are buffered and exported asynchronously, so recording never panics).

    fn test_observer() -> OtelObserver {
        // Create with a dummy endpoint — exports will silently fail
        // but the observer itself works fine for recording
        OtelObserver::new(Some("http://127.0.0.1:19999"), Some("zeroclaw-test"), None)
            .expect("observer creation should not fail with valid endpoint format")
    }

    #[test]
    fn otel_observer_name() {
        let obs = test_observer();
        assert_eq!(obs.name(), "otel");
    }

    #[test]
    fn records_all_events_without_panic() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
        });
        obs.record_event(&ObserverEvent::LlmRequest {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            messages_count: 2,
            prompt_content: None,
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(250),
            success: true,
            error_message: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            response_content: None,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(500),
            tokens_used: Some(100),
            cost_usd: Some(0.0015),
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::ZERO,
            tokens_used: None,
            cost_usd: None,
        });
        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            arguments: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(10),
            success: true,
            output: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "file_read".into(),
            duration: Duration::from_millis(5),
            success: false,
            output: None,
        });
        obs.record_event(&ObserverEvent::TurnComplete);
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
        let obs = test_observer();
        obs.record_metric(&ObserverMetric::RequestLatency(Duration::from_secs(2)));
        obs.record_metric(&ObserverMetric::TokensUsed(500));
        obs.record_metric(&ObserverMetric::TokensUsed(0));
        obs.record_metric(&ObserverMetric::ActiveSessions(3));
        obs.record_metric(&ObserverMetric::QueueDepth(42));
    }

    #[test]
    fn flush_does_not_panic() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.flush();
    }

    // ── §8.2 OTel export failure resilience tests ────────────

    #[test]
    fn otel_records_error_event_without_panic() {
        let obs = test_observer();
        // Simulate an error event — should not panic even with unreachable endpoint
        obs.record_event(&ObserverEvent::Error {
            component: "provider".into(),
            message: "connection refused to model endpoint".into(),
        });
    }

    #[test]
    fn otel_records_llm_failure_without_panic() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "openrouter".into(),
            model: "missing-model".into(),
            duration: Duration::from_millis(0),
            success: false,
            error_message: Some("404 Not Found".into()),
            input_tokens: None,
            output_tokens: None,
            response_content: None,
        });
    }

    #[test]
    fn otel_flush_idempotent_with_unreachable_endpoint() {
        let obs = test_observer();
        // Multiple flushes should not panic even when endpoint is unreachable
        obs.flush();
        obs.flush();
        obs.flush();
    }

    #[test]
    fn otel_records_zero_duration_metrics() {
        let obs = test_observer();
        obs.record_metric(&ObserverMetric::RequestLatency(Duration::ZERO));
        obs.record_metric(&ObserverMetric::TokensUsed(0));
        obs.record_metric(&ObserverMetric::ActiveSessions(0));
        obs.record_metric(&ObserverMetric::QueueDepth(0));
    }

    #[test]
    fn otel_hand_events_do_not_panic() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::HandStarted {
            hand_name: "review".into(),
        });
        obs.record_event(&ObserverEvent::HandCompleted {
            hand_name: "review".into(),
            duration_ms: 1500,
            findings_count: 3,
        });
        obs.record_event(&ObserverEvent::HandFailed {
            hand_name: "review".into(),
            error: "timeout".into(),
            duration_ms: 5000,
        });
    }

    #[test]
    fn otel_hand_metrics_do_not_panic() {
        let obs = test_observer();
        obs.record_metric(&ObserverMetric::HandRunDuration {
            hand_name: "review".into(),
            duration: Duration::from_millis(1500),
        });
        obs.record_metric(&ObserverMetric::HandFindingsCount {
            hand_name: "review".into(),
            count: 5,
        });
        obs.record_metric(&ObserverMetric::HandSuccessRate {
            hand_name: "review".into(),
            success: true,
        });
    }

    #[test]
    fn otel_observer_creation_with_valid_endpoint_succeeds() {
        // Even though endpoint is unreachable, creation should succeed
        let result = OtelObserver::new(Some("http://127.0.0.1:12345"), Some("zeroclaw-test"), None);
        assert!(
            result.is_ok(),
            "observer creation must succeed even with unreachable endpoint"
        );
    }

    #[test]
    fn otel_observer_creation_with_headers_succeeds() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer sk-test".to_string());
        headers.insert("X-Custom".to_string(), "value".to_string());
        let result = OtelObserver::new(Some("http://127.0.0.1:12345"), Some("test"), Some(headers));
        assert!(
            result.is_ok(),
            "observer creation with headers must succeed"
        );
    }

    #[test]
    fn otel_observer_with_headers_records_events() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer sk-test".to_string());
        let obs = OtelObserver::new(Some("http://127.0.0.1:19999"), Some("test"), Some(headers))
            .expect("creation should succeed");
        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "anthropic".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(100),
            success: true,
            error_message: None,
            input_tokens: Some(10),
            output_tokens: Some(5),
            response_content: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(50),
            success: true,
            output: None,
        });
    }

    #[test]
    fn otel_observer_with_empty_headers_succeeds() {
        let result = OtelObserver::new(
            Some("http://127.0.0.1:12345"),
            Some("test"),
            Some(HashMap::new()),
        );
        assert!(
            result.is_ok(),
            "observer creation with empty headers must succeed"
        );
    }
}
