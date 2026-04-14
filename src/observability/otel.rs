use super::traits::{Observer, ObserverEvent, ObserverMetric};
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::trace::{Span, SpanBuilder, SpanKind, Status, Tracer};
use opentelemetry::{Context, KeyValue, global};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use parking_lot::Mutex;
use std::any::Any;
use std::time::SystemTime;

/// OpenTelemetry-backed observer — exports traces and metrics via OTLP.
pub struct OtelObserver {
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,

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

    // ── Span parenting state ────────────────────────────────────
    // Stores the active root span context so child spans (llm.call,
    // tool.call, etc.) share the same trace_id and parent_id.
    //
    // Note: this assumes one agent invocation at a time per observer
    // instance. Concurrent invocations on the same observer will
    // overwrite the context — a follow-up change to the Observer trait
    // (adding session_id to events) would be needed for full concurrency.

    /// Context holding the active `agent.invocation` root span.
    agent_context: Mutex<Option<Context>>,
    /// Messages count from the last `LlmRequest`, pending attachment to
    /// the next `LlmResponse` span.
    pending_messages_count: Mutex<Option<usize>>,
    /// Tool arguments from the last `ToolCallStart`, pending attachment
    /// to the next `ToolCall` span.
    pending_tool_args: Mutex<Option<String>>,
}

impl OtelObserver {
    /// Create a new OTel observer exporting to the given OTLP endpoint.
    ///
    /// Uses HTTP/protobuf transport (port 4318 by default).
    /// Falls back to `http://localhost:4318` if no endpoint is provided.
    pub fn new(endpoint: Option<&str>, service_name: Option<&str>) -> Result<Self, String> {
        let base_endpoint = endpoint.unwrap_or("http://localhost:4318");
        let traces_endpoint = format!("{}/v1/traces", base_endpoint.trim_end_matches('/'));
        let metrics_endpoint = format!("{}/v1/metrics", base_endpoint.trim_end_matches('/'));
        let service_name = service_name.unwrap_or("zeroclaw");

        // ── Trace exporter ──────────────────────────────────────
        let span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(&traces_endpoint)
            .build()
            .map_err(|e| format!("Failed to create OTLP span exporter: {e}"))?;

        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_service_name(service_name.to_string())
                    .build(),
            )
            .build();

        global::set_tracer_provider(tracer_provider.clone());

        // ── Metric exporter ─────────────────────────────────────
        let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(&metrics_endpoint)
            .build()
            .map_err(|e| format!("Failed to create OTLP metric exporter: {e}"))?;

        let metric_reader =
            opentelemetry_sdk::metrics::PeriodicReader::builder(metric_exporter).build();

        let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_reader(metric_reader)
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_service_name(service_name.to_string())
                    .build(),
            )
            .build();

        let meter_provider_clone = meter_provider.clone();
        global::set_meter_provider(meter_provider);

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
            tracer_provider,
            meter_provider: meter_provider_clone,
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
            agent_context: Mutex::new(None),
            pending_messages_count: Mutex::new(None),
            pending_tool_args: Mutex::new(None),
        })
    }
}

