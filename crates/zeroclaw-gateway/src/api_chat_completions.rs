//! OpenAI-compatible Chat Completions endpoint (`POST /v1/chat/completions`).

use axum::extract::{ConnectInfo, State};
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_infra::session_backend::SessionBackend;
use zeroclaw_providers::sanitize_api_error;
use zeroclaw_runtime::agent::TurnEvent;

use crate::AppState;

// Rate limit window (matches lib.rs)
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

// Request structures

#[derive(Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChatCompletionRequest {
    #[serde(default)]
    pub model: String,
    pub messages: Vec<ChatCompletionMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f64>,
    pub stop: Option<serde_json::Value>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub tools: Option<Vec<ChatCompletionTool>>,
    pub tool_choice: Option<serde_json::Value>,
    pub stream_options: Option<StreamOptions>,
    pub n: Option<u32>,
    pub response_format: Option<serde_json::Value>,
    pub seed: Option<i64>,
    pub logprobs: Option<bool>,
    pub top_logprobs: Option<u32>,
    pub user: Option<String>,
    pub logit_bias: Option<serde_json::Value>,
    pub max_completion_tokens: Option<u32>,
}

fn default_temperature() -> f64 {
    0.7
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChatCompletionMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChatCompletionTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ToolFunction {
    pub name: String,
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

// Response structures

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct ChatCompletionResponse {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<NonStreamChoice>,
    usage: CompletionUsage,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct NonStreamChoice {
    index: u32,
    message: AssistantMessage,
    finish_reason: String,
    #[serde(default)]
    pub logprobs: Option<()>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct AssistantMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct ResponseToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: ResponseFunctionCall,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct ResponseFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct CompletionUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct ErrorResponse {
    pub(crate) error: ErrorDetail,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct ErrorDetail {
    pub(crate) message: String,
    #[serde(rename = "type")]
    pub(crate) error_type: String,
    pub(crate) code: Option<String>,
    pub(crate) param: Option<String>,
    pub(crate) status: u16,
}

// Helpers

fn generate_completion_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4())
}

fn default_agent_alias(config: &zeroclaw_config::schema::Config) -> String {
    config
        .resolved_runtime_agent_alias()
        .unwrap_or("default")
        .to_string()
}

/// Resolve a per-request memory handle for the HTTP streaming path, mirroring
/// the WebSocket agent memory handle resolution. Returns `None`
/// when the agent's `memory.backend` is `None`; otherwise constructs a memory
/// handle via `zeroclaw_memory::create_memory_for_agent`. On error the caller
/// degrades to `None` (consolidation disabled) but the turn still proceeds.
async fn resolve_http_memory_handle(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> anyhow::Result<Option<Arc<dyn zeroclaw_memory::Memory>>> {
    if config.agent(agent_alias).is_some_and(|agent| {
        matches!(
            agent.memory.backend,
            zeroclaw_config::multi_agent::MemoryBackendKind::None
        )
    }) {
        return Ok(None);
    }

    let api_key = config
        .resolved_model_provider_for_agent(agent_alias)
        .and_then(|(_, _, cfg)| cfg.api_key.clone());
    zeroclaw_memory::create_memory_for_agent(config, agent_alias, api_key.as_deref())
        .await
        .map(Some)
}

fn agent_alias_from_model(
    model: &str,
    config: &zeroclaw_config::schema::Config,
) -> Result<String, String> {
    let default = default_agent_alias(config);
    let model = model.trim();

    if model.is_empty() || model == "zeroclaw" || model == "zeroclaw/default" {
        return Ok(default);
    }

    for prefix in ["zeroclaw/", "zeroclaw:", "agent:"] {
        if let Some(rest) = model.strip_prefix(prefix) {
            let alias = rest.trim();
            if alias.is_empty() {
                return Err(format!(
                    "Invalid agent target `{model}`: missing agent alias"
                ));
            }
            return Ok(alias.to_string());
        }
    }

    // Silently route unrecognized model names (e.g. "gpt-4") to default
    // agent for standard-client compatibility.
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "request_model": model,
                "resolved_alias": default,
            })
        ),
        "chat completions: unrecognized model resolved to default agent"
    );
    Ok(default)
}

fn extract_session_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-session-key")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn normalize_role(role: &str) -> String {
    match role {
        "function" => "tool".to_string(),
        other => other.to_string(),
    }
}

fn convert_messages(msgs: &[ChatCompletionMessage]) -> Vec<ChatMessage> {
    msgs.iter()
        .map(|m| ChatMessage {
            role: normalize_role(&m.role),
            content: m.content.clone().unwrap_or_default(),
        })
        .collect()
}

fn build_user_message(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| format!("[{}] {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn split_messages(msgs: &[ChatCompletionMessage]) -> (Vec<ChatMessage>, String) {
    let active_idx = msgs
        .iter()
        .rposition(|m| matches!(m.role.as_str(), "user" | "tool" | "function"));

    let Some(active_idx) = active_idx else {
        return (Vec::new(), build_user_message(&convert_messages(msgs)));
    };

    let mut history = Vec::new();
    for (idx, msg) in msgs.iter().enumerate() {
        if idx == active_idx || matches!(msg.role.as_str(), "system" | "developer") {
            continue;
        }
        history.push(ChatMessage {
            role: normalize_role(&msg.role),
            content: msg.content.clone().unwrap_or_default(),
        });
    }

    (
        history,
        msgs[active_idx].content.clone().unwrap_or_default(),
    )
}

fn request_system_prompt_prefix(msgs: &[ChatCompletionMessage]) -> Option<String> {
    let parts: Vec<&str> = msgs
        .iter()
        .filter(|m| matches!(m.role.as_str(), "system" | "developer"))
        .filter_map(|m| m.content.as_deref())
        .filter(|s| !s.trim().is_empty())
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn request_has_authoritative_history(request: &ChatCompletionRequest) -> bool {
    request.messages.len() > 1
}

fn error_response(
    status: StatusCode,
    error_type: &str,
    message: &str,
    code: Option<&str>,
    param: Option<&str>,
) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: ErrorDetail {
                message: message.to_string(),
                error_type: error_type.to_string(),
                code: code.map(String::from),
                param: param.map(String::from),
                status: status.as_u16(),
            },
        }),
    )
        .into_response()
}

fn add_request_id_header(mut response: Response, request_id: &str) -> Response {
    response.headers_mut().insert(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_str(request_id).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    response
}

fn add_session_key_header(mut response: Response, session_id: &str) -> Response {
    response.headers_mut().insert(
        HeaderName::from_static("x-session-key"),
        HeaderValue::from_str(session_id).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    response
}

/// Structured error from ownership-check functions. The caller wraps these
/// fields into an axum `Response` with transport headers.
pub(crate) struct OwnershipError {
    pub(crate) status: StatusCode,
    pub(crate) error_type: String,
    pub(crate) message: String,
}

/// Attempt to record the session→agent binding so ownership can be enforced
/// on subsequent requests. Fails closed: if the backend cannot persist the
/// binding (and it is not merely `Unsupported`), the function returns an
/// `Err(OwnershipError)` that the caller must propagate as a 500.
///
/// The `Unsupported` error kind is treated as graceful degradation because an
/// owner-tracking-capable backend may not be configured yet.
pub(crate) fn try_persist_session_ownership(
    backend: &dyn SessionBackend,
    session_key: &str,
    agent_alias: &str,
) -> Result<(), OwnershipError> {
    match backend.set_session_agent_alias(session_key, agent_alias) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => Ok(()),
        Err(e) => {
            let sanitized =
                sanitize_api_error(&format!("Failed to persist session ownership: {e}"));
            Err(OwnershipError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                error_type: "server_error".to_string(),
                message: sanitized,
            })
        }
    }
}

/// Before loading history for an existing session (one identified by a
/// `session_id_from_header`), verify the session belongs to the requested
/// agent. Returns:
/// - `Ok(())` when the check passes (matching alias, no stored alias, or
///   backend doesn't support ownership tracking and session is empty).
/// - `Err(OwnershipError)` when the check fails (mismatched alias, or backend
///   doesn't support ownership tracking and session has data).
pub(crate) fn try_check_session_ownership_on_resume(
    backend: &dyn SessionBackend,
    session_key: &str,
    agent_alias: &str,
    session_id_from_header: Option<&str>,
) -> Result<(), OwnershipError> {
    // No session key header means this is a new session -- skip the check.
    let Some(_header_key) = session_id_from_header else {
        return Ok(());
    };

    match backend.get_session_agent_alias(session_key) {
        Ok(Some(stored_alias)) if stored_alias != agent_alias => Err(OwnershipError {
            status: StatusCode::BAD_REQUEST,
            error_type: "invalid_request_error".to_string(),
            message: format!("Session belongs to agent '{stored_alias}', not '{agent_alias}'"),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
            if !backend.load(session_key).is_empty() {
                Err(OwnershipError {
                    status: StatusCode::BAD_REQUEST,
                    error_type: "invalid_request_error".to_string(),
                    message: "Cannot resume session: backend does not track agent ownership"
                        .to_string(),
                })
            } else {
                Ok(())
            }
        }
        Err(_) => Err(OwnershipError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error_type: "internal_error".to_string(),
            message: "Failed to read session metadata".to_string(),
        }),
        Ok(None) => {
            // No ownership record exists. If the session already has
            // messages, the caller might be attempting to claim a
            // pre-migration session that belongs to a different agent.
            // Reject to prevent cross-agent context contamination.
            if !backend.load(session_key).is_empty() {
                Err(OwnershipError {
                    status: StatusCode::BAD_REQUEST,
                    error_type: "invalid_request_error".to_string(),
                    message: "Cannot resume session: no agent ownership record exists. Use `zeroclaw migrate-session-ownership` to claim this session.".to_string(),
                })
            } else {
                Ok(())
            }
        }
        // Ok(Some(matching alias)): pass through.
        _ => Ok(()),
    }
}

