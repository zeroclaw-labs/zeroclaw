use super::traits::{LlmMessageSnapshot, Observer, ObserverEvent, ObserverMetric};
use crate::agent::loop_::scrub_for_export;
use crate::observability::otel_config::OtelContentConfig;
use crate::util::{truncate_field, truncate_json_leaves};
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::trace::{Span, SpanKind, Status, TraceContextExt as _, Tracer};
use opentelemetry::{Context, KeyValue, global};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;
use zeroclaw_config::schema::OtelContentPolicy;

struct ActiveAgentSpan {
    span: global::BoxedSpan,
    context: Context,
    first_user_input: Option<String>,
    last_output_text: Option<String>,
}

/// OpenTelemetry-backed observer — exports traces and metrics via OTLP.
pub struct OtelObserver {
    /// Per-observer OTel content policy, derived once from
    /// `ObservabilityConfig` at construction. Owned by this instance so the
    /// export boundary (`record_event` + attribute builders) consults a stable
    /// privacy policy that no other observer can overwrite.
    content_config: OtelContentConfig,
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
    memory_recall_count: Counter<u64>,
    memory_recall_duration: Histogram<f64>,
    memory_store_count: Counter<u64>,
    memory_audit_count: Counter<u64>,
    rag_retrieve_count: Counter<u64>,
    rag_retrieve_duration: Histogram<f64>,

    // Turn span tracking for parent/child correlation
    active_agent_spans: Mutex<HashMap<String, ActiveAgentSpan>>,
}

impl OtelObserver {
    /// Create a new OTel observer exporting to the given OTLP endpoint.
    /// Uses HTTP/protobuf transport (port 4318 by default).
    /// Falls back to `http://localhost:4318` if no endpoint is provided.
    pub(crate) fn new(
        endpoint: Option<&str>,
        service_name: Option<&str>,
        headers: Option<HashMap<String, String>>,
        content_config: OtelContentConfig,
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

        let memory_recall_count = meter
            .u64_counter("zeroclaw.memory.recall.count")
            .with_description("Total memory.recall calls from the runtime boundary")
            .build();

        let memory_recall_duration = meter
            .f64_histogram("zeroclaw.memory.recall.duration")
            .with_description("memory.recall duration in seconds")
            .with_unit("s")
            .build();

        let memory_store_count = meter
            .u64_counter("zeroclaw.memory.store.count")
            .with_description("Total memory.store calls from the runtime boundary")
            .build();

        let memory_audit_count = meter
            .u64_counter("zeroclaw.memory.audit.count")
            .with_description("Total memory audit trail actions")
            .build();

        let rag_retrieve_count = meter
            .u64_counter("zeroclaw.rag.retrieve.count")
            .with_description("Total rag.retrieve calls from the runtime boundary")
            .build();

        let rag_retrieve_duration = meter
            .f64_histogram("zeroclaw.rag.retrieve.duration")
            .with_description("rag.retrieve duration in seconds")
            .with_unit("s")
            .build();

        Ok(Self {
            content_config,
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
            memory_recall_count,
            memory_recall_duration,
            memory_store_count,
            memory_audit_count,
            rag_retrieve_count,
            rag_retrieve_duration,
            active_agent_spans: Mutex::new(HashMap::new()),
        })
    }

    fn parent_cx_for(&self, turn_id: Option<&str>) -> Context {
        if let Some(tid) = turn_id
            && let Some(entry) = self
                .active_agent_spans
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(tid)
        {
            return entry.context.clone();
        }
        Context::current()
    }
}

