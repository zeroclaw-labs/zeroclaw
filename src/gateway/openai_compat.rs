//! OpenAI-compatible `/v1/chat/completions` and `/v1/models` endpoints.
//!
//! These endpoints allow ZeroClaw to act as a drop-in replacement for the
//! OpenAI API, enabling any OpenAI-compatible client (e.g., `openai` Python
//! library, `curl`, Aura) to send chat requests through the gateway.

use super::AppState;
use crate::providers::traits::{ChatMessage, StreamOptions};
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Instant;
use uuid::Uuid;

/// Maximum body size for chat completions requests (512KB).
/// Chat histories with many messages can be much larger than the default 64KB gateway limit.
pub const CHAT_COMPLETIONS_MAX_BODY_SIZE: usize = 524_288;

// ══════════════════════════════════════════════════════════════════════════════
// REQUEST / RESPONSE TYPES
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsRequest {
    /// Model ID (e.g. "anthropic/claude-sonnet-4"). Falls back to gateway default.
    #[serde(default)]
    pub model: Option<String>,
    /// Conversation messages.
    pub messages: Vec<ChatCompletionsMessage>,
    /// Sampling temperature. Falls back to gateway default.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Whether to stream the response as SSE events.
    #[serde(default)]
    pub stream: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionsResponse {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatCompletionsChoice>,
    pub usage: ChatCompletionsUsage,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionsChoice {
    pub index: u32,
    pub message: ChatCompletionsResponseMessage,
    pub finish_reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionsResponseMessage {
    pub role: &'static str,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionsUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// SSE streaming chunk format.
#[derive(Debug, Serialize)]
struct ChatCompletionsChunk {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<ChunkChoice>,
}

#[derive(Debug, Serialize)]
struct ChunkChoice {
    index: u32,
    delta: ChunkDelta,
    finish_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct ChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ModelsResponse {
    pub object: &'static str,
    pub data: Vec<ModelObject>,
}

#[derive(Debug, Serialize)]
pub struct ModelObject {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub owned_by: String,
}

// ══════════════════════════════════════════════════════════════════════════════
// HANDLERS
// ══════════════════════════════════════════════════════════════════════════════

/// POST /v1/chat/completions — OpenAI-compatible chat endpoint.
pub async fn handle_v1_chat_completions(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // ── Rate limit ──
    let rate_key =
        super::client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if !state.rate_limiter.allow_webhook(&rate_key) {
        tracing::warn!("/v1/chat/completions rate limit exceeded");
        let err = serde_json::json!({
            "error": {
                "message": "Rate limit exceeded. Please retry later.",
                "type": "rate_limit_error",
                "code": "rate_limit_exceeded"
            }
        });
        return (StatusCode::TOO_MANY_REQUESTS, Json(err)).into_response();
    }

    // ── Bearer token auth (pairing) ──
    if state.pairing.require_pairing() {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = auth.strip_prefix("Bearer ").unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            tracing::warn!("/v1/chat/completions: rejected — not paired / invalid bearer token");
            let err = serde_json::json!({
                "error": {
                    "message": "Invalid API key. Pair first via POST /pair, then use Authorization: Bearer <token>",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            });
            return (StatusCode::UNAUTHORIZED, Json(err)).into_response();
        }
    }

    // ── Enforce body size limit (since this route uses a separate limit) ──
    if body.len() > CHAT_COMPLETIONS_MAX_BODY_SIZE {
        let err = serde_json::json!({
            "error": {
                "message": format!("Request body too large ({} bytes, max {})", body.len(), CHAT_COMPLETIONS_MAX_BODY_SIZE),
                "type": "invalid_request_error",
                "code": "request_too_large"
            }
        });
        return (StatusCode::PAYLOAD_TOO_LARGE, Json(err)).into_response();
    }

    // ── Parse body ──
    let request: ChatCompletionsRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            tracing::warn!("/v1/chat/completions JSON parse error: {e}");
            let err = serde_json::json!({
                "error": {
                    "message": format!("Invalid JSON body: {e}"),
                    "type": "invalid_request_error",
                    "code": "invalid_json"
                }
            });
            return (StatusCode::BAD_REQUEST, Json(err)).into_response();
        }
    };

    if request.messages.is_empty() {
        let err = serde_json::json!({
            "error": {
                "message": "messages array must not be empty",
                "type": "invalid_request_error",
                "code": "invalid_messages"
            }
        });
        return (StatusCode::BAD_REQUEST, Json(err)).into_response();
    }

    let model = request
        .model
        .as_deref()
        .filter(|m| !m.is_empty())
        .unwrap_or(&state.model)
        .to_string();
    let temperature = request.temperature.unwrap_or(state.temperature);
    let stream = request.stream.unwrap_or(false);

    // Convert messages to provider format
    let messages: Vec<ChatMessage> = request
        .messages
        .iter()
        .map(|m| ChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
        })
        .collect();

    let provider_label = state
        .config
        .lock()
        .default_provider
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let started_at = Instant::now();

    state
        .observer
        .record_event(&crate::observability::ObserverEvent::LlmRequest {
            provider: provider_label.clone(),
            model: model.clone(),
            messages_count: messages.len(),
        });

    if stream {
        handle_streaming(
            state,
            messages,
            model,
            temperature,
            provider_label,
            started_at,
        )
        .into_response()
    } else {
        handle_non_streaming(
            state,
            messages,
            model,
            temperature,
            provider_label,
            started_at,
        )
        .await
        .into_response()
    }
}

/// Non-streaming chat completions.
async fn handle_non_streaming(
    state: AppState,
    messages: Vec<ChatMessage>,
    model: String,
    temperature: f64,
    provider_label: String,
    started_at: Instant,
) -> impl IntoResponse {
    match state
        .provider
        .chat_with_history(&messages, &model, temperature)
        .await
    {
        Ok(response_text) => {
            let duration = started_at.elapsed();
            record_success(&state, &provider_label, &model, duration);

            #[allow(clippy::cast_possible_truncation)]
            let completion_tokens = (response_text.len() / 4) as u32;
            #[allow(clippy::cast_possible_truncation)]
            let prompt_tokens = messages.iter().map(|m| m.content.len() / 4).sum::<usize>() as u32;

            let response = ChatCompletionsResponse {
                id: format!("chatcmpl-{}", Uuid::new_v4()),
                object: "chat.completion",
                created: unix_timestamp(),
                model: model.clone(),
                choices: vec![ChatCompletionsChoice {
                    index: 0,
                    message: ChatCompletionsResponseMessage {
                        role: "assistant",
                        content: response_text,
                    },
                    finish_reason: "stop",
                }],
                usage: ChatCompletionsUsage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens: prompt_tokens + completion_tokens,
                },
            };

            (
                StatusCode::OK,
                Json(serde_json::to_value(response).unwrap()),
            )
                .into_response()
        }
        Err(e) => {
            let duration = started_at.elapsed();
            let sanitized = crate::providers::sanitize_api_error(&e.to_string());
            record_failure(&state, &provider_label, &model, duration, &sanitized);

            tracing::error!("/v1/chat/completions provider error: {sanitized}");
            let err = serde_json::json!({
                "error": {
                    "message": "LLM request failed",
                    "type": "server_error",
                    "code": "provider_error"
                }
            });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(err)).into_response()
        }
    }
}

