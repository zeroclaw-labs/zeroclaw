//! Langfuse observability backend.
//!
//! Maps the runtime's [`ObserverEvent`] stream to Langfuse's Trace → Generation
//! → Span hierarchy by exporting OTLP spans to `{base_url}/api/public/otel/v1/traces`.
//! Builds on the same OpenTelemetry SDK crates as the OTel observer; the
//! `observability-langfuse` cargo feature gates this entire module so projects
//! that don't need Langfuse don't pay for the OTel dependencies.
//!
//! Lifecycle mapping:
//!   - `AgentStart` / `AgentEnd`     → Trace root span (`agent.invocation`)
//!   - `LlmResponse`                 → Generation (`llm.call`)
//!   - `ToolCall`                    → Span (`tool.execute`)
//!   - `LlmRequest`                  → ignored (content capture lives in the
//!                                     OTel GenAI path via `LlmMessageSnapshot`)
//!   - `ToolCallStart`               → caches `arguments` for the next
//!                                     `ToolCall` on the same tool name
//!
//! I/O capture is opt-in via the `langfuse_include_io` config; when disabled
//! the Generation only carries model name, token usage, and timing.

use super::traits::{Observer, ObserverEvent, ObserverMetric};
use opentelemetry::trace::{Span, SpanKind, Status, TraceContextExt, Tracer, TracerProvider};
use opentelemetry::{Context, KeyValue};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::trace::SdkTracerProvider;
use parking_lot::Mutex as ParkingMutex;
use std::any::Any;
use std::collections::HashMap;
use std::time::SystemTime;

/// Langfuse-backed observer — exports traces to Langfuse via OTLP.
///
/// Maps agent lifecycle events to a trace hierarchy:
///   Trace (AgentStart..AgentEnd)
///     ├── Generation (LlmResponse — LLM call)
///     ├── Span      (ToolCall   — tool execution)
///     ├── Generation (next LLM call)
///     └── ...
///
/// Uses the Langfuse-native OTLP endpoint and sets `langfuse.*` attributes
/// so Langfuse recognises observation types, model names, and token usage.
pub struct LangfuseObserver {
    tracer_provider: SdkTracerProvider,
    tracer: opentelemetry_sdk::trace::Tracer,
    /// Root span for the current agent session.  Created by `AgentStart` and
    /// ended by `AgentEnd`.
    current_root: ParkingMutex<Option<opentelemetry_sdk::trace::Span>>,
    /// Pending tool arguments, keyed by tool name.  Written by `ToolCallStart`,
    /// consumed by `ToolCall`.
    pending_tool_args: ParkingMutex<HashMap<String, String>>,
    /// Whether to include LLM input/output content in generation spans.
    include_io: bool,
}

impl LangfuseObserver {
    /// Create a new Langfuse observer.
    ///
    /// `base_url` is the Langfuse instance URL (e.g. `"https://cloud.langfuse.com"`).
    /// The OTLP traces endpoint is `{base_url}/api/public/otel/v1/traces`.
    pub fn new(
        public_key: &str,
        secret_key: &str,
        base_url: &str,
        include_io: bool,
    ) -> Result<Self, String> {
        let base_url = base_url.trim_end_matches('/');
        let traces_endpoint = format!("{base_url}/api/public/otel/v1/traces");

        // Langfuse auth is HTTP Basic: public_key = username, secret_key = password.
        let auth_value = format!(
            "Basic {}",
            base64_encode(&format!("{public_key}:{secret_key}"))
        );

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), auth_value);
        headers.insert("x-langfuse-ingestion-version".to_string(), "4".to_string());

        let span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_headers(headers)
            .with_endpoint(&traces_endpoint)
            .build()
            .map_err(|e| format!("Failed to create Langfuse OTLP span exporter: {e}"))?;

        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_service_name("zeroclaw")
                    .build(),
            )
            .build();

        let tracer = tracer_provider.tracer("zeroclaw-langfuse");

        Ok(Self {
            tracer_provider,
            tracer,
            current_root: ParkingMutex::new(None),
            pending_tool_args: ParkingMutex::new(HashMap::new()),
            include_io,
        })
    }

    fn context_with_root_parent(root: &opentelemetry_sdk::trace::Span) -> Context {
        // The live root span stays parked in `current_root` until `AgentEnd`, so
        // we cannot move it into `Context::with_span`. We only need its
        // `SpanContext` to preserve trace/parent IDs for completed child spans.
        //
        // `with_remote_span_context` accepts a raw `SpanContext`; it does not
        // flip a local parent to remote. The exported `parent_span_is_remote`
        // flag still comes from `SpanContext::is_remote()`, and SDK-created root
        // spans are local by default.
        Context::new().with_remote_span_context(root.span_context().clone())
    }

    fn usage_details_json(
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    ) -> serde_json::Value {
        let prompt_tokens = input_tokens.unwrap_or(0);
        let completion_tokens = output_tokens.unwrap_or(0);
        let total_tokens = prompt_tokens + completion_tokens;

        serde_json::json!({
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total_tokens,
        })
    }

    fn cost_details_json(cost_usd: Option<f64>) -> serde_json::Value {
        serde_json::json!({
            "total": cost_usd.unwrap_or(0.0),
        })
    }
}