impl Observer for OtelObserver {
    fn record_event(&self, event: &ObserverEvent) {
        let tracer = global::tracer("zeroclaw");

        match event {
            ObserverEvent::AgentStart {
                model_provider,
                model,
                channel,
                agent_alias,
                turn_id,
            } => {
                self.agent_starts.add(
                    1,
                    &[
                        KeyValue::new("gen_ai.provider.name", model_provider.clone()),
                        KeyValue::new("gen_ai.request.model", model.clone()),
                    ],
                );

                let span = tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("gen_ai.agent.invoke")
                        .with_kind(SpanKind::Internal)
                        .with_attributes(vec![
                            KeyValue::new("gen_ai.provider.name", model_provider.clone()),
                            KeyValue::new("gen_ai.request.model", model.clone()),
                            KeyValue::new("zeroclaw.channel", channel.clone().unwrap_or_default()),
                            KeyValue::new(
                                "gen_ai.agent.name",
                                agent_alias.clone().unwrap_or_default(),
                            ),
                            KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                        ]),
                );

                if let Some(tid) = turn_id {
                    let parent_cx =
                        Context::current().with_remote_span_context(span.span_context().clone());
                    self.active_agent_spans
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(
                            tid.clone(),
                            ActiveAgentSpan {
                                span,
                                context: parent_cx,
                                first_user_input: None,
                                last_output_text: None,
                            },
                        );
                }
            }
            ObserverEvent::LlmRequest {
                model_provider,
                model,
                messages_count,
                channel,
                agent_alias,
                parent_agent_alias,
                turn_id,
            } => {
                let parent_cx = self.parent_cx_for(turn_id.as_deref());
                let mut span = tracer.build_with_context(
                    opentelemetry::trace::SpanBuilder::from_name("llm.request")
                        .with_kind(SpanKind::Client)
                        .with_attributes(vec![
                            KeyValue::new("gen_ai.provider.name", model_provider.clone()),
                            KeyValue::new("gen_ai.request.model", model.clone()),
                            KeyValue::new("gen_ai.operation.name", "llm.request"),
                            KeyValue::new(
                                "zeroclaw.messages_count",
                                i64::try_from(*messages_count).unwrap_or(i64::MAX),
                            ),
                            KeyValue::new("zeroclaw.channel", channel.clone().unwrap_or_default()),
                            KeyValue::new(
                                "gen_ai.agent.name",
                                agent_alias.clone().unwrap_or_default(),
                            ),
                            KeyValue::new(
                                "zeroclaw.parent_agent_alias",
                                parent_agent_alias.clone().unwrap_or_default(),
                            ),
                            KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                        ]),
                    &parent_cx,
                );
                span.end();
            }
            ObserverEvent::ToolCallStart {
                tool,
                tool_call_id,
                arguments,
                channel,
                agent_alias,
                parent_agent_alias,
                turn_id,
            } => {
                let mut span_attrs = vec![
                    KeyValue::new("gen_ai.operation.name", "execute_tool"),
                    KeyValue::new("tool.name", tool.clone()),
                    KeyValue::new("zeroclaw.channel", channel.clone().unwrap_or_default()),
                    KeyValue::new("gen_ai.agent.name", agent_alias.clone().unwrap_or_default()),
                    KeyValue::new(
                        "zeroclaw.parent_agent_alias",
                        parent_agent_alias.clone().unwrap_or_default(),
                    ),
                    KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                ];
                if let Some(id) = tool_call_id {
                    span_attrs.push(KeyValue::new("gen_ai.tool.call.id", id.clone()));
                }

                // OTel-only content processing: scrub + truncate based on this
                // observer's instance-owned tool I/O policy.
                span_attrs.extend(tool_start_content_attrs(
                    arguments.as_deref(),
                    self.content_config,
                ));

                let parent_cx = self.parent_cx_for(turn_id.as_deref());
                let mut span = tracer.build_with_context(
                    opentelemetry::trace::SpanBuilder::from_name("tool_call.start")
                        .with_kind(SpanKind::Client)
                        .with_attributes(span_attrs),
                    &parent_cx,
                );
                span.end();
            }
            ObserverEvent::TurnComplete
            | ObserverEvent::CacheHit { .. }
            | ObserverEvent::CacheMiss { .. } => {}
            ObserverEvent::MemoryRecall {
                query_summary,
                duration,
                num_entries,
                backend,
                success,
                channel,
                agent_alias,
                turn_id,
            } => {
                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                let mut span_attrs = vec![
                    // Legacy / ZeroClaw-specific attrs
                    KeyValue::new("memory.backend", backend.clone()),
                    KeyValue::new("memory.hits", *num_entries as i64),
                    KeyValue::new("memory.success", *success),
                    KeyValue::new("duration_s", secs),
                    KeyValue::new("gen_ai.operation.name", "retrieval"),
                    KeyValue::new("gen_ai.system", backend.clone()),
                    KeyValue::new("zeroclaw.channel", channel.clone().unwrap_or_default()),
                    KeyValue::new("gen_ai.agent.name", agent_alias.clone().unwrap_or_default()),
                    KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                ];
                if let Some(q) = query_summary {
                    // Langfuse-specific Input/Output pane attrs. Emitting
                    // both keeps vendor-agnostic backends happy while
                    // Langfuse renders the query and the hit count in its
                    // GenAI-aware retrieval view.
                    span_attrs.push(KeyValue::new("input.value", q.clone()));
                    span_attrs.push(KeyValue::new(
                        "output.value",
                        format!("{} hits", num_entries),
                    ));
                }

                let parent_cx = self.parent_cx_for(turn_id.as_deref());
                let mut span = tracer.build_with_context(
                    opentelemetry::trace::SpanBuilder::from_name("memory.recall")
                        .with_kind(SpanKind::Internal)
                        .with_start_time(start_time)
                        .with_attributes(span_attrs),
                    &parent_cx,
                );
                if *success {
                    span.set_status(Status::Ok);
                } else {
                    span.set_status(Status::error(""));
                }
                span.end();

                let metric_attrs = [KeyValue::new("backend", backend.clone())];
                self.memory_recall_count.add(1, &metric_attrs);
                self.memory_recall_duration.record(secs, &metric_attrs);
            }
            ObserverEvent::RagRetrieve {
                query_summary,
                duration,
                num_chunks,
                num_boards,
                channel,
                agent_alias,
                turn_id,
            } => {
                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                let mut span_attrs = vec![
                    KeyValue::new("rag.num_chunks", *num_chunks as i64),
                    KeyValue::new("rag.num_boards", *num_boards as i64),
                    KeyValue::new("duration_s", secs),
                    KeyValue::new("gen_ai.operation.name", "retrieval"),
                    KeyValue::new("gen_ai.system", "zeroclaw_rag"),
                    KeyValue::new("zeroclaw.channel", channel.clone().unwrap_or_default()),
                    KeyValue::new("gen_ai.agent.name", agent_alias.clone().unwrap_or_default()),
                    KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                ];
                if let Some(q) = query_summary {
                    span_attrs.push(KeyValue::new("input.value", q.clone()));
                    span_attrs.push(KeyValue::new(
                        "output.value",
                        format!("{} chunks across {} boards", num_chunks, num_boards),
                    ));
                }

                let parent_cx = self.parent_cx_for(turn_id.as_deref());
                let mut span = tracer.build_with_context(
                    opentelemetry::trace::SpanBuilder::from_name("rag.retrieve")
                        .with_kind(SpanKind::Internal)
                        .with_start_time(start_time)
                        .with_attributes(span_attrs),
                    &parent_cx,
                );
                span.set_status(Status::Ok);
                span.end();

                self.rag_retrieve_count.add(1, &[]);
                self.rag_retrieve_duration.record(secs, &[]);
            }
            ObserverEvent::MemoryStore {
                category,
                backend,
                duration,
                success,
                channel,
                agent_alias,
                turn_id,
            } => {
                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                let span_attrs = vec![
                    KeyValue::new("memory.category", category.clone()),
                    KeyValue::new("memory.backend", backend.clone()),
                    KeyValue::new("memory.success", *success),
                    KeyValue::new("duration_s", secs),
                    KeyValue::new("db.system", backend.clone()),
                    KeyValue::new("db.operation", "INSERT"),
                    KeyValue::new("zeroclaw.channel", channel.clone().unwrap_or_default()),
                    KeyValue::new("gen_ai.agent.name", agent_alias.clone().unwrap_or_default()),
                    KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                ];

                let parent_cx = self.parent_cx_for(turn_id.as_deref());
                let mut span = tracer.build_with_context(
                    opentelemetry::trace::SpanBuilder::from_name("memory.store")
                        .with_kind(SpanKind::Internal)
                        .with_start_time(start_time)
                        .with_attributes(span_attrs),
                    &parent_cx,
                );
                if *success {
                    span.set_status(Status::Ok);
                } else {
                    span.set_status(Status::error(""));
                }
                span.end();

                let metric_attrs = [
                    KeyValue::new("category", category.clone()),
                    KeyValue::new("backend", backend.clone()),
                    KeyValue::new("success", success.to_string()),
                ];
                self.memory_store_count.add(1, &metric_attrs);
            }
            ObserverEvent::MemoryAudit {
                action,
                backend,
                duration,
                success,
            } => {
                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());
                let span_attrs = vec![
                    KeyValue::new("memory.action", action.clone()),
                    KeyValue::new("memory.backend", backend.clone()),
                    KeyValue::new("memory.success", *success),
                    KeyValue::new("duration_s", secs),
                    KeyValue::new("db.system", backend.clone()),
                    KeyValue::new("db.operation", action.clone()),
                ];

                let mut span = tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("memory.audit")
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

                let metric_attrs = [
                    KeyValue::new("action", action.clone()),
                    KeyValue::new("backend", backend.clone()),
                    KeyValue::new("success", success.to_string()),
                ];
                self.memory_audit_count.add(1, &metric_attrs);
            }
            ObserverEvent::LlmResponse {
                model_provider,
                model,
                duration,
                success,
                error_message: _,
                input_tokens,
                output_tokens,
                channel,
                agent_alias,
                parent_agent_alias,
                turn_id,
                messages,
            } => {
                let secs = duration.as_secs_f64();
                let attrs = [
                    KeyValue::new("gen_ai.provider.name", model_provider.clone()),
                    KeyValue::new("gen_ai.request.model", model.clone()),
                    KeyValue::new("gen_ai.response.model", model.clone()),
                    KeyValue::new("gen_ai.operation.name", "llm.response"),
                    KeyValue::new("success", *success),
                    KeyValue::new("duration_s", secs),
                    KeyValue::new("zeroclaw.channel", channel.clone().unwrap_or_default()),
                    KeyValue::new("gen_ai.agent.name", agent_alias.clone().unwrap_or_default()),
                    KeyValue::new(
                        "zeroclaw.parent_agent_alias",
                        parent_agent_alias.clone().unwrap_or_default(),
                    ),
                    KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                ];
                self.llm_calls.add(1, &attrs);
                self.llm_duration.record(secs, &attrs);

                let mut span_attrs = vec![
                    KeyValue::new("gen_ai.provider.name", model_provider.clone()),
                    KeyValue::new("gen_ai.request.model", model.clone()),
                    KeyValue::new("gen_ai.response.model", model.clone()),
                    KeyValue::new("gen_ai.operation.name", "llm.response"),
                    KeyValue::new("success", *success),
                    KeyValue::new("duration_s", secs),
                    KeyValue::new("zeroclaw.channel", channel.clone().unwrap_or_default()),
                    KeyValue::new("gen_ai.agent.name", agent_alias.clone().unwrap_or_default()),
                    KeyValue::new(
                        "zeroclaw.parent_agent_alias",
                        parent_agent_alias.clone().unwrap_or_default(),
                    ),
                    KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                ];
                if let Some(input) = input_tokens {
                    span_attrs.push(KeyValue::new("gen_ai.usage.input_tokens", *input as i64));
                }
                if let Some(output) = output_tokens {
                    span_attrs.push(KeyValue::new("gen_ai.usage.output_tokens", *output as i64));
                }
                span_attrs.extend(message_attrs(messages, self.content_config));

                // Update agent span aggregation for turn-level gen_ai.input.messages / gen_ai.output.messages
                if let Some(tid) = turn_id
                    && let Ok(mut spans) = self.active_agent_spans.lock()
                    && let Some(agent_span) = spans.get_mut(tid)
                {
                    // Capture first user input (last user message in the input)
                    if agent_span.first_user_input.is_none()
                        && let Some(snap) = messages
                    {
                        agent_span.first_user_input = snap
                            .input
                            .iter()
                            .rev()
                            .find(|m| m.role == "user")
                            .map(|m| m.content.clone());
                    }
                    // Capture last output text (overwrites on each LLM call)
                    if let Some(snap) = messages
                        && let Some(text) = &snap.output_text
                    {
                        agent_span.last_output_text = Some(text.clone());
                    }
                }

                let parent_cx = self.parent_cx_for(turn_id.as_deref());
                let mut span = tracer.build_with_context(
                    opentelemetry::trace::SpanBuilder::from_name("llm.response")
                        .with_kind(SpanKind::Client)
                        .with_attributes(span_attrs),
                    &parent_cx,
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
                channel,
                agent_alias,
                turn_id,
            } => {
                if let Some(tid) = turn_id {
                    let entry = self
                        .active_agent_spans
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(tid);
                    if let Some(mut agent_span) = entry {
                        let secs = duration.as_secs_f64();
                        agent_span
                            .span
                            .set_attribute(KeyValue::new("duration_s", secs));
                        agent_span.span.set_attribute(KeyValue::new(
                            "zeroclaw.channel",
                            channel.clone().unwrap_or_default(),
                        ));
                        agent_span.span.set_attribute(KeyValue::new(
                            "gen_ai.agent.name",
                            agent_alias.clone().unwrap_or_default(),
                        ));
                        if let Some(usage) = tokens_used {
                            agent_span.span.set_attribute(KeyValue::new(
                                "gen_ai.usage.input_tokens",
                                usage.input_tokens as i64,
                            ));
                            agent_span.span.set_attribute(KeyValue::new(
                                "gen_ai.usage.output_tokens",
                                usage.output_tokens as i64,
                            ));
                        }
                        if let Some(c) = cost_usd {
                            agent_span.span.set_attribute(KeyValue::new("cost_usd", *c));
                        }

                        // Set agent span aggregation attributes based on this
                        // observer's instance-owned genai policy. Emit the
                        // GenAI semconv `gen_ai.input.messages` /
                        // `gen_ai.output.messages` (JSON-string encoded).
                        let config = self.content_config;
                        if config.genai_policy != OtelContentPolicy::Off {
                            if let Some(input) = agent_span.first_user_input
                                && let Some(val) = process_agent_message(
                                    &input,
                                    "user",
                                    config.genai_policy,
                                    config.genai_max_chars,
                                )
                            {
                                agent_span
                                    .span
                                    .set_attribute(KeyValue::new("gen_ai.input.messages", val));
                            }
                            if let Some(output) = agent_span.last_output_text
                                && let Some(val) = process_agent_message(
                                    &output,
                                    "assistant",
                                    config.genai_policy,
                                    config.genai_max_chars,
                                )
                            {
                                agent_span
                                    .span
                                    .set_attribute(KeyValue::new("gen_ai.output.messages", val));
                            }
                        }

                        agent_span.span.end();
                    }
                }

                let secs = duration.as_secs_f64();
                self.agent_duration.record(
                    secs,
                    &[
                        KeyValue::new("gen_ai.provider.name", model_provider.clone()),
                        KeyValue::new("gen_ai.request.model", model.clone()),
                        KeyValue::new("gen_ai.agent.name", agent_alias.clone().unwrap_or_default()),
                    ],
                );
            }
            ObserverEvent::ToolCall {
                tool,
                tool_call_id,
                duration,
                success,
                arguments,
                result,
                channel,
                agent_alias,
                parent_agent_alias,
                turn_id,
            } => {
                let secs = duration.as_secs_f64();

                let status = if *success {
                    Status::Ok
                } else {
                    Status::error("")
                };

                let mut span_attrs = vec![
                    KeyValue::new("gen_ai.operation.name", "execute_tool"),
                    KeyValue::new("tool.name", tool.clone()),
                    KeyValue::new("tool.success", *success),
                    KeyValue::new("duration_s", secs),
                    KeyValue::new("zeroclaw.channel", channel.clone().unwrap_or_default()),
                    KeyValue::new("gen_ai.agent.name", agent_alias.clone().unwrap_or_default()),
                    KeyValue::new(
                        "zeroclaw.parent_agent_alias",
                        parent_agent_alias.clone().unwrap_or_default(),
                    ),
                    KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                ];
                if let Some(id) = tool_call_id {
                    span_attrs.push(KeyValue::new("gen_ai.tool.call.id", id.clone()));
                }

                // OTel-only content processing: scrub + truncate based on this
                // observer's instance-owned tool I/O policy.
                span_attrs.extend(tool_result_content_attrs(
                    arguments.as_deref(),
                    result.as_deref(),
                    self.content_config,
                ));
                let parent_cx = self.parent_cx_for(turn_id.as_deref());
                let mut span = tracer.build_with_context(
                    opentelemetry::trace::SpanBuilder::from_name("tool_call.result")
                        .with_kind(SpanKind::Internal)
                        .with_attributes(span_attrs),
                    &parent_cx,
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
            // `ObserverEvent` is `#[non_exhaustive]` — silently ignore any
            // future variant added by upstream `zeroclaw-api`.
            _ => {}
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
        // Flush orphan live spans (turns that ended without AgentEnd)
        let orphans: Vec<ActiveAgentSpan> = self
            .active_agent_spans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, v)| v)
            .collect();
        for mut orphan in orphans {
            orphan.span.end();
        }

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

fn clean_for_display(content: &str) -> String {
    let mut cleaned = content.to_string();

    // Remove memory context blocks - only if both start and end tags present
    let memory_start = "[Memory context]";
    let memory_end = "[/Memory context]";
    if let Some(start) = cleaned.find(memory_start)
        && let Some(end) = cleaned.find(memory_end)
    {
        cleaned.replace_range(start..(end + memory_end.len()), "");
    }

    // Remove tool result blocks - only if both start and end tags present
    let tool_result_start = "<tool_result";
    let tool_result_end = "</tool_result>";
    if let Some(start) = cleaned.find(tool_result_start)
        && let Some(end) = cleaned.find(tool_result_end)
    {
        cleaned.replace_range(start..(end + tool_result_end.len()), "");
    }

    // Remove thinking blocks (<thinking>...</thinking>) - only if both tags present
    let thinking_start = "<thinking>";
    let thinking_end = "</thinking>";
    if let Some(start) = cleaned.find(thinking_start)
        && let Some(end) = cleaned.find(thinking_end)
    {
        cleaned.replace_range(start..(end + thinking_end.len()), "");
    }

    // Remove think blocks (<think>...</think>`) - only if both tags present
    let think_start = "<think>";
    let think_end = "</think>";
    if let Some(start) = cleaned.find(think_start)
        && let Some(end) = cleaned.find(think_end)
    {
        cleaned.replace_range(start..(end + think_end.len()), "");
    }

    // Remove timestamp patterns like [2026-06-30 16:44:51 +08:00]
    let timestamp_regex =
        regex::Regex::new(r"\[\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\s+\S+\]").unwrap();
    cleaned = timestamp_regex.replace_all(&cleaned, "").to_string();

    // Remove tool results prefix
    let tool_results_prefix_regex = regex::Regex::new(r"(?m)^\[Tool results\]\s*\n?").unwrap();
    cleaned = tool_results_prefix_regex
        .replace_all(&cleaned, "")
        .to_string();

    // Clean up extra whitespace
    cleaned = cleaned
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    cleaned.trim().to_string()
}

fn process_agent_message(
    content: &str,
    role: &str,
    policy: OtelContentPolicy,
    max_chars: usize,
) -> Option<String> {
    let cleaned = clean_for_display(content);
    let processed = if policy == OtelContentPolicy::Redacted {
        truncate_field(&cleaned, max_chars)
    } else {
        Some(cleaned)
    }?;
    let messages = vec![serde_json::json!({ "role": role, "content": processed })];
    serde_json::to_string(&messages).ok()
}

/// Scrub + (under `Redacted`) JSON-leaf-truncate tool arguments. Shared by
/// `tool_start_content_attrs` and `tool_result_content_attrs` so the JSON
/// truncation / scrubbing logic lives in one place.
fn process_tool_arguments(raw: &str, policy: OtelContentPolicy, max_chars: usize) -> String {
    let scrubbed = scrub_for_export(raw);
    if policy == OtelContentPolicy::Redacted {
        // Try JSON leaf truncation first, fall back to string truncation.
        match serde_json::from_str::<serde_json::Value>(&scrubbed) {
            Ok(parsed) => truncate_json_leaves(&parsed, max_chars)
                .and_then(|v| serde_json::to_string(&v).ok())
                .unwrap_or_else(|| truncate_field(&scrubbed, max_chars).unwrap_or(scrubbed)),
            Err(_) => truncate_field(&scrubbed, max_chars).unwrap_or(scrubbed),
        }
    } else {
        scrubbed
    }
}

/// Scrub + (under `Redacted`) string-truncate tool result text. Result text is
/// not assumed to be JSON, so it does not go through JSON-leaf truncation.
fn process_tool_result(raw: &str, policy: OtelContentPolicy, max_chars: usize) -> String {
    let scrubbed = scrub_for_export(raw);
    if policy == OtelContentPolicy::Redacted {
        truncate_field(&scrubbed, max_chars).unwrap_or(scrubbed)
    } else {
        scrubbed
    }
}

/// Build the `gen_ai.tool.arguments` / `input.value` attributes for a
/// `ToolCallStart` event under the given observer-owned content policy.
/// Returns an empty vec when the policy is `Off` or no arguments were supplied.
fn tool_start_content_attrs(arguments: Option<&str>, config: OtelContentConfig) -> Vec<KeyValue> {
    let (policy, max_chars) = (config.tool_io_policy, config.tool_io_max_chars);
    if policy == OtelContentPolicy::Off {
        return Vec::new();
    }
    let Some(args) = arguments else {
        return Vec::new();
    };
    let processed = process_tool_arguments(args, policy, max_chars);
    vec![
        KeyValue::new("gen_ai.tool.arguments", processed.clone()),
        KeyValue::new("input.value", processed),
    ]
}

/// Build the `gen_ai.tool.arguments` / `input.value` and `gen_ai.tool.result` /
/// `output.value` attributes for a `ToolCall` event under the given
/// observer-owned content policy. Returns an empty vec when the policy is `Off`;
/// otherwise emits whichever of arguments / result are present.
fn tool_result_content_attrs(
    arguments: Option<&str>,
    result: Option<&str>,
    config: OtelContentConfig,
) -> Vec<KeyValue> {
    let (policy, max_chars) = (config.tool_io_policy, config.tool_io_max_chars);
    if policy == OtelContentPolicy::Off {
        return Vec::new();
    }
    let mut attrs = Vec::new();
    if let Some(args) = arguments {
        let processed = process_tool_arguments(args, policy, max_chars);
        attrs.push(KeyValue::new("gen_ai.tool.arguments", processed.clone()));
        attrs.push(KeyValue::new("input.value", processed));
    }
    if let Some(res) = result {
        let processed = process_tool_result(res, policy, max_chars);
        attrs.push(KeyValue::new("gen_ai.tool.result", processed.clone()));
        attrs.push(KeyValue::new("output.value", processed));
    }
    attrs
}

fn message_attrs(
    messages: &Option<LlmMessageSnapshot>,
    config: OtelContentConfig,
) -> Vec<KeyValue> {
    let Some(snap) = messages else {
        return Vec::new();
    };

    let (policy, max_chars) = (config.genai_policy, config.genai_max_chars);

    if policy == OtelContentPolicy::Off {
        return Vec::new();
    }

    let mut attrs = Vec::new();

    if let Some(sys) = snap.system_instructions.as_ref()
        && let Some(truncated) = if policy == OtelContentPolicy::Redacted {
            truncate_field(sys, max_chars)
        } else {
            Some(sys.clone())
        }
    {
        attrs.push(KeyValue::new("gen_ai.system_instructions", truncated));
    }

    if !snap.input.is_empty() {
        let input_json = serde_json::to_string(
            &snap
                .input
                .iter()
                .map(|m| {
                    let content = if policy == OtelContentPolicy::Redacted {
                        truncate_field(&m.content, max_chars).unwrap_or_else(|| m.content.clone())
                    } else {
                        m.content.clone()
                    };
                    serde_json::json!({ "role": m.role, "content": content })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".to_string());
        attrs.push(KeyValue::new("gen_ai.input.messages", input_json));
    }

    let mut output_msg = serde_json::Map::new();
    output_msg.insert("role".into(), serde_json::Value::String("assistant".into()));
    if let Some(text) = snap.output_text.as_ref()
        && let Some(truncated) = if policy == OtelContentPolicy::Redacted {
            truncate_field(text, max_chars)
        } else {
            Some(text.clone())
        }
    {
        output_msg.insert("content".into(), serde_json::Value::String(truncated));
    }
    if !snap.output_tool_calls.is_empty() {
        let calls: Vec<serde_json::Value> = snap
            .output_tool_calls
            .iter()
            .map(|tc| {
                let arguments = if policy == OtelContentPolicy::Redacted {
                    match serde_json::from_str::<serde_json::Value>(&tc.arguments_json) {
                        Ok(parsed) => truncate_json_leaves(&parsed, max_chars)
                            .and_then(|v| serde_json::to_string(&v).ok())
                            .unwrap_or_else(|| tc.arguments_json.clone()),
                        Err(_) => truncate_field(&tc.arguments_json, max_chars)
                            .unwrap_or_else(|| tc.arguments_json.clone()),
                    }
                } else {
                    tc.arguments_json.clone()
                };
                serde_json::json!({
                    "id": tc.id,
                    "name": tc.name,
                    "arguments": serde_json::from_str::<serde_json::Value>(&arguments)
                        .unwrap_or(serde_json::Value::String(arguments)),
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

    /// `Full` GenAI policy with a generous char cap so truncation is a no-op
    /// and the exact captured strings can be asserted verbatim. Tool I/O is
    /// `Off` so GenAI-only tests don't accidentally exercise tool helpers.
    fn genai_full_config() -> OtelContentConfig {
        OtelContentConfig {
            genai_policy: OtelContentPolicy::Full,
            genai_max_chars: 10_000,
            tool_io_policy: OtelContentPolicy::Off,
            tool_io_max_chars: 0,
        }
    }

    /// `Full` tool I/O policy with a generous char cap. GenAI is `Off`.
    fn tool_io_full_config() -> OtelContentConfig {
        OtelContentConfig {
            genai_policy: OtelContentPolicy::Off,
            genai_max_chars: 0,
            tool_io_policy: OtelContentPolicy::Full,
            tool_io_max_chars: 10_000,
        }
    }

    /// All-off policy — no content attributes emitted.
    fn all_off_config() -> OtelContentConfig {
        OtelContentConfig::off()
    }

    /// A populated snapshot reused by the GenAI policy-isolation tests.
    fn sample_llm_snapshot() -> LlmMessageSnapshot {
        LlmMessageSnapshot {
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
        }
    }

    #[test]
    fn message_attrs_emits_genai_semconv() {
        let snap = sample_llm_snapshot();
        let attrs = message_attrs(&Some(snap), genai_full_config());

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
        let attrs = message_attrs(&Some(snap), genai_full_config());
        let keys: Vec<&str> = attrs.iter().map(|kv| kv.key.as_str()).collect();
        assert!(keys.contains(&"gen_ai.system_instructions"));
        assert!(!keys.contains(&"gen_ai.input.messages"));
        assert!(!keys.contains(&"gen_ai.output.messages"));

        // None → no attrs.
        assert!(message_attrs(&None, genai_full_config()).is_empty());
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
        let attrs = message_attrs(&Some(snap), genai_full_config());
        let output: serde_json::Value =
            serde_json::from_str(&attr_value(&attrs, "gen_ai.output.messages").unwrap()).unwrap();
        // Malformed arguments fall back to the raw string, not null / dropped.
        assert_eq!(output[0]["tool_calls"][0]["arguments"], "not valid json");
    }

    #[test]
    fn message_attrs_returns_empty_when_genai_policy_off() {
        let snap = sample_llm_snapshot();
        let attrs = message_attrs(&Some(snap), all_off_config());
        assert!(attrs.is_empty());
    }

    fn test_observer() -> OtelObserver {
        // Create with a dummy endpoint — exports will silently fail
        // but the observer itself works fine for recording. Content policy is
        // all-off; smoke tests don't assert exported content, only that the
        // recording path doesn't panic.
        OtelObserver::new(
            Some("http://127.0.0.1:19999"),
            Some("zeroclaw-test"),
            None,
            all_off_config(),
        )
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
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::LlmRequest {
            parent_agent_alias: None,
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            messages_count: 2,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            parent_agent_alias: None,
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(250),
            success: true,
            error_message: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            messages: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(500),
            tokens_used: None,
            cost_usd: Some(0.0015),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::ZERO,
            tokens_used: None,
            cost_usd: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::ToolCallStart {
            parent_agent_alias: None,
            tool: "shell".into(),
            tool_call_id: None,
            arguments: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            parent_agent_alias: None,
            tool: "shell".into(),
            tool_call_id: None,
            duration: Duration::from_millis(10),
            success: true,
            arguments: None,
            result: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            parent_agent_alias: None,
            tool: "file_read".into(),
            tool_call_id: None,
            duration: Duration::from_millis(5),
            success: false,
            arguments: None,
            result: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
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

    #[test]
    fn memory_rag_events_do_not_panic() {
        let obs = test_observer();

        // MemoryRecall with populated query_summary (Langfuse path).
        obs.record_event(&ObserverEvent::MemoryRecall {
            query_summary: Some("what did the user say about coffee".into()),
            duration: Duration::from_millis(45),
            num_entries: 7,
            backend: "sqlite".into(),
            success: true,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        // MemoryRecall failure path with query_summary: None.
        obs.record_event(&ObserverEvent::MemoryRecall {
            query_summary: None,
            duration: Duration::from_millis(12),
            num_entries: 0,
            backend: "qdrant".into(),
            success: false,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });

        // RagRetrieve with populated query_summary.
        obs.record_event(&ObserverEvent::RagRetrieve {
            query_summary: Some("ESP32-S3 GPIO pinout".into()),
            duration: Duration::from_millis(120),
            num_chunks: 12,
            num_boards: 3,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        // RagRetrieve with query_summary: None.
        obs.record_event(&ObserverEvent::RagRetrieve {
            query_summary: None,
            duration: Duration::ZERO,
            num_chunks: 0,
            num_boards: 0,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });

        // MemoryStore success path.
        obs.record_event(&ObserverEvent::MemoryStore {
            category: "conversation".into(),
            backend: "sqlite".into(),
            duration: Duration::from_millis(8),
            success: true,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        // MemoryStore failure path.
        obs.record_event(&ObserverEvent::MemoryStore {
            category: "fact".into(),
            backend: "qdrant".into(),
            duration: Duration::from_millis(3),
            success: false,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::MemoryAudit {
            action: "store".into(),
            backend: "sqlite".into(),
            duration: Duration::from_millis(5),
            success: true,
        });
        obs.record_event(&ObserverEvent::MemoryAudit {
            action: "purge".into(),
            backend: "qdrant".into(),
            duration: Duration::from_millis(2),
            success: false,
        });
    }

    #[test]
    fn tool_call_with_id_args_and_result_does_not_panic() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::ToolCallStart {
            parent_agent_alias: None,
            tool: "shell".into(),
            tool_call_id: Some("toolu_01ABC".into()),
            arguments: Some(r#"{"command":"ls -la /tmp"}"#.into()),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            parent_agent_alias: None,
            tool: "shell".into(),
            tool_call_id: Some("toolu_01ABC".into()),
            duration: Duration::from_millis(42),
            success: true,
            arguments: Some(r#"{"command":"ls -la /tmp"}"#.into()),
            result: Some("total 0\ndrwxr-xr-x  2 root root 40 Apr 22 12:00 .\n".into()),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        // Failure case — the issue author specifically wants to see *why*
        // a tool call failed, so the result field is the error text.
        obs.record_event(&ObserverEvent::ToolCall {
            parent_agent_alias: None,
            tool: "shell".into(),
            tool_call_id: Some("toolu_02DEF".into()),
            duration: Duration::from_millis(3),
            success: false,
            arguments: Some(r#"{"command":"rm -rf /"}"#.into()),
            result: Some("Error: command denied by allowlist policy".into()),
            channel: None,
            agent_alias: None,
            turn_id: None,
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
            parent_agent_alias: None,
            model_provider: "openrouter".into(),
            model: "missing-model".into(),
            duration: Duration::from_millis(0),
            success: false,
            error_message: Some("404 Not Found".into()),
            input_tokens: None,
            output_tokens: None,
            messages: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
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
    fn turn_id_opens_and_closes_agent_span() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            channel: Some("wss".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
        });

        assert!(
            obs.active_agent_spans
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key("turn-1"),
            "AgentStart should open a live span keyed by turn_id"
        );

        obs.record_event(&ObserverEvent::LlmRequest {
            parent_agent_alias: None,
            model_provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            messages_count: 2,
            channel: Some("wss".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            parent_agent_alias: None,
            model_provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            duration: Duration::from_millis(25),
            success: true,
            error_message: None,
            input_tokens: Some(10),
            output_tokens: Some(5),
            channel: Some("wss".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
            messages: None,
        });
        obs.record_event(&ObserverEvent::ToolCallStart {
            parent_agent_alias: None,
            tool: "shell".into(),
            tool_call_id: Some("call-1".into()),
            arguments: Some(r#"{"command":"date"}"#.into()),
            channel: Some("wss".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
        });
        obs.record_event(&ObserverEvent::ToolCall {
            parent_agent_alias: None,
            tool: "shell".into(),
            tool_call_id: Some("call-1".into()),
            duration: Duration::from_millis(5),
            success: true,
            arguments: Some(r#"{"command":"date"}"#.into()),
            result: Some("Mon Apr 22 12:00:00 UTC 2026".into()),
            channel: Some("wss".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            duration: Duration::from_millis(50),
            tokens_used: Some(zeroclaw_api::observability_traits::TurnTokenUsage {
                input_tokens: 10,
                output_tokens: 5,
            }),
            cost_usd: None,
            channel: Some("wss".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
        });

        assert!(
            !obs.active_agent_spans
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key("turn-1"),
            "AgentEnd should close the live span"
        );
    }

    /// Memory/RAG events carrying a live turn_id must parent under the
    /// active agent span. As with the other span tests we
    /// cannot assert exported parent/child linkage (OTLP export is async);
    /// we assert the recording path exercises `parent_cx_for` for all three
    /// arms without panicking and without disturbing the live turn entry.
    #[test]
    fn memory_rag_spans_nest_under_turn() {
        let obs = test_observer();
        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            channel: Some("cli".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-m1".into()),
        });
        assert!(
            obs.active_agent_spans
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key("turn-m1"),
            "AgentStart should open a live span keyed by turn_id"
        );

        obs.record_event(&ObserverEvent::MemoryRecall {
            query_summary: Some("coffee preferences".into()),
            duration: Duration::from_millis(45),
            num_entries: 3,
            backend: "sqlite".into(),
            success: true,
            channel: Some("cli".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-m1".into()),
        });
        obs.record_event(&ObserverEvent::RagRetrieve {
            query_summary: Some("ESP32 pinout".into()),
            duration: Duration::from_millis(12),
            num_chunks: 4,
            num_boards: 1,
            channel: Some("cli".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-m1".into()),
        });
        obs.record_event(&ObserverEvent::MemoryStore {
            category: "conversation".into(),
            backend: "sqlite".into(),
            duration: Duration::from_millis(8),
            success: true,
            channel: Some("cli".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-m1".into()),
        });

        assert!(
            obs.active_agent_spans
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key("turn-m1"),
            "memory/RAG child events must not remove the live turn entry"
        );

        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            duration: Duration::from_millis(90),
            tokens_used: None,
            cost_usd: None,
            channel: Some("cli".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-m1".into()),
        });
        assert!(
            !obs.active_agent_spans
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key("turn-m1"),
            "AgentEnd should close the live span"
        );

        // Unknown or absent turn_id degrades to a root span, no panic.
        obs.record_event(&ObserverEvent::MemoryRecall {
            query_summary: None,
            duration: Duration::from_millis(5),
            num_entries: 0,
            backend: "sqlite".into(),
            success: false,
            channel: None,
            agent_alias: None,
            turn_id: Some("no-such-turn".into()),
        });
    }

    #[test]
    fn otel_observer_creation_with_valid_endpoint_succeeds() {
        // Even though endpoint is unreachable, creation should succeed
        let result = OtelObserver::new(
            Some("http://127.0.0.1:12345"),
            Some("zeroclaw-test"),
            None,
            all_off_config(),
        );
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
        let result = OtelObserver::new(
            Some("http://127.0.0.1:12345"),
            Some("test"),
            Some(headers),
            all_off_config(),
        );
        assert!(
            result.is_ok(),
            "observer creation with headers must succeed"
        );
    }

    #[test]
    fn otel_observer_with_headers_records_events() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer sk-test".to_string());
        let obs = OtelObserver::new(
            Some("http://127.0.0.1:19999"),
            Some("test"),
            Some(headers),
            all_off_config(),
        )
        .expect("creation should succeed");
        obs.record_event(&ObserverEvent::LlmResponse {
            parent_agent_alias: None,
            model_provider: "anthropic".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(100),
            success: true,
            error_message: None,
            input_tokens: Some(10),
            output_tokens: Some(5),
            messages: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            parent_agent_alias: None,
            tool: "shell".into(),
            tool_call_id: None,
            duration: Duration::from_millis(50),
            success: true,
            arguments: None,
            result: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
    }

    #[test]
    fn otel_observer_with_empty_headers_succeeds() {
        let result = OtelObserver::new(
            Some("http://127.0.0.1:12345"),
            Some("test"),
            Some(HashMap::new()),
            all_off_config(),
        );
        assert!(
            result.is_ok(),
            "observer creation with empty headers must succeed"
        );
    }

    #[test]
    fn genai_policy_off_is_not_changed_by_later_full_config() {
        let snap = sample_llm_snapshot();
        let off = all_off_config();
        let full = genai_full_config();

        assert!(message_attrs(&Some(snap.clone()), off).is_empty());
        assert!(!message_attrs(&Some(snap.clone()), full).is_empty());
        // The later `full` config must not have mutated the earlier `off`.
        assert!(message_attrs(&Some(snap), off).is_empty());
    }

    #[test]
    fn genai_policy_full_is_not_changed_by_later_off_config() {
        let snap = sample_llm_snapshot();
        let full = genai_full_config();
        let off = all_off_config();

        assert!(!message_attrs(&Some(snap.clone()), full).is_empty());
        assert!(message_attrs(&Some(snap.clone()), off).is_empty());
        // The later `off` config must not have silenced the earlier `full`.
        assert!(!message_attrs(&Some(snap), full).is_empty());
    }

    #[test]
    fn tool_io_policy_is_instance_owned() {
        let off = all_off_config();
        let full = tool_io_full_config();

        let off_attrs = tool_result_content_attrs(Some(r#"{"cmd":"ls"}"#), Some("ok"), off);
        assert!(off_attrs.is_empty());

        let full_attrs = tool_result_content_attrs(Some(r#"{"cmd":"ls"}"#), Some("ok"), full);
        assert!(attr_value(&full_attrs, "gen_ai.tool.arguments").is_some());
        assert!(attr_value(&full_attrs, "gen_ai.tool.result").is_some());
        assert!(attr_value(&full_attrs, "input.value").is_some());
        assert!(attr_value(&full_attrs, "output.value").is_some());

        // The earlier `off` config is unaffected by the `full` config used above.
        let off_attrs_again = tool_result_content_attrs(Some(r#"{"cmd":"ls"}"#), Some("ok"), off);
        assert!(off_attrs_again.is_empty());
    }

    #[test]
    fn tool_start_content_attrs_off_returns_empty() {
        let attrs = tool_start_content_attrs(Some(r#"{"cmd":"ls"}"#), all_off_config());
        assert!(attrs.is_empty());
    }

    #[test]
    fn tool_start_content_attrs_full_emits_arguments() {
        let attrs = tool_start_content_attrs(Some(r#"{"cmd":"ls"}"#), tool_io_full_config());
        assert!(attr_value(&attrs, "gen_ai.tool.arguments").is_some());
        assert!(attr_value(&attrs, "input.value").is_some());
        assert!(attr_value(&attrs, "gen_ai.tool.result").is_none());
        assert!(attr_value(&attrs, "output.value").is_none());
    }

    #[test]
    fn process_agent_message_emits_genai_messages_json() {
        // Full policy: clean_for_display strips the memory-context block, no
        // truncation; output is a single-message GenAI JSON array.
        let val = process_agent_message(
            "hi [Memory context]old[/Memory context]",
            "user",
            OtelContentPolicy::Full,
            10_000,
        )
        .expect("Some under non-off policy");
        let parsed: serde_json::Value = serde_json::from_str(&val).unwrap();
        assert_eq!(parsed[0]["role"], "user");
        assert_eq!(parsed[0]["content"], "hi");
    }

    #[test]
    fn process_agent_message_redacted_truncates() {
        // Truncation drops the content entirely → None.
        assert!(process_agent_message("abcdef", "user", OtelContentPolicy::Redacted, 0,).is_none());
    }

    #[test]
    fn content_config_normalizes_zero_max_chars_to_off() {
        use zeroclaw_config::schema::{ObservabilityConfig, OtelContentPolicy};

        let cfg = ObservabilityConfig {
            otel_genai_content: OtelContentPolicy::Full,
            otel_genai_content_max_chars: 0,
            otel_tool_io: OtelContentPolicy::Full,
            otel_tool_io_max_chars: 0,
            ..ObservabilityConfig::default()
        };

        let content = OtelContentConfig::from_observability_config(&cfg);
        assert_eq!(content.genai_policy, OtelContentPolicy::Off);
        assert_eq!(content.tool_io_policy, OtelContentPolicy::Off);
    }

    #[test]
    fn content_config_preserves_nonzero_max_chars_policy() {
        use zeroclaw_config::schema::{ObservabilityConfig, OtelContentPolicy};

        let cfg = ObservabilityConfig {
            otel_genai_content: OtelContentPolicy::Redacted,
            otel_genai_content_max_chars: 500,
            otel_tool_io: OtelContentPolicy::Full,
            otel_tool_io_max_chars: 800,
            ..ObservabilityConfig::default()
        };

        let content = OtelContentConfig::from_observability_config(&cfg);
        assert_eq!(content.genai_policy, OtelContentPolicy::Redacted);
        assert_eq!(content.genai_max_chars, 500);
        assert_eq!(content.tool_io_policy, OtelContentPolicy::Full);
        assert_eq!(content.tool_io_max_chars, 800);
    }

    // ── clean_for_display tests ───────────────────────────────────────

    #[test]
    fn clean_for_display_removes_memory_context() {
        let input =
            "What is the weather today[Memory context]previous conversation[/Memory context]";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "What is the weather today");
    }

    #[test]
    fn clean_for_display_removes_timestamps() {
        let input = "Hello world[2026-06-30 16:44:51 +08:00]";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "Hello world");
    }

    #[test]
    fn clean_for_display_removes_timestamps_with_different_tz() {
        let input = "Hello world[2026-06-30 16:44:51 UTC]";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "Hello world");
    }

    #[test]
    fn clean_for_display_removes_thinking_blocks() {
        let input = "Answer<thinking>Let me think about this</thinking>Here is the answer";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "AnswerHere is the answer");
    }

    #[test]
    fn clean_for_display_removes_think_blocks() {
        let input = "Answer<think>Let me think about this</think>Here is the answer";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "AnswerHere is the answer");
    }

    #[test]
    fn clean_for_display_removes_tool_result_blocks() {
        let input = "Result<tool_result>some tool output</tool_result>Final answer";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "ResultFinal answer");
    }

    #[test]
    fn clean_for_display_removes_tool_results_prefix() {
        let input = "[Tool results]\nHere is the answer";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "Here is the answer");
    }

    #[test]
    fn clean_for_display_handles_combined_patterns() {
        let input = "What is the weather today[Memory context]old history[/Memory context][2026-06-30 16:44:51 +08:00]";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "What is the weather today");
    }

    #[test]
    fn clean_for_display_preserves_clean_content() {
        let input = "This is a clean message";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "This is a clean message");
    }

    #[test]
    fn clean_for_display_handles_empty_input() {
        let input = "";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "");
    }

    #[test]
    fn clean_for_display_handles_whitespace_only() {
        let input = "   \n\n   ";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "");
    }

    // Unclosed tags should be preserved (no removal)
    #[test]
    fn clean_for_display_preserves_unclosed_memory_context() {
        let input = "What is the weather today[Memory context]some content";
        let cleaned = clean_for_display(input);
        assert_eq!(
            cleaned,
            "What is the weather today[Memory context]some content"
        );
    }

    #[test]
    fn clean_for_display_preserves_unclosed_thinking() {
        let input = "Answer<thinking>Let me think";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "Answer<thinking>Let me think");
    }

    #[test]
    fn clean_for_display_preserves_unclosed_think() {
        let input = "Answer<think>Let me think";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "Answer<think>Let me think");
    }

    #[test]
    fn clean_for_display_preserves_unclosed_tool_result() {
        let input = "Result<tool_result>some tool output";
        let cleaned = clean_for_display(input);
        assert_eq!(cleaned, "Result<tool_result>some tool output");
    }
}