fn add_rate_limit_headers(
    mut response: Response,
    limit: u32,
    remaining: u32,
    reset: u64,
) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("x-ratelimit-limit"),
        HeaderValue::from(limit),
    );
    headers.insert(
        HeaderName::from_static("x-ratelimit-remaining"),
        HeaderValue::from(remaining),
    );
    headers.insert(
        HeaderName::from_static("x-ratelimit-reset"),
        HeaderValue::from(reset),
    );
    response
}

fn make_chunk(
    id: &str,
    created: u64,
    model: &str,
    role: Option<&str>,
    content: Option<String>,
    tool_calls: Option<Vec<serde_json::Value>>,
    finish_reason: Option<&str>,
) -> Event {
    let data = chunk_json(id, created, model, role, content, tool_calls, finish_reason);
    Event::default().data(data.to_string())
}

/// Build the SSE chunk JSON payload (separated for testability).
fn chunk_json(
    id: &str,
    created: u64,
    model: &str,
    role: Option<&str>,
    content: Option<String>,
    tool_calls: Option<Vec<serde_json::Value>>,
    finish_reason: Option<&str>,
) -> serde_json::Value {
    let mut delta = serde_json::Map::new();
    if let Some(r) = role {
        delta.insert("role".into(), serde_json::Value::String(r.into()));
    }
    if let Some(c) = content {
        delta.insert("content".into(), serde_json::Value::String(c));
    }
    if let Some(tc) = tool_calls {
        delta.insert("tool_calls".into(), serde_json::Value::Array(tc));
    }

    let choice = serde_json::json!({
        "index": 0,
        "delta": delta,
        "finish_reason": finish_reason,
        "logprobs": null
    });

    serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [choice]
    })
}

fn make_usage_chunk(
    id: &str,
    created: u64,
    model: &str,
    prompt: u64,
    completion: u64,
    total: u64,
) -> Event {
    let data = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": total
        }
    });
    Event::default().data(data.to_string())
}

// Handler

#[axum::debug_handler]
pub async fn handle_chat_completions(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Result<Json<ChatCompletionRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id_from_header = extract_session_key(&headers);
    let session_id = session_id_from_header
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_key = format!(
        "gw_{}",
        zeroclaw_api::session_keys::sanitize_session_key(&session_id)
    );

    let Json(request) = match request {
        Ok(req) => req,
        Err(e) => {
            let msg = format!("Invalid request: {}", e.body_text());
            return add_session_key_header(
                add_request_id_header(
                    error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        &msg,
                        None,
                        None,
                    ),
                    &request_id,
                ),
                &session_id,
            );
        }
    };

    let chat_allowed = state.check_chat_rate_limit(Some(peer_addr), &headers);
    let chat_rate_limit = state.config.read().gateway.chat_rate_limit_per_minute;
    if !chat_allowed {
        let reset_ts = Utc::now().timestamp() as u64 + RATE_LIMIT_WINDOW_SECS;
        let err = error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            &format!(
                "Rate limit exceeded. Retry after {} seconds.",
                RATE_LIMIT_WINDOW_SECS
            ),
            Some("rate_limit_exceeded"),
            None,
        );
        let mut resp = add_request_id_header(err, &request_id);
        resp = add_rate_limit_headers(resp, chat_rate_limit, 0, reset_ts);
        resp.headers_mut().insert(
            HeaderName::from_static("retry-after"),
            HeaderValue::from(RATE_LIMIT_WINDOW_SECS),
        );
        resp = add_session_key_header(resp, &session_id);
        return resp;
    }

    let rate_limit_remaining = chat_rate_limit.saturating_sub(1);
    let reset_ts = Utc::now().timestamp() as u64 + RATE_LIMIT_WINDOW_SECS;

    if let Err((status, json_body)) = super::api::require_auth(&state, &headers) {
        let body_value = serde_json::to_value(json_body.0).unwrap_or_default();
        let msg = body_value
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("Invalid or missing authentication token");
        let err_type = if status == StatusCode::UNAUTHORIZED {
            "authentication_error"
        } else {
            "api_error"
        };
        return add_session_key_header(
            add_request_id_header(
                error_response(status, err_type, msg, None, None),
                &request_id,
            ),
            &session_id,
        );
    }

    if let Err(e) = validate_request(&request) {
        return add_session_key_header(add_request_id_header(e, &request_id), &session_id);
    }
    if let Err(e) = validate_unsupported_params(&request) {
        return add_session_key_header(add_request_id_header(e, &request_id), &session_id);
    }

    let config = state.config.read().clone();

    let agent_alias = match agent_alias_from_model(&request.model, &config) {
        Ok(alias) => alias,
        Err(e) => {
            return add_session_key_header(
                add_request_id_header(
                    error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        &e,
                        None,
                        Some("model"),
                    ),
                    &request_id,
                ),
                &session_id,
            );
        }
    };

    if config.agent(&agent_alias).is_none() {
        return add_session_key_header(
            add_request_id_header(
                error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    &format!(
                        "Unknown agent `{agent_alias}` — no [agents.{agent_alias}] entry configured."
                    ),
                    None,
                    Some("model"),
                ),
                &request_id,
            ),
            &session_id,
        );
    }

    if config
        .resolved_model_provider_for_agent(&agent_alias)
        .and_then(|(_, _, cfg)| cfg.model.as_deref().filter(|m| !m.trim().is_empty()))
        .is_none()
    {
        return add_session_key_header(
            add_request_id_header(
                error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "server_error",
                    "Agent not configured — complete onboarding at /onboard",
                    None,
                    Some("model"),
                ),
                &request_id,
            ),
            &session_id,
        );
    }

    let response_model = if request.model.trim().is_empty() {
        "zeroclaw".to_string()
    } else {
        request.model.clone()
    };

    let mut agent =
        match zeroclaw_runtime::agent::Agent::from_config_with_session_cwd_and_mcp_backchannel(
            &config,
            &agent_alias,
            None,
            true,
            false,
            None,
            None,
            None,
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                let sanitized = sanitize_api_error(&e.to_string());
                return add_session_key_header(
                    add_request_id_header(
                        error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "internal_error",
                            &sanitized,
                            None,
                            None,
                        ),
                        &request_id,
                    ),
                    &session_id,
                );
            }
        };

    let (request_history, current_turn) = split_messages(&request.messages);
    let user_message = match request_system_prompt_prefix(&request.messages) {
        Some(prefix) => format!("{prefix}\n\n{current_turn}"),
        None => current_turn.clone(),
    };
    let request_has_authoritative_history = request_has_authoritative_history(&request);

    agent.set_memory_session_id(Some(zeroclaw_api::session_keys::canonical_memory_id(
        &session_id,
    )));

    // Acquire the per-session queue BEFORE any backend access so concurrent
    // requests sharing the same session are serialized across the complete
    // lifecycle: alias check → history load → turn execution → persistence.
    let session_guard = match state.session_queue.acquire(&session_key).await {
        Ok(guard) => guard,
        Err(e) => {
            let mut resp = add_request_id_header(
                error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limit_error",
                    &format!("Session busy: {e}"),
                    Some("rate_limit_exceeded"),
                    None,
                ),
                &request_id,
            );
            resp = add_rate_limit_headers(resp, chat_rate_limit, 0, reset_ts);
            resp.headers_mut().insert(
                HeaderName::from_static("retry-after"),
                HeaderValue::from(RATE_LIMIT_WINDOW_SECS),
            );
            return add_session_key_header(resp, &session_id);
        }
    };

    // Cross-transport guard: reject HTTP requests when an active WebSocket
    // connection holds this session. Checked AFTER session_queue acquisition
    // after session queue acquisition and against ws_connections which is
    // connection-scoped.
    {
        let ws = state.ws_connections.lock();
        if ws.contains(&session_key) {
            return add_session_key_header(
                add_request_id_header(
                    error_response(
                        StatusCode::CONFLICT,
                        "cross_transport_session_in_use",
                        "This session is currently owned by an active WebSocket connection. Disconnect the WebSocket or use a different session key.",
                        None,
                        None,
                    ),
                    &request_id,
                ),
                &session_id,
            );
        }
    }

    if let Some(ref backend) = state.session_backend {
        if let Err(err) = try_check_session_ownership_on_resume(
            backend.as_ref(),
            &session_key,
            &agent_alias,
            session_id_from_header.as_deref(),
        ) {
            return add_session_key_header(
                add_request_id_header(
                    error_response(err.status, &err.error_type, &err.message, None, None),
                    &request_id,
                ),
                &session_id,
            );
        }

        // Load history only after confirming the session belongs to this agent.
        if session_id_from_header.is_some() && !request_has_authoritative_history {
            let messages = backend.load(&session_key);
            if !messages.is_empty() {
                agent.seed_history(&messages);
            }
        }
    }

    if !request_history.is_empty() {
        agent.seed_history(&request_history);
    }

    agent.set_temperature(Some(request.temperature));
    let tool_choice_mode = parse_tool_choice(&request.tool_choice);
    let configured_tools: std::collections::HashSet<String> =
        agent.get_configured_tool_names().into_iter().collect();
    match tool_choice_mode {
        ToolChoiceMode::None => {
            agent.disable_tools();
        }
        _ => {
            match resolve_tool_specs(&request.tool_choice, &request.tools, &configured_tools) {
                Err(e) => {
                    return add_session_key_header(
                        add_request_id_header(e, &request_id),
                        &session_id,
                    );
                }
                Ok(Some(specs)) => {
                    if specs.is_empty() {
                        agent.disable_tools();
                    } else {
                        agent.set_tool_specs(specs);
                    }
                }
                Ok(None) => {
                    // Auto + tools=None → use default tool set
                }
            }
        }
    }

    if let Some(ref backend) = state.session_backend {
        if let Err(err) =
            try_persist_session_ownership(backend.as_ref(), &session_key, &agent_alias)
        {
            return add_request_id_header(
                add_session_key_header(
                    error_response(err.status, &err.error_type, &err.message, None, None),
                    &session_id,
                ),
                &request_id,
            );
        }
    }

    // Resolve a per-request memory handle for consolidation, mirroring the
    // WS per-connection memory handle path.
    // On failure we degrade to `None` (consolidation disabled) but the turn
    // still proceeds — same graceful-degradation contract as WS. Both the
    // streaming and blocking paths go through `run_gateway_turn`, which owns
    // cost-tracking context construction, `agent_start`/`agent_end`
    // broadcasts, and the `gateway_<channel>_turn` tracing record.
    let ws_memory = match resolve_http_memory_handle(&config, &agent_alias).await {
        Ok(memory) => memory,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "agent": &agent_alias,
                        "error": format!("{e:#}"),
                    })),
                "HTTP per-agent memory resolution failed; consolidation disabled for request"
            );
            None
        }
    };

    let include_usage = request
        .stream_options
        .as_ref()
        .map(|o| o.include_usage)
        .unwrap_or(false);
    let timeout_secs = state
        .config
        .read()
        .gateway
        .long_running_request_timeout_secs;
    let turn_request_id = request_id.clone();
    let turn_session_id = session_id.clone();
    let turn_result = tokio::time::timeout(Duration::from_secs(timeout_secs), async move {
        if request.stream {
            stream_mode(
                agent,
                user_message,
                response_model,
                include_usage,
                turn_request_id,
                chat_rate_limit,
                rate_limit_remaining,
                reset_ts,
                session_key,
                turn_session_id,
                state,
                ws_memory,
                session_guard,
            )
            .await
        } else {
            drop(session_guard);
            blocking_mode(
                agent,
                user_message,
                response_model,
                turn_request_id,
                chat_rate_limit,
                rate_limit_remaining,
                reset_ts,
                session_key,
                turn_session_id,
                state,
                ws_memory,
            )
            .await
        }
    })
    .await;

    match turn_result {
        Ok(response) => response,
        Err(_elapsed) => {
            let msg = format!("Request exceeded the {}s timeout", timeout_secs);
            add_session_key_header(
                add_request_id_header(
                    error_response(
                        StatusCode::REQUEST_TIMEOUT,
                        "timeout_error",
                        &msg,
                        None,
                        None,
                    ),
                    &request_id,
                ),
                &session_id,
            )
        }
    }
}

