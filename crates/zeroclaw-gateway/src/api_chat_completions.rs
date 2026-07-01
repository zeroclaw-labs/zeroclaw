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
use zeroclaw_providers::sanitize_api_error;
use zeroclaw_runtime::agent::TurnEvent;

use crate::AppState;

// Rate limit window (matches lib.rs)
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

// Request structures

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionMessage {
    pub role: String,
    pub content: String,
    pub name: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

// Response structures

#[derive(Debug, Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<NonStreamChoice>,
    usage: CompletionUsage,
}

#[derive(Debug, Serialize)]
struct NonStreamChoice {
    index: u32,
    message: AssistantMessage,
    finish_reason: String,
}

#[derive(Debug, Serialize)]
struct AssistantMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Debug, Serialize)]
struct ResponseToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: ResponseFunctionCall,
}

#[derive(Debug, Serialize)]
struct ResponseFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct CompletionUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Debug, Serialize)]
struct ErrorDetail {
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

fn resolve_backend_model_override(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-zeroclaw-model")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
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

    let data = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [choice]
    });

    Event::default().data(data.to_string())
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

    let Json(request) = match request {
        Ok(req) => req,
        Err(e) => {
            let msg = format!("Invalid request: {}", e.body_text());
            return add_request_id_header(
                error_response(StatusCode::BAD_REQUEST, "invalid_request_error", &msg),
                &request_id,
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
        return add_request_id_header(error_response(status, err_type, msg), &request_id);
    }

    if let Err(e) = validate_request(&request) {
        return add_request_id_header(e, &request_id);
    }
    if let Err(e) = validate_unsupported_params(&request) {
        return add_request_id_header(e, &request_id);
    }

    let session_key_from_header = extract_session_key(&headers);
    let session_id = session_key_from_header
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_key = format!(
        "gw_{}",
        zeroclaw_api::session_keys::sanitize_session_key(&session_id)
    );

    let config = state.config.read().clone();

    let agent_alias = match agent_alias_from_model(&request.model, &config) {
        Ok(alias) => alias,
        Err(e) => {
            return add_request_id_header(
                error_response(StatusCode::BAD_REQUEST, "invalid_request_error", &e),
                &request_id,
            );
        }
    };

    if config.agent(&agent_alias).is_none() {
        return add_request_id_header(
            error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!(
                    "Unknown agent `{agent_alias}` — no [agents.{agent_alias}] entry configured."
                ),
            ),
            &request_id,
        );
    }

    if config
        .resolved_model_provider_for_agent(&agent_alias)
        .and_then(|(_, _, cfg)| cfg.model.as_deref().filter(|m| !m.trim().is_empty()))
        .is_none()
    {
        return add_request_id_header(
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_error",
                "Agent not configured — complete onboarding at /onboard",
            ),
            &request_id,
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
                return add_request_id_header(
                    error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        &sanitized,
                    ),
                    &request_id,
                );
            }
        };

    let (request_history, current_turn) = split_messages(&request.messages);
    let user_message = match request_system_prompt_prefix(&request.messages) {
        Some(prefix) => format!("{prefix}\n\n{current_turn}"),
        None => current_turn.clone(),
    };
    let persisted_user_message = current_turn;
    let request_has_authoritative_history = request_has_authoritative_history(&request);

    agent.set_memory_session_id(Some(session_id.clone()));
    if let Some(ref backend) = state.session_backend {
        if session_key_from_header.is_some() && !request_has_authoritative_history {
            let messages = backend.load(&session_key);
            if !messages.is_empty() {
                agent.seed_history(&messages);
            }
        }

        // Session agent_alias consistency check
        if session_key_from_header.is_some() {
            if let Ok(Some(stored_alias)) = backend.get_session_agent_alias(&session_key) {
                if stored_alias != agent_alias {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "session_key": session_key,
                                "stored_alias": stored_alias,
                                "requested_alias": agent_alias,
                                "error": format!(
                                    "Session was created for agent '{}' but now accessed via '{}'",
                                    stored_alias, agent_alias
                                ),
                            })),
                        "Session agent_alias mismatch"
                    );
                }
            }
        }
    }

    if !request_history.is_empty() {
        agent.seed_history(&request_history);
    }

    agent.set_temperature(Some(request.temperature));
    if let Some(override_model) = resolve_backend_model_override(&headers) {
        agent.set_model_name(override_model);
    }

    let tool_choice_mode = parse_tool_choice(&request.tool_choice);
    let configured_tools: std::collections::HashSet<String> =
        agent.get_configured_tool_names().into_iter().collect();
    match tool_choice_mode {
        ToolChoiceMode::None => {
            agent.disable_tools();
        }
        _ => {
            match resolve_tool_specs(&request.tool_choice, &request.tools, &configured_tools) {
                Err(e) => return add_request_id_header(e, &request_id),
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
        let user_msg = ChatMessage::user(&persisted_user_message);
        let _ = backend.append(&session_key, &user_msg);
        let _ = backend.set_session_agent_alias(&session_key, &agent_alias);
    }

    let cost_tracking_context = state.cost_tracker.as_ref().map(|tracker| {
        let pricing: std::collections::HashMap<String, std::collections::HashMap<String, f64>> =
            config
                .providers
                .models
                .iter_entries()
                .filter(|(_, _, base)| !base.pricing.is_empty())
                .map(|(type_k, alias_k, base)| {
                    (format!("{type_k}.{alias_k}"), base.pricing.clone())
                })
                .collect();
        zeroclaw_runtime::agent::cost::ToolLoopCostTrackingContext::new(
            tracker.clone(),
            Arc::new(pricing),
        )
        .with_agent_alias(&agent_alias)
    });
    let captured_usage = cost_tracking_context
        .as_ref()
        .map(|ctx| ctx.turn_usage.clone());

    let (turn_alias, turn_provider, turn_model) = agent.attribution_fields();
    let _ = state.event_tx.send(serde_json::json!({
        "type": "agent_start",
        "model_provider": turn_provider.clone(),
        "model": turn_model.clone(),
    }));

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
            cost_tracking_context,
            captured_usage,
            turn_alias,
            turn_provider,
        )
        .await
    } else {
        blocking_mode(
            agent,
            user_message,
            &response_model,
            request_id,
            chat_rate_limit,
            rate_limit_remaining,
            reset_ts,
            session_key,
            session_id,
            state,
            cost_tracking_context,
            captured_usage,
            turn_alias,
            turn_provider,
        )
        .await
    }
}

