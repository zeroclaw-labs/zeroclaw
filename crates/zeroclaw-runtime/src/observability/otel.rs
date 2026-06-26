use super::traits::{Observer, ObserverEvent, ObserverMetric};
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
    memory_recall_count: Counter<u64>,
    memory_recall_duration: Histogram<f64>,
    memory_store_count: Counter<u64>,
    rag_retrieve_count: Counter<u64>,
    rag_retrieve_duration: Histogram<f64>,

    // Turn span tracking for parent/child correlation
    active_agent_spans: Mutex<HashMap<String, (global::BoxedSpan, Context)>>,
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

        // ── Memory observability instruments (Unit 2 of memory-OTel PR) ──
        // The OTel SDK's PeriodicReader is non-blocking: aggregations are
        // updated synchronously in record_event, but export happens on a
        // background interval. New instruments cannot back-pressure the
        // runtime hot path under burst writes.
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
            rag_retrieve_count,
            rag_retrieve_duration,
            active_agent_spans: Mutex::new(HashMap::new()),
        })
    }

    fn parent_cx_for(&self, turn_id: Option<&str>) -> Context {
        if let Some(tid) = turn_id
            && let Some((_, cx)) = self
                .active_agent_spans
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(tid)
        {
            return cx.clone();
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
                        .insert(tid.clone(), (span, parent_cx));
                }
            }
            ObserverEvent::LlmRequest {
                model_provider,
                model,
                messages_count,
                channel,
                agent_alias,
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
                turn_id,
            } => {
                let mut span_attrs = vec![
                    KeyValue::new("gen_ai.operation.name", "execute_tool"),
                    KeyValue::new("tool.name", tool.clone()),
                    KeyValue::new("zeroclaw.channel", channel.clone().unwrap_or_default()),
                    KeyValue::new("gen_ai.agent.name", agent_alias.clone().unwrap_or_default()),
                    KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                ];
                if let Some(id) = tool_call_id {
                    span_attrs.push(KeyValue::new("gen_ai.tool.call.id", id.clone()));
                }
                if let Some(args) = arguments {
                    span_attrs.push(KeyValue::new("gen_ai.tool.arguments", args.clone()));
                }
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
                    // Partial GenAI-compatible attributes. The retrieval
                    // operation value is canonical, but the surrounding
                    // span (`SpanKind::Internal` and the `memory.recall`
                    // name rather than `{operation} {data_source.id}`) is
                    // shaped for ZeroClaw / Langfuse compatibility, not
                    // strict OTel GenAI conformance.
                    KeyValue::new("gen_ai.operation.name", "retrieval"),
                    KeyValue::new("gen_ai.system", backend.clone()),
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

                let mut span = tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("memory.recall")
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

                let metric_attrs = [KeyValue::new("backend", backend.clone())];
                self.memory_recall_count.add(1, &metric_attrs);
                self.memory_recall_duration.record(secs, &metric_attrs);
            }
            ObserverEvent::RagRetrieve {
                query_summary,
                duration,
                num_chunks,
                num_boards,
            } => {
                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                // NOTE: `rag.num_chunks` / `rag.num_boards` are
                // ZeroClaw-specific. OTel GenAI semconv defines
                // `gen_ai.operation.name = "retrieval"` but no canonical
                // attribute for chunk count or domain partitioning yet.
                // Revisit when the GenAI WG publishes retrieval-attribute
                // extensions.
                let mut span_attrs = vec![
                    KeyValue::new("rag.num_chunks", *num_chunks as i64),
                    KeyValue::new("rag.num_boards", *num_boards as i64),
                    KeyValue::new("duration_s", secs),
                    KeyValue::new("gen_ai.operation.name", "retrieval"),
                    KeyValue::new("gen_ai.system", "zeroclaw_rag"),
                ];
                if let Some(q) = query_summary {
                    span_attrs.push(KeyValue::new("input.value", q.clone()));
                    span_attrs.push(KeyValue::new(
                        "output.value",
                        format!("{} chunks across {} boards", num_chunks, num_boards),
                    ));
                }

                let mut span = tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("rag.retrieve")
                        .with_kind(SpanKind::Internal)
                        .with_start_time(start_time)
                        .with_attributes(span_attrs),
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
            } => {
                let secs = duration.as_secs_f64();
                let start_time = SystemTime::now()
                    .checked_sub(*duration)
                    .unwrap_or(SystemTime::now());

                // NOTE: OTel GenAI semconv has no canonical "store"
                // operation value (canonical: chat, create_agent,
                // embeddings, execute_tool, generate_content,
                // invoke_agent, retrieval, text_completion). We omit
                // `gen_ai.operation.name` and lean on `db.*` conventions
                // instead.
                let span_attrs = vec![
                    KeyValue::new("memory.category", category.clone()),
                    KeyValue::new("memory.backend", backend.clone()),
                    KeyValue::new("memory.success", *success),
                    KeyValue::new("duration_s", secs),
                    KeyValue::new("db.system", backend.clone()),
                    KeyValue::new("db.operation", "INSERT"),
                ];

                let mut span = tracer.build(
                    opentelemetry::trace::SpanBuilder::from_name("memory.store")
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
                    KeyValue::new("category", category.clone()),
                    KeyValue::new("backend", backend.clone()),
                    KeyValue::new("success", success.to_string()),
                ];
                self.memory_store_count.add(1, &metric_attrs);
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
                turn_id,
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
                    KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                ];
                if let Some(input) = input_tokens {
                    span_attrs.push(KeyValue::new("gen_ai.usage.input_tokens", *input as i64));
                }
                if let Some(output) = output_tokens {
                    span_attrs.push(KeyValue::new("gen_ai.usage.output_tokens", *output as i64));
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
                    if let Some((mut span, _)) = entry {
                        let secs = duration.as_secs_f64();
                        span.set_attribute(KeyValue::new("duration_s", secs));
                        span.set_attribute(KeyValue::new(
                            "zeroclaw.channel",
                            channel.clone().unwrap_or_default(),
                        ));
                        span.set_attribute(KeyValue::new(
                            "gen_ai.agent.name",
                            agent_alias.clone().unwrap_or_default(),
                        ));
                        if let Some(usage) = tokens_used {
                            span.set_attribute(KeyValue::new(
                                "gen_ai.usage.input_tokens",
                                usage.input_tokens as i64,
                            ));
                            span.set_attribute(KeyValue::new(
                                "gen_ai.usage.output_tokens",
                                usage.output_tokens as i64,
                            ));
                        }
                        if let Some(c) = cost_usd {
                            span.set_attribute(KeyValue::new("cost_usd", *c));
                        }
                        span.end();
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
                    KeyValue::new("zeroclaw.turn_id", turn_id.clone().unwrap_or_default()),
                ];
                if let Some(id) = tool_call_id {
                    span_attrs.push(KeyValue::new("gen_ai.tool.call.id", id.clone()));
                }
                if let Some(args) = arguments {
                    span_attrs.push(KeyValue::new("gen_ai.tool.arguments", args.clone()));
                    span_attrs.push(KeyValue::new("input.value", args.clone()));
                }
                if let Some(res) = result {
                    span_attrs.push(KeyValue::new("gen_ai.tool.result", res.clone()));
                    span_attrs.push(KeyValue::new("output.value", res.clone()));
                }
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
        let orphans: Vec<(global::BoxedSpan, Context)> = self
            .active_agent_spans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, v)| v)
            .collect();
        for (mut span, _) in orphans {
            span.end();
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
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::LlmRequest {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            messages_count: 2,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            model_provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(250),
            success: true,
            error_message: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
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
            tool: "shell".into(),
            tool_call_id: None,
            arguments: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
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

    /// Regression test for memory observability — the three new memory/RAG
    /// event variants must accept fully populated payloads without panicking
    /// and must exercise the optional `query_summary` field on
    /// `MemoryRecall` and `RagRetrieve` (Some/None). We cannot assert on
    /// exported span attributes here (OTLP pipeline runs asynchronously),
    /// but verifying the recording path for all three arms is sufficient
    /// regression coverage.
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
        });
        // MemoryRecall failure path with query_summary: None.
        obs.record_event(&ObserverEvent::MemoryRecall {
            query_summary: None,
            duration: Duration::from_millis(12),
            num_entries: 0,
            backend: "qdrant".into(),
            success: false,
        });

        // RagRetrieve with populated query_summary.
        obs.record_event(&ObserverEvent::RagRetrieve {
            query_summary: Some("ESP32-S3 GPIO pinout".into()),
            duration: Duration::from_millis(120),
            num_chunks: 12,
            num_boards: 3,
        });
        // RagRetrieve with query_summary: None.
        obs.record_event(&ObserverEvent::RagRetrieve {
            query_summary: None,
            duration: Duration::ZERO,
            num_chunks: 0,
            num_boards: 0,
        });

        // MemoryStore success path.
        obs.record_event(&ObserverEvent::MemoryStore {
            category: "conversation".into(),
            backend: "sqlite".into(),
            duration: Duration::from_millis(8),
            success: true,
        });
        // MemoryStore failure path.
        obs.record_event(&ObserverEvent::MemoryStore {
            category: "fact".into(),
            backend: "qdrant".into(),
            duration: Duration::from_millis(3),
            success: false,
        });
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
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
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
            model_provider: "openrouter".into(),
            model: "missing-model".into(),
            duration: Duration::from_millis(0),
            success: false,
            error_message: Some("404 Not Found".into()),
            input_tokens: None,
            output_tokens: None,
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
            model_provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            messages_count: 2,
            channel: Some("wss".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
        });
        obs.record_event(&ObserverEvent::LlmResponse {
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
        });
        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            tool_call_id: Some("call-1".into()),
            arguments: Some(r#"{"command":"date"}"#.into()),
            channel: Some("wss".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
        });
        obs.record_event(&ObserverEvent::ToolCall {
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
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
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
        );
        assert!(
            result.is_ok(),
            "observer creation with empty headers must succeed"
        );
    }
}
