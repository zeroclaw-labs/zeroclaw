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
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_providers::sanitize_api_error;
use zeroclaw_runtime::agent::TurnEvent;

use crate::AppState;

// Rate limit window (matches lib.rs)
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

// Request structures

#[derive(Debug, Deserialize, Serialize)]
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
    pub content: String,
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
    error: ErrorDetail,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct ErrorDetail {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
    code: Option<()>,
    param: Option<()>,
    status: u16,
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
/// the WebSocket `resolve_ws_memory_handle` (ws.rs:246-265). Returns `None`
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
            content: m.content.clone(),
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
            content: msg.content.clone(),
        });
    }

    (history, msgs[active_idx].content.clone())
}

fn request_system_prompt_prefix(msgs: &[ChatCompletionMessage]) -> Option<String> {
    let parts: Vec<&str> = msgs
        .iter()
        .filter(|m| matches!(m.role.as_str(), "system" | "developer"))
        .map(|m| m.content.as_str())
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

fn error_response(status: StatusCode, error_type: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: ErrorDetail {
                message: message.to_string(),
                error_type: error_type.to_string(),
                code: None,
                param: None,
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
        "finish_reason": finish_reason
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
                    error_response(StatusCode::BAD_REQUEST, "invalid_request_error", &msg),
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
            add_request_id_header(error_response(status, err_type, msg), &request_id),
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
                    error_response(StatusCode::BAD_REQUEST, "invalid_request_error", &e),
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

    if let Some(ref backend) = state.session_backend {
        // Session agent_alias consistency check — must run BEFORE loading
        // history to prevent cross-agent context contamination.
        if session_id_from_header.is_some() {
            match backend.get_session_agent_alias(&session_key) {
                Ok(Some(stored_alias)) if stored_alias != agent_alias => {
                    return add_session_key_header(
                        add_request_id_header(
                            error_response(
                                StatusCode::BAD_REQUEST,
                                "invalid_request_error",
                                &format!(
                                    "Session belongs to agent '{stored_alias}', not '{agent_alias}'"
                                ),
                            ),
                            &request_id,
                        ),
                        &session_id,
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                    if !backend.load(&session_key).is_empty() {
                        return add_session_key_header(
                            add_request_id_header(
                                error_response(
                                    StatusCode::BAD_REQUEST,
                                    "invalid_request_error",
                                    "Cannot resume session: backend does not track agent ownership",
                                ),
                                &request_id,
                            ),
                            &session_id,
                        );
                    }
                }
                Err(_) => {
                    return add_session_key_header(
                        add_request_id_header(
                            error_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "internal_error",
                                "Failed to read session metadata",
                            ),
                            &request_id,
                        ),
                        &session_id,
                    );
                }
                _ => {}
            }
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
        match backend.set_session_agent_alias(&session_key, &agent_alias) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                // Backend doesn't support ownership — already warned above if session had data.
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_key": &session_key,
                            "agent_alias": &agent_alias,
                            "error": format!("{e}"),
                        })),
                    "Failed to persist session ownership metadata"
                );
            }
        }
    }

    // Resolve a per-request memory handle for consolidation, mirroring the
    // WS per-connection `resolve_ws_memory_handle` path (ws.rs:246-265).
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

    if request.stream {
        let include_usage = request
            .stream_options
            .as_ref()
            .map(|o| o.include_usage)
            .unwrap_or(false);
        stream_mode(
            agent,
            user_message,
            response_model,
            include_usage,
            request_id,
            chat_rate_limit,
            rate_limit_remaining,
            reset_ts,
            session_key,
            session_id,
            state,
            ws_memory,
            session_guard,
        )
        .await
    } else {
        blocking_mode(
            agent,
            user_message,
            response_model,
            request_id,
            chat_rate_limit,
            rate_limit_remaining,
            reset_ts,
            session_key,
            session_id,
            state,
            ws_memory,
        )
        .await
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
    let _runner_task = zeroclaw_spawn::spawn!(async move {
        // Hold the session queue guard for the full turn duration.
        // The guard serialises concurrent same-session requests across
        // the complete lifecycle: turn execution → persistence → state.
        let _session_guard = session_guard;
        let mut agent = agent;
        let outcome = crate::turn_runner::run_gateway_turn(
            &state,
            &mut agent,
            &user_message_for_runner,
            &session_key_for_runner,
            &ws_memory,
            None,
            "http",
            forward,
        )
        .await;

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
                    .json_data(&error_body)
                    .unwrap_or_else(|_| Event::default().data("[Error]"));
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
    mut agent: zeroclaw_runtime::agent::Agent,
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

    let outcome = crate::turn_runner::run_gateway_turn(
        &state,
        &mut agent,
        &user_message,
        &session_key,
        &ws_memory,
        None,
        "http",
        forward,
    )
    .await;

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
        ));
    }

    if let Some(ref tools) = req.tools {
        for tool in tools {
            if tool.kind != "function" {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "Only 'function' tool type is supported",
                ));
            }
            if tool.function.name.trim().is_empty() {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "tool.function.name is required",
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
                ));
            }
        } else if tc.is_object() {
            // Specific-function tool_choice is not yet wired through to
            // providers; reject early rather than silently degrading.
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "unsupported_parameter",
                "tool_choice with a specific function is not yet supported; use \"auto\" instead",
            ));
        } else {
            // Malformed: number, array, bool, null
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "tool_choice must be a string (\"auto\", \"none\", \"required\") or a function object",
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
        ));
    }
    if req.top_p.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "top_p is not supported per-request",
        ));
    }
    if req.stop.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "stop is not supported per-request",
        ));
    }
    if req.presence_penalty.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "presence_penalty is not supported per-request",
        ));
    }
    if req.frequency_penalty.is_some() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "frequency_penalty is not supported per-request",
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
            content: content.to_string(),
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
                content: "hi".into(),
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
                content: "hi".into(),
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
                content: "hi".into(),
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
}
