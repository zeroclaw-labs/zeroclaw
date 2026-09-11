//! Native OTLP log export for the canonical `zeroclaw-log` event stream.
//!
//! The local JSONL/ECS projection remains unchanged for the dashboard, CLI,
//! TUI, and file shippers. When `backend = "otel"`, this bridge additionally
//! maps each canonical event into the stable OpenTelemetry Logs Data Model and
//! lets the official SDK batch/export an OTLP/HTTP protobuf request to
//! `<otel_endpoint>/v1/logs`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use opentelemetry::logs::{
    AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity as OtelSeverity,
};
use opentelemetry::trace::{SpanId, TraceId};
use opentelemetry::{InstrumentationScope, Key, KeyValue};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::logs::{SdkLogger, SdkLoggerProvider};
use zeroclaw_config::schema::{ObservabilityBackend, ObservabilityConfig};
use zeroclaw_log::{LogEvent, LogRecordExporter};

pub(super) fn configure(config: &ObservabilityConfig) {
    zeroclaw_log::clear_log_exporter();
    if config.backend != ObservabilityBackend::Otel {
        return;
    }

    match OtlpLogExporter::new(
        config.otel_endpoint.as_deref(),
        config.otel_service_name.as_deref(),
        config.otel_headers.clone(),
    ) {
        Ok(exporter) => {
            zeroclaw_log::set_log_exporter(Arc::new(exporter));
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "endpoint": format!(
                            "{}/v1/logs",
                            config
                                .otel_endpoint
                                .as_deref()
                                .unwrap_or("http://localhost:4318")
                                .trim_end_matches('/')
                        ),
                        "protocol": "http/protobuf",
                    })),
                "OpenTelemetry log exporter initialized"
            );
        }
        Err(error) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": error})),
                "Failed to create OTel log exporter; local logs remain available"
            );
        }
    }
}

struct OtlpLogExporter {
    provider: SdkLoggerProvider,
    logger: SdkLogger,
}

impl OtlpLogExporter {
    fn new(
        endpoint: Option<&str>,
        service_name: Option<&str>,
        headers: Option<HashMap<String, String>>,
    ) -> Result<Self, String> {
        let base_endpoint = endpoint.unwrap_or("http://localhost:4318");
        let logs_endpoint = format!("{}/v1/logs", base_endpoint.trim_end_matches('/'));
        let mut exporter_builder = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_endpoint(logs_endpoint);
        if let Some(headers) = headers {
            exporter_builder = exporter_builder.with_headers(headers);
        }
        let exporter = exporter_builder
            .build()
            .map_err(|error| format!("Failed to create OTLP log exporter: {error}"))?;

        let resource = opentelemetry_sdk::Resource::builder()
            .with_service_name(service_name.unwrap_or("zeroclaw").to_string())
            .with_attribute(KeyValue::new(
                "service.version",
                env!("CARGO_PKG_VERSION").to_string(),
            ))
            .build();
        let provider = SdkLoggerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(exporter)
            .build();
        Ok(Self::from_provider(provider))
    }

    fn from_provider(provider: SdkLoggerProvider) -> Self {
        let scope = InstrumentationScope::builder("zeroclaw.log")
            .with_version(env!("CARGO_PKG_VERSION"))
            .build();
        let logger = provider.logger_with_scope(scope);
        Self { provider, logger }
    }

    fn emit_event(&self, event: &LogEvent) {
        let mut record = self.logger.create_log_record();
        if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&event.timestamp) {
            record.set_timestamp(SystemTime::from(timestamp));
        }
        record.set_observed_timestamp(SystemTime::now());

        let severity = otel_severity(event.severity_number);
        record.set_severity_number(severity);
        record.set_severity_text(severity.name());
        record.set_target(format!("zeroclaw.{}", event.event.category));
        record.set_body(AnyValue::String(
            event
                .message
                .clone()
                .unwrap_or_else(|| format!("{}.{}", event.event.category, event.event.action))
                .into(),
        ));

        if let Some((trace_id, span_id)) = trace_context(event) {
            record.set_trace_context(trace_id, span_id, None);
        }
        record.add_attributes(otel_attributes(event));
        self.logger.emit(record);
    }
}

impl LogRecordExporter for OtlpLogExporter {
    fn emit(&self, event: &LogEvent) {
        self.emit_event(event);
    }

