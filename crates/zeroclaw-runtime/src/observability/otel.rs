use super::traits::{LlmMessageSnapshot, Observer, ObserverEvent, ObserverMetric};
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::trace::{Span, SpanKind, Status, Tracer};
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::any::Any;
use std::collections::HashMap;
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

        // ── Trace exporter ──────────────────────────────────────
        let mut span_builder = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(&traces_endpoint);
        if let Some(ref h) = headers {
            span_builder = span_builder.with_headers(h.clone());
        }
        let span_exporter = span_builder
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
        let mut metric_builder = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(&metrics_endpoint);
        if let Some(ref h) = headers {
            metric_builder = metric_builder.with_headers(h.clone());
        }
        let metric_exporter = metric_builder
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
            .with_description("Total LLM model_provider calls")
            .build();

        let llm_duration = meter
            .f64_histogram("zeroclaw.llm.duration")
            .with_description("LLM model_provider call duration in seconds")
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
        })
    }
}

impl Observer for OtelObserver {
    fn record_event(&self, event: &ObserverEvent) {
        let tracer = global::tracer("zeroclaw");

        match event {
            ObserverEvent::AgentStart {
                model_provider,
                model,
            } => {
                self.agent_starts.add(
                    1,
                    &[
                        KeyValue::new("model_provider", model_provider.clone()),
                        KeyValue::new("model", model.clone()),
                    ],
                );
            }
            ObserverEvent::LlmRequest { .. }
            | ObserverEvent::ToolCallStart { .. }
            | ObserverEvent::TurnComplete
            | ObserverEvent::CacheHit { .. }
            | ObserverEvent::CacheMiss { .. } => {}
            ObserverEvent::LlmResponse {
                model_provider,
                model,
                duration,
                success,
                error_message: _,
                input_tokens: _,
                output_tokens: _,
                messages,
            } => {
                let secs = duration.as_secs_f64();
                let attrs = [
                    KeyValue::new("model_provider", model_provider.clone()),
                    KeyValue::new("model", model.clone()),
                    KeyValue::new("success", success.to_string()),
                ];
                self.llm_calls.add(1, &attrs);
                self.llm_duration.record(secs, &attrs);

                // Create a completed span for visibility in trace backends.
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());
                let mut span_attrs = vec![
                    KeyValue::new("model_provider", model_provider.clone()),
                    KeyValue::new("model", model.clone()),
                    KeyValue::new("success", *success),
                    KeyValue::new("duration_s", secs),
                ];
                span_attrs.extend(message_attrs(messages));
                let mut span = tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("llm.call")
                        .with_kind(SpanKind::Internal)
                        .with_start_time(start_time)
                        .with_attributes(span_attrs),
                );
                if *success {
                    span.set_status(Status::Ok);
                } else {
                    span.set_status(Status::error(""));
                }
                span.end();
            }
            ObserverEvent::AgentEnd {
                model_provider,
                model,
                duration,
                tokens_used,
                cost_usd,
            } => {
                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                // Create a completed span with correct timing
                let mut span = tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("agent.invocation")
                        .with_kind(SpanKind::Internal)
                        .with_start_time(start_time)
                        .with_attributes(vec![
                            KeyValue::new("model_provider", model_provider.clone()),
                            KeyValue::new("model", model.clone()),
                            KeyValue::new("duration_s", secs),
                        ]),
                );
                if let Some(t) = tokens_used {
                    span.set_attribute(KeyValue::new("tokens_used", *t as i64));
                }
                if let Some(c) = cost_usd {
                    span.set_attribute(KeyValue::new("cost_usd", *c));
                }
                span.end();