impl Observer for LangfuseObserver {
    fn record_event(&self, event: &ObserverEvent) {
        match event {
            // ── Trace start ─────────────────────────────────────────
            ObserverEvent::AgentStart {
                model_provider,
                model,
                ..
            } => {
                let mut root = self.tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("agent.invocation")
                        .with_kind(SpanKind::Server)
                        .with_attributes(vec![
                            KeyValue::new("langfuse.observation.type", "agent"),
                            KeyValue::new("langfuse.trace.name", "ZeroClaw Agent Session"),
                            KeyValue::new("provider", model_provider.clone()),
                            KeyValue::new("model", model.clone()),
                        ]),
                );
                root.set_status(Status::Ok);
                *self.current_root.lock() = Some(root);
            }

            // ── LLM call (generation) ───────────────────────────────
            ObserverEvent::LlmResponse {
                model_provider,
                model,
                duration,
                success,
                error_message,
                input_tokens,
                output_tokens,
                ..
            } => {
                let root_guard = self.current_root.lock();
                let Some(ref root) = *root_guard else {
                    return;
                };

                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                // Serialize usage_details as JSON string (Langfuse attribute convention).
                let usage_details = Self::usage_details_json(*input_tokens, *output_tokens);

                let mut attrs = vec![
                    KeyValue::new("langfuse.observation.type", "generation"),
                    KeyValue::new("langfuse.observation.model.name", model.clone()),
                    KeyValue::new(
                        "langfuse.observation.usage_details",
                        usage_details.to_string(),
                    ),
                    KeyValue::new(
                        "langfuse.observation.metadata.provider",
                        model_provider.clone(),
                    ),
                    KeyValue::new("langfuse.observation.metadata.success", *success),
                    KeyValue::new("provider", model_provider.clone()),
                    KeyValue::new("model", model.clone()),
                    KeyValue::new("duration_s", secs),
                ];
                if let Some(msg) = error_message {
                    attrs.push(KeyValue::new(
                        "langfuse.observation.status_message",
                        msg.clone(),
                    ));
                }

                let cx = Self::context_with_root_parent(root);
                let mut generation = self.tracer.build_with_context(
                    opentelemetry::trace::SpanBuilder::from_name("llm.call")
                        .with_kind(SpanKind::Internal)
                        .with_start_time(start_time)
                        .with_attributes(attrs),
                    &cx,
                );
                if *success {
                    generation.set_status(Status::Ok);
                } else {
                    generation.set_status(Status::error(error_message.clone().unwrap_or_default()));
                }
                generation.end();
            }

            // ── Tool call (span) ────────────────────────────────────
            ObserverEvent::ToolCallStart {
                tool, arguments, ..
            } => {
                let mut pending = self.pending_tool_args.lock();
                pending.insert(tool.clone(), arguments.clone().unwrap_or_default());
            }
            ObserverEvent::ToolCall {
                tool,
                duration,
                success,
                arguments,
                result,
                ..
            } => {
                let root_guard = self.current_root.lock();
                let Some(ref root) = *root_guard else {
                    return;
                };

                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                // Prefer the arguments carried on the matching `ToolCallStart`;
                // fall back to whatever the agent loop forwarded on `ToolCall`
                // (some tool paths skip the start event).
                let args = self
                    .pending_tool_args
                    .lock()
                    .remove(tool.as_str())
                    .or_else(|| arguments.clone())
                    .unwrap_or_default();

                let mut attrs = vec![
                    KeyValue::new("langfuse.observation.type", "span"),
                    KeyValue::new("langfuse.observation.metadata.tool", tool.clone()),
                    KeyValue::new("langfuse.observation.metadata.success", *success),
                    KeyValue::new("langfuse.observation.input", args),
                    KeyValue::new("tool.name", tool.clone()),
                    KeyValue::new("duration_s", secs),
                ];
                if let Some(output) = result {
                    attrs.push(KeyValue::new("langfuse.observation.output", output.clone()));
                }

                let cx = Self::context_with_root_parent(root);
                let mut span = self.tracer.build_with_context(
                    opentelemetry::trace::SpanBuilder::from_name("tool.execute")
                        .with_kind(SpanKind::Internal)
                        .with_start_time(start_time)
                        .with_attributes(attrs),
                    &cx,
                );
                if *success {
                    span.set_status(Status::Ok);
                } else {
                    span.set_status(Status::error(""));
                }
                span.end();
            }

            // ── Trace end ───────────────────────────────────────────
            ObserverEvent::AgentEnd {
                duration,
                tokens_used,
                cost_usd,
                ..
            } => {
                let Some(mut root) = self.current_root.lock().take() else {
                    return;
                };

                // Attach aggregate trace metadata before ending.
                let secs = duration.as_secs_f64();
                root.set_attribute(KeyValue::new("duration_s", secs));
                if let Some(t) = tokens_used {
                    let total_tokens = t.input_tokens + t.output_tokens;
                    root.set_attribute(KeyValue::new("tokens_used", total_tokens as i64));
                }
                if let Some(c) = cost_usd {
                    root.set_attribute(KeyValue::new(
                        "langfuse.observation.cost_details",
                        Self::cost_details_json(Some(*c)).to_string(),
                    ));
                }
                root.end();
            }

            // ── Ignored events ──────────────────────────────────────
            ObserverEvent::LlmRequest { .. }
            | ObserverEvent::TurnComplete
            | ObserverEvent::ChannelMessage { .. }
            | ObserverEvent::HeartbeatTick
            | ObserverEvent::CacheHit { .. }
            | ObserverEvent::CacheMiss { .. }
            | ObserverEvent::Error { .. }
            | ObserverEvent::DeploymentStarted { .. }
            | ObserverEvent::DeploymentCompleted { .. }
            | ObserverEvent::DeploymentFailed { .. }
            | ObserverEvent::RecoveryCompleted { .. }
            | ObserverEvent::MemoryRecall { .. }
            | ObserverEvent::MemoryStore { .. }
            | ObserverEvent::MemoryAudit { .. }
            | ObserverEvent::RagRetrieve { .. }
            | ObserverEvent::HistoryTrimmed { .. } => {}
            _ => {}
        }
    }

    fn record_metric(&self, _metric: &ObserverMetric) {
        // Langfuse receives scores/usage via span attributes rather than
        // streaming metric signals.  Custom metric backends (Prometheus, OTel
        // metrics) are the right channel for numeric series.
    }

    fn flush(&self) {
        if let Err(e) = self.tracer_provider.force_flush() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "Langfuse OTLP flush failed"
            );
        }
    }

    fn name(&self) -> &str {
        "langfuse"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for LangfuseObserver {
    fn drop(&mut self) {
        // End any dangling root span so the trace is not left open.
        if let Some(mut root) = self.current_root.lock().take() {
            root.end();
        }
        if let Err(e) = self.tracer_provider.force_flush() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "Langfuse OTLP flush on drop failed"
            );
        }
    }
}

