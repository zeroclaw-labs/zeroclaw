//! OpenClaw migration compatibility layer.
//!
//! Provides two endpoints for callers migrating from OpenClaw to ZeroClaw:
//!
//! 1. **`POST /api/chat`** (recommended) — ZeroClaw-native endpoint that invokes the
//!    full agent loop (`process_message`) with tools, memory recall, and context
//!    enrichment. Same code path as Linq/WhatsApp/Nextcloud Talk handlers.
//!
//! 2. **`POST /v1/chat/completions`** override — OpenAI-compatible shim that accepts
//!    standard `messages[]` arrays, extracts the last user message plus recent history,
//!    and routes through the full agent loop. Drop-in replacement for OpenClaw callers.
//!
//! ## Why this exists
//!
//! OpenClaw exposed `/v1/chat/completions` as an OpenAI-compatible API server.
//! ZeroClaw's existing `/v1/chat/completions` (in `openai_compat.rs`) uses the
//! simpler `provider.chat_with_history()` path — no tools, no memory, no agent loop.
//!
//! This module bridges the gap so callers coming from OpenClaw get the full agent
//! experience without code changes on their side.
//!
//! ## Migration path
//!
//! New integrations should use `POST /api/chat`. The `/v1/chat/completions` shim
//! is provided for backward compatibility and may be deprecated once all callers
//! have migrated to the native endpoint.

use super::{
    client_key_from_request, run_gateway_chat_with_tools, sanitize_gateway_response, AppState,
    RATE_LIMIT_WINDOW_SECS,
};
use crate::memory::MemoryCategory;
use crate::providers;
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Instant;
use uuid::Uuid;

// ══════════════════════════════════════════════════════════════════════════════
// /api/chat — ZeroClaw-native endpoint
// ══════════════════════════════════════════════════════════════════════════════

/// Request body for `POST /api/chat`.
#[derive(Debug, Deserialize)]
pub struct ApiChatBody {
    /// The user message to process.
    pub message: String,

    /// Optional session ID for memory scoping.
    /// When provided, memory store/recall operations are isolated to this session.
    #[serde(default)]
    pub session_id: Option<String>,

    /// Optional context lines to prepend to the message.
    /// Use this to inject recent conversation history that ZeroClaw's
    /// semantic memory might not surface (e.g., the last few exchanges).
    #[serde(default)]
    pub context: Vec<String>,

    /// Optional LLM provider override (e.g. "anthropic", "openai", "gemini").
    /// When provided, overrides the server's default_provider for this request.
    #[serde(default)]
    pub provider: Option<String>,

    /// Optional model ID override (e.g. "claude-opus-4-6", "gpt-4o").
    /// When provided, overrides the server's default_model for this request.
    #[serde(default)]
    pub model: Option<String>,

    /// Optional API key for the selected provider.
    /// When provided, overrides the server's configured api_key for this request.
    #[serde(default)]
    pub api_key: Option<String>,
}

fn api_chat_memory_key() -> String {
    format!("api_chat_msg_{}", Uuid::new_v4())
}