    fn force_flush(&self) -> Result<(), String> {
        self.provider
            .force_flush()
            .map_err(|error| error.to_string())
    }
}

fn otel_severity(number: u8) -> OtelSeverity {
    match number {
        1 => OtelSeverity::Trace,
        2 => OtelSeverity::Trace2,
        3 => OtelSeverity::Trace3,
        4 => OtelSeverity::Trace4,
        5 => OtelSeverity::Debug,
        6 => OtelSeverity::Debug2,
        7 => OtelSeverity::Debug3,
        8 => OtelSeverity::Debug4,
        9 => OtelSeverity::Info,
        10 => OtelSeverity::Info2,
        11 => OtelSeverity::Info3,
        12 => OtelSeverity::Info4,
        13 => OtelSeverity::Warn,
        14 => OtelSeverity::Warn2,
        15 => OtelSeverity::Warn3,
        16 => OtelSeverity::Warn4,
        17 => OtelSeverity::Error,
        18 => OtelSeverity::Error2,
        19 => OtelSeverity::Error3,
        20 => OtelSeverity::Error4,
        21 => OtelSeverity::Fatal,
        22 => OtelSeverity::Fatal2,
        23 => OtelSeverity::Fatal3,
        _ => OtelSeverity::Fatal4,
    }
}

fn trace_context(event: &LogEvent) -> Option<(TraceId, SpanId)> {
    let trace = event.trace_id.as_deref()?.replace('-', "");
    let span = event.span_id.as_deref()?.replace('-', "");
    if trace.len() != 32 || span.len() != 16 {
        return None;
    }
    let trace_id = TraceId::from_hex(&trace).ok()?;
    let span_id = SpanId::from_hex(&span).ok()?;
    if trace_id == TraceId::INVALID || span_id == SpanId::INVALID {
        return None;
    }
    Some((trace_id, span_id))
}

fn otel_attributes(event: &LogEvent) -> Vec<(Key, AnyValue)> {
    let mut attributes = HashMap::<String, AnyValue>::new();

    if let Some(object) = event.attributes.as_object() {
        for (key, value) in object {
            if key == "_file" || key == "_line" {
                continue;
            }
            attributes.insert(key.clone(), json_to_any_value(value));
        }
        if let Some(path) = object.get("_file").and_then(serde_json::Value::as_str) {
            attributes.insert("code.file.path".into(), path.to_string().into());
        }
        if let Some(line) = object.get("_line").and_then(serde_json::Value::as_i64) {
            attributes.insert("code.line.number".into(), line.into());
        }
    } else if !event.attributes.is_null() {
        attributes.insert(
            "zeroclaw.attributes".into(),
            json_to_any_value(&event.attributes),
        );
    }

    attributes.insert("event.id".into(), event.id.clone().into());
    attributes.insert("event.category".into(), event.event.category.clone().into());
    attributes.insert("event.action".into(), event.event.action.clone().into());
    if !event.event.outcome.is_empty() && event.event.outcome != "unknown" {
        attributes.insert("event.outcome".into(), event.event.outcome.clone().into());
    }
    attributes.insert(
        "zeroclaw.schema_version".into(),
        i64::from(event.schema_version).into(),
    );

    for (key, value) in &event.zeroclaw.fields {
        attributes.insert(format!("zeroclaw.{key}"), value.clone().into());
    }
    if let Some(duration_ms) = event.zeroclaw.duration_ms {
        let duration_ms = i64::try_from(duration_ms).unwrap_or(i64::MAX);
        attributes.insert("zeroclaw.duration_ms".into(), duration_ms.into());
        attributes.insert(
            "event.duration".into(),
            duration_ms.saturating_mul(1_000_000).into(),
        );
    }
    if let Some(trace_id) = &event.trace_id {
        attributes.insert("zeroclaw.trace_id".into(), trace_id.clone().into());
    }
    if let Some(span_id) = &event.span_id {
        attributes.insert("zeroclaw.span_id".into(), span_id.clone().into());
    }

    let provider = event
        .zeroclaw
        .get("model_provider_type")
        .or_else(|| event.zeroclaw.get("model_provider"));
    if let Some(provider) = provider {
        attributes.insert("gen_ai.provider.name".into(), provider.to_string().into());
    }
    let model = event.zeroclaw.get("model").or_else(|| {
        event
            .attributes
            .get("model")
            .and_then(serde_json::Value::as_str)
    });
    if let Some(model) = model {
        let model_key = if event.event.category == "provider" && event.event.action == "receive" {
            "gen_ai.response.model"
        } else {
            "gen_ai.request.model"
        };
        attributes.insert(model_key.into(), model.to_string().into());
    }
    let tool = event.zeroclaw.get("tool").or_else(|| {
        event
            .attributes
            .get("tool")
            .and_then(serde_json::Value::as_str)
    });
    if let Some(tool) = tool {
        attributes.insert("tool.name".into(), tool.to_string().into());
    }
    if let Some(input_tokens) = event
        .attributes
        .get("input_tokens")
        .and_then(serde_json::Value::as_i64)
    {
        attributes.insert("gen_ai.usage.input_tokens".into(), input_tokens.into());
    }
    if let Some(output_tokens) = event
        .attributes
        .get("output_tokens")
        .and_then(serde_json::Value::as_i64)
    {
        attributes.insert("gen_ai.usage.output_tokens".into(), output_tokens.into());
    }

    attributes
        .into_iter()
        .map(|(key, value)| (Key::new(key), value))
        .collect()
}