/// Streaming chat completions via SSE.
fn handle_streaming(
    state: AppState,
    messages: Vec<ChatMessage>,
    model: String,
    temperature: f64,
    provider_label: String,
    started_at: Instant,
) -> impl IntoResponse {
    let request_id = format!("chatcmpl-{}", Uuid::new_v4());
    let created = unix_timestamp();

    if !state.provider.supports_streaming() {
        // Provider doesn't support streaming — fall back to a single-chunk response
        let model_clone = model.clone();
        let id = request_id.clone();

        let stream = futures_util::stream::once(async move {
            match state
                .provider
                .chat_with_history(&messages, &model_clone, temperature)
                .await
            {
                Ok(text) => {
                    let duration = started_at.elapsed();
                    record_success(&state, &provider_label, &model_clone, duration);

                    let chunk = ChatCompletionsChunk {
                        id: id.clone(),
                        object: "chat.completion.chunk",
                        created,
                        model: model_clone,
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                role: Some("assistant"),
                                content: Some(text),
                            },
                            finish_reason: Some("stop"),
                        }],
                    };
                    let json = serde_json::to_string(&chunk).unwrap_or_else(|_| "{}".to_string());
                    let mut output = format!("data: {json}\n\n");
                    output.push_str("data: [DONE]\n\n");
                    Ok::<_, std::io::Error>(axum::body::Bytes::from(output))
                }
                Err(e) => {
                    let duration = started_at.elapsed();
                    let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                    record_failure(&state, &provider_label, &model_clone, duration, &sanitized);

                    let error_json = serde_json::json!({"error": sanitized});
                    let output = format!("data: {error_json}\n\ndata: [DONE]\n\n");
                    Ok(axum::body::Bytes::from(output))
                }
            }
        });

        return axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .body(Body::from_stream(stream))
            .unwrap()
            .into_response();
    }

    // Provider supports native streaming
    let provider_stream = state.provider.stream_chat_with_history(
        &messages,
        &model,
        temperature,
        StreamOptions::new(true),
    );

    let model_for_stream = model.clone();
    let state_for_stream = state.clone();
    let provider_label_for_stream = provider_label.clone();
    let mut first_chunk = true;
    let mut errored = false;

    let sse_stream = provider_stream.map(move |result| match result {
        Ok(chunk) if chunk.is_final => {
            if !errored {
                let duration = started_at.elapsed();
                record_success(
                    &state_for_stream,
                    &provider_label_for_stream,
                    &model_for_stream,
                    duration,
                );
            }
            Ok::<_, std::io::Error>(axum::body::Bytes::from("data: [DONE]\n\n"))
        }
        Ok(chunk) => {
            let role = if first_chunk {
                first_chunk = false;
                Some("assistant")
            } else {
                None
            };

            let sse_chunk = ChatCompletionsChunk {
                id: request_id.clone(),
                object: "chat.completion.chunk",
                created,
                model: model_for_stream.clone(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: ChunkDelta {
                        role,
                        content: if chunk.delta.is_empty() {
                            None
                        } else {
                            Some(chunk.delta)
                        },
                    },
                    finish_reason: None,
                }],
            };
            let json = serde_json::to_string(&sse_chunk).unwrap_or_else(|_| "{}".to_string());
            Ok(axum::body::Bytes::from(format!("data: {json}\n\n")))
        }
        Err(e) => {
            errored = true;
            let duration = started_at.elapsed();
            let msg = e.to_string();
            record_failure(
                &state_for_stream,
                &provider_label_for_stream,
                &model_for_stream,
                duration,
                &msg,
            );
            let error_json = serde_json::json!({"error": msg});
            Ok(axum::body::Bytes::from(format!("data: {error_json}\n\n")))
        }
    });

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(sse_stream))
        .unwrap()
        .into_response()
}