// Stream mode

#[allow(clippy::too_many_arguments)]
async fn stream_mode(
    agent: zeroclaw_runtime::agent::Agent,
    user_message: String,
    model: String,
    include_usage: bool,
    request_id: String,
    rate_limit: u32,
    rate_limit_remaining: u32,
    rate_limit_reset: u64,
    session_key: String,
    session_id: String,
    state: AppState,
    ws_memory: Option<Arc<dyn zeroclaw_memory::Memory>>,
    session_guard: crate::session_queue::SessionGuard,
) -> Response {
    let chunk_id = generate_completion_id();
    let created = Utc::now().timestamp() as u64;
    let model_for_bcast = model.clone();

    // Bridge channel: the spawned runner task pushes OpenAI SSE `Event`s here;
    // the HTTP response body pulls from the receiver. When the runner task
    // completes (turn + post-turn spine) it pushes the terminal chunks
    // (error-if-empty / stop / usage / [DONE]) and drops the sender, which
    // closes the SSE stream.
    let (sse_tx, sse_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(64);
    // One sender is moved into the forward closure (mid-stream chunks); the
    // other is retained by the spawned task for terminal-chunk emission.
    let sse_tx_forward = sse_tx.clone();

    // The forward closure drains `handle.event_rx` (TurnEvents from the running
    // turn) and maps each to an OpenAI SSE chunk, pushing it into `sse_tx`.
    // It returns the aggregated (input, output) token counts (from
    // `TurnEvent::Usage`) so the runner can record them in the tracing record.
    // On SSE write failure (client disconnect) it cancels the turn's cancel
    // token and stops draining — aligning with the WS disconnect path.
    let model_for_forward = model.clone();
    let chunk_id_for_forward = chunk_id.clone();
    let model_for_forward_bcast = model_for_forward.clone();
    let forward = move |handle: crate::turn_runner::TurnRunnerHandle| {
        let crate::turn_runner::TurnRunnerHandle {
            event_rx,
            cancel_token,
        } = handle;
        let mut event_rx = event_rx;
        let chunk_id = chunk_id_for_forward;
        let created = created;
        let model_for_bcast = model_for_forward_bcast;
        let sse_tx = sse_tx_forward;
        async move {
            let mut total_input_tokens: Option<u64> = None;
            let mut total_output_tokens: Option<u64> = None;
            let mut last_input_tokens: Option<u64> = None;

            // First role chunk (OpenAI streams an initial assistant role delta).
            if sse_tx
                .send(Ok::<_, Infallible>(make_chunk(
                    &chunk_id,
                    created,
                    &model_for_bcast,
                    Some("assistant"),
                    Some(String::new()),
                    None,
                    None,
                )))
                .await
                .is_err()
            {
                cancel_token.cancel();
                return (total_input_tokens, total_output_tokens, last_input_tokens);
            }

            while let Some(event) = event_rx.recv().await {
                let chunk = match event {
                    TurnEvent::Chunk { delta } => make_chunk(
                        &chunk_id,
                        created,
                        &model_for_bcast,
                        None,
                        Some(delta),
                        None,
                        None,
                    ),
                    TurnEvent::Thinking { .. } => {
                        // reasoning events are internal — do not emit as
                        // delta.content (streaming and blocking must match).
                        continue;
                    }
                    TurnEvent::ToolCall { .. } => {
                        // transparent execution — ZeroClaw has already
                        // executed the tool; do not emit client-actionable
                        // delta.tool_calls chunks (RFC 8603).
                        continue;
                    }
                    TurnEvent::ToolResult { .. } => {
                        // transparent — no SSE chunk
                        continue;
                    }
                    TurnEvent::ApprovalRequest { .. } => {
                        // non-interactive: auto-handled by the runtime
                        continue;
                    }
                    TurnEvent::Usage {
                        input_tokens,
                        output_tokens,
                        ..
                    } => {
                        // Aggregate per-call usage; the runner reports the
                        // totals in the tracing record and the SSE usage
                        // chunk uses the runner's aggregated totals.
                        if let Some(it) = input_tokens {
                            total_input_tokens =
                                Some(total_input_tokens.unwrap_or(0).saturating_add(it));
                            last_input_tokens = Some(it);
                        }
                        if let Some(ot) = output_tokens {
                            total_output_tokens =
                                Some(total_output_tokens.unwrap_or(0).saturating_add(ot));
                        }
                        continue;
                    }
                    TurnEvent::HistoryTrimmed { .. } => {
                        // transparent
                        continue;
                    }
                    TurnEvent::Plan { .. } => {
                        // transparent
                        continue;
                    }
                };
                if sse_tx.send(Ok::<_, Infallible>(chunk)).await.is_err() {
                    // Client disconnected (SSE body dropped). Cancel the turn
                    // so the runtime stops producing events, then stop
                    // draining — matching the WS disconnect behavior.
                    cancel_token.cancel();
                    break;
                }
            }

            (total_input_tokens, total_output_tokens, last_input_tokens)
        }
    };

    // The runner takes `&AppState` and `&mut Agent` and is async; for HTTP the
    // SSE body is consumed by axum AFTER `stream_mode` returns, so we cannot
    // `await` the runner inline (it would block the handler until the turn
    // completes and the SSE body — which drains the runner's event channel —
    // would never start). Spawn the entire runner call as a background task
    // that owns the agent; the forward closure (capturing `sse_tx`) pushes
    // chunks to the response body concurrently with the turn.
    let session_key_for_runner = session_key.clone();
    let user_message_for_runner = user_message.clone();

    // Read timeout config before spawning -- the value is Copy (u64).
    let timeout_secs = {
        let cfg = state.config.read();
        cfg.gateway.long_running_request_timeout_secs
    };

    let _runner_task = zeroclaw_spawn::spawn!(async move {
        // Hold the session queue guard for the full turn duration.
        // The guard serialises concurrent same-session requests across
        // the complete lifecycle: turn execution → persistence → state.
        let _session_guard = session_guard;
        let mut agent = agent;
        let outcome = match tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            crate::turn_runner::run_gateway_turn(
                &state,
                &mut agent,
                &user_message_for_runner,
                &session_key_for_runner,
                &ws_memory,
                None,
                "http",
                forward,
            ),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_elapsed) => {
                // ── Timeout: run_gateway_turn was cancelled mid-flight ──
                //
                // run_gateway_turn registered a CancellationToken in
                // state.cancel_tokens and set the session state to
                // "running" BEFORE the tokio::join! -- those side-effects
                // survive the cancellation and must be cleaned up here.
                //
                // 1. Cancel the token so any waiters (e.g., DELETE session)
                //    are unblocked, then remove the entry.
                {
                    let mut tokens = state.cancel_tokens.lock();
                    if let Some(token) = tokens.remove(&session_key_for_runner) {
                        token.cancel();
                    }
                }

                // 2. Best-effort session state reset to "idle".
                if let Some(ref backend) = state.session_backend {
                    let _ = backend.set_session_state(&session_key_for_runner, "idle", None);
                }

                // 3. Broadcast agent_end so dashboards don't show a
                //    perpetual "running" indicator.
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "agent_end",
                }));

                // 4. Synthetic error outcome -- the terminal SSE handler
                //    below will emit an error event to the client.
                crate::turn_runner::TurnOutcome {
                    status: crate::turn_runner::TurnStatus::Error,
                    error: Some(format!("Gateway turn timed out after {}s", timeout_secs)),
                    response_text: String::new(),
                    new_messages: vec![],
                    total_input_tokens: None,
                    total_output_tokens: None,
                    last_input_tokens: None,
                    usage: None,
                    max_context_tokens: 0,
                    turn_id: String::new(),
                    turn_provider: String::new(),
                    turn_model: String::new(),
                }
            }
        };

        // ── Terminal SSE chunks (transport-specific) ───────────────
        // The runner has already persisted `new_messages`, transitioned
        // session state, broadcast `agent_end`, and written the tracing
        // record. Here we only emit the SSE terminal chunks. We need the
        // accumulated full response to decide whether to emit an error chunk
        // (current HTTP behavior: only emit `[Error: ...]` when no content
        // was streamed). The forward closure accumulated it internally; the
        // runner's `response_text` is the runtime's committed response
        // (partial + marker on cancel) — but for the "empty vs non-empty"
        // Terminal chunk: Success → stop; Error/Cancelled → SSE error event.
        // Aligned with the blocking response's 500 ErrorResponse contract.
        match outcome.status {
            crate::turn_runner::TurnStatus::Success => {
                let _ = sse_tx
                    .send(Ok::<_, Infallible>(make_chunk(
                        &chunk_id,
                        created,
                        &model_for_bcast,
                        None,
                        None,
                        None,
                        Some("stop"),
                    )))
                    .await;
            }
            _ => {
                let msg = outcome
                    .error
                    .clone()
                    .unwrap_or_else(|| "agent task terminated unexpectedly".to_string());
                let error_body = serde_json::json!({
                    "error": {
                        "message": msg,
                        "type": "internal_error",
                        "code": null,
                        "param": null,
                        "status": 500
                    }
                });
                let error_event = Event::default()
                    .event("error")
                    .json_data(&error_body)
                    .unwrap_or_else(|_| Event::default().event("error").data("[Error]"));
                let _ = sse_tx.send(Ok::<_, Infallible>(error_event)).await;
            }
        }

        // Usage chunk (only when `include_usage` was requested). The runner
        // aggregates input/output tokens from the forward closure's return.
        if include_usage {
            let input = outcome.total_input_tokens.unwrap_or(0);
            let output = outcome.total_output_tokens.unwrap_or(0);
            let _ = sse_tx
                .send(Ok::<_, Infallible>(make_usage_chunk(
                    &chunk_id,
                    created,
                    &model_for_bcast,
                    input,
                    output,
                    input + output,
                )))
                .await;
        }

        // [DONE] sentinel (always last).
        let _ = sse_tx
            .send(Ok::<_, Infallible>(Event::default().data("[DONE]")))
            .await;

        // `sse_tx` drops here, closing the SSE stream.
        drop(outcome);
    });

    let sse_stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);
    let mut response = Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    response = add_request_id_header(response, &request_id);
    response = add_rate_limit_headers(response, rate_limit, rate_limit_remaining, rate_limit_reset);

    response.headers_mut().insert(
        HeaderName::from_static("x-session-key"),
        HeaderValue::from_str(&session_id).unwrap_or_else(|_| HeaderValue::from_static("")),
    );

    response
}