fn base64_encode(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Helper: construct an observer pointed at an unreachable endpoint.
    /// All recording must still succeed (export failures are best-effort).
    fn test_observer() -> LangfuseObserver {
        LangfuseObserver::new(
            "pk-test-dummy",
            "sk-test-dummy",
            "http://127.0.0.1:19999",
            false,
        )
        .expect("test observer creation")
    }

    #[test]
    fn langfuse_observer_name() {
        assert_eq!(test_observer().name(), "langfuse");
    }

    #[test]
    fn usage_details_includes_prompt_and_completion_tokens() {
        let usage_details = LangfuseObserver::usage_details_json(Some(500), Some(200));
        assert_eq!(usage_details["prompt_tokens"], 500);
        assert_eq!(usage_details["completion_tokens"], 200);
        assert_eq!(usage_details["total_tokens"], 700);
    }

    #[test]
    fn records_full_session_without_panic() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "openai".into(),
            model: "gpt-4o".into(),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::LlmRequest {
            model_provider: "openai".into(),
            model: "gpt-4o".into(),
            messages_count: 5,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            model_provider: "openai".into(),
            model: "gpt-4o".into(),
            duration: Duration::from_millis(1200),
            success: true,
            error_message: None,
            input_tokens: Some(500),
            output_tokens: Some(200),
            messages: None,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            tool_call_id: None,
            arguments: Some(r#"{"cmd":"ls"}"#.into()),
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            tool_call_id: None,
            duration: Duration::from_millis(300),
            success: true,
            arguments: None,
            result: None,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openai".into(),
            model: "gpt-4o".into(),
            duration: Duration::from_secs(5),
            tokens_used: Some(700),
            cost_usd: Some(0.03),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.flush();
    }

    #[test]
    fn llm_response_without_agent_start_is_noop() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::LlmResponse {
            model_provider: "openai".into(),
            model: "gpt-4o".into(),
            duration: Duration::from_millis(500),
            success: false,
            error_message: Some("timeout".into()),
            input_tokens: None,
            output_tokens: None,
            messages: None,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        });
        // Must not panic.
    }

    #[test]
    fn tool_call_without_start_args_uses_empty() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "openai".into(),
            model: "gpt-4o".into(),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        // No ToolCallStart — ToolCall sees empty arguments.
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "read_file".into(),
            tool_call_id: None,
            duration: Duration::from_millis(100),
            success: true,
            arguments: None,
            result: None,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openai".into(),
            model: "gpt-4o".into(),
            duration: Duration::from_secs(1),
            tokens_used: None,
            cost_usd: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.flush();
    }

    #[test]
    fn zero_duration_and_zero_tokens() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "anthropic".into(),
            model: "claude-3".into(),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            model_provider: "anthropic".into(),
            model: "claude-3".into(),
            duration: Duration::ZERO,
            success: true,
            error_message: None,
            input_tokens: Some(0),
            output_tokens: Some(0),
            messages: None,
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "anthropic".into(),
            model: "claude-3".into(),
            duration: Duration::ZERO,
            tokens_used: Some(0),
            cost_usd: Some(0.0),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.flush();
    }

    #[test]
    fn records_metrics_without_panic() {
        let obs = test_observer();
        obs.record_metric(&ObserverMetric::TokensUsed(500));
        obs.record_metric(&ObserverMetric::RequestLatency(Duration::from_secs(2)));
        obs.record_metric(&ObserverMetric::ActiveSessions(3));
        obs.record_metric(&ObserverMetric::QueueDepth(10));
    }

    #[test]
    fn drop_ends_dangling_root_span() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "openai".into(),
            model: "gpt-4".into(),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        // AgentEnd never called — Drop should end the span.
        drop(obs);
    }

    #[test]
    fn multiple_llm_calls_in_one_trace() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "openrouter".into(),
            model: "sonnet".into(),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        for i in 0..3 {
            obs.record_event(&ObserverEvent::LlmRequest {
                model_provider: "openrouter".into(),
                model: "sonnet".into(),
                messages_count: 10 + i,
                channel: None,
                agent_alias: None,
                parent_agent_alias: None,
                turn_id: None,
            });
            obs.record_event(&ObserverEvent::LlmResponse {
                model_provider: "openrouter".into(),
                model: "sonnet".into(),
                duration: Duration::from_secs(2),
                success: true,
                error_message: None,
                input_tokens: Some(1000),
                output_tokens: Some(500),
                messages: None,
                channel: None,
                agent_alias: None,
                parent_agent_alias: None,
                turn_id: None,
            });
        }
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openrouter".into(),
            model: "sonnet".into(),
            duration: Duration::from_secs(10),
            tokens_used: Some(4500),
            cost_usd: Some(0.15),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.flush();
    }

    #[test]
    fn local_root_span_context_keeps_child_parent_local() {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("langfuse-context-test");

        let root = tracer.build(
            opentelemetry::trace::SpanBuilder::from_name("agent.invocation")
                .with_kind(SpanKind::Server),
        );
        let root_span_context = root.span_context().clone();
        let root_span_id = root_span_context.span_id();
        let root_trace_id = root_span_context.trace_id();

        assert!(!root_span_context.is_remote());

        let cx = LangfuseObserver::context_with_root_parent(&root);
        let child = tracer.build_with_context(
            opentelemetry::trace::SpanBuilder::from_name("llm.call").with_kind(SpanKind::Internal),
            &cx,
        );

        let child_data = child.exported_data().expect("child exported data");
        assert_eq!(child.span_context().trace_id(), root_trace_id);
        assert_eq!(child_data.parent_span_id, root_span_id);
        assert!(!child_data.parent_span_is_remote);
    }
}