/// GET /v1/models — List available models.
pub async fn handle_v1_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // ── Bearer token auth (pairing) ──
    if state.pairing.require_pairing() {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = auth.strip_prefix("Bearer ").unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            let err = serde_json::json!({
                "error": {
                    "message": "Invalid API key",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            });
            return (StatusCode::UNAUTHORIZED, Json(err));
        }
    }

    let response = ModelsResponse {
        object: "list",
        data: vec![ModelObject {
            id: state.model.clone(),
            object: "model",
            created: unix_timestamp(),
            owned_by: "zeroclaw".to_string(),
        }],
    };

    (
        StatusCode::OK,
        Json(serde_json::to_value(response).unwrap()),
    )
}

// ══════════════════════════════════════════════════════════════════════════════
// HELPERS
// ══════════════════════════════════════════════════════════════════════════════

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn record_success(
    state: &AppState,
    provider_label: &str,
    model: &str,
    duration: std::time::Duration,
) {
    state
        .observer
        .record_event(&crate::observability::ObserverEvent::LlmResponse {
            provider: provider_label.to_string(),
            model: model.to_string(),
            duration,
            success: true,
            error_message: None,
            input_tokens: None,
            output_tokens: None,
        });
    state
        .observer
        .record_metric(&crate::observability::traits::ObserverMetric::RequestLatency(duration));
}