// Blocking mode

#[allow(clippy::too_many_arguments)]
async fn blocking_mode(
    agent: zeroclaw_runtime::agent::Agent,
    user_message: String,
    model: String,
    request_id: String,
    rate_limit: u32,
    rate_limit_remaining: u32,
    rate_limit_reset: u64,
    session_key: String,
    session_id: String,
    state: AppState,
    ws_memory: Option<Arc<dyn zeroclaw_memory::Memory>>,
) -> Response {
    // Blocking (non-streaming) mode: run the turn through the shared
    // `run_gateway_turn` spine (same as stream_mode), but instead of emitting
    // SSE chunks the forward closure simply drains `event_rx` to completion —
    // accumulating the response text and per-call usage — so the turn future
    // doesn't backpressure on the capped (64) event channel. After the runner
    // returns `TurnOutcome` we build a single `ChatCompletionResponse` JSON.
    //
    // The runner owns: cost-tracking context, `agent_start`/`agent_end`
    // broadcasts, `scope_session_key`, cancel-token lifecycle, session-state
    // transitions, `persist_conversation_messages`, memory-consolidation spawn,
    // and the `gateway_http_turn` tracing record.
    //
    // Blocking mode has no streaming body to drop, so the forward closure does
    // NOT cancel-on-disconnect: if the client disconnects the HTTP connection
    // drops but the turn runs to completion — preserving prior blocking
    // behavior (`agent.turn()` could not be cancelled either).
    let forward = |handle: crate::turn_runner::TurnRunnerHandle| {
        let crate::turn_runner::TurnRunnerHandle {
            event_rx,
            cancel_token: _,
        } = handle;
        let mut event_rx = event_rx;
        async move {
            let mut total_input_tokens: Option<u64> = None;
            let mut total_output_tokens: Option<u64> = None;
            let mut last_input_tokens: Option<u64> = None;

            while let Some(event) = event_rx.recv().await {
                match event {
                    TurnEvent::Chunk { delta } => {
                        // The runner's `outcome.response_text` is the
                        // authoritative full response; we only need to drain
                        // the channel here to avoid backpressure.
                        let _ = delta;
                    }
                    TurnEvent::Thinking { delta: _ } => {}
                    TurnEvent::ToolCall { .. } | TurnEvent::ToolResult { .. } => {}
                    TurnEvent::ApprovalRequest { .. } => {}
                    TurnEvent::Usage {
                        input_tokens,
                        output_tokens,
                        ..
                    } => {
                        if let Some(it) = input_tokens {
                            total_input_tokens =
                                Some(total_input_tokens.unwrap_or(0).saturating_add(it));
                            last_input_tokens = Some(it);
                        }
                        if let Some(ot) = output_tokens {
                            total_output_tokens =
                                Some(total_output_tokens.unwrap_or(0).saturating_add(ot));
                        }
                    }
                    TurnEvent::HistoryTrimmed { .. } => {}
                    TurnEvent::Plan { .. } => {}
                }
            }

            (total_input_tokens, total_output_tokens, last_input_tokens)
        }
    };

    // ── Spawned runner task (same pattern as stream_mode) ──
    // By spawning the turn lifecycle in a separate task, cleanup
    // (cancel_tokens removal, session state transition, persistence,
    // agent_end broadcast, tracing) is guaranteed to execute even when
    // the enclosing HTTP request future is dropped by the TimeoutLayer.
    let (outcome_tx, outcome_rx) =
        tokio::sync::oneshot::channel::<crate::turn_runner::TurnOutcome>();

    let session_key_for_spawn = session_key.clone();
    let user_message_for_spawn = user_message.clone();
    let ws_memory_for_spawn = ws_memory.clone();
    let state_for_spawn = state.clone();

    let _runner_task = zeroclaw_spawn::spawn!(async move {
        let mut agent = agent;
        let outcome = crate::turn_runner::run_gateway_turn(
            &state_for_spawn,
            &mut agent,
            &user_message_for_spawn,
            &session_key_for_spawn,
            &ws_memory_for_spawn,
            None,
            "http",
            forward,
        )
        .await;
        // If the receiver was dropped (TimeoutLayer killed the request
        // future), the send fails silently -- the spawned task has still
        // completed all cleanup. The outcome is simply discarded.
        let _ = outcome_tx.send(outcome);
    });

    // ── Await the oneshot (inside TimeoutLayer scope) ──
    let outcome = match outcome_rx.await {
        Ok(outcome) => outcome,
        Err(_recv_error) => {
            // RecvError means the sender was dropped without sending.
            // This can only happen if the spawned task panicked (tokio
            // aborts the task on panic). Return a 500 to the caller.
            // Note: the standard timeout path does NOT reach here -- the
            // TimeoutLayer drops the entire future, so this match arm is
            // also dropped. It only serves as a panic-safety net.
            let mut resp = add_request_id_header(
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "Agent task terminated unexpectedly",
                    None,
                    None,
                ),
                &request_id,
            );
            resp = add_rate_limit_headers(resp, rate_limit, rate_limit_remaining, rate_limit_reset);
            return add_session_key_header(resp, &session_id);
        }
    };

    match outcome.status {
        crate::turn_runner::TurnStatus::Success => {
            // Transparent execution: the runtime has already executed any
            // tool calls internally. Return the final text answer — do not
            // expose already-executed tool_calls that an OpenAI client would
            // misinterpret as work it must perform.
            let input_tokens = outcome.total_input_tokens.unwrap_or(0);
            let output_tokens = outcome.total_output_tokens.unwrap_or(0);

            let body = ChatCompletionResponse {
                id: generate_completion_id(),
                object: "chat.completion",
                created: Utc::now().timestamp() as u64,
                model: model.clone(),
                choices: vec![NonStreamChoice {
                    index: 0,
                    message: AssistantMessage {
                        role: "assistant",
                        content: Some(outcome.response_text.clone()),
                        tool_calls: None,
                    },
                    finish_reason: "stop".to_string(),
                    logprobs: None,
                }],
                usage: CompletionUsage {
                    prompt_tokens: input_tokens,
                    completion_tokens: output_tokens,
                    total_tokens: input_tokens + output_tokens,
                },
            };

            let mut resp = add_request_id_header(Json(body).into_response(), &request_id);
            resp = add_rate_limit_headers(resp, rate_limit, rate_limit_remaining, rate_limit_reset);
            resp.headers_mut().insert(
                HeaderName::from_static("x-session-key"),
                HeaderValue::from_str(&session_id).unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            resp
        }
        crate::turn_runner::TurnStatus::Error => {
            let sanitized = outcome
                .error
                .clone()
                .unwrap_or_else(|| "agent task terminated unexpectedly".to_string());
            let mut resp = add_request_id_header(
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    &sanitized,
                    None,
                    None,
                ),
                &request_id,
            );
            resp = add_rate_limit_headers(resp, rate_limit, rate_limit_remaining, rate_limit_reset);
            add_session_key_header(resp, &session_id)
        }
        crate::turn_runner::TurnStatus::Cancelled => {
            // Blocking mode has no streaming body to drop, so a client
            // disconnect cannot signal cancellation the way an SSE body can.
            // If the turn is nonetheless cancelled (e.g. via the abort
            // endpoint hitting the cancel token), preserve the conservative
            // error mapping: return 500 internal_error with the outcome's
            // error message (or a sanitized default).
            let sanitized = outcome
                .error
                .clone()
                .unwrap_or_else(|| "agent turn cancelled".to_string());
            let mut resp = add_request_id_header(
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    &sanitized,
                    None,
                    None,
                ),
                &request_id,
            );
            resp = add_rate_limit_headers(resp, rate_limit, rate_limit_remaining, rate_limit_reset);
            add_session_key_header(resp, &session_id)
        }
    }
}

