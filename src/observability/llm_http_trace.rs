use crate::config::ObservabilityConfig;
use crate::observability::runtime_trace;
use anyhow::Result;
use axum::http::Response as HttpResponse;
use http_body_util::BodyExt;
use reqwest::header::ACCEPT;
use reqwest::header::{HeaderMap, HeaderName};
use reqwest_middleware::ClientBuilder;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use uuid::Uuid;

const HEADER_VALUE_PREVIEW_LIMIT: usize = 4096;
const BODY_PREVIEW_LIMIT: usize = 1 * 1024 * 1024;
const MAX_RESPONSE_CONTENT_BYTES: usize = 2 * 1024 * 1024;

static RUNTIME_TRACE_RECORD_HTTP: AtomicBool = AtomicBool::new(false);

pub fn init_from_config(config: &ObservabilityConfig) {
    let trace_storage_enabled =
        crate::observability::runtime_trace::storage_mode_from_config(config)
            != crate::observability::runtime_trace::RuntimeTraceStorageMode::None;
    RUNTIME_TRACE_RECORD_HTTP.store(
        config.runtime_trace_record_http && trace_storage_enabled,
        Ordering::Relaxed,
    );
}

fn runtime_trace_record_http_enabled() -> bool {
    RUNTIME_TRACE_RECORD_HTTP.load(Ordering::Relaxed)
}

pub async fn send_with_middleware(
    service_key: &str,
    request_builder: reqwest::RequestBuilder,
) -> Result<reqwest::Response> {
    if !runtime_trace_record_http_enabled() {
        return Ok(request_builder.send().await?);
    }

    let (client, request) = request_builder.build_split();
    let request = request?;
    let traced_client = ClientBuilder::new(client)
        .with(reqwest_tracing::TracingMiddleware::<
            reqwest_tracing::DefaultSpanBackend,
        >::new())
        .build();

    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();
    let method = request.method().to_string();
    let url = sanitize_url(request.url());
    let request_headers = sanitize_headers(request.headers());
    let request_body = request_body_preview(&request);
    let provider = provider_name(service_key);
    let streaming_request = request
        .headers()
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false);

    runtime_trace::record_event(
        "llm_http_request",
        None,
        provider,
        None,
        None,
        None,
        None,
        json!({
            "request_id": request_id,
            "service_key": service_key,
            "method": method,
            "url": url,
            "headers": request_headers,
            "body": request_body,
        }),
    );

    match traced_client.execute(request).await {
        Ok(response) => {
            let duration_ms = started.elapsed().as_millis();
            let status = response.status();
            let response_content_length = response.content_length();
            let response_headers = sanitize_headers(response.headers());

            if !streaming_request && !is_streaming_response(&response) {
                let http_response: HttpResponse<reqwest::Body> = response.into();
                let (parts, body) = http_response.into_parts();
                let body_bytes = BodyExt::collect(body).await?.to_bytes();
                let (response_content, truncated) =
                    response_content_payload_from_bytes(&body_bytes, MAX_RESPONSE_CONTENT_BYTES);

                runtime_trace::record_event(
                    "llm_http_response",
                    None,
                    provider,
                    None,
                    None,
                    Some(status.is_success()),
                    None,
                    json!({
                        "request_id": request_id,
                        "service_key": service_key,
                        "status": status.as_u16(),
                        "headers": response_headers,
                        "content_length": response_content_length,
                        "duration_ms": duration_ms,
                        "content": response_content,
                        "content_truncated": truncated,
                    }),
                );

                let rebuilt = HttpResponse::from_parts(parts, reqwest::Body::from(body_bytes));
                let response = reqwest::Response::from(rebuilt);
                return Ok(response);
            }

            runtime_trace::record_event(
                "llm_http_response",
                None,
                provider,
                None,
                None,
                Some(status.is_success()),
                None,
                json!({
                    "request_id": request_id,
                    "service_key": service_key,
                    "status": status.as_u16(),
                    "headers": response_headers,
                    "content_length": response_content_length,
                    "duration_ms": duration_ms,
                    "content": Value::Null,
                    "content_truncated": false,
                }),
            );

            Ok(response)
        }
        Err(err) => {
            let duration_ms = started.elapsed().as_millis();
            let message = crate::providers::sanitize_api_error(&err.to_string());
            runtime_trace::record_event(
                "llm_http_response",
                None,
                provider,
                None,
                None,
                Some(false),
                Some(&message),
                json!({
                    "request_id": request_id,
                    "service_key": service_key,
                    "duration_ms": duration_ms,
                }),
            );
            Err(match err {
                reqwest_middleware::Error::Reqwest(reqwest_err) => reqwest_err.into(),
                other => anyhow::anyhow!("{other}"),
            })
        }
    }
}

fn provider_name(service_key: &str) -> Option<&str> {
    service_key.strip_prefix("provider.").or(Some(service_key))
}