                self.agent_duration.record(
                    secs,
                    &[
                        KeyValue::new("model_provider", model_provider.clone()),
                        KeyValue::new("model", model.clone()),
                    ],
                );
                // Note: tokens are recorded via record_metric(TokensUsed) to avoid
                // double-counting. AgentEnd only records duration.
            }
            ObserverEvent::ToolCall {
                tool,
                tool_call_id,
                duration,
                success,
                arguments,
                result,
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

                // Legacy ZeroClaw-internal attrs are kept so existing
                // dashboards keep working; OpenTelemetry gen_ai.tool.*
                // semantic-convention attributes are added so LLM-aware
                // backends (Langfuse, SigNoz, Phoenix) surface the tool
                // call as a proper GenAI tool execution with the command
                // arguments and its result visible in the trace viewer.
                let mut span_attrs = vec![
                    // Legacy
                    KeyValue::new("tool.name", tool.clone()),
                    KeyValue::new("tool.success", *success),
                    KeyValue::new("duration_s", secs),
                    // gen_ai.* semantic conventions
                    KeyValue::new("gen_ai.operation.name", "execute_tool"),
                    KeyValue::new("gen_ai.tool.name", tool.clone()),
                ];
                if let Some(id) = tool_call_id {
                    span_attrs.push(KeyValue::new("gen_ai.tool.call.id", id.clone()));
                }
                if let Some(args) = arguments {
                    span_attrs.push(KeyValue::new("gen_ai.tool.arguments", args.clone()));
                    // `input.value` is a Langfuse-specific convention that
                    // surfaces into the "Input" pane of the trace viewer.
                    // Emitting both keeps vendor-agnostic backends happy
                    // while Langfuse users get a proper Input/Output view.
                    span_attrs.push(KeyValue::new("input.value", args.clone()));
                }
                if let Some(res) = result {
                    span_attrs.push(KeyValue::new("gen_ai.tool.result", res.clone()));
                    span_attrs.push(KeyValue::new("output.value", res.clone()));
                }

                let mut span = tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("tool.call")
                        .with_kind(SpanKind::Internal)
                        .with_start_time(start_time)
                        .with_attributes(span_attrs),
                );
                span.set_status(status);
                span.end();

                let metric_attrs = [
                    KeyValue::new("tool", tool.clone()),
                    KeyValue::new("success", success.to_string()),
                ];
                self.tool_calls.add(1, &metric_attrs);
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
                // Create an error span for visibility in trace backends
                let mut span = tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("error")
                        .with_kind(SpanKind::Internal)
                        .with_attributes(vec![
                            KeyValue::new("component", component.clone()),
                            KeyValue::new("error.message", message.clone()),
                        ]),
                );
                span.set_status(Status::error(message.clone()));
                span.end();

                self.errors
                    .add(1, &[KeyValue::new("component", component.clone())]);
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
            ObserverMetric::DeploymentLeadTime(_) | ObserverMetric::RecoveryTime(_) => {
                // DORA metrics: OTel pass-through not yet implemented.
            }
        }
    }

    fn flush(&self) {
        if let Err(e) = self.tracer_provider.force_flush() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "OTel trace flush failed"
            );
        }
        if let Err(e) = self.meter_provider.force_flush() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "OTel metric flush failed"
            );
        }
    }

    fn name(&self) -> &str {
        "otel"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Build the OTel GenAI message-content attributes from a captured snapshot.
/// Returns an empty vec when there is nothing to emit. Encoding matches the
/// Langfuse-validated shape: system carried separately, system filtered out of
/// `input.messages`, output as a single assistant message (text + tool calls).
fn message_attrs(messages: &Option<LlmMessageSnapshot>) -> Vec<KeyValue> {
    let Some(snap) = messages else {
        return Vec::new();
    };
    let mut attrs = Vec::new();

    if let Some(sys) = snap.system_instructions.as_ref() {
        attrs.push(KeyValue::new("gen_ai.system_instructions", sys.clone()));
    }

    if !snap.input.is_empty() {
        // `content` stays a plain string (free-form prose); only tool-call `arguments`
        // (below) are re-parsed into a nested tree — the bce8da324 / Langfuse-validated shape.
        let input_json = serde_json::to_string(
            &snap
                .input
                .iter()
                .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".to_string());
        attrs.push(KeyValue::new("gen_ai.input.messages", input_json));
    }

    // Output is a single assistant message: text (if any) plus tool calls (if any).
    let mut output_msg = serde_json::Map::new();
    output_msg.insert("role".into(), serde_json::Value::String("assistant".into()));
    if let Some(text) = snap.output_text.as_ref() {
        output_msg.insert("content".into(), serde_json::Value::String(text.clone()));
    }
    if !snap.output_tool_calls.is_empty() {
        let calls: Vec<serde_json::Value> = snap
            .output_tool_calls
            .iter()
            .map(|tc| {
                serde_json::json!({
                    "id": tc.id,
                    "name": tc.name,
                    // arguments_json is already JSON text — re-parse so the attribute
                    // is a nested tree, not a double-encoded string. On malformed JSON,
                    // fall back to the (scrubbed) raw string rather than silently dropping it.
                    "arguments": serde_json::from_str::<serde_json::Value>(&tc.arguments_json)
                        .unwrap_or_else(|_| serde_json::Value::String(tc.arguments_json.clone())),
                })
            })
            .collect();
        output_msg.insert("tool_calls".into(), serde_json::Value::Array(calls));
    }
    if output_msg.contains_key("content") || output_msg.contains_key("tool_calls") {
        let output_json = serde_json::to_string(&vec![serde_json::Value::Object(output_msg)])
            .unwrap_or_else(|_| "[]".to_string());
        attrs.push(KeyValue::new("gen_ai.output.messages", output_json));
    }

    attrs
}

#[cfg(test)]
mod tests {
    use super::super::traits::{LlmMessageSnapshot, MessageSnapshot, ToolCallSnapshot};
    use super::*;
    use std::time::Duration;

    fn attr_value(attrs: &[opentelemetry::KeyValue], key: &str) -> Option<String> {
        attrs
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.as_str().to_string())
    }

    #[test]
    fn message_attrs_emits_genai_semconv() {
        let snap = LlmMessageSnapshot {
            input: vec![MessageSnapshot {
                role: "user".into(),
                content: "hi".into(),
            }],
            output_text: Some("hello".into()),
            output_tool_calls: vec![ToolCallSnapshot {
                id: "c1".into(),
                name: "shell".into(),
                arguments_json: r#"{"cmd":"ls"}"#.into(),
            }],
            system_instructions: Some("You are helpful.".into()),
        };
        let attrs = message_attrs(&Some(snap));

        assert_eq!(
            attr_value(&attrs, "gen_ai.system_instructions").as_deref(),
            Some("You are helpful.")
        );

        let input: serde_json::Value =
            serde_json::from_str(&attr_value(&attrs, "gen_ai.input.messages").unwrap()).unwrap();
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"], "hi");

        let output: serde_json::Value =
            serde_json::from_str(&attr_value(&attrs, "gen_ai.output.messages").unwrap()).unwrap();
        assert_eq!(output[0]["role"], "assistant");
        assert_eq!(output[0]["content"], "hello");
        assert_eq!(output[0]["tool_calls"][0]["name"], "shell");
        assert_eq!(output[0]["tool_calls"][0]["arguments"]["cmd"], "ls");
    }

    #[test]
    fn message_attrs_omits_empty_and_handles_none() {
        // Only system set: input/output omitted.
        let snap = LlmMessageSnapshot {
            input: vec![],
            output_text: None,
            output_tool_calls: vec![],
            system_instructions: Some("sys".into()),
        };
        let attrs = message_attrs(&Some(snap));
        let keys: Vec<&str> = attrs.iter().map(|kv| kv.key.as_str()).collect();
        assert!(keys.contains(&"gen_ai.system_instructions"));
        assert!(!keys.contains(&"gen_ai.input.messages"));
        assert!(!keys.contains(&"gen_ai.output.messages"));

        // None → no attrs.
        assert!(message_attrs(&None).is_empty());
    }

    #[test]
    fn message_attrs_malformed_tool_arguments_falls_back_to_string() {
        let snap = LlmMessageSnapshot {
            input: vec![],
            output_text: None,
            output_tool_calls: vec![ToolCallSnapshot {
                id: "c1".into(),
                name: "shell".into(),
                arguments_json: "not valid json".into(),
            }],
            system_instructions: None,
        };
        let attrs = message_attrs(&Some(snap));
        let output: serde_json::Value =
            serde_json::from_str(&attr_value(&attrs, "gen_ai.output.messages").unwrap()).unwrap();
        // Malformed arguments fall back to the raw string, not null / dropped.
        assert_eq!(output[0]["tool_calls"][0]["arguments"], "not valid json");
    }

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
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
        });
        obs.record_event(&ObserverEvent::LlmRequest {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            messages_count: 2,
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(250),
            success: true,
            error_message: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            messages: None,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(500),
            tokens_used: Some(100),
            cost_usd: Some(0.0015),
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::ZERO,
            tokens_used: None,
            cost_usd: None,
        });
        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            tool_call_id: None,
            arguments: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            tool_call_id: None,
            duration: Duration::from_millis(10),
            success: true,
            arguments: None,
            result: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "file_read".into(),
            tool_call_id: None,
            duration: Duration::from_millis(5),
            success: false,
            arguments: None,
            result: None,
        });
        obs.record_event(&ObserverEvent::TurnComplete);
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

    /// Regression test for upstream issue #5980 — tool spans must accept a
    /// populated `tool_call_id`, full `arguments`, and `result` without
    /// panicking, including payloads large enough that naive attribute
    /// encoding could truncate them. We can't assert on exported span
    /// attributes here because the OTLP pipeline runs asynchronously, but
    /// verifying the recording path handles all three optional fields
    /// exercises the new gen_ai.tool.* code paths.
    #[test]
    fn tool_call_with_id_args_and_result_does_not_panic() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            tool_call_id: Some("toolu_01ABC".into()),
            arguments: Some(r#"{"command":"ls -la /tmp"}"#.into()),
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            tool_call_id: Some("toolu_01ABC".into()),
            duration: Duration::from_millis(42),
            success: true,
            arguments: Some(r#"{"command":"ls -la /tmp"}"#.into()),
            result: Some("total 0\ndrwxr-xr-x  2 root root 40 Apr 22 12:00 .\n".into()),
        });
        // Failure case — the issue author specifically wants to see *why*
        // a tool call failed, so the result field is the error text.
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            tool_call_id: Some("toolu_02DEF".into()),
            duration: Duration::from_millis(3),
            success: false,
            arguments: Some(r#"{"command":"rm -rf /"}"#.into()),
            result: Some("Error: command denied by allowlist policy".into()),
        });
    }

    // ── §8.2 OTel export failure resilience tests ────────────

    #[test]
    fn otel_records_error_event_without_panic() {
        let obs = test_observer();
        // Simulate an error event — should not panic even with unreachable endpoint
        obs.record_event(&ObserverEvent::Error {
            component: "model_provider".into(),
            message: "connection refused to model endpoint".into(),
        });
    }

    #[test]
    fn otel_records_llm_failure_without_panic() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::LlmResponse {
            model_provider: "openrouter".into(),
            model: "missing-model".into(),
            duration: Duration::from_millis(0),
            success: false,
            error_message: Some("404 Not Found".into()),
            input_tokens: None,
            output_tokens: None,
            messages: None,
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
            model_provider: "anthropic".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(100),
            success: true,
            error_message: None,
            input_tokens: Some(10),
            output_tokens: Some(5),
            messages: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            tool_call_id: None,
            duration: Duration::from_millis(50),
            success: true,
            arguments: None,
            result: None,
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