// Request Validation

#[allow(clippy::result_large_err)]
fn validate_request(req: &ChatCompletionRequest) -> Result<(), Response> {
    if req.messages.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "messages must not be empty",
            None,
            Some("messages"),
        ));
    }

    if let Some(ref tools) = req.tools {
        for tool in tools {
            if tool.kind != "function" {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "Only 'function' tool type is supported",
                    None,
                    Some("tools"),
                ));
            }
            if tool.function.name.trim().is_empty() {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "tool.function.name is required",
                    None,
                    Some("tools"),
                ));
            }
        }
    }

    if let Some(ref tc) = req.tool_choice {
        if let Some(s) = tc.as_str() {
            if !["auto", "none", "required"].contains(&s) {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    &format!("Unsupported tool_choice: {s}"),
                    None,
                    Some("tool_choice"),
                ));
            }
        } else if tc.is_object() {
            // Specific-function tool_choice is not yet wired through to
            // providers; reject early rather than silently degrading.
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "unsupported_parameter",
                "tool_choice with a specific function is not yet supported; use \"auto\" instead",
                None,
                Some("tool_choice"),
            ));
        } else {
            // Malformed: number, array, bool, null
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "tool_choice must be a string (\"auto\", \"none\", \"required\") or a function object",
                None,
                Some("tool_choice"),
            ));
        }
    }

    Ok(())
}

/// Resolve tool specs from request parameters, returning an error if the
/// tool configuration is invalid (fail-closed).
///
/// Returns `Ok(Some(specs))` when tools should be restricted,
/// `Ok(None)` when the default tool set should be used,
/// `Err(response)` when the request is invalid.
#[allow(clippy::result_large_err)]
fn resolve_tool_specs(
    tool_choice: &Option<serde_json::Value>,
    tools: &Option<Vec<ChatCompletionTool>>,
    configured_tools: &std::collections::HashSet<String>,
) -> Result<Option<Vec<zeroclaw_runtime::tools::ToolSpec>>, Response> {
    let mode = parse_tool_choice(tool_choice);
    match mode {
        ToolChoiceMode::None => {
            // disable_tools() is handled by the caller
            Ok(Some(Vec::new()))
        }
        ToolChoiceMode::Auto => {
            match tools {
                None => Ok(None), // Auto + no tools → use default set
                Some(requested_tools) => {
                    if requested_tools.is_empty() {
                        return Err(error_response(
                            StatusCode::BAD_REQUEST,
                            "invalid_request_error",
                            "tools list must not be empty",
                            None,
                            Some("tools"),
                        ));
                    }
                    // Reject if any named tool is unavailable (fail-closed contract)
                    // (fail-closed, not silently filtering).
                    let unknown: Vec<_> = requested_tools
                        .iter()
                        .filter(|t| !configured_tools.contains(&t.function.name))
                        .collect();
                    if !unknown.is_empty() {
                        return Err(error_response(
                            StatusCode::BAD_REQUEST,
                            "invalid_request_error",
                            &format!(
                                "Unknown tool(s): {}",
                                unknown
                                    .iter()
                                    .map(|t| t.function.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            None,
                            Some("tools"),
                        ));
                    }
                    Ok(Some(convert_request_tools(requested_tools)))
                }
            }
        }
        ToolChoiceMode::Required => {
            // tool_choice: "required" is not yet wired through to providers;
            // the choice mode is never carried into the LLM request. Reject
            // early rather than silently degrading to auto behaviour.
            Err(error_response(
                StatusCode::BAD_REQUEST,
                "unsupported_parameter",
                "tool_choice: \"required\" is not yet supported; use \"auto\" instead",
                None,
                Some("tool_choice"),
            ))
        }
        ToolChoiceMode::SpecificFunction { ref name } => {
            // Named-function tool_choice is not yet wired through to
            // providers; only the available-tool list is narrowed, but the
            // provider still uses its default auto choice. Reject early
            // rather than silently degrading.
            let _ = name; // consumed by the error message
            Err(error_response(
                StatusCode::BAD_REQUEST,
                "unsupported_parameter",
                "tool_choice with a specific function is not yet supported; use \"auto\" instead",
                None,
                Some("tool_choice"),
            ))
        }
    }
}

/// Reject unsupported per-request generation parameters with a clear error.
///
/// These fields are parsed from the request body but silently ignored by the
/// current runtime. Returning a 400 instead avoids surprising callers who
/// expect them to take effect.
#[allow(clippy::result_large_err)]
fn validate_unsupported_params(req: &ChatCompletionRequest) -> Result<(), Response> {
    if req.max_tokens.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "max_tokens is not supported per-request; configure it in provider settings",
            None,
            Some("max_tokens"),
        ));
    }
    if req.top_p.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "top_p is not supported per-request",
            None,
            Some("top_p"),
        ));
    }
    if req.stop.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "stop is not supported per-request",
            None,
            Some("stop"),
        ));
    }
    if req.presence_penalty.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "presence_penalty is not supported per-request",
            None,
            Some("presence_penalty"),
        ));
    }
    if req.frequency_penalty.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "frequency_penalty is not supported per-request",
            None,
            Some("frequency_penalty"),
        ));
    }
    if req.n.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "n is not supported; ZeroClaw returns a single completion per request",
            None,
            Some("n"),
        ));
    }
    if req.response_format.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "response_format is not supported; configure output format in provider settings",
            None,
            Some("response_format"),
        ));
    }
    if req.seed.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "seed is not supported; configure in provider settings",
            None,
            Some("seed"),
        ));
    }
    if req.logprobs.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "logprobs is not supported",
            None,
            Some("logprobs"),
        ));
    }
    if req.top_logprobs.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "top_logprobs is not supported",
            None,
            Some("top_logprobs"),
        ));
    }
    if req.user.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "user is not supported",
            None,
            Some("user"),
        ));
    }
    if req.logit_bias.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "logit_bias is not supported",
            None,
            Some("logit_bias"),
        ));
    }
    if req.max_completion_tokens.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "max_completion_tokens is not supported; use provider settings",
            None,
            Some("max_completion_tokens"),
        ));
    }
    Ok(())
}

// Tool Choice Helpers

enum ToolChoiceMode {
    Auto,
    None,
    Required,
    SpecificFunction { name: String },
}

fn parse_tool_choice(value: &Option<serde_json::Value>) -> ToolChoiceMode {
    match value {
        None => ToolChoiceMode::Auto,
        Some(v) => {
            if let Some(s) = v.as_str() {
                match s {
                    "auto" => ToolChoiceMode::Auto,
                    "none" => ToolChoiceMode::None,
                    "required" => ToolChoiceMode::Required,
                    // Guarded by validate_request; defense-in-depth fallback.
                    _ => ToolChoiceMode::Auto,
                }
            } else if let Some(obj) = v.as_object() {
                if let Some(func) = obj.get("function").and_then(|f| f.as_object()) {
                    if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                        return ToolChoiceMode::SpecificFunction {
                            name: name.to_string(),
                        };
                    }
                }
                ToolChoiceMode::Required
            } else {
                ToolChoiceMode::Auto
            }
        }
    }
}