/// `POST /api/chat` — full agent loop with tools and memory.
///
/// Request:  `{ "message": "...", "session_id": "...", "context": [...] }`
/// Response: `{ "reply": "...", "model": "..." }`
pub async fn handle_api_chat(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<ApiChatBody>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    // ── Rate limit ──
    let rate_key =
        client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if !state.rate_limiter.allow_webhook(&rate_key) {
        tracing::warn!("/api/chat rate limit exceeded");
        let err = serde_json::json!({
            "error": "Too many chat requests. Please retry later.",
            "retry_after": RATE_LIMIT_WINDOW_SECS,
        });
        return (StatusCode::TOO_MANY_REQUESTS, Json(err));
    }

    // ── Auth: require at least one layer for non-loopback ──
    // Accept either pairing token or JWT session token for non-loopback requests
    let is_loopback = peer_addr.ip().is_loopback();

    if !is_loopback {
        let auth_header = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let bearer_token = auth_header.strip_prefix("Bearer ").unwrap_or("");

        // Try JWT session auth first (from /api/auth/login)
        let jwt_ok = state
            .auth_store
            .as_ref()
            .and_then(|store| store.validate_session(bearer_token))
            .is_some();

        // Then try pairing auth
        let pairing_ok = state.pairing.require_pairing()
            && state.pairing.is_authenticated(bearer_token);

        // Then try webhook secret (verify X-Webhook-Secret header against stored hash)
        let webhook_ok = if let Some(ref secret_hash) = state.webhook_secret_hash {
            headers
                .get("X-Webhook-Secret")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|v| {
                    use sha2::{Digest, Sha256};
                    let digest = Sha256::digest(v.as_bytes());
                    let hashed = hex::encode(digest);
                    crate::security::pairing::constant_time_eq(&hashed, secret_hash.as_ref())
                })
                .unwrap_or(false)
        } else {
            false
        };

        if !jwt_ok && !pairing_ok && !webhook_ok {
            tracing::warn!("/api/chat: rejected unauthenticated non-loopback request");
            let err = serde_json::json!({
                "error": "Unauthorized — please login or configure pairing"
            });
            return (StatusCode::UNAUTHORIZED, Json(err));
        }
    }

    // ── Parse body ──
    let Json(chat_body) = match body {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("/api/chat JSON parse error: {e}");
            let err = serde_json::json!({
                "error": "Invalid JSON body. Expected: {\"message\": \"...\"}"
            });
            return (StatusCode::BAD_REQUEST, Json(err));
        }
    };

    let message = chat_body.message.trim();
    let session_id = chat_body
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if message.is_empty() {
        let err = serde_json::json!({ "error": "Message cannot be empty" });
        return (StatusCode::BAD_REQUEST, Json(err));
    }

    // ── Auto-save to memory ──
    if state.auto_save {
        let key = api_chat_memory_key();
        let _ = state
            .mem
            .store(&key, message, MemoryCategory::Conversation, session_id)
            .await;
    }

    // ── Build enriched message with optional context ──
    let enriched_message = if chat_body.context.is_empty() {
        message.to_string()
    } else {
        let recent: Vec<&String> = chat_body.context.iter().rev().take(10).rev().collect();
        let context_block = recent
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<&str>>()
            .join("\n");
        format!(
            "Recent conversation context:\n{}\n\nCurrent message:\n{}",
            context_block, message
        )
    };

    // ── Build config with client-provided overrides ──
    let mut config = state.config.lock().clone();

    // Map frontend provider names to backend provider names
    if let Some(ref client_provider) = chat_body.provider {
        let backend_provider = match client_provider.as_str() {
            "claude" => "anthropic",
            p => p,
        };
        config.default_provider = Some(backend_provider.to_string());
    }

    if let Some(ref client_model) = chat_body.model {
        if !client_model.trim().is_empty() {
            config.default_model = Some(client_model.clone());
        }
    }

    if let Some(ref client_key) = chat_body.api_key {
        if !client_key.trim().is_empty() {
            config.api_key = Some(client_key.clone());
        }
    }

    // ── Resolve provider-specific key from provider_api_keys map ──
    // When the client doesn't send an api_key in the request body,
    // look up the per-provider key stored via Settings → /api/config/api-key.
    let provider_name = config
        .default_provider
        .as_deref()
        .unwrap_or("gemini");

    if config.api_key.as_ref().map_or(true, |k| k.trim().is_empty()) {
        if let Some(stored_key) = config.provider_api_keys.get(provider_name) {
            if !stored_key.trim().is_empty() {
                config.api_key = Some(stored_key.clone());
            }
        }
    }

    // ── Validate API key for cloud providers ──
    // Check both the config-level key (from request body or config.toml) AND
    // provider-specific env vars (e.g. GEMINI_API_KEY, ANTHROPIC_API_KEY).
    // The provider factory uses resolve_provider_credential() which checks
    // env vars as fallback, so we mirror that logic here.
    if providers::provider_requires_credential(provider_name) {
        let has_key = providers::has_provider_credential(
            provider_name,
            config.api_key.as_deref(),
        );
        if !has_key {
            let env_hint = match provider_name {
                "anthropic" => "ANTHROPIC_API_KEY",
                "openai" => "OPENAI_API_KEY",
                "gemini" | "google" | "google-gemini" => "GEMINI_API_KEY",
                _ => "<PROVIDER>_API_KEY",
            };
            let err = serde_json::json!({
                "error": format!(
                    "No API key configured for provider '{}'. Please add your API key in Settings or set {} env var.",
                    provider_name, env_hint
                ),
                "code": "missing_api_key",
                "fallback_to_relay": true
            });
            return (StatusCode::BAD_REQUEST, Json(err));
        }
    }

    // ── Observability ──
    let provider_label = config
        .default_provider
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let model_label = config
        .default_model
        .clone()
        .unwrap_or_else(|| state.model.clone());
    let started_at = Instant::now();

    state
        .observer
        .record_event(&crate::observability::ObserverEvent::AgentStart {
            provider: provider_label.clone(),
            model: model_label.clone(),
        });
    state
        .observer
        .record_event(&crate::observability::ObserverEvent::LlmRequest {
            provider: provider_label.clone(),
            model: model_label.clone(),
            messages_count: 1,
        });

    // ── Run the full agent loop ──
    match crate::agent::process_message_with_session(config, &enriched_message, session_id).await {
        Ok(response) => {
            let leak_guard_cfg = state.config.lock().security.outbound_leak_guard.clone();
            let safe_response = sanitize_gateway_response(
                &response,
                state.tools_registry_exec.as_ref(),
                &leak_guard_cfg,
            );
            let duration = started_at.elapsed();

            state
                .observer
                .record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: true,
                    error_message: None,
                    input_tokens: None,
                    output_tokens: None,
                });
            state.observer.record_metric(
                &crate::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::AgentEnd {
                    provider: provider_label,
                    model: model_label.clone(),
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                });

            let body = serde_json::json!({
                "reply": safe_response,
                "model": model_label,
                "session_id": chat_body.session_id,
            });
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let duration = started_at.elapsed();
            let sanitized = providers::sanitize_api_error(&e.to_string());

            state
                .observer
                .record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: false,
                    error_message: Some(sanitized.clone()),
                    input_tokens: None,
                    output_tokens: None,
                });
            state.observer.record_metric(
                &crate::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::AgentEnd {
                    provider: provider_label,
                    model: model_label,
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                });

            tracing::error!("/api/chat provider error: {sanitized}");

            // Detect provider authentication errors (401 Unauthorized) so the
            // client can fall back to the relay server with operator keys.
            let is_auth_error = sanitized.contains("401")
                || sanitized.contains("Unauthorized")
                || sanitized.contains("authentication");
            if is_auth_error {
                let err = serde_json::json!({
                    "error": format!("LLM request failed: {sanitized}"),
                    "code": "provider_auth_error",
                    "fallback_to_relay": true,
                });
                return (StatusCode::BAD_REQUEST, Json(err));
            }

            let err = serde_json::json!({
                "error": format!("LLM request failed: {sanitized}"),
            });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(err))
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// /v1/chat/completions — OpenAI-compatible shim (full agent loop)
// ══════════════════════════════════════════════════════════════════════════════