fn json_to_any_value(value: &serde_json::Value) -> AnyValue {
    match value {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(value) => (*value).into(),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                value.into()
            } else if let Some(value) = value.as_u64() {
                i64::try_from(value)
                    .map(AnyValue::from)
                    .unwrap_or_else(|_| value.to_string().into())
            } else {
                value.as_f64().unwrap_or_default().into()
            }
        }
        serde_json::Value::String(value) => value.clone().into(),
        serde_json::Value::Array(values) => {
            AnyValue::ListAny(Box::new(values.iter().map(json_to_any_value).collect()))
        }
        serde_json::Value::Object(values) => AnyValue::Map(Box::new(
            values
                .iter()
                .map(|(key, value)| (Key::new(key.clone()), json_to_any_value(value)))
                .collect(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::routing::post;
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::logs::{LogProcessor, SdkLogRecord};
    use parking_lot::Mutex;
    use prost::Message as _;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use zeroclaw_log::{EventCategory, EventOutcome, Severity, ZeroclawAttribution};

    #[derive(Clone, Debug)]
    struct CaptureProcessor {
        records: Arc<Mutex<Vec<SdkLogRecord>>>,
    }

    impl LogProcessor for CaptureProcessor {
        fn emit(&self, record: &mut SdkLogRecord, _scope: &InstrumentationScope) {
            self.records.lock().push(record.clone());
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            Ok(())
        }

        fn set_resource(&mut self, _resource: &Resource) {}
    }

    #[test]
    fn canonical_event_maps_to_otel_log_record_without_ephemeral_data() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let provider = SdkLoggerProvider::builder()
            .with_log_processor(CaptureProcessor {
                records: Arc::clone(&records),
            })
            .build();
        let exporter = OtlpLogExporter::from_provider(provider);

        let mut event = LogEvent::new(Severity::Warn, "receive", EventCategory::Provider);
        event.timestamp = "2026-08-21T04:45:44.645Z".into();
        event.message = Some("llm_response".into());
        event.set_outcome(EventOutcome::Success);
        event.trace_id = Some("5b8efff798038103d269b633813fc60c".into());
        event.span_id = Some("eee19b7ec3c1b174".into());
        let mut attribution = ZeroclawAttribution::default();
        attribution.set_composite("model_provider", "openai.mock");
        attribution.set("model", "demo-model");
        attribution.set("sop_run_id", "run-1");
        attribution.duration_ms = Some(13);
        event.zeroclaw = attribution;
        event.attributes = serde_json::json!({
            "_file": "crates/runtime.rs",
            "_line": 42,
            "input_tokens": 10,
            "output_tokens": 5,
            "nested": {"ok": true},
        });
        event.ephemeral_attributes = serde_json::json!({"pairing_code": "never-export"});

        exporter.emit_event(&event);

        let records = records.lock();
        let record = records.first().expect("one OTLP log record");
        assert_eq!(record.severity_number(), Some(OtelSeverity::Warn));
        assert_eq!(record.severity_text(), Some("WARN"));
        assert_eq!(
            record.body(),
            Some(&AnyValue::from("llm_response".to_string()))
        );
        let trace = record.trace_context().expect("valid trace context");
        assert_eq!(trace.trace_id.to_string(), event.trace_id.unwrap());
        assert_eq!(trace.span_id.to_string(), event.span_id.unwrap());

        let attributes: HashMap<&str, &AnyValue> = record
            .attributes_iter()
            .map(|(key, value)| (key.as_str(), value))
            .collect();
        assert_eq!(
            attributes.get("gen_ai.provider.name"),
            Some(&&AnyValue::from("openai".to_string()))
        );
        assert_eq!(
            attributes.get("gen_ai.response.model"),
            Some(&&AnyValue::from("demo-model".to_string()))
        );
        assert_eq!(
            attributes.get("zeroclaw.sop_run_id"),
            Some(&&AnyValue::from("run-1".to_string()))
        );
        assert_eq!(
            attributes.get("code.line.number"),
            Some(&&AnyValue::from(42_i64))
        );
        assert!(!attributes.contains_key("pairing_code"));
        assert!(
            attributes
                .values()
                .all(|value| !format!("{value:?}").contains("never-export"))
        );
    }

    #[test]
    fn malformed_application_trace_ids_remain_attributes_not_otel_context() {
        let mut event = LogEvent::new(Severity::Info, "send", EventCategory::Provider);
        event.trace_id = Some("application-turn-id".into());
        event.span_id = Some("step-id".into());
        assert!(trace_context(&event).is_none());

        let attributes = otel_attributes(&event);
        assert!(attributes.iter().any(|(key, value)| {
            key.as_str() == "zeroclaw.trace_id"
                && value == &AnyValue::from("application-turn-id".to_string())
        }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exporter_posts_decodable_otlp_protobuf_to_standard_logs_endpoint() {
        type Capture = (HeaderMap, Bytes);

        async fn capture(
            State(sender): State<mpsc::Sender<Capture>>,
            headers: HeaderMap,
            body: Bytes,
        ) -> (StatusCode, [(header::HeaderName, &'static str); 1], Bytes) {
            let _ = sender.send((headers, body)).await;
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/x-protobuf")],
                Bytes::new(),
            )
        }

        let (sender, mut receiver) = mpsc::channel::<Capture>(1);
        let app = Router::new()
            .route("/v1/logs", post(capture))
            .with_state(sender);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("OTLP capture listener");
        let address = listener.local_addr().expect("capture address");
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("OTLP capture server");
        });

        let exporter = OtlpLogExporter::new(
            Some(&format!("http://{address}")),
            Some("zeroclaw-test"),
            None,
        )
        .expect("OTLP log exporter");
        let mut event = LogEvent::new(Severity::Info, "complete", EventCategory::Tool);
        event.message = Some("tool_call_result".into());
        event.attributes = serde_json::json!({"tool": "shell", "output": "ok"});
        exporter.emit_event(&event);
        exporter.force_flush().expect("flush OTLP log batch");

        let (headers, body) = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .expect("OTLP export timeout")
            .expect("one OTLP request");
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/x-protobuf")
        );
        let request = ExportLogsServiceRequest::decode(body)
            .expect("standard ExportLogsServiceRequest protobuf");
        let resource_logs = request.resource_logs.first().expect("resource logs");
        let resource = resource_logs.resource.as_ref().expect("resource");
        assert!(resource.attributes.iter().any(|attribute| {
            attribute.key == "service.name"
                && attribute
                    .value
                    .as_ref()
                    .and_then(|value| value.value.as_ref())
                    .is_some_and(|value| {
                        matches!(
                            value,
                            opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(name)
                                if name == "zeroclaw-test"
                        )
                    })
        }));
        let record = resource_logs
            .scope_logs
            .first()
            .and_then(|scope| scope.log_records.first())
            .expect("one OTLP log record");
        assert_eq!(record.severity_number, i32::from(OtelSeverity::Info as u8));
        assert_eq!(record.severity_text, "INFO");
        assert!(record.body.as_ref().is_some_and(|body| {
            matches!(
                body.value.as_ref(),
                Some(opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(message))
                    if message == "tool_call_result"
            )
        }));

        server.abort();
    }
}