fn convert_request_tools(tools: &[ChatCompletionTool]) -> Vec<zeroclaw_runtime::tools::ToolSpec> {
    tools
        .iter()
        .map(|tool| zeroclaw_runtime::tools::ToolSpec {
            name: tool.function.name.clone(),
            description: tool.function.description.clone().unwrap_or_default(),
            parameters: Arc::new(tool.function.parameters.clone()),
            output: None,
            param_domains: std::collections::BTreeMap::new(),
        })
        .collect()
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_message(role: &str, content: &str) -> ChatCompletionMessage {
        ChatCompletionMessage {
            role: role.to_string(),
            content: Some(content.to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn test_agent_alias_from_model_defaults() {
        let config = zeroclaw_config::schema::Config::default();

        assert_eq!(agent_alias_from_model("", &config).unwrap(), "default");
        assert_eq!(
            agent_alias_from_model("zeroclaw", &config).unwrap(),
            "default"
        );
        assert_eq!(
            agent_alias_from_model("zeroclaw/default", &config).unwrap(),
            "default"
        );
        assert_eq!(agent_alias_from_model("gpt-4", &config).unwrap(), "default");
    }

    #[test]
    fn test_agent_alias_from_model_explicit_alias() {
        let config = zeroclaw_config::schema::Config::default();

        assert_eq!(
            agent_alias_from_model("zeroclaw/coding", &config).unwrap(),
            "coding"
        );
        assert_eq!(
            agent_alias_from_model("zeroclaw:coding", &config).unwrap(),
            "coding"
        );
        assert_eq!(
            agent_alias_from_model("agent:coding", &config).unwrap(),
            "coding"
        );
    }

    #[test]
    fn test_agent_alias_from_model_rejects_empty_explicit_alias() {
        let config = zeroclaw_config::schema::Config::default();

        assert!(agent_alias_from_model("zeroclaw/", &config).is_err());
        assert!(agent_alias_from_model("zeroclaw:", &config).is_err());
        assert!(agent_alias_from_model("agent:", &config).is_err());
    }

    #[test]
    fn test_chat_completion_request_defaults_missing_model() {
        let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap();

        assert_eq!(req.model, "");
    }

    #[test]
    fn test_split_messages_single_user() {
        let messages = vec![chat_message("user", "hello")];
        let (history, current) = split_messages(&messages);

        assert!(history.is_empty());
        assert_eq!(current, "hello");
    }

    #[test]
    fn test_split_messages_system_and_user() {
        let messages = vec![
            chat_message("system", "be brief"),
            chat_message("user", "hello"),
        ];
        let (history, current) = split_messages(&messages);

        assert!(history.is_empty());
        assert_eq!(current, "hello");
        assert_eq!(
            request_system_prompt_prefix(&messages).as_deref(),
            Some("be brief")
        );
    }

    #[test]
    fn test_split_messages_multiturn() {
        let messages = vec![
            chat_message("user", "hello"),
            chat_message("assistant", "hi"),
            chat_message("user", "continue"),
        ];
        let (history, current) = split_messages(&messages);

        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "hello");
        assert_eq!(history[1].role, "assistant");
        assert_eq!(history[1].content, "hi");
        assert_eq!(current, "continue");
    }

    #[test]
    fn test_split_messages_tool_current_turn() {
        let messages = vec![
            chat_message("user", "call tool"),
            chat_message("assistant", "calling"),
            chat_message("tool", "result"),
        ];
        let (history, current) = split_messages(&messages);

        assert_eq!(history.len(), 2);
        assert_eq!(current, "result");
    }

    #[test]
    fn test_split_messages_normalizes_function_role() {
        let messages = vec![
            chat_message("function", "legacy result"),
            chat_message("user", "continue"),
        ];
        let (history, current) = split_messages(&messages);

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "tool");
        assert_eq!(history[0].content, "legacy result");
        assert_eq!(current, "continue");
    }

    #[test]
    fn test_split_messages_fallback_without_active_turn() {
        let messages = vec![
            chat_message("system", "be brief"),
            chat_message("assistant", "done"),
        ];
        let (history, current) = split_messages(&messages);

        assert!(history.is_empty());
        assert_eq!(current, "[system] be brief\n[assistant] done");
    }

    #[test]
    fn test_request_system_prompt_prefix_filters_empty_parts() {
        let messages = vec![
            chat_message("system", "be brief"),
            chat_message("developer", "  "),
            chat_message("developer", "use json"),
            chat_message("user", "hello"),
        ];

        assert_eq!(
            request_system_prompt_prefix(&messages).as_deref(),
            Some("be brief\n\nuse json")
        );
    }

    #[test]
    fn test_request_has_authoritative_history() {
        let single = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![chat_message("user", "hi")],
            stream: false,
            temperature: 0.7,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
            n: None,
            response_format: None,
            seed: None,
            logprobs: None,
            top_logprobs: None,
            user: None,
            logit_bias: None,
            max_completion_tokens: None,
        };
        assert!(!request_has_authoritative_history(&single));

        let multi = ChatCompletionRequest {
            messages: vec![
                chat_message("user", "hi"),
                chat_message("assistant", "hello"),
            ],
            ..single
        };
        assert!(request_has_authoritative_history(&multi));
    }

    #[test]
    fn test_parse_tool_choice_none() {
        let result = parse_tool_choice(&None);
        assert!(matches!(result, ToolChoiceMode::Auto));
    }

    #[test]
    fn test_parse_tool_choice_string_auto() {
        let value = serde_json::json!("auto");
        let result = parse_tool_choice(&Some(value));
        assert!(matches!(result, ToolChoiceMode::Auto));
    }

    #[test]
    fn test_parse_tool_choice_string_none() {
        let value = serde_json::json!("none");
        let result = parse_tool_choice(&Some(value));
        assert!(matches!(result, ToolChoiceMode::None));
    }

    #[test]
    fn test_parse_tool_choice_string_required() {
        let value = serde_json::json!("required");
        let result = parse_tool_choice(&Some(value));
        assert!(matches!(result, ToolChoiceMode::Required));
    }

    #[test]
    fn test_parse_tool_choice_string_unknown() {
        let value = serde_json::json!("unknown");
        let result = parse_tool_choice(&Some(value));
        assert!(matches!(result, ToolChoiceMode::Auto));
    }

    #[test]
    fn test_parse_tool_choice_object_specific_function() {
        let value = serde_json::json!({
            "type": "function",
            "function": { "name": "weather_query" }
        });
        let result = parse_tool_choice(&Some(value));
        match result {
            ToolChoiceMode::SpecificFunction { name } => assert_eq!(name, "weather_query"),
            _ => panic!("expected SpecificFunction"),
        }
    }

    #[test]
    fn test_parse_tool_choice_object_missing_name() {
        let value = serde_json::json!({
            "type": "function",
            "function": {}
        });
        let result = parse_tool_choice(&Some(value));
        assert!(matches!(result, ToolChoiceMode::Required));
    }

    #[test]
    fn test_parse_tool_choice_invalid_type() {
        let value = serde_json::json!(123);
        let result = parse_tool_choice(&Some(value));
        assert!(matches!(result, ToolChoiceMode::Auto));
    }

    #[test]
    fn test_validate_request_empty_messages() {
        let req = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![],
            stream: false,
            temperature: 0.7,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
            ..Default::default()
        };
        let result = validate_request(&req);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_request_invalid_tool_type() {
        let req = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![ChatCompletionMessage {
                role: "user".into(),
                content: Some("hi".into()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: false,
            temperature: 0.7,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tools: Some(vec![ChatCompletionTool {
                kind: "other".into(),
                function: ToolFunction {
                    name: "test".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            tool_choice: None,
            stream_options: None,
            ..Default::default()
        };
        let result = validate_request(&req);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_request_unsupported_tool_choice() {
        let req = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![ChatCompletionMessage {
                role: "user".into(),
                content: Some("hi".into()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: false,
            temperature: 0.7,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tools: None,
            tool_choice: Some(serde_json::json!("invalid")),
            stream_options: None,
            ..Default::default()
        };
        let result = validate_request(&req);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_request_valid() {
        let req = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![ChatCompletionMessage {
                role: "user".into(),
                content: Some("hi".into()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: false,
            temperature: 0.7,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tools: Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "test_tool".into(),
                    description: Some("A test tool".into()),
                    parameters: serde_json::json!({"type": "object"}),
                },
            }]),
            tool_choice: Some(serde_json::json!("auto")),
            stream_options: None,
            ..Default::default()
        };
        let result = validate_request(&req);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_unsupported_params_max_tokens_rejected() {
        let req = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![chat_message("user", "hi")],
            stream: false,
            temperature: 0.7,
            max_tokens: Some(100),
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
            ..Default::default()
        };
        let result = validate_unsupported_params(&req);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_unsupported_params_top_p_rejected() {
        let req = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![chat_message("user", "hi")],
            stream: false,
            temperature: 0.7,
            max_tokens: None,
            top_p: Some(0.9),
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
            ..Default::default()
        };
        let result = validate_unsupported_params(&req);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_unsupported_params_all_rejected() {
        let req = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![chat_message("user", "hi")],
            stream: false,
            temperature: 0.7,
            max_tokens: Some(100),
            top_p: Some(0.9),
            stop: Some(serde_json::json!("stop")),
            presence_penalty: Some(0.5),
            frequency_penalty: Some(0.5),
            tools: None,
            tool_choice: None,
            stream_options: None,
            ..Default::default()
        };
        let result = validate_unsupported_params(&req);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_unsupported_params_none_accepted() {
        let req = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![chat_message("user", "hi")],
            stream: false,
            temperature: 0.7,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
            ..Default::default()
        };
        let result = validate_unsupported_params(&req);
        assert!(result.is_ok());
    }

    #[test]
    fn test_specific_function_unknown_tool_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!({"type":"function","function":{"name":"nonexistent"}})),
            &Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "nonexistent".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            &configured,
        );
        assert!(result.is_err(), "unknown tool should be rejected");
    }

    #[test]
    fn test_specific_function_not_in_tools_list_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!({"type":"function","function":{"name":"weather_query"}})),
            &Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "other_tool".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            &configured,
        );
        assert!(
            result.is_err(),
            "function not in tools list should be rejected"
        );
    }

    #[test]
    fn test_specific_function_without_tools_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!({"type":"function","function":{"name":"weather_query"}})),
            &None,
            &configured,
        );
        assert!(
            result.is_err(),
            "specific function without tools should be rejected"
        );
    }

    #[test]
    fn test_required_without_tools_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(&Some(serde_json::json!("required")), &None, &configured);
        assert!(result.is_err(), "required without tools should be rejected");
    }

    #[test]
    fn test_auto_all_unknown_tools_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!("auto")),
            &Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "nonexistent".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            &configured,
        );
        assert!(result.is_err(), "all unknown tools should be rejected");
    }

    #[test]
    fn test_required_all_unknown_tools_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!("required")),
            &Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "nonexistent".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            &configured,
        );
        assert!(
            result.is_err(),
            "required with all unknown tools should be rejected"
        );
    }

    #[test]
    fn test_auto_with_known_tools_succeeds() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!("auto")),
            &Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "weather_query".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            &configured,
        );
        assert!(result.is_ok(), "known tools should succeed");
        let specs = result.unwrap();
        assert!(specs.is_some());
        assert_eq!(specs.unwrap().len(), 1);
    }

    #[test]
    fn test_auto_without_tools_returns_none() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(&Some(serde_json::json!("auto")), &None, &configured);
        assert!(result.is_ok(), "auto without tools should succeed");
        assert!(
            result.unwrap().is_none(),
            "auto without tools should return None"
        );
    }

    // ── tool_choice=None (→ Auto) scenarios ─────────────────────────────

    #[test]
    fn test_none_tool_choice_with_known_tools_succeeds() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &None,
            &Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "weather_query".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            &configured,
        );
        assert!(
            result.is_ok(),
            "None tool_choice with known tools should succeed"
        );
        let specs = result.unwrap();
        assert!(specs.is_some());
        assert_eq!(specs.unwrap().len(), 1);
    }

    #[test]
    fn test_none_tool_choice_with_unknown_tools_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &None,
            &Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "nonexistent".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            &configured,
        );
        assert!(
            result.is_err(),
            "None tool_choice with unknown tools should be rejected"
        );
    }

    #[test]
    fn test_none_tool_choice_with_mixed_tools_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &None,
            &Some(vec![
                ChatCompletionTool {
                    kind: "function".into(),
                    function: ToolFunction {
                        name: "weather_query".into(),
                        description: None,
                        parameters: serde_json::json!({}),
                    },
                },
                ChatCompletionTool {
                    kind: "function".into(),
                    function: ToolFunction {
                        name: "nonexistent".into(),
                        description: None,
                        parameters: serde_json::json!({}),
                    },
                },
            ]),
            &configured,
        );
        assert!(
            result.is_err(),
            "mixed known+unknown tools must return 400 (fail-closed, per 8550)"
        );
    }

    // ── tool_choice="none" scenarios ────────────────────────────────────

    #[test]
    fn test_none_tool_choice_disables_tools() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(&Some(serde_json::json!("none")), &None, &configured);
        assert!(result.is_ok(), "none tool_choice should succeed");
        let specs = result.unwrap();
        assert!(specs.is_some());
        assert!(
            specs.unwrap().is_empty(),
            "none tool_choice should return empty specs"
        );
    }

    #[test]
    fn test_none_tool_choice_ignores_tools() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!("none")),
            &Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "weather_query".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            &configured,
        );
        assert!(
            result.is_ok(),
            "none tool_choice should succeed even with tools"
        );
        let specs = result.unwrap();
        assert!(specs.is_some());
        assert!(
            specs.unwrap().is_empty(),
            "none tool_choice should ignore tools param"
        );
    }

    // ── tool_choice="required" + known tools ────────────────────────────

    #[test]
    fn test_required_with_known_tools_rejected_unsupported() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!("required")),
            &Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "weather_query".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            &configured,
        );
        assert!(
            result.is_err(),
            "required should be rejected (not yet supported)"
        );
    }

    // ── tool_choice={function} + known function rejected (not yet supported)

    #[test]
    fn test_specific_function_with_known_tool_rejected_unsupported() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!({"type":"function","function":{"name":"weather_query"}})),
            &Some(vec![ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: "weather_query".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                },
            }]),
            &configured,
        );
        assert!(
            result.is_err(),
            "specific function should be rejected (not yet supported)"
        );
    }

    // ── Empty tools array scenarios ─────────────────────────────────────

    #[test]
    fn test_auto_with_empty_tools_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result =
            resolve_tool_specs(&Some(serde_json::json!("auto")), &Some(vec![]), &configured);
        assert!(result.is_err(), "auto with empty tools should be rejected");
    }

    #[test]
    fn test_required_with_empty_tools_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!("required")),
            &Some(vec![]),
            &configured,
        );
        assert!(
            result.is_err(),
            "required with empty tools should be rejected"
        );
    }

    #[test]
    fn test_specific_function_with_empty_tools_rejected() {
        let configured: std::collections::HashSet<String> =
            ["weather_query"].iter().map(|s| s.to_string()).collect();
        let result = resolve_tool_specs(
            &Some(serde_json::json!({"type":"function","function":{"name":"weather_query"}})),
            &Some(vec![]),
            &configured,
        );
        assert!(
            result.is_err(),
            "specific function with empty tools should be rejected"
        );
    }

    // ── SSE wire-level regression tests ──
    //
    // `chunk_json` is the testable inner layer of `make_chunk` that
    // builds the SSE delta JSON. These tests prove the wire shape
    // matches the transparent-execution contract: content chunks carry
    // no tool_calls, and terminal stop chunks carry neither content
    // nor tool_calls.

    #[test]
    fn chunk_json_no_tool_calls_in_content_chunk() {
        // The content chunk delta must NOT carry tool_calls.
        let v = super::chunk_json("id-1", 1, "m", None, Some("hello".into()), None, None);
        let delta = &v["choices"][0]["delta"];
        assert_eq!(delta["content"], "hello");
        assert!(
            delta.get("tool_calls").is_none(),
            "content chunk must not carry tool_calls — delta: {}",
            delta
        );
    }

    #[test]
    fn chunk_json_no_content_nor_tool_calls_in_stop_chunk() {
        // The terminal stop chunk delta must be empty (no content, no tool_calls).
        let v = super::chunk_json("id-1", 1, "m", None, None, None, Some("stop"));
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        let delta = &v["choices"][0]["delta"];
        assert!(
            delta.get("content").is_none(),
            "stop chunk must not have content — delta: {}",
            delta
        );
        assert!(
            delta.get("tool_calls").is_none(),
            "stop chunk must not have tool_calls — delta: {}",
            delta
        );
    }

    #[test]
    fn chunk_json_stop_finish_reason_empty_delta() {
        // Combined regression: terminal stop delta must be empty (no
        // content, no tool_calls, no role).
        let v = super::chunk_json("id-1", 1, "m", None, None, None, Some("stop"));
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        let delta = &v["choices"][0]["delta"];
        assert!(
            delta.as_object().is_none_or(|o| o.is_empty()),
            "stop delta must be empty — got: {}",
            delta
        );
    }

    // ── Ownership test infrastructure ───────────────────────────────────
    //
    // Extracted ownership-check functions tested with injectable
    // `SessionBackend` implementations. The extracted functions are
    // synchronous and require zero AppState/Config/Provider/Agent
    // construction.

    /// Builder for ad-hoc `SessionBackend` implementations used in ownership
    /// tests. Only the methods that are configured matter; unconfigured
    /// methods return vacuously-successful defaults.
    struct MockBackendBuilder {
        set_alias_result: std::io::Result<()>,
        get_alias_result: std::io::Result<Option<String>>,
        load_data: Vec<ChatMessage>,
    }

    impl MockBackendBuilder {
        fn new() -> Self {
            Self {
                set_alias_result: Ok(()),
                get_alias_result: Ok(None),
                load_data: vec![],
            }
        }

        fn set_alias_result(mut self, result: std::io::Result<()>) -> Self {
            self.set_alias_result = result;
            self
        }

        fn get_alias_result(mut self, result: std::io::Result<Option<String>>) -> Self {
            self.get_alias_result = result;
            self
        }

        fn load_data(mut self, data: Vec<ChatMessage>) -> Self {
            self.load_data = data;
            self
        }

        fn build(self) -> MockSessionBackend {
            MockSessionBackend {
                set_alias_result: self.set_alias_result,
                get_alias_result: self.get_alias_result,
                load_data: self.load_data,
            }
        }
    }

    struct MockSessionBackend {
        set_alias_result: std::io::Result<()>,
        get_alias_result: std::io::Result<Option<String>>,
        load_data: Vec<ChatMessage>,
    }

    impl SessionBackend for MockSessionBackend {
        fn load(&self, _session_key: &str) -> Vec<ChatMessage> {
            self.load_data.clone()
        }

        fn append(&self, _session_key: &str, _message: &ChatMessage) -> std::io::Result<()> {
            Ok(())
        }

        fn remove_last(&self, _session_key: &str) -> std::io::Result<bool> {
            Ok(false)
        }

        fn list_sessions(&self) -> Vec<String> {
            vec![]
        }

        fn session_exists(&self, _session_key: &str) -> bool {
            false
        }

        fn set_session_agent_alias(
            &self,
            _session_key: &str,
            _agent_alias: &str,
        ) -> std::io::Result<()> {
            match &self.set_alias_result {
                Ok(()) => Ok(()),
                Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
            }
        }

        fn get_session_agent_alias(&self, _session_key: &str) -> std::io::Result<Option<String>> {
            match &self.get_alias_result {
                Ok(val) => Ok(val.clone()),
                Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
            }
        }
    }

    /// Helper: create a simple user `ChatMessage` for load_data.
    fn user_msg(content: &str) -> ChatMessage {
        ChatMessage::user(content)
    }

    // ── WRITE tests (try_persist_session_ownership) ──────────────────────

    #[test]
    fn persist_session_ownership_succeeds() {
        let backend = MockBackendBuilder::new().build();
        let result = try_persist_session_ownership(&backend, "gw_test", "default");
        assert!(result.is_ok(), "successful write should return Ok");
    }

    #[test]
    fn persist_session_ownership_write_error_returns_500() {
        let backend = MockBackendBuilder::new()
            .set_alias_result(Err(std::io::Error::other("disk full")))
            .build();
        let result = try_persist_session_ownership(&backend, "gw_test", "default");
        assert!(result.is_err(), "failing write should return Err");
        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.error_type, "server_error");
    }

    #[test]
    fn persist_session_ownership_unsupported_is_ok() {
        let backend = MockBackendBuilder::new()
            .set_alias_result(Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not supported",
            )))
            .build();
        let result = try_persist_session_ownership(&backend, "gw_test", "default");
        assert!(
            result.is_ok(),
            "Unsupported should be treated as Ok (graceful degradation)"
        );
    }

    #[test]
    fn persist_session_ownership_error_message_is_sanitized() {
        // sanitize_api_error scrubs secret patterns (API keys, tokens) and
        // truncates overly long messages to MAX_API_ERROR_CHARS (500). This
        // test verifies the error message is present, correctly typed, and
        // truncated when it exceeds the limit.
        let long_err = "E".repeat(600);
        let backend = MockBackendBuilder::new()
            .set_alias_result(Err(std::io::Error::other(long_err.clone())))
            .build();
        let result = try_persist_session_ownership(&backend, "gw_test", "default");
        assert!(result.is_err(), "write error should be an Err");
        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.error_type, "server_error");
        // The message must contain the "Failed to persist session ownership"
        // prefix and be truncated (≤ 500 chars + "...").
        assert!(
            err.message
                .starts_with("Failed to persist session ownership:"),
            "message must include the descriptive prefix; got: {}",
            err.message
        );
        assert!(
            err.message.len() <= 503,
            "message should be truncated by sanitize_api_error (max 500 chars + '...'); \
             len={}",
            err.message.len()
        );
    }

    #[test]
    fn persist_session_ownership_permission_denied_is_500() {
        let backend = MockBackendBuilder::new()
            .set_alias_result(Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "permission denied",
            )))
            .build();
        let result = try_persist_session_ownership(&backend, "gw_test", "default");
        assert!(result.is_err(), "PermissionDenied should fail-closed");
        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // ── READ tests (try_check_session_ownership_on_resume) ──────────────

    #[test]
    fn ownership_resume_no_session_header_is_ok() {
        let backend = MockBackendBuilder::new().build();
        let result = try_check_session_ownership_on_resume(
            &backend, "gw_test", "default", None, // no session header => new session
        );
        assert!(result.is_ok(), "no session header should skip the check");
    }

    #[test]
    fn ownership_resume_matching_alias_is_ok() {
        let backend = MockBackendBuilder::new()
            .get_alias_result(Ok(Some("default".to_string())))
            .build();
        let result = try_check_session_ownership_on_resume(
            &backend,
            "gw_test",
            "default",
            Some("existing-session-id"),
        );
        assert!(result.is_ok(), "matching alias should pass");
    }

    #[test]
    fn ownership_resume_no_stored_alias_is_ok() {
        let backend = MockBackendBuilder::new().get_alias_result(Ok(None)).build();
        let result = try_check_session_ownership_on_resume(
            &backend,
            "gw_test",
            "default",
            Some("existing-session-id"),
        );
        assert!(
            result.is_ok(),
            "no stored alias (empty session) should pass"
        );
    }

    #[test]
    fn ownership_resume_no_stored_alias_non_empty_session_returns_400() {
        let backend = MockBackendBuilder::new()
            .get_alias_result(Ok(None))
            .load_data(vec![ChatMessage::user("hi")])
            .build();
        let result = try_check_session_ownership_on_resume(
            &backend,
            "gw_test",
            "default",
            Some("existing-session-id"),
        );
        assert!(
            result.is_err(),
            "Ok(None) with non-empty session should be rejected (pre-migration session)"
        );
    }

    #[test]
    fn ownership_resume_mismatched_alias_returns_400() {
        let backend = MockBackendBuilder::new()
            .get_alias_result(Ok(Some("coding".to_string())))
            .build();
        let result = try_check_session_ownership_on_resume(
            &backend,
            "gw_test",
            "default",
            Some("existing-session-id"),
        );
        assert!(result.is_err(), "mismatched alias should return Err");
        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert_eq!(err.error_type, "invalid_request_error");
        assert!(
            err.message.contains("coding"),
            "error must mention the stored alias; got: {}",
            err.message
        );
        assert!(
            err.message.contains("default"),
            "error must mention the requested alias; got: {}",
            err.message
        );
    }

    #[test]
    fn ownership_resume_unsupported_empty_session_is_ok() {
        let backend = MockBackendBuilder::new()
            .get_alias_result(Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not supported",
            )))
            .load_data(vec![])
            .build();
        let result = try_check_session_ownership_on_resume(
            &backend,
            "gw_test",
            "default",
            Some("existing-session-id"),
        );
        assert!(
            result.is_ok(),
            "Unsupported + empty session should pass (graceful degradation)"
        );
    }

    #[test]
    fn ownership_resume_unsupported_non_empty_session_returns_400() {
        // When backend doesn't support ownership tracking AND the
        // session already has data, return 400 to prevent cross-agent
        // context contamination.
        let backend = MockBackendBuilder::new()
            .get_alias_result(Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not supported",
            )))
            .load_data(vec![user_msg("existing message")])
            .build();
        let result = try_check_session_ownership_on_resume(
            &backend,
            "gw_test",
            "default",
            Some("existing-session-id"),
        );
        assert!(
            result.is_err(),
            "Unsupported + non-empty session should return Err"
        );
        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert_eq!(err.error_type, "invalid_request_error");
        assert!(
            err.message.contains("does not track agent ownership"),
            "error must mention ownership tracking; got: {}",
            err.message
        );
    }

    #[test]
    fn ownership_resume_io_error_returns_500() {
        let backend = MockBackendBuilder::new()
            .get_alias_result(Err(std::io::Error::other("I/O failure")))
            .build();
        let result = try_check_session_ownership_on_resume(
            &backend,
            "gw_test",
            "default",
            Some("existing-session-id"),
        );
        assert!(result.is_err(), "I/O error should return Err");
        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.error_type, "internal_error");
    }

    // ── ErrorDetail serialization tests (code/param as Option<String>) ────────

    #[test]
    fn error_detail_serializes_code_and_param_when_set() {
        let detail = ErrorDetail {
            message: "Rate limit exceeded".into(),
            error_type: "rate_limit_error".into(),
            code: Some("rate_limit_exceeded".into()),
            param: None,
            status: 429,
        };
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json["code"], "rate_limit_exceeded");
        assert_eq!(json["param"], serde_json::Value::Null);
    }

    #[test]
    fn error_detail_serializes_code_null_and_param_set() {
        let detail = ErrorDetail {
            message: "max_tokens is not supported".into(),
            error_type: "unsupported_parameter".into(),
            code: None,
            param: Some("max_tokens".into()),
            status: 400,
        };
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json["code"], serde_json::Value::Null);
        assert_eq!(json["param"], "max_tokens");
    }

    #[test]
    fn error_detail_serializes_both_null() {
        let detail = ErrorDetail {
            message: "Internal error".into(),
            error_type: "internal_error".into(),
            code: None,
            param: None,
            status: 500,
        };
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json["code"], serde_json::Value::Null);
        assert_eq!(json["param"], serde_json::Value::Null);
    }

    #[test]
    fn error_detail_serializes_both_set() {
        let detail = ErrorDetail {
            message: "Invalid model".into(),
            error_type: "invalid_request_error".into(),
            code: Some("invalid_request_error".into()),
            param: Some("model".into()),
            status: 400,
        };
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json["code"], "invalid_request_error");
        assert_eq!(json["param"], "model");
    }

    #[test]
    fn error_response_includes_code_and_param() {
        let resp = error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "top_p is not supported per-request",
            None,
            Some("top_p"),
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let body_bytes = rt
            .block_on(axum::body::to_bytes(resp.into_body(), usize::MAX))
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let error = &body["error"];
        assert_eq!(error["message"], "top_p is not supported per-request");
        assert_eq!(error["type"], "unsupported_parameter");
        assert_eq!(error["code"], serde_json::Value::Null);
        assert_eq!(error["param"], "top_p");
        assert_eq!(error["status"], 400);
    }

    #[test]
    fn error_response_includes_code_for_rate_limit() {
        let resp = error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "Rate limit exceeded",
            Some("rate_limit_exceeded"),
            None,
        );
        let rt = tokio::runtime::Runtime::new().unwrap();
        let body_bytes = rt
            .block_on(axum::body::to_bytes(resp.into_body(), usize::MAX))
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let error = &body["error"];
        assert_eq!(error["code"], "rate_limit_exceeded");
        assert_eq!(error["param"], serde_json::Value::Null);
    }
}