impl Observer for OtelObserver {
    fn record_event(&self, event: &ObserverEvent) {
        let tracer = global::tracer("zeroclaw");

        match event {
            // ── Agent lifecycle: root span ───────────────────────
            ObserverEvent::AgentStart { provider, model } => {
                self.agent_starts.add(
                    1,
                    &[
                        KeyValue::new("provider", provider.clone()),
                        KeyValue::new("model", model.clone()),
                    ],
                );

                // Create root span — child spans will reference this as parent.
                let span = tracer.build(
                    SpanBuilder::from_name("agent.invocation")
                        .with_kind(SpanKind::Server)
                        .with_attributes(vec![
                            KeyValue::new("provider", provider.clone()),
                            KeyValue::new("model", model.clone()),
                        ]),
                );
                let cx = Context::current_with_span(span);
                *self.agent_context.lock() = Some(cx);
            }
            ObserverEvent::AgentEnd {
                provider,
                model,
                duration,
                tokens_used,
                cost_usd,
            } => {
                let secs = duration.as_secs_f64();

                // Finalize and end the root span created at AgentStart.
                let cx = self.agent_context.lock().take();
                if let Some(cx) = cx {
                    let span = cx.span();
                    span.set_attribute(KeyValue::new("duration_s", secs));
                    if let Some(t) = tokens_used {
                        span.set_attribute(KeyValue::new(
                            "gen_ai.usage.total_tokens",
                            *t as i64,
                        ));
                    }
                    if let Some(c) = cost_usd {
                        span.set_attribute(KeyValue::new("cost_usd", *c));
                    }
                    span.set_status(Status::Ok);
                    span.end();
                }

                self.agent_duration.record(
                    secs,
                    &[
                        KeyValue::new("provider", provider.clone()),
                        KeyValue::new("model", model.clone()),
                    ],
                );
                // Note: tokens are recorded via record_metric(TokensUsed) to avoid
                // double-counting. AgentEnd only records duration.
            }

            // ── LLM calls: child spans with token attributes ────
            ObserverEvent::LlmRequest {
                messages_count, ..
            } => {
                *self.pending_messages_count.lock() = Some(*messages_count);
            }
            ObserverEvent::LlmResponse {
                provider,
                model,
                duration,
                success,
                error_message,
                input_tokens,
                output_tokens,
            } => {
                let secs = duration.as_secs_f64();
                let metric_attrs = [
                    KeyValue::new("provider", provider.clone()),
                    KeyValue::new("model", model.clone()),
                    KeyValue::new("success", success.to_string()),
                ];
                self.llm_calls.add(1, &metric_attrs);
                self.llm_duration.record(secs, &metric_attrs);

                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                let mut attrs = vec![
                    KeyValue::new("provider", provider.clone()),
                    KeyValue::new("model", model.clone()),
                    KeyValue::new("success", *success),
                ];
                if let Some(t) = input_tokens {
                    attrs.push(KeyValue::new("gen_ai.usage.input_tokens", *t as i64));
                }
                if let Some(t) = output_tokens {
                    attrs.push(KeyValue::new("gen_ai.usage.output_tokens", *t as i64));
                }
                if let (Some(i), Some(o)) = (input_tokens, output_tokens) {
                    attrs.push(KeyValue::new(
                        "gen_ai.usage.total_tokens",
                        (*i + *o) as i64,
                    ));
                }
                if let Some(msg) = error_message {
                    attrs.push(KeyValue::new("error.message", msg.clone()));
                }
                if let Some(count) = self.pending_messages_count.lock().take() {
                    attrs.push(KeyValue::new("messages_count", count as i64));
                }

                let builder = SpanBuilder::from_name("llm.call")
                    .with_kind(SpanKind::Client)
                    .with_start_time(start_time)
                    .with_attributes(attrs);

                let parent_cx = self.agent_context.lock().clone();
                let mut span = match parent_cx {
                    Some(ref cx) => tracer.build_with_context(builder, cx),
                    None => tracer.build(builder),
                };
                if *success {
                    span.set_status(Status::Ok);
                } else {
                    span.set_status(Status::error(
                        error_message.as_deref().unwrap_or("LLM call failed"),
                    ));
                }
                span.end();
            }

            // ── Tool calls: child spans with arguments ──────────
            ObserverEvent::ToolCallStart { arguments, .. } => {
                *self.pending_tool_args.lock() = arguments.clone();
            }
            ObserverEvent::ToolCall {
                tool,
                duration,
                success,
            } => {
                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                let mut attrs = vec![
                    KeyValue::new("tool.name", tool.clone()),
                    KeyValue::new("tool.success", *success),
                ];
                if let Some(args) = self.pending_tool_args.lock().take() {
                    // Truncate to 1 KiB to avoid span size limits.
                    let truncated = if args.len() > 1024 {
                        let mut end = 1024;
                        while end > 0 && !args.is_char_boundary(end) {
                            end -= 1;
                        }
                        format!("{}…", &args[..end])
                    } else {
                        args
                    };
                    attrs.push(KeyValue::new("tool.arguments", truncated));
                }

                let builder = SpanBuilder::from_name("tool.call")
                    .with_kind(SpanKind::Internal)
                    .with_start_time(start_time)
                    .with_attributes(attrs);

                let parent_cx = self.agent_context.lock().clone();
                let mut span = match parent_cx {
                    Some(ref cx) => tracer.build_with_context(builder, cx),
                    None => tracer.build(builder),
                };
                span.set_status(if *success {
                    Status::Ok
                } else {
                    Status::error("tool call failed")
                });
                span.end();

                let metric_attrs = [
                    KeyValue::new("tool", tool.clone()),
                    KeyValue::new("success", success.to_string()),
                ];
                self.tool_calls.add(1, &metric_attrs);
                self.tool_duration
                    .record(secs, &[KeyValue::new("tool", tool.clone())]);
            }

            // ── Lightweight events (metrics only) ───────────────
            ObserverEvent::TurnComplete
            | ObserverEvent::CacheHit { .. }
            | ObserverEvent::CacheMiss { .. } => {}
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

            // ── Errors: child span when inside an invocation ────
            ObserverEvent::Error { component, message } => {
                let builder = SpanBuilder::from_name("error")
                    .with_kind(SpanKind::Internal)
                    .with_attributes(vec![
                        KeyValue::new("component", component.clone()),
                        KeyValue::new("error.message", message.clone()),
                    ]);

                let parent_cx = self.agent_context.lock().clone();
                let mut span = match parent_cx {
                    Some(ref cx) => tracer.build_with_context(builder, cx),
                    None => tracer.build(builder),
                };
                span.set_status(Status::error(message.clone()));
                span.end();

                self.errors
                    .add(1, &[KeyValue::new("component", component.clone())]);
            }

            // ── Hand runs: child spans ──────────────────────────
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

                let builder = SpanBuilder::from_name("hand.run")
                    .with_kind(SpanKind::Internal)
                    .with_start_time(start_time)
                    .with_attributes(vec![
                        KeyValue::new("hand.name", hand_name.clone()),
                        KeyValue::new("hand.success", true),
                        KeyValue::new("hand.findings", *findings_count as i64),
                    ]);

                let parent_cx = self.agent_context.lock().clone();
                let mut span = match parent_cx {
                    Some(ref cx) => tracer.build_with_context(builder, cx),
                    None => tracer.build(builder),
                };
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

                let builder = SpanBuilder::from_name("hand.run")
                    .with_kind(SpanKind::Internal)
                    .with_start_time(start_time)
                    .with_attributes(vec![
                        KeyValue::new("hand.name", hand_name.clone()),
                        KeyValue::new("hand.success", false),
                        KeyValue::new("error.message", error.clone()),
                    ]);

                let parent_cx = self.agent_context.lock().clone();
                let mut span = match parent_cx {
                    Some(ref cx) => tracer.build_with_context(builder, cx),
                    None => tracer.build(builder),
                };
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

            // ── DORA deployment events (not yet wired) ──────────
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
                self.tokens_used.add(*t as u64, &[]);
            }
            ObserverMetric::ActiveSessions(s) => {
                self.active_sessions.record(*s as u64, &[]);
            }
            ObserverMetric::QueueDepth(d) => {
                self.queue_depth.record(*d as u64, &[]);
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
        if let Err(e) = self.tracer_provider.force_flush() {
            tracing::warn!("OTel trace flush failed: {e}");
        }
        if let Err(e) = self.meter_provider.force_flush() {
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
        OtelObserver::new(Some("http://127.0.0.1:19999"), Some("zeroclaw-test"))
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
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(250),
            success: true,
            error_message: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
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
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "file_read".into(),
            duration: Duration::from_millis(5),
            success: false,
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

    /// Verifies that the full agent lifecycle (start → children → end)
    /// records without panics and that the root context is properly
    /// created and cleaned up.
    #[test]
    fn span_parenting_lifecycle() {
        let obs = test_observer();

        // No active context before AgentStart
        assert!(obs.agent_context.lock().is_none());

        // AgentStart creates the root context
        obs.record_event(&ObserverEvent::AgentStart {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
        });
        assert!(obs.agent_context.lock().is_some());

        // LlmRequest stores pending messages count
        obs.record_event(&ObserverEvent::LlmRequest {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            messages_count: 5,
        });
        assert_eq!(*obs.pending_messages_count.lock(), Some(5));

        // LlmResponse consumes pending messages count, creates child span
        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            duration: Duration::from_millis(800),
            success: true,
            error_message: None,
            input_tokens: Some(1200),
            output_tokens: Some(350),
        });
        assert!(obs.pending_messages_count.lock().is_none());

        // ToolCallStart stores pending args
        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            arguments: Some(r#"{"command":"ls -la"}"#.into()),
        });
        assert!(obs.pending_tool_args.lock().is_some());

        // ToolCall consumes pending args, creates child span
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(50),
            success: true,
        });
        assert!(obs.pending_tool_args.lock().is_none());

        // Error creates child span under active context
        obs.record_event(&ObserverEvent::Error {
            component: "provider".into(),
            message: "rate limit exceeded".into(),
        });

        // AgentEnd closes the root span and clears context
        obs.record_event(&ObserverEvent::AgentEnd {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            duration: Duration::from_millis(2000),
            tokens_used: Some(1550),
            cost_usd: Some(0.012),
        });
        assert!(obs.agent_context.lock().is_none());
    }

    /// Child spans outside an agent invocation (no active context)
    /// still record without panics — they become independent root spans.
    #[test]
    fn child_spans_without_parent_do_not_panic() {
        let obs = test_observer();

        // No AgentStart — these should still work as standalone spans
        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            duration: Duration::from_millis(100),
            success: true,
            error_message: None,
            input_tokens: Some(50),
            output_tokens: Some(20),
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "file_read".into(),
            duration: Duration::from_millis(5),
            success: true,
        });
        obs.record_event(&ObserverEvent::Error {
            component: "gateway".into(),
            message: "connection reset".into(),
        });
    }

    /// LlmResponse with error message propagates to span status.
    #[test]
    fn llm_error_message_propagated() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            duration: Duration::from_millis(0),
            success: false,
            error_message: Some("429 Too Many Requests".into()),
            input_tokens: None,
            output_tokens: None,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            duration: Duration::from_millis(100),
            tokens_used: None,
            cost_usd: None,
        });
    }

    /// Tool arguments longer than 1 KiB are truncated.
    #[test]
    fn tool_arguments_truncated() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
        });
        let long_args = "x".repeat(2048);
        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            arguments: Some(long_args),
        });
        // Should not panic — args are truncated to 1 KiB internally
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(10),
            success: true,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            duration: Duration::from_millis(100),
            tokens_used: None,
            cost_usd: None,
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
        let result = OtelObserver::new(Some("http://127.0.0.1:12345"), Some("zeroclaw-test"));
        assert!(
            result.is_ok(),
            "observer creation must succeed even with unreachable endpoint"
        );
    }
}