/// Maximum context messages extracted from the `messages[]` array for injection.
const MAX_CONTEXT_MESSAGES: usize = 10;

/// OpenAI-compatible request body.
#[derive(Debug, Deserialize)]
pub struct OaiChatRequest {
    pub messages: Vec<OaiMessage>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub stream: Option<bool>,
    // Accept and ignore other OpenAI params for compat
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub frequency_penalty: Option<f64>,
    #[serde(default)]
    pub presence_penalty: Option<f64>,
    #[serde(default)]
    pub stop: Option<serde_json::Value>,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OaiMessage {
    pub role: String,
    pub content: String,
}

// Response types — reuse the ones from openai_compat.rs via the same format
#[derive(Debug, Serialize)]
struct OaiChatResponse {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<OaiChoice>,
    usage: OaiUsage,
}

#[derive(Debug, Serialize)]
struct OaiChoice {
    index: u32,
    message: OaiMessage,
    finish_reason: &'static str,
}

#[derive(Debug, Serialize)]
struct OaiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Debug, Serialize)]
struct OaiStreamChunk {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<OaiStreamChoice>,
}

#[derive(Debug, Serialize)]
struct OaiStreamChoice {
    index: u32,
    delta: OaiDelta,
    finish_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct OaiDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

/// `POST /v1/chat/completions` — OpenAI-compatible shim over ZeroClaw's agent loop.
///
/// This replaces the simple `provider.chat_with_history()` path from `openai_compat.rs`
/// with the full `run_gateway_chat_with_tools()` agent loop, giving OpenClaw callers
/// the same tools + memory experience as native ZeroClaw channels.
pub async fn handle_v1_chat_completions_with_tools(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // ── Rate limit ──
    let rate_key =
        client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if !state.rate_limiter.allow_webhook(&rate_key) {
        tracing::warn!("/v1/chat/completions (compat) rate limit exceeded");
        let err = serde_json::json!({
            "error": {
                "message": "Rate limit exceeded. Please retry later.",
                "type": "rate_limit_error",
                "code": "rate_limit_exceeded"
            }
        });
        return (StatusCode::TOO_MANY_REQUESTS, Json(err)).into_response();
    }

    // ── Auth: require at least one layer for non-loopback ──
    if !state.pairing.require_pairing()
        && state.webhook_secret_hash.is_none()
        && !peer_addr.ip().is_loopback()
    {
        tracing::warn!(
            "/v1/chat/completions (compat): rejected unauthenticated non-loopback request"
        );
        let err = serde_json::json!({
            "error": {
                "message": "Unauthorized — configure pairing or X-Webhook-Secret for non-local access",
                "type": "invalid_request_error",
                "code": "unauthorized"
            }
        });
        return (StatusCode::UNAUTHORIZED, Json(err)).into_response();
    }

    // ── Bearer token auth (pairing) ──
    if state.pairing.require_pairing() {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = auth.strip_prefix("Bearer ").unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            tracing::warn!(
                "/v1/chat/completions (compat): rejected — not paired / invalid bearer token"
            );
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

    // ── Body size ──
    if body.len() > super::openai_compat::CHAT_COMPLETIONS_MAX_BODY_SIZE {
        let err = serde_json::json!({
            "error": {
                "message": format!(
                    "Request body too large ({} bytes, max {})",
                    body.len(),
                    super::openai_compat::CHAT_COMPLETIONS_MAX_BODY_SIZE
                ),
                "type": "invalid_request_error",
                "code": "request_too_large"
            }
        });
        return (StatusCode::PAYLOAD_TOO_LARGE, Json(err)).into_response();
    }

    // ── Parse body ──
    let request: OaiChatRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            tracing::warn!("/v1/chat/completions (compat) JSON parse error: {e}");
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

    // ── Extract last user message + context ──
    let last_user_msg = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone());

    let message = match last_user_msg {
        Some(m) if !m.trim().is_empty() => m,
        _ => {
            let err = serde_json::json!({
                "error": {
                    "message": "No user message found in messages array",
                    "type": "invalid_request_error",
                    "code": "invalid_messages"
                }
            });
            return (StatusCode::BAD_REQUEST, Json(err)).into_response();
        }
    };

    // Build context from conversation history (exclude the last user message)
    let context_messages: Vec<String> = request
        .messages
        .iter()
        .rev()
        .skip(1)
        .rev()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .map(|m| {
            let role_label = if m.role == "user" {
                "User"
            } else {
                "Assistant"
            };
            format!("{}: {}", role_label, m.content)
        })
        .collect();

    let enriched_message = if context_messages.is_empty() {
        message.clone()
    } else {
        let recent: Vec<&String> = context_messages
            .iter()
            .rev()
            .take(MAX_CONTEXT_MESSAGES)
            .rev()
            .collect();
        let context_block = recent
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<&str>>()
            .join("\n");
        format!(
            "Recent conversation context:\n{}\n\nCurrent message:\n{}",
            context_block, message
        )
    };

    let is_stream = request.stream.unwrap_or(false);
    let session_id = request
        .user
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let request_id = format!("chatcmpl-{}", Uuid::new_v4().to_string().replace('-', ""));
    let created = unix_timestamp();

    // ── Auto-save ──
    if state.auto_save {
        let key = api_chat_memory_key();
        let _ = state
            .mem
            .store(&key, &message, MemoryCategory::Conversation, session_id)
            .await;
    }

    // ── Observability ──
    let provider_label = state
        .config
        .lock()
        .default_provider
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let model_label = state.model.clone();
    let started_at = Instant::now();

    state
        .observer
        .record_event(&crate::observability::ObserverEvent::AgentStart {
            provider: provider_label.clone(),
            model: model_label.clone(),
        });
    state
        .observer
        .record_event(&crate::observability::ObserverEvent::LlmRequest {
            provider: provider_label.clone(),
            model: model_label.clone(),
            messages_count: request.messages.len(),
        });

    tracing::info!(
        stream = is_stream,
        messages_count = request.messages.len(),
        "Processing /v1/chat/completions (compat shim — full agent loop)"
    );

    // ── Run the full agent loop ──
    let reply = match run_gateway_chat_with_tools(&state, &enriched_message, session_id).await {
        Ok(response) => {
            let leak_guard_cfg = state.config.lock().security.outbound_leak_guard.clone();
            let safe = sanitize_gateway_response(
                &response,
                state.tools_registry_exec.as_ref(),
                &leak_guard_cfg,
            );
            let duration = started_at.elapsed();

            state
                .observer
                .record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: true,
                    error_message: None,
                    input_tokens: None,
                    output_tokens: None,
                });
            state.observer.record_metric(
                &crate::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::AgentEnd {
                    provider: provider_label,
                    model: model_label,
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                });

            safe
        }
        Err(e) => {
            let duration = started_at.elapsed();
            let sanitized = providers::sanitize_api_error(&e.to_string());

            state
                .observer
                .record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: provider_label.clone(),
                    model: model_label.clone(),
                    duration,
                    success: false,
                    error_message: Some(sanitized.clone()),
                    input_tokens: None,
                    output_tokens: None,
                });
            state.observer.record_metric(
                &crate::observability::traits::ObserverMetric::RequestLatency(duration),
            );
            state
                .observer
                .record_event(&crate::observability::ObserverEvent::AgentEnd {
                    provider: provider_label,
                    model: model_label,
                    duration,
                    tokens_used: None,
                    cost_usd: None,
                });

            tracing::error!("/v1/chat/completions (compat) provider error: {sanitized}");
            let err = serde_json::json!({
                "error": {
                    "message": format!("LLM request failed: {sanitized}"),
                    "type": "server_error",
                    "code": "provider_error"
                }
            });
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(err)).into_response();
        }
    };

    let model_name = request.model.unwrap_or_else(|| state.model.clone());

    #[allow(clippy::cast_possible_truncation)]
    let prompt_tokens = (enriched_message.len() / 4) as u32;
    #[allow(clippy::cast_possible_truncation)]
    let completion_tokens = (reply.len() / 4) as u32;

    if is_stream {
        // ── Simulated streaming SSE ──
        // The full agent loop returns a complete response; we chunk it into SSE format.
        let role_chunk = OaiStreamChunk {
            id: request_id.clone(),
            object: "chat.completion.chunk",
            created,
            model: model_name.clone(),
            choices: vec![OaiStreamChoice {
                index: 0,
                delta: OaiDelta {
                    role: Some("assistant"),
                    content: None,
                },
                finish_reason: None,
            }],
        };

        let content_chunk = OaiStreamChunk {
            id: request_id.clone(),
            object: "chat.completion.chunk",
            created,
            model: model_name.clone(),
            choices: vec![OaiStreamChoice {
                index: 0,
                delta: OaiDelta {
                    role: None,
                    content: Some(reply),
                },
                finish_reason: None,
            }],
        };

        let stop_chunk = OaiStreamChunk {
            id: request_id,
            object: "chat.completion.chunk",
            created,
            model: model_name,
            choices: vec![OaiStreamChoice {
                index: 0,
                delta: OaiDelta {
                    role: None,
                    content: None,
                },
                finish_reason: Some("stop"),
            }],
        };

        let mut output = String::new();
        output.push_str("data: ");
        output.push_str(&serde_json::to_string(&role_chunk).unwrap_or_else(|_| "{}".into()));
        output.push_str("\n\n");
        output.push_str("data: ");
        output.push_str(&serde_json::to_string(&content_chunk).unwrap_or_else(|_| "{}".into()));
        output.push_str("\n\n");
        output.push_str("data: ");
        output.push_str(&serde_json::to_string(&stop_chunk).unwrap_or_else(|_| "{}".into()));
        output.push_str("\n\n");
        output.push_str("data: [DONE]\n\n");

        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .body(Body::from(output))
            .expect("static SSE headers are valid")
            .into_response()
    } else {
        // ── Non-streaming JSON ──
        let response = OaiChatResponse {
            id: request_id,
            object: "chat.completion",
            created,
            model: model_name,
            choices: vec![OaiChoice {
                index: 0,
                message: OaiMessage {
                    role: "assistant".into(),
                    content: reply,
                },
                finish_reason: "stop",
            }],
            usage: OaiUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
        };
        Json(response).into_response()
    }
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