fn record_failure(
    state: &AppState,
    provider_label: &str,
    model: &str,
    duration: std::time::Duration,
    error_message: &str,
) {
    state
        .observer
        .record_event(&crate::observability::ObserverEvent::LlmResponse {
            provider: provider_label.to_string(),
            model: model.to_string(),
            duration,
            success: false,
            error_message: Some(error_message.to_string()),
            input_tokens: None,
            output_tokens: None,
        });
    state
        .observer
        .record_metric(&crate::observability::traits::ObserverMetric::RequestLatency(duration));
    state
        .observer
        .record_event(&crate::observability::ObserverEvent::Error {
            component: "gateway".to_string(),
            message: error_message.to_string(),
        });
}

// ══════════════════════════════════════════════════════════════════════════════
// TESTS
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completions_request_deserializes_minimal() {
        let json = r#"{"messages": [{"role": "user", "content": "Hello"}]}"#;
        let req: ChatCompletionsRequest = serde_json::from_str(json).unwrap();
        assert!(req.model.is_none());
        assert!(req.temperature.is_none());
        assert!(req.stream.is_none());
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[0].content, "Hello");
    }

    #[test]
    fn chat_completions_request_deserializes_full() {
        let json = r#"{
            "model": "anthropic/claude-sonnet-4",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hi"}
            ],
            "temperature": 0.5,
            "stream": true
        }"#;
        let req: ChatCompletionsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model.as_deref(), Some("anthropic/claude-sonnet-4"));
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.stream, Some(true));
        assert_eq!(req.messages.len(), 2);
    }

    #[test]
    fn chat_completions_response_serializes() {
        let response = ChatCompletionsResponse {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion",
            created: 1_234_567_890,
            model: "test-model".to_string(),
            choices: vec![ChatCompletionsChoice {
                index: 0,
                message: ChatCompletionsResponseMessage {
                    role: "assistant",
                    content: "Hello!".to_string(),
                },
                finish_reason: "stop",
            }],
            usage: ChatCompletionsUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("chatcmpl-test"));
        assert!(json.contains("chat.completion"));
        assert!(json.contains("Hello!"));
        assert!(json.contains("stop"));
    }

    #[test]
    fn models_response_serializes() {
        let response = ModelsResponse {
            object: "list",
            data: vec![ModelObject {
                id: "anthropic/claude-sonnet-4".to_string(),
                object: "model",
                created: 1_234_567_890,
                owned_by: "zeroclaw".to_string(),
            }],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"object\":\"list\""));
        assert!(json.contains("anthropic/claude-sonnet-4"));
        assert!(json.contains("zeroclaw"));
    }

    #[test]
    fn streaming_chunk_serializes() {
        let chunk = ChatCompletionsChunk {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion.chunk",
            created: 1_234_567_890,
            model: "test-model".to_string(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: Some("assistant"),
                    content: Some("Hello".to_string()),
                },
                finish_reason: None,
            }],
        };
        let json = serde_json::to_string(&chunk).unwrap();
        assert!(json.contains("chat.completion.chunk"));
        assert!(json.contains("Hello"));
        assert!(json.contains("assistant"));
    }

    #[test]
    fn streaming_chunk_omits_none_fields() {
        let chunk = ChatCompletionsChunk {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion.chunk",
            created: 1_234_567_890,
            model: "test-model".to_string(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: ChunkDelta {
                    role: None,
                    content: None,
                },
                finish_reason: None,
            }],
        };
        let json = serde_json::to_string(&chunk).unwrap();
        assert!(!json.contains("role"));
        assert!(!json.contains("content"));
    }

    #[test]
    fn unix_timestamp_is_reasonable() {
        let ts = unix_timestamp();
        // Should be after 2024-01-01 and before 2030-01-01
        assert!(ts > 1_704_067_200);
        assert!(ts < 1_893_456_000);
    }

    #[test]
    fn body_size_limit_is_512kb() {
        assert_eq!(CHAT_COMPLETIONS_MAX_BODY_SIZE, 524_288);
    }
}