fn request_body_preview(request: &reqwest::Request) -> Value {
    match request.body().and_then(reqwest::Body::as_bytes) {
        Some(bytes) => {
            let raw = String::from_utf8_lossy(bytes);
            if let Ok(mut value) = serde_json::from_str::<Value>(&raw) {
                sanitize_json_value_in_place(&mut value);
                value
            } else {
                Value::String(sanitize_text_preview(&raw, BODY_PREVIEW_LIMIT))
            }
        }
        None => Value::Null,
    }
}

fn sanitize_json_value_in_place(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map.iter_mut() {
                if is_sensitive_query_key(key) {
                    *nested = Value::String("[REDACTED]".to_string());
                    continue;
                }
                sanitize_json_value_in_place(nested);
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_json_value_in_place(item);
            }
        }
        Value::String(s) => {
            *s = sanitize_text_preview(s, BODY_PREVIEW_LIMIT);
        }
        _ => {}
    }
}

fn sanitize_headers(headers: &HeaderMap) -> Value {
    let mut out = Map::new();
    for (name, value) in headers {
        let key = name.as_str().to_ascii_lowercase();
        let value = if is_sensitive_header(name) {
            "[REDACTED]".to_string()
        } else {
            let rendered = value
                .to_str()
                .map(|v| sanitize_text_preview(v, HEADER_VALUE_PREVIEW_LIMIT))
                .unwrap_or_else(|_| "<non-utf8>".to_string());
            if rendered.is_empty() {
                "<empty>".to_string()
            } else {
                rendered
            }
        };
        out.insert(key, Value::String(value));
    }
    Value::Object(out)
}

fn sanitize_url(url: &reqwest::Url) -> String {
    let mut sanitized = url.clone();
    if sanitized.query().is_none() {
        return sanitized.to_string();
    }

    let pairs: Vec<(String, String)> = sanitized
        .query_pairs()
        .map(|(key, value)| {
            let key = key.to_string();
            if is_sensitive_query_key(&key) {
                (key, "[REDACTED]".to_string())
            } else {
                (
                    key,
                    sanitize_text_preview(value.as_ref(), HEADER_VALUE_PREVIEW_LIMIT),
                )
            }
        })
        .collect();
    sanitized.set_query(None);
    {
        let mut qp = sanitized.query_pairs_mut();
        for (key, value) in pairs {
            qp.append_pair(&key, &value);
        }
    }
    sanitized.to_string()
}

fn sanitize_text_preview(text: &str, limit: usize) -> String {
    let scrubbed = sanitize_text(text);
    truncate_with_ellipsis(&scrubbed, limit)
}

fn sanitize_text(text: &str) -> String {
    crate::providers::scrub_secret_patterns(text)
}

fn truncate_with_ellipsis(input: &str, limit: usize) -> String {
    if input.chars().count() <= limit {
        return input.to_string();
    }
    let truncated: String = input.chars().take(limit).collect();
    format!("{truncated}...")
}

fn is_streaming_response(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|value| {
            let lower = value.to_ascii_lowercase();
            lower.contains("text/event-stream")
                || lower.contains("application/vnd.amazon.eventstream")
        })
        .unwrap_or(false)
}

fn response_content_payload_from_bytes(bytes: &[u8], max_bytes: usize) -> (Value, bool) {
    let (effective, truncated) = if bytes.len() > max_bytes {
        (&bytes[..max_bytes], true)
    } else {
        (bytes, false)
    };
    let raw = String::from_utf8_lossy(effective);
    if let Ok(mut value) = serde_json::from_str::<Value>(&raw) {
        sanitize_json_value_in_place(&mut value);
        (value, truncated)
    } else {
        (Value::String(sanitize_text(&raw)), truncated)
    }
}

fn is_sensitive_header(header: &HeaderName) -> bool {
    let key = header.as_str().to_ascii_lowercase();
    matches!(
        key.as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-api-key"
            | "api-key"
            | "x-auth-token"
    ) || key.contains("api-key")
        || key.contains("access-token")
        || key.contains("session-token")
}

fn is_sensitive_query_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("token")
        || key.contains("key")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("signature")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_url_redacts_sensitive_query_params() {
        let raw = reqwest::Url::parse("https://example.com/v1/test?api_key=abc&safe=ok").unwrap();
        let sanitized = sanitize_url(&raw);
        assert!(sanitized.contains("api_key=%5BREDACTED%5D"));
        assert!(sanitized.contains("safe=ok"));
        assert!(!sanitized.contains("abc"));
    }

    #[test]
    fn sanitize_headers_redacts_authorization() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer secret-token".parse().unwrap());
        headers.insert("x-test", "ok".parse().unwrap());
        let value = sanitize_headers(&headers).to_string();
        assert!(value.contains("[REDACTED]"));
        assert!(value.contains("\"x-test\":\"ok\""));
        assert!(!value.contains("secret-token"));
    }
}