// ══════════════════════════════════════════════════════════════════════════════
// TESTS
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_chat_body_deserializes_minimal() {
        let json = r#"{"message": "Hello"}"#;
        let body: ApiChatBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.message, "Hello");
        assert!(body.session_id.is_none());
        assert!(body.context.is_empty());
    }

    #[test]
    fn api_chat_body_deserializes_full() {
        let json = r#"{
            "message": "What's my schedule?",
            "session_id": "sess-123",
            "context": ["User: hi", "Assistant: hello"],
            "provider": "anthropic",
            "model": "claude-opus-4-6"
        }"#;
        let body: ApiChatBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.message, "What's my schedule?");
        assert_eq!(body.session_id.as_deref(), Some("sess-123"));
        assert_eq!(body.context.len(), 2);
        assert_eq!(body.provider.as_deref(), Some("anthropic"));
        assert_eq!(body.model.as_deref(), Some("claude-opus-4-6"));
    }

    #[test]
    fn oai_request_deserializes_with_extra_fields() {
        let json = r#"{
            "messages": [{"role": "user", "content": "Hi"}],
            "model": "claude-sonnet-4-6",
            "temperature": 0.5,
            "stream": false,
            "max_tokens": 1000,
            "top_p": 0.9,
            "frequency_penalty": 0.1,
            "presence_penalty": 0.0,
            "user": "test-user"
        }"#;
        let req: OaiChatRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.stream, Some(false));
        assert_eq!(req.max_tokens, Some(1000));
    }

    #[test]
    fn oai_response_serializes_correctly() {
        let response = OaiChatResponse {
            id: "chatcmpl-test".into(),
            object: "chat.completion",
            created: 1_234_567_890,
            model: "test-model".into(),
            choices: vec![OaiChoice {
                index: 0,
                message: OaiMessage {
                    role: "assistant".into(),
                    content: "Hello!".into(),
                },
                finish_reason: "stop",
            }],
            usage: OaiUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("chatcmpl-test"));
        assert!(json.contains("chat.completion"));
        assert!(json.contains("Hello!"));
    }

    #[test]
    fn streaming_chunk_omits_none_fields() {
        let chunk = OaiStreamChunk {
            id: "chatcmpl-test".into(),
            object: "chat.completion.chunk",
            created: 1_234_567_890,
            model: "test-model".into(),
            choices: vec![OaiStreamChoice {
                index: 0,
                delta: OaiDelta {
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
    fn memory_key_is_unique() {
        let k1 = api_chat_memory_key();
        let k2 = api_chat_memory_key();
        assert_ne!(k1, k2);
        assert!(k1.starts_with("api_chat_msg_"));
    }

    // ── Handler-level validation tests ──
    // These verify the input shapes that the handlers validate at runtime.

    #[test]
    fn api_chat_body_rejects_missing_message() {
        let json = r#"{"session_id": "s1"}"#;
        let result: Result<ApiChatBody, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "missing `message` field should fail deserialization"
        );
    }

    #[test]
    fn oai_request_rejects_empty_messages() {
        let json = r#"{"messages": []}"#;
        let req: OaiChatRequest = serde_json::from_str(json).unwrap();
        assert!(
            req.messages.is_empty(),
            "empty messages should parse but be caught by handler"
        );
    }

    #[test]
    fn oai_request_no_user_message_detected() {
        let json = r#"{"messages": [{"role": "system", "content": "You are helpful."}]}"#;
        let req: OaiChatRequest = serde_json::from_str(json).unwrap();
        let last_user = req.messages.iter().rev().find(|m| m.role == "user");
        assert!(last_user.is_none(), "should detect no user message");
    }

    #[test]
    fn oai_request_whitespace_only_user_message() {
        let json = r#"{"messages": [{"role": "user", "content": "   "}]}"#;
        let req: OaiChatRequest = serde_json::from_str(json).unwrap();
        let last_user = req.messages.iter().rev().find(|m| m.role == "user");
        assert!(
            last_user.map_or(true, |m| m.content.trim().is_empty()),
            "whitespace-only user message should be treated as empty"
        );
    }

    #[test]
    fn oai_context_extraction_skips_last_user_message() {
        let json = r#"{
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "reply"},
                {"role": "user", "content": "second"}
            ]
        }"#;
        let req: OaiChatRequest = serde_json::from_str(json).unwrap();

        // Replicate the handler's context extraction logic
        let context_messages: Vec<String> = req
            .messages
            .iter()
            .rev()
            .skip(1)
            .rev()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .map(|m| {
                format!(
                    "{}: {}",
                    if m.role == "user" {
                        "User"
                    } else {
                        "Assistant"
                    },
                    m.content
                )
            })
            .collect();

        assert_eq!(context_messages.len(), 2);
        assert!(context_messages[0].starts_with("User: first"));
        assert!(context_messages[1].starts_with("Assistant: reply"));
    }
}