// Stream mode

#[allow(clippy::too_many_arguments)]
async fn stream_mode(
    mut agent: zeroclaw_runtime::agent::Agent,
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
    cost_tracking_context: Option<zeroclaw_runtime::agent::cost::ToolLoopCostTrackingContext>,
    captured_usage: Option<Arc<parking_lot::Mutex<zeroclaw_runtime::agent::cost::TurnUsage>>>,
    turn_alias: String,
    turn_provider: String,
) -> Response {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let chunk_id = generate_completion_id();
    let created = Utc::now().timestamp() as u64;

    let user_message_for_turn = user_message.clone();
    let session_key_for_turn = session_key.clone();
    let provider_for_bcast = turn_provider.clone();
    let model_for_bcast = model.clone();
    let turn_handle = zeroclaw_spawn::spawn!(async move {
        use ::zeroclaw_log::Instrument as _;
        let span = ::zeroclaw_log::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            session_key = %session_key_for_turn,
            agent_alias = %turn_alias,
            model_provider = %turn_provider,
            model = %model,
            channel = "http",
        );
        let turn_result = if let Some(ctx) = cost_tracking_context {
            zeroclaw_runtime::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(
                    Some(ctx),
                    agent.turn_streamed(&user_message_for_turn, event_tx, None),
                )
                .await
        } else {
            agent
                .turn_streamed(&user_message_for_turn, event_tx, None)
                .await
        };
        zeroclaw_runtime::agent::loop_::scope_session_key(Some(session_key_for_turn), async move {
            turn_result
        })
        .instrument(span)
        .await
    });

    let session_key_for_stream = session_key.clone();
    let state_for_persist = state.clone();
    let event_tx_for_bcast = state.event_tx.clone();
    let sse_stream = async_stream::stream! {
        yield Ok::<_, Infallible>(make_chunk(
            &chunk_id, created, &model_for_bcast,
            Some("assistant"), Some(String::new()), None, None,
        ));

        let mut full_response = String::new();
        let mut last_partial_save = std::time::Instant::now();
        let partial_save_interval = Duration::from_millis(500);
        let mut partial_saved = false;

        while let Some(event) = event_rx.recv().await {
            match event {
                TurnEvent::Chunk { delta } => {
                    full_response.push_str(&delta);
                    yield Ok::<_, Infallible>(make_chunk(
                        &chunk_id, created, &model_for_bcast,
                        None, Some(delta), None, None,
                    ));

                    if last_partial_save.elapsed() >= partial_save_interval {
                        if let Some(ref backend) = state_for_persist.session_backend {
                            let partial = ChatMessage::assistant(&full_response);
                            if partial_saved {
                                let _ = backend.update_last(&session_key_for_stream, &partial);
                            } else {
                                let _ = backend.append(&session_key_for_stream, &partial);
                                partial_saved = true;
                            }
                        }
                        last_partial_save = std::time::Instant::now();
                    }
                }
                TurnEvent::Thinking { delta } => {
                    yield Ok::<_, Infallible>(make_chunk(
                        &chunk_id, created, &model_for_bcast,
                        None, Some(delta), None, None,
                    ));
                }
                TurnEvent::ToolCall { id: _, name, args } => {
                    let tool_delta = serde_json::json!({
                        "index": 0,
                        "id": format!("call_{}", uuid::Uuid::new_v4()),
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": args.to_string()
                        }
                    });
                    yield Ok::<_, Infallible>(make_chunk(
                        &chunk_id, created, &model_for_bcast,
                        None, None, Some(vec![tool_delta]), None,
                    ));
                }
                TurnEvent::ToolResult { .. } => { /* transparent */ }
                TurnEvent::ApprovalRequest { .. } => { /* non-interactive: auto-handled */ }
                TurnEvent::Usage { .. } => { /* captured via cost tracking */ }
                TurnEvent::HistoryTrimmed { .. } => { /* transparent */ }
            }
        }

        match turn_handle.await {
            Ok(Ok(_)) => {}
            Ok(Err(turn_err)) => {
                if full_response.is_empty() {
                    let sanitized = sanitize_api_error(&turn_err.to_string());
                    yield Ok::<_, Infallible>(make_chunk(
                        &chunk_id, created, &model_for_bcast,
                        None, Some(format!("[Error: {sanitized}]")), None, None,
                    ));
                }
            }
            Err(_) => {
                if full_response.is_empty() {
                    yield Ok::<_, Infallible>(make_chunk(
                        &chunk_id, created, &model_for_bcast,
                        None, Some("[Error: agent task terminated unexpectedly]".into()), None, None,
                    ));
                }
            }
        }

        let _ = event_tx_for_bcast.send(serde_json::json!({
            "type": "agent_end",
            "model_provider": provider_for_bcast,
            "model": model_for_bcast,
        }));

        yield Ok::<_, Infallible>(make_chunk(
            &chunk_id, created, &model_for_bcast, None, None, None, Some("stop"),
        ));

        if include_usage {
            let (input, output) = captured_usage
                .as_ref()
                .map(|cell| {
                    let u = cell.lock();
                    (u.input_tokens, u.output_tokens)
                })
                .unwrap_or((0, 0));
            yield Ok::<_, Infallible>(make_usage_chunk(
                &chunk_id, created, &model_for_bcast,
                input, output, input + output,
            ));
        }

        yield Ok::<_, Infallible>(Event::default().data("[DONE]"));

        if let Some(ref backend) = state_for_persist.session_backend {
            let assistant_msg = ChatMessage::assistant(&full_response);
            if partial_saved {
                let _ = backend.update_last(&session_key_for_stream, &assistant_msg);
            } else {
                let _ = backend.append(&session_key_for_stream, &assistant_msg);
            }
        }
    };

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
    model: &str,
    request_id: String,
    rate_limit: u32,
    rate_limit_remaining: u32,
    rate_limit_reset: u64,
    session_key: String,
    session_id: String,
    state: AppState,
    cost_tracking_context: Option<zeroclaw_runtime::agent::cost::ToolLoopCostTrackingContext>,
    captured_usage: Option<Arc<parking_lot::Mutex<zeroclaw_runtime::agent::cost::TurnUsage>>>,
    turn_alias: String,
    turn_provider: String,
) -> Response {
    let session_key_for_turn = session_key.clone();
    let model_owned = model.to_string();
    let provider_for_bcast = turn_provider.clone();
    let model_for_bcast = model_owned.clone();
    let result = {
        use ::zeroclaw_log::Instrument as _;
        let span = ::zeroclaw_log::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            session_key = %session_key_for_turn,
            agent_alias = %turn_alias,
            model_provider = %turn_provider,
            model = %model_owned,
            channel = "http",
        );
        let turn_result = if let Some(ctx) = cost_tracking_context {
            zeroclaw_runtime::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(Some(ctx), agent.turn(&user_message))
                .await
        } else {
            agent.turn(&user_message).await
        };
        zeroclaw_runtime::agent::loop_::scope_session_key(Some(session_key_for_turn), async move {
            turn_result
        })
        .instrument(span)
        .await
    };

    let _ = state.event_tx.send(serde_json::json!({
        "type": "agent_end",
        "model_provider": provider_for_bcast,
        "model": model_for_bcast,
    }));

    match result {
        Ok(response_text) => {
            let response_tool_calls: Option<Vec<ResponseToolCall>> =
                agent.history().iter().rev().find_map(|msg| match msg {
                    zeroclaw_api::model_provider::ConversationMessage::AssistantToolCalls {
                        tool_calls,
                        ..
                    } if !tool_calls.is_empty() => Some(
                        tool_calls
                            .iter()
                            .map(|tc| ResponseToolCall {
                                id: tc.id.clone(),
                                kind: "function".to_string(),
                                function: ResponseFunctionCall {
                                    name: tc.name.clone(),
                                    arguments: tc.arguments.clone(),
                                },
                            })
                            .collect(),
                    ),
                    _ => None,
                });

            if let Some(ref backend) = state.session_backend {
                let assistant_msg = ChatMessage::assistant(&response_text);
                let _ = backend.append(&session_key, &assistant_msg);
            }

            let (input_tokens, output_tokens, total_tokens) = captured_usage
                .as_ref()
                .map(|cell| {
                    let u = cell.lock();
                    (
                        u.input_tokens,
                        u.output_tokens,
                        u.input_tokens + u.output_tokens,
                    )
                })
                .filter(|(i, o, _)| *i > 0 || *o > 0)
                .unwrap_or((0, 0, 0));

            let has_tool_calls = response_tool_calls.is_some();
            let body = ChatCompletionResponse {
                id: generate_completion_id(),
                object: "chat.completion",
                created: Utc::now().timestamp() as u64,
                model: model.to_string(),
                choices: vec![NonStreamChoice {
                    index: 0,
                    message: AssistantMessage {
                        role: "assistant",
                        content: if has_tool_calls {
                            None
                        } else {
                            Some(response_text)
                        },
                        tool_calls: response_tool_calls,
                    },
                    finish_reason: if has_tool_calls {
                        "tool_calls".to_string()
                    } else {
                        "stop".to_string()
                    },
                }],
                usage: CompletionUsage {
                    prompt_tokens: input_tokens,
                    completion_tokens: output_tokens,
                    total_tokens,
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
        Err(e) => {
            let sanitized = sanitize_api_error(&e.to_string());
            let resp = add_request_id_header(
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    &sanitized,
                ),
                &request_id,
            );
            add_rate_limit_headers(resp, rate_limit, rate_limit_remaining, rate_limit_reset)
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
                    let filtered_tools: Vec<_> = requested_tools
                        .iter()
                        .filter(|t| configured_tools.contains(&t.function.name))
                        .cloned()
                        .collect();
                    if filtered_tools.is_empty() {
                        return Err(error_response(
                            StatusCode::BAD_REQUEST,
                            "invalid_request_error",
                            "None of the requested tools are configured for this agent",
                        ));
                    }
                    Ok(Some(convert_request_tools(&filtered_tools)))
                }
            }
        }
        ToolChoiceMode::Required => match tools {
            None => Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "tool_choice: \"required\" requires a non-empty tools list",
            )),
            Some(requested_tools) => {
                let filtered_tools: Vec<_> = requested_tools
                    .iter()
                    .filter(|t| configured_tools.contains(&t.function.name))
                    .cloned()
                    .collect();
                if filtered_tools.is_empty() {
                    return Err(error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        "None of the requested tools are configured for this agent",
                    ));
                }
                Ok(Some(convert_request_tools(&filtered_tools)))
            }
        },
        ToolChoiceMode::SpecificFunction { ref name } => {
            if !configured_tools.contains(name) {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    &format!(
                        "tool_choice function '{}' is not configured for this agent",
                        name
                    ),
                ));
            }
            match tools {
                None => Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "tool_choice with a specific function requires a tools list",
                )),
                Some(requested_tools) => {
                    if !requested_tools.iter().any(|t| t.function.name == *name) {
                        return Err(error_response(
                            StatusCode::BAD_REQUEST,
                            "invalid_request_error",
                            &format!(
                                "tool_choice function '{}' not found in the provided tools list",
                                name
                            ),
                        ));
                    }
                    if let Some(tool) = requested_tools.iter().find(|t| t.function.name == *name) {
                        let tool_specs = vec![zeroclaw_runtime::tools::ToolSpec {
                            name: tool.function.name.clone(),
                            description: tool.function.description.clone().unwrap_or_default(),
                            parameters: tool.function.parameters.clone(),
                        }];
                        Ok(Some(tool_specs))
                    } else {
                        // Should not reach here due to the .any() check above
                        Ok(None)
                    }
                }
            }
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
            parameters: tool.function.parameters.clone(),
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
    fn test_resolve_backend_model_override() {
        let mut headers = HeaderMap::new();
        assert_eq!(resolve_backend_model_override(&headers), None);

        headers.insert("x-zeroclaw-model", HeaderValue::from_static("   "));
        assert_eq!(resolve_backend_model_override(&headers), None);

        headers.insert("x-zeroclaw-model", HeaderValue::from_static("qwen-plus"));
        assert_eq!(
            resolve_backend_model_override(&headers).as_deref(),
            Some("qwen-plus")
        );
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
    fn test_none_tool_choice_with_mixed_tools_filters() {
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
            result.is_ok(),
            "None tool_choice with mixed tools should succeed"
        );
        let specs = result.unwrap();
        assert!(specs.is_some());
        assert_eq!(specs.unwrap().len(), 1, "only known tool should pass");
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
    fn test_required_with_known_tools_succeeds() {
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
        assert!(result.is_ok(), "required with known tools should succeed");
        let specs = result.unwrap();
        assert!(specs.is_some());
        assert_eq!(specs.unwrap().len(), 1);
    }

    // ── tool_choice={function} + known function succeeds ────────────────

    #[test]
    fn test_specific_function_with_known_tool_succeeds() {
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
            result.is_ok(),
            "specific function with known tool should succeed"
        );
        let specs = result.unwrap();
        assert!(specs.is_some());
        assert_eq!(specs.unwrap().len(), 1);
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
}
