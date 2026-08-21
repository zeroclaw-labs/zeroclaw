//! OpenAI-compatible `POST /v1/chat/completions` adapter.
//!
//! This module owns the chat-completions wire contract: request/response
//! types, the OpenAI-compatible error envelope, request validation, agent
//! routing, handler orchestration, SSE/JSON dispatch, and the tool whitelist.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::limit::RequestBodyLimitLayer;
use zeroclaw_api::session_keys::sanitize_session_key;
use zeroclaw_api::tool::ToolSpec;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::session_backend::ClaimOutcome;
use zeroclaw_infra::session_queue::{SessionGuard, SessionQueueError};
use zeroclaw_providers::ChatMessage;
use zeroclaw_runtime::agent::{Agent, TurnEvent};

use crate::turn_runner::{
    TurnForwardResult, TurnOutcome, TurnRunnerHandle, TurnStatus, run_gateway_turn,
};
use crate::{
    AppState, RateLimitDecision, gateway_long_running_request_timeout_secs, ws_session_active,
};

// ── Request wire types ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChatCompletionRequest {
    #[serde(default)]
    pub model: String, // missing -> "" (default-agent shorthand)
    pub messages: Vec<ChatCompletionMessage>, // required
    #[serde(default)]
    pub stream: bool,    // missing -> false
    pub temperature: Option<f64>,
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
    // Behavior-changing fields: modeled so `validate_unsupported_params` can
    // reject them explicitly rather than silently dropping them.
    pub parallel_tool_calls: Option<bool>,
    pub service_tier: Option<String>,
    pub functions: Option<serde_json::Value>,
    pub function_call: Option<serde_json::Value>,
    pub reasoning_effort: Option<String>,
    pub modalities: Option<serde_json::Value>,
    pub audio: Option<serde_json::Value>,
    pub prediction: Option<serde_json::Value>,
    pub web_search_options: Option<serde_json::Value>,
    // Benign annotation fields: modeled + tolerated, never rejected.
    pub metadata: Option<serde_json::Value>,
    pub store: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChatCompletionMessage {
    pub role: String,
    pub content: Option<String>,
    pub name: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChatCompletionTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ToolFunction {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: serde_json::Value, // missing -> Null; OpenAI allows omitting parameters
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

// ── Response wire types ─────────────────────────────────────────────────────

// Constructed by `blocking_mode`. `tool_calls` is always `None` under
// transparent execution, so the `ResponseToolCall`/`ResponseFunctionCall`
// shapes below are only referenced by `#[serde(skip_serializing_if)]` and stay
// `#[allow(dead_code)]`.

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct ChatCompletionResponse {
    id: String,           // "chatcmpl-{uuid}"
    object: &'static str, // "chat.completion"
    created: u64,         // Unix seconds
    model: String,        // echoes the request model
    choices: Vec<NonStreamChoice>,
    usage: CompletionUsage,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct NonStreamChoice {
    index: u32,
    message: AssistantMessage,
    finish_reason: String,    // "stop"
    pub logprobs: Option<()>, // always null; placeholder to keep the field
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct AssistantMessage {
    role: &'static str, // "assistant"
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub(crate) struct ResponseToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: ResponseFunctionCall,
}

#[allow(dead_code)]
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

// ── Error envelope ──────────────────────────────────────────────────────────

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
    pub(crate) param: Option<String>, // the rejected field name; message-level rejections use "messages"
    pub(crate) status: u16, // HTTP status redundantly carried in the body (OpenAI-compatible)
}

pub(crate) fn error_response(
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

/// Lightweight error value threaded through the request pipeline and turned
/// into an axum `Response` only at the HTTP boundary. Unlike `Response` it is a
/// small struct, so internal `Result<T, ApiError>` returns do not trip
/// `clippy::result_large_err`. `IntoResponse` reuses `error_response`, so the
/// wire envelope is byte-for-byte identical to returning the error directly.
#[derive(Debug, Clone)]
pub(crate) struct ApiError {
    status: StatusCode,
    error_type: &'static str,
    message: String,
    code: Option<&'static str>,
    param: Option<&'static str>,
}

impl ApiError {
    pub(crate) fn new(
        status: StatusCode,
        error_type: &'static str,
        message: &str,
        code: Option<&'static str>,
        param: Option<&'static str>,
    ) -> Self {
        Self {
            status,
            error_type,
            message: message.to_string(),
            code,
            param,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error_response(
            self.status,
            self.error_type,
            &self.message,
            self.code,
            self.param,
        )
    }
}

// ── Request validation ──────────────────────────────────────────────────────

/// Reject the 23 request-level fields ZeroClaw does not support, each with a
/// precise `param` + `message`. 14 explicit `if`s for generation-settings
/// fields + 9 array-loop over behavior-control fields.
///
/// A field is rejected only when it is present (`Some`), whatever its value —
/// "explicit 400 instead of silent ignore". `metadata`/`store` are benign
/// annotations and intentionally skipped here.
fn validate_unsupported_params(req: &ChatCompletionRequest) -> Result<(), ApiError> {
    // 5.1 — generation-settings fields, each with a distinct message.
    if req.max_tokens.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "max_tokens is not supported per-request; configure in provider settings",
            None,
            Some("max_tokens"),
        ));
    }
    if req.top_p.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "top_p is not supported per-request",
            None,
            Some("top_p"),
        ));
    }
    if req.stop.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "stop is not supported per-request",
            None,
            Some("stop"),
        ));
    }
    if req.presence_penalty.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "presence_penalty is not supported per-request",
            None,
            Some("presence_penalty"),
        ));
    }
    if req.frequency_penalty.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "frequency_penalty is not supported per-request",
            None,
            Some("frequency_penalty"),
        ));
    }
    if req.n.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "n is not supported; single completion per request",
            None,
            Some("n"),
        ));
    }
    if req.response_format.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "response_format is not supported; configure output format in provider settings",
            None,
            Some("response_format"),
        ));
    }
    if req.seed.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "seed is not supported; configure in provider settings",
            None,
            Some("seed"),
        ));
    }
    if req.logprobs.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "logprobs is not supported",
            None,
            Some("logprobs"),
        ));
    }
    if req.top_logprobs.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "top_logprobs is not supported",
            None,
            Some("top_logprobs"),
        ));
    }
    if req.user.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "user is not supported",
            None,
            Some("user"),
        ));
    }
    if req.logit_bias.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "logit_bias is not supported",
            None,
            Some("logit_bias"),
        ));
    }
    if req.max_completion_tokens.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "max_completion_tokens is not supported; use provider settings",
            None,
            Some("max_completion_tokens"),
        ));
    }
    // Omission keeps the routed agent's configured temperature; explicit
    // per-request temperature is rejected.
    if req.temperature.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "temperature is not supported per-request; set `temperature` on the routed agent's provider model",
            None,
            Some("temperature"),
        ));
    }

    // 5.2 — behavior-control fields, short messages, array-able.
    for (present, param, message) in [
        (
            req.parallel_tool_calls.is_some(),
            "parallel_tool_calls",
            "parallel_tool_calls is not supported; tools executed transparently",
        ),
        (
            req.service_tier.is_some(),
            "service_tier",
            "service_tier is not applicable; routing is ZeroClaw config",
        ),
        (
            req.functions.is_some(),
            "functions",
            "legacy function-calling is not supported; use `tools`",
        ),
        (
            req.function_call.is_some(),
            "function_call",
            "legacy function_call is not supported; use `tool_choice`",
        ),
        (
            req.reasoning_effort.is_some(),
            "reasoning_effort",
            "reasoning_effort is not supported per-request; configure model in provider settings",
        ),
        (
            req.modalities.is_some(),
            "modalities",
            "only text output supported",
        ),
        (req.audio.is_some(), "audio", "audio output not supported"),
        (
            req.prediction.is_some(),
            "prediction",
            "predicted outputs not supported",
        ),
        (
            req.web_search_options.is_some(),
            "web_search_options",
            "web search not supported per-request",
        ),
    ] {
        if present {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                message,
                None,
                Some(param),
            ));
        }
    }
    Ok(())
}

/// Validate the message list: `messages` must be non-empty, and each message
/// is checked against 4 message-level rejections — `name`, `tool_call_id`,
/// role allow-list (`system`/`developer`/`user`/`assistant`, with
/// `tool`/`function` explicitly 400), and `tool_calls`. Message-level errors
/// all use `param = "messages"` with the fine-grained index in the message
/// text.
///
/// `tools` shape checks and the `tool_choice` shape gate run at the end of
/// this function; the name→authoritative-spec mapping itself happens in
/// the handler where the agent's tool directory is available.
fn validate_request(req: &ChatCompletionRequest) -> Result<(), ApiError> {
    if req.messages.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "messages must not be empty",
            None,
            Some("messages"),
        ));
    }
    for (i, msg) in req.messages.iter().enumerate() {
        // ① name: not propagated under transparent execution.
        if msg.name.is_some() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!(
                    "messages[{i}].name is not supported; tool results are transparently executed"
                ),
                None,
                Some("messages"),
            ));
        }
        // ② tool_call_id: same rationale.
        if msg.tool_call_id.is_some() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!(
                    "messages[{i}].tool_call_id is not supported; tool execution is transparent"
                ),
                None,
                Some("messages"),
            ));
        }
        // ③ role allow-list (4); tool/function roles are explicitly rejected
        // (RFC line 36), not silently folded into prompt text.
        if !matches!(
            msg.role.as_str(),
            "system" | "developer" | "user" | "assistant"
        ) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!(
                    "messages[{i}].role '{}' is not supported; allowed: system, developer, user, \
                     assistant (tool/function roles are transparently executed and rejected)",
                    msg.role
                ),
                None,
                Some("messages"),
            ));
        }
        // ④ tool_calls: meaningless under transparent execution.
        if msg.tool_calls.is_some() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!(
                    "messages[{i}].tool_calls is not supported; tool execution is transparent"
                ),
                None,
                Some("messages"),
            ));
        }
    }

    // ── tools: shape only (name allow-list resolution lives in the handler) ──
    if let Some(ref tools) = req.tools {
        for tool in tools {
            if tool.kind != "function" {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "Only 'function' tool type is supported",
                    None,
                    Some("tools"),
                ));
            }
            if tool.function.name.trim().is_empty() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "tool.function.name is required",
                    None,
                    Some("tools"),
                ));
            }
        }
    }

    // ── tool_choice: front-loaded so `"required"` / specific-function /
    //    malformed shapes are rejected here, before `parse_tool_choice`
    //    (which therefore only ever sees `auto`/`none`/absent).
    if let Some(ref tc) = req.tool_choice {
        match tc {
            serde_json::Value::String(s) if s == "auto" || s == "none" => {}
            // A function object is a recognized-but-unsupported shape; it gets
            // `unsupported_parameter` (parameter supported, value not), while
            // malformed shapes stay `invalid_request_error`.
            serde_json::Value::Object(_) => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "unsupported_parameter",
                    "specific-function tool_choice is not supported; use \"auto\" or \"none\"",
                    None,
                    Some("tool_choice"),
                ));
            }
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "tool_choice supports only \"auto\" or \"none\"",
                    None,
                    Some("tool_choice"),
                ));
            }
        }
    }
    Ok(())
}

/// The two reachable `tool_choice` modes after `validate_request`'s shape
/// gate: absent / `"auto"` → `Auto`; `"none"` → `None`. Everything else was
/// already rejected up front, so there is no `Required` / specific-function
/// variant to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolChoiceMode {
    Auto,
    None,
}

/// Parse `tool_choice` into a mode. Safe to call only after `validate_request`
/// (which rejects every shape outside `"auto"`/`"none"`/absent); the default
/// arm is defense-in-depth for absent / unexpected values.
fn parse_tool_choice(value: &Option<serde_json::Value>) -> ToolChoiceMode {
    match value {
        Some(v) if v.as_str() == Some("none") => ToolChoiceMode::None,
        _ => ToolChoiceMode::Auto,
    }
}

/// Resolve the authoritative server spec for each requested tool, preserving
/// the client's request order. Callers must have already rejected any name not
/// present in `configured`, so every lookup succeeds; the client's
/// description/parameters are intentionally ignored (schema forgery guard).
/// When a client attached a schema that differs from the authoritative one, a
/// WARN audit record is emitted but the authoritative spec is still used.
fn authoritative_specs_for(
    requested: &[ChatCompletionTool],
    configured: &HashMap<String, ToolSpec>,
) -> Vec<ToolSpec> {
    requested
        .iter()
        .filter_map(|tool| {
            let authoritative = configured.get(&tool.function.name)?;
            // Client-supplied description/parameters are never used; log
            // mismatches as audit signal. A missing/null client description
            // against an authoritative one counts as a mismatch.
            let desc_differs = tool
                .function
                .description
                .as_deref()
                .is_none_or(|c| c != authoritative.description);
            let schema_differs = desc_differs
                || (!tool.function.parameters.is_null()
                    && tool.function.parameters != *authoritative.parameters);
            if schema_differs {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_attrs(::serde_json::json!({ "tool": tool.function.name })),
                    "client-supplied tool schema differs from authoritative Tool::spec(); using server spec"
                );
            }
            Some(authoritative.clone())
        })
        .collect()
}

/// Map a validated `tools` allow-list to authoritative server specs.
///
/// `tool_choice` here has already passed `validate_request`, so only
/// `auto`/`none`/absent are reachable. `none` → empty override (the caller
/// disables tools); `auto` with no `tools` → `Ok(None)` = the default agent
/// tool set; `auto` with an allow-list → name-only whitelist resolution
/// (unknown names are a fail-closed 400, empty lists are rejected).
fn resolve_tool_specs(
    tool_choice: &Option<serde_json::Value>,
    tools: &Option<Vec<ChatCompletionTool>>,
    configured: &HashMap<String, ToolSpec>,
) -> Result<Option<Vec<ToolSpec>>, ApiError> {
    match parse_tool_choice(tool_choice) {
        ToolChoiceMode::None => Ok(Some(Vec::new())),
        ToolChoiceMode::Auto => match tools {
            None => Ok(None),
            Some(requested) => {
                if requested.is_empty() {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        "tools list must not be empty",
                        None,
                        Some("tools"),
                    ));
                }
                let unknown: Vec<&str> = requested
                    .iter()
                    .filter(|t| !configured.contains_key(&t.function.name))
                    .map(|t| t.function.name.as_str())
                    .collect();
                if !unknown.is_empty() {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        &format!("Unknown tool(s): {}", unknown.join(", ")),
                        None,
                        Some("tools"),
                    ));
                }
                Ok(Some(authoritative_specs_for(requested, configured)))
            }
        },
    }
}

// ── Agent routing (model → alias) ───────────────────────────────────────────

/// Map an OpenAI `model` value to a ZeroClaw agent alias.
///
/// - `""` / `zeroclaw` / `zeroclaw/default` → `resolved_runtime_agent_alias()`
///   (the same default-agent semantics webhook chat uses);
/// - `zeroclaw/<alias>` → `alias` — only this prefix is recognized
///   (`zeroclaw:`/`agent:` are not part of the contract); existence of the
///   alias is verified by the caller;
/// - anything else → `Err` (fail closed, no silent routing).
///
/// Only the string mapping happens here; alias existence is a handler-side
/// concern so the two failure modes stay distinguishable.
fn agent_alias_from_model(model: &str, config: &Config) -> Result<String, String> {
    let model = model.trim();

    // ① default shorthand: empty / zeroclaw / zeroclaw/default
    if model.is_empty() || model == "zeroclaw" || model == "zeroclaw/default" {
        return config
            .resolved_runtime_agent_alias()
            .map(str::to_owned)
            .ok_or_else(|| "no enabled [agents.<alias>] configured".to_string());
    }

    // ② single prefix: zeroclaw/<alias>
    if let Some(rest) = model.strip_prefix("zeroclaw/") {
        let alias = rest.trim();
        if alias.is_empty() {
            return Err(format!(
                "Invalid agent target `{model}`: missing agent alias"
            ));
        }
        return Ok(alias.to_string());
    }

    // ③ everything else is rejected
    Err(format!(
        "Unrecognized model `{model}`: this endpoint routes to ZeroClaw agents only. \
         Use `zeroclaw/<agent-alias>`, or omit `model` (or send `zeroclaw`) for the \
         default agent. Provider and model are ZeroClaw configuration, not per-request."
    ))
}

/// Resolve `model` to an agent alias and verify it exists, folding both
/// failures into the 400 `invalid_request_error` envelope the handler returns.
/// Explicitly *not* checking `enabled`: an alias that exists but is disabled
/// is still reachable when named directly (same as WS `?agent=`).
/// The `Err` Response is the handler's return type (same shape as
/// `validate_request`).
pub(crate) fn resolve_agent_alias_from_model(
    model: &str,
    config: &Config,
) -> Result<String, ApiError> {
    let alias = match agent_alias_from_model(model, config) {
        Ok(alias) => alias,
        Err(e) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &e,
                None,
                Some("model"),
            ));
        }
    };

    if config.agent(&alias).is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            &format!("Unknown agent `{alias}` — no [agents.{alias}] entry configured."),
            None,
            Some("model"),
        ));
    }
    Ok(alias)
}

// ── HTTP handler orchestration ──────────────────────────────────────────────

/// HTTP chat-completions session-key gate: ASCII lowercase `[a-z0-9_-]` only.
/// Narrower than the WebSocket path's broader check; the chat endpoint is new,
/// so no pre-existing HTTP sessions break. Restricting to ASCII-lowercase keeps
/// the `gw_`-prefixed persistence key injective on case-insensitive filesystems
/// (no `gw_Alpha` vs `gw_alpha` collision) at this entry point.
pub fn is_http_canonical_session_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Extract the complete `gw_`-prefixed session key from `x-session-key`.
///
/// Full-key model: the header value *is* the persistence key (with `gw_`), so
/// it is handed straight to `session_queue`/`ws_connections`/`session_backend`/
/// memory scope with no second prefixing. An absent header produces a fresh
/// `gw_{uuid}`. Returns `(session_key, had_header)`; the bool drives the
/// history-load precedence. Invalid values fail closed with 400.
fn extract_session_key(headers: &HeaderMap) -> Result<(String, bool), ApiError> {
    match headers.get("x-session-key") {
        Some(value) => {
            // A non-UTF-8 header is a client error, not a missing key:
            // rejecting loudly avoids silently minting a fresh session that
            // would claim ownership away from the caller's real one.
            let key = value.to_str().map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "x-session-key must be a valid UTF-8 string",
                    None,
                    Some("x-session-key"),
                )
            })?;
            if key.is_empty() {
                // An empty value is a client error, not a missing key: an
                // explicit 400 avoids silently minting a fresh session (and
                // stamping ownership on it) the caller did not ask for.
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "x-session-key must not be empty",
                    None,
                    Some("x-session-key"),
                ));
            }
            if !key.starts_with("gw_") || !is_http_canonical_session_key(key) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "x-session-key must be a canonical `gw_`-prefixed key (lowercase ASCII [a-z0-9_-])",
                    None,
                    Some("x-session-key"),
                ));
            }
            Ok((key.to_string(), true))
        }
        None => Ok((format!("gw_{}", uuid::Uuid::new_v4()), false)),
    }
}

/// Split the request `messages` into the runner's inputs:
///   history: `Vec<ChatMessage>` — the user/assistant messages before the
///     active turn, fed to `agent.seed_history`;
///   current_turn: `String` — the last user message's content, with the
///     non-empty system/developer contents joined ahead as a prefix.
///
/// 4-role convergence: `tool`/`function` roles are already 400'd by
/// `validate_request`, so no role normalization is needed here.
fn split_messages(msgs: &[ChatCompletionMessage]) -> (Vec<ChatMessage>, String) {
    // ① prefix: non-empty system/developer contents joined with "\n\n"
    // (subordinate to ZeroClaw's system prompt).
    let prefix: String = msgs
        .iter()
        .filter(|m| m.role == "system" || m.role == "developer")
        .filter_map(|m| m.content.as_deref().filter(|c| !c.trim().is_empty()))
        .collect::<Vec<_>>()
        .join("\n\n");

    // ② active turn = the last user message (tool/function already rejected).
    let active_idx = msgs.iter().rposition(|m| m.role == "user");

    // ③ history = non-system/developer messages before the active turn.
    let history: Vec<ChatMessage> = msgs[..active_idx.unwrap_or(0)]
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .map(|m| ChatMessage {
            role: m.role.clone(),
            content: m.content.clone().unwrap_or_default(),
        })
        .collect();

    // ④ current_turn = the last user message's content. Edge case: no user
    // message at all (only system/developer/assistant) joins the assistant
    // contents instead, with an empty history.
    let mut current_turn = match active_idx {
        Some(i) => msgs[i].content.clone().unwrap_or_default(),
        None => msgs
            .iter()
            .filter(|m| m.role == "assistant")
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n"),
    };

    if !prefix.is_empty() {
        current_turn = format!("{prefix}\n\n{current_turn}");
    }

    (history, current_turn)
}

/// The memory bucket a session lands in, mirroring the WebSocket transport's
/// scope for a shared session: the complete key minus its `gw_` prefix (the
/// WebSocket side receives the raw id without any prefix, so stripping brings
/// both transports to the same value).
fn http_memory_scope(session_key: &str) -> String {
    sanitize_session_key(session_key.strip_prefix("gw_").unwrap_or(session_key))
}

/// Mirror of the WebSocket transport's memory-handle resolution so HTTP and WS
/// sessions land in the same memory scope for a shared agent. Failures degrade
/// to `None` and the turn continues (same graceful behavior as WS).
async fn resolve_http_memory_handle(
    config: &Config,
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

/// Stamp `x-session-key` (the complete key) onto a response. Called for every
/// error after session-key extraction so clients can retry with the same key.
fn add_session_key_header(mut response: Response, session_key: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(session_key) {
        response.headers_mut().insert("x-session-key", value);
    }
    response
}

/// Stamp the real sliding-window `x-ratelimit-*` headers from a
/// [`RateLimitDecision`]. `x-ratelimit-reset` is an absolute Unix epoch
/// (`Utc::now() + reset_after_secs`), matching the reference API contract so
/// clients can compute a backoff directly.
fn add_rate_limit_headers(mut response: Response, decision: &RateLimitDecision) -> Response {
    let header_value = |value: u64| {
        HeaderValue::from_str(&value.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"))
    };
    let headers = response.headers_mut();
    headers.insert("x-ratelimit-limit", header_value(decision.limit.into()));
    headers.insert(
        "x-ratelimit-remaining",
        header_value(decision.remaining.into()),
    );
    headers.insert(
        "x-ratelimit-reset",
        header_value(
            (chrono::Utc::now().timestamp() + decision.reset_after_secs as i64).max(0) as u64,
        ),
    );
    response
}

/// Rewrite the body-size 413 emitted by `RequestBodyLimitLayer` into the
/// OpenAI-compatible error envelope, so an oversized body stays JSON.
async fn rewrite_payload_too_large(response: Response) -> Response {
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request_error",
            &format!(
                "Request body exceeds the {} byte limit",
                crate::MAX_BODY_SIZE
            ),
            None,
            None,
        )
    } else {
        response
    }
}

/// Build `/v1/chat/completions`. When `enabled` is false the route is absent
/// (POST then yields 405, the gateway's existing missing-path behavior);
/// the body-size limit and 413 rewrite always apply.
/// Shared by production wiring and tests so neither drifts from the other.
pub(crate) fn build_chat_completions_router(state: AppState, enabled: bool) -> Router {
    let router: Router<AppState> = if enabled {
        Router::new().route("/v1/chat/completions", post(handle_chat_completions))
    } else {
        Router::new()
    };
    router
        .with_state(state)
        .layer(RequestBodyLimitLayer::new(crate::MAX_BODY_SIZE))
        .layer(axum::middleware::map_response(rewrite_payload_too_large))
}

/// `POST /v1/chat/completions` handler: the 12-step orchestration.
///
/// Steps 1–4 run before the session key is known and do not echo it; from step
/// 5 onward every error echoes the complete `x-session-key`. Step 11 (tool
/// whitelist) and step 12 (dispatch) close the orchestration.
pub(crate) async fn handle_chat_completions(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Manual serde parse instead of the axum `Json` extractor: a
    // deserialization failure would otherwise surface as a 422 text/plain
    // body, breaking the OpenAI-compatible JSON error envelope.
    let request: ChatCompletionRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Invalid JSON body: expected a chat-completions request",
                None,
                Some("messages"),
            );
        }
    };

    // ── ① authentication ────────────────────────────────────────────────
    if crate::api::require_auth(&state, &headers).is_err() {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>",
            None,
            None,
        );
    }

    // ── ② rate limit ────────────────────────────────────────────────────
    let rate_limit = state.decide_chat_rate_limit(Some(peer_addr), &headers);
    if !rate_limit.allowed {
        return add_rate_limit_headers(
            error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                "Rate limit exceeded for chat completions",
                None,
                None,
            ),
            &rate_limit,
        );
    }

    // ── ③ validation ────────────────────────────────────────────────────
    // Request-level rejections run before message-level ones.
    if let Err(e) = validate_unsupported_params(&request).and_then(|_| validate_request(&request)) {
        return e.into_response();
    }

    // ── ④ model routing + provider-model presence ───────────────────────
    let config = state.config.read().clone();
    let alias = match resolve_agent_alias_from_model(&request.model, &config) {
        Ok(alias) => alias,
        Err(e) => return e.into_response(),
    };
    // Agent exists (400 above) but no provider model is configured → 503.
    // `resolved_model_provider_for_agent` returns None when the agent has no
    // `model_provider` field or the provider is undefined.
    if config
        .resolved_model_provider_for_agent(&alias)
        .and_then(|(_, _, cfg)| cfg.model.as_deref().filter(|m| !m.trim().is_empty()))
        .is_none()
    {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            "Agent not configured — complete onboarding at /onboard",
            None,
            Some("model"),
        );
    }

    // ── ⑤ session key ───────────────────────────────────────────────────
    let (session_key, had_header) = match extract_session_key(&headers) {
        Ok(key) => key,
        Err(e) => return e.into_response(),
    };
    // Every error from here on echoes the complete key.
    let session_key_echo = session_key.clone();
    let add_session_key =
        move |response: Response| add_session_key_header(response, &session_key_echo);

    // ── Blank-message interception ──────────────────────────────────────
    // Runs before ownership stamping (a rejected blank message must never
    // mint a ghost session) and before queue acquire (it must not consume a
    // slot). `content: null` / a missing field is as invalid as a blank
    // string: `as_deref()` would yield None and silently turn the turn into
    // an empty provider call, so both are intercepted here. HTTP new-callers
    // only; the WebSocket transport keeps its `is_empty()` checks unchanged.
    let last_user = request.messages.iter().rev().find(|m| m.role == "user");
    if last_user.is_some_and(|m| m.content.as_deref().is_none_or(|c| c.trim().is_empty())) {
        return add_session_key(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "The last user message content cannot be null, empty, or blank",
            None,
            Some("messages"),
        ));
    }

    // ── ⑥ per-session serialization ─────────────────────────────────────
    // Acquired before any backend access so "history load → turn → persist"
    // is serialized per session; the guard is held by the transport for the
    // whole turn.
    let session_guard = match state.session_queue.acquire(&session_key).await {
        Ok(guard) => guard,
        Err(SessionQueueError::QueueFull { .. }) => {
            return add_session_key(error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                "Session queue full — too many concurrent requests for this session",
                None,
                None,
            ));
        }
        Err(SessionQueueError::Timeout { .. }) => {
            return add_session_key(error_response(
                StatusCode::REQUEST_TIMEOUT,
                "timeout",
                "Timed out waiting for the session lock",
                None,
                None,
            ));
        }
    };

    // ── ⑦ cross-transport exclusivity ───────────────────────────────────
    // Only keys derived from the WS `?session_id=` query (the live-connection
    // lease) are registered in `ws_connections`; a connect-frame override is
    // not reflected there — that is the documented boundary.
    if ws_session_active(&state, &session_key) {
        return add_session_key(error_response(
            StatusCode::CONFLICT,
            "cross_transport_session_in_use",
            "Session is actively held by a WebSocket connection; close it first",
            None,
            Some("x-session-key"),
        ));
    }

    // ── ⑧+⑨ history precedence + ownership ─────────────────────────────
    let request_has_authoritative_history = request.messages.len() > 1;
    let stored_messages: Vec<ChatMessage> = if request_has_authoritative_history || !had_header {
        // State B (>1 message): the request is authoritative, no backend load.
        // State C (no key): fresh `gw_{uuid}`, nothing to load.
        Vec::new()
    } else if let Some(backend) = state.session_backend.as_ref() {
        // State A (single message + key): load the backend transcript.
        backend.load(&session_key)
    } else {
        Vec::new()
    };

    // Fail-closed ownership: reuse of a key with a different agent is
    // rejected. Atomic claim: no owner (or the same owner) → the caller owns
    // the session; a different incumbent owner → 400. A backend without
    // ownership tracking (JSONL `SessionStore`) fails closed on a non-empty
    // session and passes a fresh empty one.
    if let Some(backend) = state.session_backend.as_ref() {
        match backend.claim_session_agent_alias(&session_key, &alias) {
            Ok(ClaimOutcome::Claimed) => {}
            Ok(ClaimOutcome::Conflict(existing)) => {
                return add_session_key(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    &format!("Session belongs to agent `{existing}`, not `{alias}`"),
                    None,
                    Some("x-session-key"),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                if !stored_messages.is_empty() {
                    return add_session_key(error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        "The session persistence backend does not track session ownership; reuse of a non-empty session is refused (use the default SQLite backend, or start a fresh session)",
                        None,
                        Some("x-session-key"),
                    ));
                }
            }
            Err(e) => {
                return add_session_key(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    &zeroclaw_providers::sanitize_api_error(&e.to_string()),
                    None,
                    Some("x-session-key"),
                ));
            }
        }
    }

    // ── ⑩ memory scope ─────────────────────────────────────────────────
    // Same scope as WS: `sanitize_session_key(raw_id)` where the raw id is the
    // complete key minus the `gw_` prefix. This keeps HTTP and WS on the same
    // memory bucket for a shared session.
    let memory_session_id = http_memory_scope(&session_key);
    let ws_memory = resolve_http_memory_handle(&config, &alias)
        .await
        .unwrap_or_default(); // graceful degradation; the turn continues without memory

    // ── ⑫ dispatch ──────────────────────────────────────────────────────
    let (history, current_turn) = split_messages(&request.messages);
    let mut agent =
        match Agent::from_config_with_session_cwd_and_mcp(&config, &alias, None, true).await {
            Ok(agent) => agent,
            Err(e) => {
                return add_session_key(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    &zeroclaw_providers::sanitize_api_error(&e.to_string()),
                    None,
                    None,
                ));
            }
        };
    agent.set_channel_name("http".to_string());
    agent.set_memory_session_id(Some(memory_session_id));
    if !stored_messages.is_empty() {
        agent.seed_history(&stored_messages);
    } else if !history.is_empty() {
        agent.seed_history(&history);
    }

    // ── ⑪ tools ─────────────────────────────────────────────────────────
    // `tool_choice` shape was already gated in `validate_request`; here we
    // map the name allow-list to authoritative `Tool::spec()`s and scope them
    // onto the per-request agent (propagated to the turn + delegate spawns via
    // `TOOL_SPECS_OVERRIDE`). `tool_choice: "none"` disables all tools; absent
    // `tools` with `auto` leaves the default agent tool set untouched.
    let configured: HashMap<String, ToolSpec> = agent
        .get_configured_tool_specs()
        .into_iter()
        .map(|spec| (spec.name.clone(), spec))
        .collect();
    match parse_tool_choice(&request.tool_choice) {
        ToolChoiceMode::None => agent.disable_tools(),
        ToolChoiceMode::Auto => {
            match resolve_tool_specs(&request.tool_choice, &request.tools, &configured) {
                Err(e) => return add_session_key(e.into_response()),
                Ok(Some(specs)) => {
                    if specs.is_empty() {
                        agent.disable_tools();
                    } else {
                        agent.set_tool_specs(specs);
                    }
                }
                Ok(None) => {} // default tool set; leave the agent untouched
            }
        }
    }

    let include_usage = request
        .stream_options
        .as_ref()
        .map(|o| o.include_usage)
        .unwrap_or(false);
    let timeout = Duration::from_secs(gateway_long_running_request_timeout_secs(&config.gateway));
    let model = request.model.clone();
    if request.stream {
        stream_mode(
            state,
            agent,
            current_turn,
            session_key,
            ws_memory,
            timeout,
            include_usage,
            rate_limit,
            model,
            session_guard,
            add_session_key,
        )
        .await
    } else {
        blocking_mode(
            state,
            agent,
            current_turn,
            session_key,
            ws_memory,
            timeout,
            rate_limit,
            model,
            session_guard,
            add_session_key,
        )
        .await
    }
}

/// Stream a turn as OpenAI SSE chunks over an mpsc bridge. The runner is
/// spawned so the response body starts immediately; the transport's `forward`
/// maps `TurnEvent`s to chunks, and a second task emits the terminal frames
/// (finish / usage / `[DONE]`) once `run_gateway_turn` returns.
#[allow(clippy::too_many_arguments)]
async fn stream_mode(
    state: AppState,
    mut agent: Agent,
    user_message: String,
    session_key: String,
    ws_memory: Option<Arc<dyn zeroclaw_memory::Memory>>,
    timeout: Duration,
    include_usage: bool,
    rate_limit: RateLimitDecision,
    model: String,
    session_guard: SessionGuard,
    add_session_key: impl Fn(Response) -> Response,
) -> Response {
    let (body_tx, body_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(64);
    let (outcome_tx, outcome_rx) = oneshot::channel::<TurnOutcome>();

    let id = generate_completion_id();
    let created = chrono::Utc::now().timestamp() as u64;

    let id_fwd = id.clone();
    let model_fwd = model.clone();
    let body_tx_fwd = body_tx.clone();

    let forward = move |handle: TurnRunnerHandle| async move {
        let TurnRunnerHandle {
            mut event_rx,
            cancel_token,
        } = handle;
        // Usage aggregates and partial text for cancellation/timeout fallback.
        let mut accumulated_text = String::new();
        let mut total_input_tokens: Option<u64> = None;
        let mut total_output_tokens: Option<u64> = None;
        let mut last_input_tokens: Option<u64> = None;

        // Role chunk first (OpenAI convention: the first chunk carries `role`).
        let first = make_chunk(
            &id_fwd,
            created,
            &model_fwd,
            Some("assistant"),
            Some(String::new()),
            None,
            None,
        );
        if body_tx_fwd.send(Ok(first)).await.is_err() {
            cancel_token.cancel();
            return TurnForwardResult {
                total_input_tokens,
                total_output_tokens,
                last_input_tokens,
                accumulated_text,
            };
        }

        while let Some(event) = event_rx.recv().await {
            match event {
                TurnEvent::Chunk { delta } => {
                    accumulated_text.push_str(&delta);
                    let chunk =
                        make_chunk(&id_fwd, created, &model_fwd, None, Some(delta), None, None);
                    if body_tx_fwd.send(Ok(chunk)).await.is_err() {
                        // Client disconnect: stop the turn and the drain.
                        cancel_token.cancel();
                        break;
                    }
                }
                TurnEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    if let Some(it) = input_tokens {
                        total_input_tokens = Some(total_input_tokens.unwrap_or(0) + it);
                        last_input_tokens = Some(it);
                    }
                    if let Some(ot) = output_tokens {
                        total_output_tokens = Some(total_output_tokens.unwrap_or(0) + ot);
                    }
                }
                // Thinking / ToolCall / ToolResult / ApprovalRequest /
                // HistoryTrimmed / Plan are suppressed: tool execution is
                // transparent, and mixing thinking into `delta` would break the
                // streaming == blocking text invariant.
                _ => {}
            }
        }

        TurnForwardResult {
            total_input_tokens,
            total_output_tokens,
            last_input_tokens,
            accumulated_text,
        }
    };

    // The per-session guard is moved into the spawned task and held for the
    // whole turn, so the session lock is only released when the turn ends.
    let state_for_task = state.clone();
    let _runner_task = zeroclaw_spawn::spawn!(async move {
        let _held_session = session_guard;
        let outcome = run_gateway_turn(
            &state_for_task,
            &mut agent,
            &user_message,
            &session_key,
            &ws_memory,
            None, // HTTP never enters steering-drain mode
            "http",
            Some(timeout),
            forward,
        )
        .await;
        let _ = outcome_tx.send(outcome);
    });

    // Terminal frames, emitted by a second task after the turn completes. The
    // order is fixed: finish → (optional usage) → `[DONE]` on Success; a bare
    // error envelope + `[DONE]` otherwise.
    let id_terminal = id;
    let model_terminal = model;
    let _terminal_task = zeroclaw_spawn::spawn!(async move {
        let outcome = match outcome_rx.await {
            Ok(outcome) => outcome,
            Err(_) => {
                let _ = body_tx
                    .send(Ok(stream_error_event(
                        "internal_error",
                        500,
                        "Agent task terminated unexpectedly",
                    )))
                    .await;
                let _ = body_tx.send(Ok(Event::default().data("[DONE]"))).await;
                return;
            }
        };
        match outcome.status {
            TurnStatus::Success => {
                let finish = make_chunk(
                    &id_terminal,
                    created,
                    &model_terminal,
                    None,
                    None,
                    None,
                    Some("stop"),
                );
                if body_tx.send(Ok(finish)).await.is_err() {
                    return;
                }
                if include_usage {
                    let usage = make_usage_chunk(
                        &id_terminal,
                        created,
                        &model_terminal,
                        outcome.total_input_tokens.unwrap_or(0),
                        outcome.total_output_tokens.unwrap_or(0),
                        outcome.total_tokens.unwrap_or(0),
                    );
                    if body_tx.send(Ok(usage)).await.is_err() {
                        return;
                    }
                }
                let _ = body_tx.send(Ok(Event::default().data("[DONE]"))).await;
            }
            TurnStatus::TimedOut => {
                let _ = body_tx
                    .send(Ok(stream_error_event("timeout", 408, "Request timed out")))
                    .await;
                let _ = body_tx.send(Ok(Event::default().data("[DONE]"))).await;
            }
            TurnStatus::Error => {
                let message = outcome
                    .error
                    .as_ref()
                    .map(|f| {
                        f.user_message.clone().unwrap_or_else(|| {
                            zeroclaw_providers::sanitize_api_error(&f.diagnostic)
                        })
                    })
                    .unwrap_or_else(|| "Agent turn failed".to_string());
                let _ = body_tx
                    .send(Ok(stream_error_event("internal_error", 500, &message)))
                    .await;
                let _ = body_tx.send(Ok(Event::default().data("[DONE]"))).await;
            }
            TurnStatus::Cancelled => {
                let _ = body_tx
                    .send(Ok(stream_error_event(
                        "internal_error",
                        500,
                        "Request cancelled",
                    )))
                    .await;
                let _ = body_tx.send(Ok(Event::default().data("[DONE]"))).await;
            }
        }
    });

    // The SSE body consumes the bridge; the handler's headers are stamped on
    // top (content-type: text/event-stream comes from axum's `Sse`).
    let stream = ReceiverStream::new(body_rx);
    let response = Sse::new(stream).into_response();
    let response = add_rate_limit_headers(response, &rate_limit);
    add_session_key(response)
}

/// Non-streaming JSON mode: drain the turn events, await the outcome over a
/// oneshot, and map the four `TurnStatus`es onto OpenAI responses.
#[allow(clippy::too_many_arguments)]
async fn blocking_mode(
    state: AppState,
    mut agent: Agent,
    user_message: String,
    session_key: String,
    ws_memory: Option<Arc<dyn zeroclaw_memory::Memory>>,
    timeout: Duration,
    rate_limit: RateLimitDecision,
    model: String,
    session_guard: SessionGuard,
    add_session_key: impl Fn(Response) -> Response,
) -> Response {
    let (outcome_tx, outcome_rx) = oneshot::channel::<TurnOutcome>();

    // Drain: `Chunk` deltas are discarded (the full text comes from
    // `outcome.response_text`, not delta concatenation); `Usage` is aggregated.
    let forward = move |handle: TurnRunnerHandle| async move {
        let TurnRunnerHandle {
            mut event_rx,
            cancel_token: _cancel_token,
        } = handle;
        let mut accumulated_text = String::new();
        let mut total_input_tokens: Option<u64> = None;
        let mut total_output_tokens: Option<u64> = None;
        let mut last_input_tokens: Option<u64> = None;
        while let Some(event) = event_rx.recv().await {
            match event {
                TurnEvent::Chunk { delta } => accumulated_text.push_str(&delta),
                TurnEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    if let Some(it) = input_tokens {
                        total_input_tokens = Some(total_input_tokens.unwrap_or(0) + it);
                        last_input_tokens = Some(it);
                    }
                    if let Some(ot) = output_tokens {
                        total_output_tokens = Some(total_output_tokens.unwrap_or(0) + ot);
                    }
                }
                _ => {}
            }
        }
        TurnForwardResult {
            total_input_tokens,
            total_output_tokens,
            last_input_tokens,
            accumulated_text,
        }
    };

    let state_for_task = state.clone();
    let _runner_task = zeroclaw_spawn::spawn!(async move {
        let _held_session = session_guard;
        let outcome = run_gateway_turn(
            &state_for_task,
            &mut agent,
            &user_message,
            &session_key,
            &ws_memory,
            None,
            "http",
            Some(timeout),
            forward,
        )
        .await;
        let _ = outcome_tx.send(outcome);
    });

    let outcome = match outcome_rx.await {
        Ok(outcome) => outcome,
        Err(_) => {
            // Oneshot sender dropped without sending = the task panicked
            // (tokio abort); report a conservative 500.
            return add_session_key(add_rate_limit_headers(
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "Agent task terminated unexpectedly",
                    None,
                    None,
                ),
                &rate_limit,
            ));
        }
    };

    let response = match outcome.status {
        TurnStatus::Success => {
            let response = ChatCompletionResponse {
                id: generate_completion_id(),
                object: "chat.completion",
                created: chrono::Utc::now().timestamp() as u64,
                model,
                choices: vec![NonStreamChoice {
                    index: 0,
                    message: AssistantMessage {
                        role: "assistant",
                        content: Some(outcome.response_text),
                        tool_calls: None,
                    },
                    finish_reason: "stop".to_string(),
                    logprobs: None,
                }],
                usage: CompletionUsage {
                    prompt_tokens: outcome.total_input_tokens.unwrap_or(0),
                    completion_tokens: outcome.total_output_tokens.unwrap_or(0),
                    total_tokens: outcome.total_tokens.unwrap_or(0),
                },
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        TurnStatus::Error => {
            let message = outcome
                .error
                .as_ref()
                .map(|f| {
                    f.user_message
                        .clone()
                        .unwrap_or_else(|| zeroclaw_providers::sanitize_api_error(&f.diagnostic))
                })
                .unwrap_or_else(|| "Agent turn failed".to_string());
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                &message,
                None,
                None,
            )
        }
        TurnStatus::Cancelled => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Request cancelled",
            None,
            None,
        ),
        TurnStatus::TimedOut => error_response(
            StatusCode::REQUEST_TIMEOUT,
            "timeout",
            "Request timed out",
            None,
            None,
        ),
    };
    add_session_key(add_rate_limit_headers(response, &rate_limit))
}

// ── SSE chunk helpers ────────────────────────────────────────────────────────

/// OpenAI-compatible chunk JSON (the `data:` payload of every SSE frame except
/// `[DONE]` and the error envelope). Only present fields appear in `delta`.
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
        delta.insert("role".into(), r.into());
    }
    if let Some(c) = content {
        delta.insert("content".into(), c.into());
    }
    if let Some(tc) = tool_calls {
        delta.insert("tool_calls".into(), tc.into());
    }

    serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason,
            "logprobs": null,
        }],
    })
}

/// `chunk_json` wrapped as a `data:` SSE event (no `event:` line).
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

/// Terminal usage chunk (`choices: []`, OpenAI convention). Only emitted on
/// Success when `stream_options.include_usage` is set.
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
            "total_tokens": total,
        },
    });
    Event::default().data(data.to_string())
}

fn generate_completion_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4())
}

/// In-stream error envelope: a bare `data:` line carrying `{"error": {...}}`
/// with the same fields as the HTTP `ErrorResponse`. Only `timeout` (408) and
/// `internal_error` (500) appear inside a stream; handler-level types are
/// returned before the stream starts. The `json_data` fallback mirrors the
/// defensive `[Error]` degradation in the reference.
fn stream_error_event(error_type: &str, status: u16, message: &str) -> Event {
    Event::default()
        .json_data(serde_json::json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": serde_json::Value::Null,
                "param": serde_json::Value::Null,
                "status": status,
            }
        }))
        .unwrap_or_else(|_| Event::default().data("[Error]"))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_state;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    fn parse_request(v: serde_json::Value) -> ChatCompletionRequest {
        serde_json::from_value(v).expect("request must deserialize")
    }

    /// Run both validators; the first rejection wins (unsupported params
    /// checked before message-level validation).
    fn run_validators(req: &ChatCompletionRequest) -> Result<(), ApiError> {
        validate_unsupported_params(req).and_then(|_| validate_request(req))
    }

    async fn response_json(response: Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    async fn assert_rejected(req: serde_json::Value, expected_param: &str) {
        let r = parse_request(req);
        let err = run_validators(&r).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], expected_param);
    }

    fn base_request() -> serde_json::Value {
        json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hello"}],
        })
    }

    #[tokio::test]
    async fn rejects_max_tokens() {
        let mut v = base_request();
        v["max_tokens"] = json!(1);
        assert_rejected(v, "max_tokens").await;
    }

    #[tokio::test]
    async fn rejects_top_p() {
        let mut v = base_request();
        v["top_p"] = json!(0.5);
        assert_rejected(v, "top_p").await;
    }

    #[tokio::test]
    async fn rejects_stop() {
        let mut v = base_request();
        v["stop"] = json!("END");
        assert_rejected(v, "stop").await;
    }

    #[tokio::test]
    async fn rejects_seed() {
        let mut v = base_request();
        v["seed"] = json!(42);
        assert_rejected(v, "seed").await;
    }

    #[tokio::test]
    async fn rejects_logprobs() {
        let mut v = base_request();
        v["logprobs"] = json!(true);
        assert_rejected(v, "logprobs").await;
    }

    #[tokio::test]
    async fn rejects_all_23_unsupported() {
        let cases: &[(&str, serde_json::Value)] = &[
            ("max_tokens", json!(1)),
            ("top_p", json!(0.5)),
            ("stop", json!("END")),
            ("presence_penalty", json!(0.0)),
            ("frequency_penalty", json!(0.0)),
            ("n", json!(1)),
            ("response_format", json!({"type": "text"})),
            ("seed", json!(42)),
            ("logprobs", json!(true)),
            ("top_logprobs", json!(5)),
            ("user", json!("u-1")),
            ("logit_bias", json!({})),
            ("max_completion_tokens", json!(100)),
            ("temperature", json!(0.7)),
            ("parallel_tool_calls", json!(true)),
            ("service_tier", json!("auto")),
            ("functions", json!([{"name": "f"}])),
            ("function_call", json!("auto")),
            ("reasoning_effort", json!("medium")),
            ("modalities", json!(["text"])),
            ("audio", json!({})),
            ("prediction", json!({})),
            ("web_search_options", json!({})),
        ];
        assert_eq!(cases.len(), 23);
        for (param, value) in cases {
            let mut v = base_request();
            v[param] = value.clone();
            assert_rejected(v, param).await;
        }
    }

    #[tokio::test]
    async fn accepts_none_unsupported() {
        let r = parse_request(base_request());
        assert!(run_validators(&r).is_ok());
    }

    #[tokio::test]
    async fn rejects_explicit_temperature() {
        let mut v = base_request();
        v["temperature"] = json!(0.7);
        assert_rejected(v, "temperature").await;
    }

    #[tokio::test]
    async fn omits_temperature_uses_agent_config() {
        // No temperature in the request -> passes; the routed agent's
        // configured temperature is used (actual value verified by the handler).
        let r = parse_request(base_request());
        assert!(run_validators(&r).is_ok());
    }

    #[tokio::test]
    async fn rejects_n_eq_1() {
        // Strict: any `n`, including n=1, is rejected.
        let mut v = base_request();
        v["n"] = json!(1);
        assert_rejected(v, "n").await;
    }

    #[tokio::test]
    async fn tolerates_metadata_and_store() {
        let mut v = base_request();
        v["metadata"] = json!({"trace_id": "abc", "user_tags": ["x"]});
        v["store"] = json!(true);
        let r = parse_request(v);
        assert!(run_validators(&r).is_ok());
    }

    #[tokio::test]
    async fn rejects_unknown_role() {
        let v = json!({
            "model": "gpt-4o",
            "messages": [{"role": "admin", "content": "hi"}],
        });
        let r = parse_request(v);
        let err = run_validators(&r).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "messages");
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("messages[0].role"));
        assert!(msg.contains("admin"));
    }

    #[tokio::test]
    async fn rejects_tool_role() {
        // Rejected at the role check itself, not indirectly via tool_call_id.
        let v = json!({
            "model": "gpt-4o",
            "messages": [{"role": "tool", "content": "result"}],
        });
        let r = parse_request(v);
        let err = run_validators(&r).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "messages");
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("messages[0].role"));
        assert!(msg.contains("tool"));
    }

    #[tokio::test]
    async fn rejects_function_role() {
        let v = json!({
            "model": "gpt-4o",
            "messages": [{"role": "function", "content": "legacy"}],
        });
        let r = parse_request(v);
        let err = run_validators(&r).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "messages");
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("messages[0].role"));
        assert!(msg.contains("function"));
    }

    #[tokio::test]
    async fn rejects_tool_calls_in_history() {
        let v = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "assistant",
                "content": "ok",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{}"},
                }],
            }],
        });
        let r = parse_request(v);
        let err = run_validators(&r).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "messages");
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("messages[0].tool_calls"));
    }

    #[tokio::test]
    async fn rejects_name_and_tool_call_id() {
        // name alone.
        let v = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi", "name": "alice"}],
        });
        let r = parse_request(v);
        let err = run_validators(&r).unwrap_err();
        let (_, body) = response_json(err.into_response()).await;
        assert_eq!(body["error"]["param"], "messages");
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("messages[0].name"));

        // tool_call_id alone.
        let v2 = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi", "tool_call_id": "call_1"}],
        });
        let r2 = parse_request(v2);
        let err2 = run_validators(&r2).unwrap_err();
        let (_, body2) = response_json(err2.into_response()).await;
        assert_eq!(body2["error"]["param"], "messages");
        let msg2 = body2["error"]["message"].as_str().unwrap();
        assert!(msg2.contains("messages[0].tool_call_id"));
    }

    #[tokio::test]
    async fn rejects_empty_messages() {
        let v = json!({"model": "gpt-4o", "messages": []});
        let r = parse_request(v);
        let err = run_validators(&r).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "messages");
        assert_eq!(body["error"]["message"], "messages must not be empty");
    }

    #[tokio::test]
    async fn error_envelope_shape() {
        let mut v = base_request();
        v["max_tokens"] = json!(1);
        let r = parse_request(v);
        let err = run_validators(&r).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"]["message"].is_string());
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert!(body["error"]["code"].is_null());
        assert_eq!(body["error"]["param"], "max_tokens");
        assert_eq!(body["error"]["status"], 400);
    }

    #[tokio::test]
    async fn null_unsupported_fields_treated_as_unset() {
        // `null` folds into `None` for Option fields (indistinguishable from
        // omission), matching OpenAI's "null means unset" convention — so none
        // of the 23 rejected fields trips validation when passed as null.
        let fields = [
            "max_tokens",
            "top_p",
            "stop",
            "presence_penalty",
            "frequency_penalty",
            "n",
            "response_format",
            "seed",
            "logprobs",
            "top_logprobs",
            "user",
            "logit_bias",
            "max_completion_tokens",
            "temperature",
            "parallel_tool_calls",
            "service_tier",
            "functions",
            "function_call",
            "reasoning_effort",
            "modalities",
            "audio",
            "prediction",
            "web_search_options",
        ];
        assert_eq!(fields.len(), 23);
        for param in fields {
            let mut v = base_request();
            v[param] = serde_json::Value::Null;
            let r = parse_request(v);
            assert!(
                run_validators(&r).is_ok(),
                "null for `{param}` should be treated as unset"
            );
        }
    }

    #[tokio::test]
    async fn request_level_checked_before_message_level() {
        // Request-level rejections run first; a message-level violation is not
        // reached when a request-level field is also present.
        let mut v = base_request();
        v["max_tokens"] = json!(1);
        v["messages"][0]["role"] = json!("admin");
        assert_rejected(v, "max_tokens").await;
    }

    #[tokio::test]
    async fn max_tokens_beats_temperature() {
        // Both present: the 14 explicit checks run in field order, with
        // `temperature` last — so max_tokens wins.
        let mut v = base_request();
        v["max_tokens"] = json!(1);
        v["temperature"] = json!(0.7);
        assert_rejected(v, "max_tokens").await;
    }

    #[tokio::test]
    async fn second_message_index_reported() {
        // The index in the message text is the 0-based position of the
        // offending message, not always 0.
        let v = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "admin", "content": "second"},
            ],
        });
        let r = parse_request(v);
        let err = run_validators(&r).unwrap_err();
        let (_, body) = response_json(err.into_response()).await;
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("messages[1].role"));
    }

    #[tokio::test]
    async fn first_message_violation_wins_within_message() {
        // Within one message the checks run name → tool_call_id → role →
        // tool_calls, so `name` wins when both name and role are invalid.
        let v = json!({
            "model": "gpt-4o",
            "messages": [{"role": "admin", "content": "hi", "name": "alice"}],
        });
        let r = parse_request(v);
        let err = run_validators(&r).unwrap_err();
        let (_, body) = response_json(err.into_response()).await;
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("messages[0].name"));
        assert!(!msg.contains("messages[0].role"));
    }

    #[test]
    fn deserialization_defaults_and_tolerance() {
        // Missing model -> "", missing stream -> false, missing Options -> None,
        // unknown fields (e.g. a typo'd known field) silently ignored — no
        // deny_unknown_fields.
        let r: ChatCompletionRequest = serde_json::from_value(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "unknown_field": "ignored",
            "max_tokenss": 5,
        }))
        .unwrap();
        assert_eq!(r.model, "");
        assert!(!r.stream);
        assert_eq!(r.messages.len(), 1);
        assert_eq!(r.messages[0].role, "user");
        assert_eq!(r.messages[0].content.as_deref(), Some("hi"));
        assert!(r.messages[0].name.is_none());
        assert!(r.max_tokens.is_none());
        assert!(r.temperature.is_none());
        assert!(r.metadata.is_none());
        assert!(r.store.is_none());
    }

    #[test]
    fn deserialization_full_openai_payload() {
        let r: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hi"},
            ],
            "stream": true,
            "stream_options": {"include_usage": true},
            "metadata": {"k": "v"},
            "store": false,
        }))
        .unwrap();
        assert_eq!(r.model, "gpt-4o");
        assert!(r.stream);
        let so = r.stream_options.expect("stream_options present");
        assert!(so.include_usage);
        assert_eq!(r.metadata.as_ref().unwrap()["k"], "v");
        // store is Option<bool>: explicit false is distinct from unset (None).
        assert_eq!(r.store, Some(false));
    }

    // ── Model → agent routing ─────────────────────────────────────────────

    use zeroclaw_config::schema::AliasedAgentConfig;

    fn config_with_agents(agents: &[(&str, bool)]) -> Config {
        let mut config = Config::default();
        for (alias, enabled) in agents {
            config.agents.insert(
                (*alias).to_string(),
                AliasedAgentConfig {
                    enabled: *enabled,
                    ..Default::default()
                },
            );
        }
        config
    }

    #[test]
    fn default_shorthand_routes_to_default_agent() {
        let config = config_with_agents(&[("default", true)]);
        assert_eq!(
            agent_alias_from_model("", &config),
            Ok("default".to_string())
        );
        assert_eq!(
            agent_alias_from_model("zeroclaw", &config),
            Ok("default".to_string())
        );
        assert_eq!(
            agent_alias_from_model("zeroclaw/default", &config),
            Ok("default".to_string())
        );
        assert_eq!(
            agent_alias_from_model("  zeroclaw  ", &config),
            Ok("default".to_string()),
            "model is trimmed before matching"
        );
    }

    #[test]
    fn default_falls_back_to_lexicographically_smallest_enabled() {
        // No `default` key: empty model resolves via the runtime fallback —
        // lexicographically smallest *enabled* agent (same as webhook chat).
        let config = config_with_agents(&[("research", true), ("coding", true)]);
        assert_eq!(
            agent_alias_from_model("", &config),
            Ok("coding".to_string())
        );
    }

    #[test]
    fn default_no_enabled_agent_is_error() {
        let config = config_with_agents(&[("research", false)]);
        let err = agent_alias_from_model("", &config).unwrap_err();
        assert!(err.contains("no enabled"), "err = {err}");
    }

    #[test]
    fn explicit_alias_strips_prefix() {
        let config = config_with_agents(&[("coding", true)]);
        assert_eq!(
            agent_alias_from_model("zeroclaw/coding", &config),
            Ok("coding".to_string())
        );
        assert_eq!(
            agent_alias_from_model("zeroclaw/coding ", &config),
            Ok("coding".to_string()),
            "alias is trimmed after the prefix"
        );
    }

    #[test]
    fn rejects_non_zeroclaw_prefixes() {
        let config = config_with_agents(&[("coding", true)]);
        for model in ["gpt-4", "zeroclaw:coding", "agent:coding"] {
            let err = agent_alias_from_model(model, &config).unwrap_err();
            assert!(
                err.contains("routes to ZeroClaw agents only"),
                "{model} should be rejected, err = {err}"
            );
        }
    }

    #[test]
    fn rejects_empty_explicit_alias() {
        let config = config_with_agents(&[("coding", true)]);
        for model in ["zeroclaw/", "zeroclaw/   "] {
            let err = agent_alias_from_model(model, &config).unwrap_err();
            assert!(err.contains("missing agent alias"), "{model:?} err = {err}");
        }
    }

    #[tokio::test]
    async fn unknown_alias_400_in_handler() {
        let config = config_with_agents(&[("coding", true)]);
        let err = resolve_agent_alias_from_model("zeroclaw/nope", &config).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "model");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Unknown agent `nope`")
        );
    }

    #[test]
    fn disabled_agent_not_rejected() {
        // ① Explicit alias pointing at a disabled agent resolves fine —
        // existence is the only gate, matching WS `?agent=`.
        let config = config_with_agents(&[("coding", true), ("research", false)]);
        assert_eq!(
            agent_alias_from_model("zeroclaw/research", &config),
            Ok("research".to_string())
        );
        assert_eq!(
            resolve_agent_alias_from_model("zeroclaw/research", &config).unwrap(),
            "research"
        );

        // ② `default` key present but disabled still wins the empty-model
        // resolution (resolved_runtime_agent_alias prefers the literal
        // `default` key without an enabled check — inherited master behaviour).
        let config = config_with_agents(&[("default", false), ("coding", true)]);
        assert_eq!(
            agent_alias_from_model("", &config),
            Ok("default".to_string())
        );
    }

    // ── Chat-completions handler orchestration ───────────────────────────

    use axum::http::header;
    use axum::routing::post;
    use zeroclaw_config::multi_agent::AgentWorkspaceConfig;
    use zeroclaw_config::schema::{
        AnthropicModelProviderConfig, ModelProviderConfig, RiskProfileConfig, RuntimeProfileConfig,
    };
    use zeroclaw_infra::session_backend::SessionBackend;
    use zeroclaw_infra::session_store::SessionStore;

    /// Config with a provider model present (so the 503 check passes) and one
    /// enabled agent wired to an anthropic provider. Rejection-branch tests
    /// use this; full-turn tests override the provider `uri` with a local
    /// fixture.
    fn chat_config(alias: &str) -> Config {
        let mut config = Config::default();
        config.providers.models.anthropic.insert(
            "fixture".to_string(),
            AnthropicModelProviderConfig {
                base: ModelProviderConfig {
                    api_key: Some("test-key".to_string()),
                    // Unreachable; rejection-branch tests never reach the turn.
                    uri: Some("http://127.0.0.1:1".to_string()),
                    model: Some("claude-test".to_string()),
                    ..Default::default()
                },
            },
        );
        config
            .risk_profiles
            .insert("test-profile".to_string(), RiskProfileConfig::default());
        config.agents.insert(
            alias.to_string(),
            AliasedAgentConfig {
                model_provider: "anthropic.fixture".into(),
                risk_profile: "test-profile".into(),
                ..Default::default()
            },
        );
        config
    }

    /// Drive the handler directly (no HTTP server), mirroring the axum
    /// extractor arguments. Returns status, response headers, and the JSON body.
    async fn call_handler(
        state: AppState,
        session_key: Option<&str>,
        body: serde_json::Value,
    ) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
        let mut headers = HeaderMap::new();
        if let Some(k) = session_key {
            headers.insert("x-session-key", k.parse().unwrap());
        }
        let response = handle_chat_completions(
            State(state),
            ConnectInfo("127.0.0.1:8080".parse().unwrap()),
            headers,
            Bytes::from(body.to_string()),
        )
        .await;
        let out_headers = response.headers().clone();
        let (status, json) = response_json(response).await;
        (status, out_headers, json)
    }

    /// Like [`call_handler`] but returns the raw SSE body text (stream mode).
    async fn call_stream(
        state: AppState,
        session_key: Option<&str>,
        body: serde_json::Value,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let mut headers = HeaderMap::new();
        if let Some(k) = session_key {
            headers.insert("x-session-key", k.parse().unwrap());
        }
        let response = handle_chat_completions(
            State(state),
            ConnectInfo("127.0.0.1:8080".parse().unwrap()),
            headers,
            Bytes::from(body.to_string()),
        )
        .await;
        let out_headers = response.headers().clone();
        let status = response.status();
        let text = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        (status, out_headers, text)
    }

    /// Split an SSE body into its `data:` payloads, in order.
    fn sse_data_frames(text: &str) -> Vec<String> {
        text.split("\n\n")
            .filter_map(|block| {
                block
                    .lines()
                    .find_map(|line| line.strip_prefix("data: ").map(str::to_string))
            })
            .collect()
    }

    fn chat_body(model: &str, stream: bool) -> serde_json::Value {
        json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": stream,
        })
    }

    /// In-memory backend recording owner + messages, overriding the default
    /// no-op ownership methods so the handler's two-phase get/set is real.
    #[derive(Default)]
    struct MockBackend {
        messages: std::sync::Mutex<Vec<ChatMessage>>,
        owner: std::sync::Mutex<Option<String>>,
        load_count: std::sync::Mutex<usize>,
    }

    impl SessionBackend for MockBackend {
        fn load(&self, _key: &str) -> Vec<ChatMessage> {
            *self.load_count.lock().unwrap() += 1;
            self.messages.lock().unwrap().clone()
        }
        fn append(&self, _key: &str, msg: &ChatMessage) -> std::io::Result<()> {
            self.messages.lock().unwrap().push(msg.clone());
            Ok(())
        }
        fn remove_last(&self, _key: &str) -> std::io::Result<bool> {
            Ok(false)
        }
        fn list_sessions(&self) -> Vec<String> {
            vec![]
        }
        fn session_exists(&self, _session_key: &str) -> bool {
            // Override the default (which routes through `load`) so the
            // turn-persist existence check does not pollute `load_count`:
            // that counter observes only the handler's explicit history load.
            !self.messages.lock().unwrap().is_empty()
        }
        fn get_session_agent_alias(&self, _key: &str) -> std::io::Result<Option<String>> {
            Ok(self.owner.lock().unwrap().clone())
        }
        fn set_session_agent_alias(&self, _key: &str, alias: &str) -> std::io::Result<()> {
            *self.owner.lock().unwrap() = Some(alias.to_string());
            Ok(())
        }
        fn claim_session_agent_alias(
            &self,
            _key: &str,
            alias: &str,
        ) -> std::io::Result<ClaimOutcome> {
            let mut owner = self.owner.lock().unwrap();
            match owner.as_deref() {
                None => {
                    *owner = Some(alias.to_string());
                    Ok(ClaimOutcome::Claimed)
                }
                Some(existing) if existing == alias => Ok(ClaimOutcome::Claimed),
                Some(existing) => Ok(ClaimOutcome::Conflict(existing.to_string())),
            }
        }
    }

    // ── Session-key canonicality (pure) ───────────────────────────────────

    #[test]
    fn canonical_key_gate() {
        assert!(is_http_canonical_session_key("gw_abc"));
        assert!(is_http_canonical_session_key("gw_a-b_c-123"));
        assert!(!is_http_canonical_session_key(""));
        assert!(!is_http_canonical_session_key("gw_Abc")); // uppercase
        assert!(!is_http_canonical_session_key("gw_a b")); // space
        assert!(!is_http_canonical_session_key("gw_!")); // punctuation
        assert!(!is_http_canonical_session_key("gw_中文"));
    }

    // ── split_messages (pure) ─────────────────────────────────────────────

    fn msgs_from(v: serde_json::Value) -> Vec<ChatCompletionMessage> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn split_messages_prefix_and_history() {
        let msgs = msgs_from(json!([
            {"role": "system", "content": "be brief"},
            {"role": "developer", "content": "strict"},
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "reply"},
            {"role": "user", "content": "second"},
        ]));
        let (history, current) = split_messages(&msgs);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "first");
        assert_eq!(history[1].role, "assistant");
        assert_eq!(history[1].content, "reply");
        assert_eq!(current, "be brief\n\nstrict\n\nsecond");
    }

    #[test]
    fn split_messages_no_user_message() {
        let msgs = msgs_from(json!([
            {"role": "assistant", "content": "a1"},
            {"role": "assistant", "content": "a2"},
        ]));
        let (history, current) = split_messages(&msgs);
        assert!(history.is_empty());
        assert_eq!(current, "a1\n\na2");
    }

    #[test]
    fn split_messages_blank_system_prefix_skipped() {
        let msgs = msgs_from(json!([
            {"role": "system", "content": "   "},
            {"role": "user", "content": "hi"},
        ]));
        let (history, current) = split_messages(&msgs);
        assert!(history.is_empty());
        assert_eq!(current, "hi");
    }

    // ── chunk / usage helpers (pure) ──────────────────────────────────────

    #[test]
    fn chunk_json_only_present_fields() {
        let chunk = chunk_json(
            "id",
            1,
            "m",
            Some("assistant"),
            Some(String::new()),
            None,
            None,
        );
        assert_eq!(chunk["object"], "chat.completion.chunk");
        assert_eq!(chunk["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(chunk["choices"][0]["delta"]["content"], "");
        assert!(chunk["choices"][0]["delta"].get("tool_calls").is_none());
        assert!(chunk["choices"][0]["finish_reason"].is_null());
        assert!(chunk["choices"][0]["logprobs"].is_null());

        let finish = chunk_json("id", 1, "m", None, None, None, Some("stop"));
        assert_eq!(finish["choices"][0]["finish_reason"], "stop");
        assert!(finish["choices"][0]["delta"].get("role").is_none());
        assert!(finish["choices"][0]["delta"].get("content").is_none());
        assert_eq!(finish["choices"][0]["delta"], json!({}));
    }

    #[test]
    fn completion_id_format() {
        let id = generate_completion_id();
        assert!(id.starts_with("chatcmpl-"));
        assert_eq!(id.len(), "chatcmpl-".len() + 36); // UUID v4
    }

    // ── Handler rejection branches ────────────────────────────────────────

    #[tokio::test]
    async fn rejects_noncanonical_session_key() {
        let state = test_state(chat_config("test-agent"));
        // Space and `!` fall outside the ASCII `[a-z0-9_-]` canonical gate.
        let (status, headers, body) = call_handler(
            state,
            Some("bad key!"),
            chat_body("zeroclaw/test-agent", false),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "x-session-key");
        // Step-5-before errors do not echo the key.
        assert!(headers.get("x-session-key").is_none());
    }

    #[tokio::test]
    async fn rejects_missing_gw_prefix() {
        let state = test_state(chat_config("test-agent"));
        let (status, _, body) = call_handler(
            state,
            Some("abc123"),
            chat_body("zeroclaw/test-agent", false),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "x-session-key");
        assert!(body["error"]["message"].as_str().unwrap().contains("gw_"));
    }

    #[tokio::test]
    async fn rejects_invalid_json_body() {
        let state = test_state(chat_config("test-agent"));
        let response = handle_chat_completions(
            State(state),
            ConnectInfo("127.0.0.1:8080".parse().unwrap()),
            HeaderMap::new(),
            Bytes::from("not json"),
        )
        .await;
        let (status, body) = response_json(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "messages");
    }

    #[tokio::test]
    async fn unknown_alias_400_no_echo() {
        let state = test_state(chat_config("test-agent"));
        let (status, headers, body) =
            call_handler(state, Some("gw_x"), chat_body("zeroclaw/nope", false)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "model");
        assert!(headers.get("x-session-key").is_none());
    }

    #[tokio::test]
    async fn unconfigured_provider_503() {
        // config_with_agents gives the agent no `model_provider` at all, so
        // resolution yields None -> 503 (step 4), before the session key.
        let state = test_state(config_with_agents(&[("coding", true)]));
        let (status, headers, body) =
            call_handler(state, Some("gw_x"), chat_body("zeroclaw/coding", false)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["type"], "internal_error");
        assert_eq!(body["error"]["param"], "model");
        assert!(headers.get("x-session-key").is_none());
    }

    #[tokio::test]
    async fn rejects_blank_last_user_message() {
        let state = test_state(chat_config("test-agent"));
        let mut body = chat_body("zeroclaw/test-agent", false);
        body["messages"][0]["content"] = json!("   ");
        let (status, _, out) = call_handler(state, None, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(out["error"]["type"], "invalid_request_error");
        assert!(out["error"]["message"].as_str().unwrap().contains("blank"));
    }

    #[tokio::test]
    async fn session_busy_429() {
        let state = test_state(chat_config("test-agent"));
        let key = "gw_busy_session";
        let mut headers = HeaderMap::new();
        headers.insert("x-session-key", key.parse().unwrap());

        // Hold the lock and fill the queue (max depth 8) so the handler's
        // acquire observes `current >= max_queue_depth`.
        let _held = state
            .session_queue
            .acquire(key)
            .await
            .expect("first acquire");
        let mut waiters = Vec::new();
        for _ in 0..7 {
            let queue = Arc::clone(&state.session_queue);
            waiters.push(zeroclaw_spawn::spawn!(async move {
                let _ = queue.acquire(key).await;
            }));
        }
        for _ in 0..200 {
            if state.session_queue.queue_depth(key).await >= 8 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(state.session_queue.queue_depth(key).await, 8);

        let (status, out_headers, body) =
            call_handler(state, Some(key), chat_body("zeroclaw/test-agent", false)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(out_headers["x-session-key"].to_str().unwrap(), key);
        let _ = _held;
    }

    #[tokio::test]
    async fn ws_owns_session_http_409() {
        let state = test_state(chat_config("test-agent"));
        let key = "gw_ws_held";
        state
            .ws_connections
            .lock()
            .unwrap()
            .insert(key.to_string(), 1);
        let (status, headers, body) =
            call_handler(state, Some(key), chat_body("zeroclaw/test-agent", false)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["type"], "cross_transport_session_in_use");
        assert_eq!(body["error"]["param"], "x-session-key");
        assert_eq!(headers["x-session-key"].to_str().unwrap(), key);
    }

    #[tokio::test]
    async fn rejects_reuse_key_different_agent() {
        let backend = Arc::new(MockBackend {
            messages: std::sync::Mutex::new(vec![]),
            owner: std::sync::Mutex::new(Some("other-agent".to_string())),
            load_count: std::sync::Mutex::new(0),
        });
        let mut state = test_state(chat_config("test-agent"));
        state.session_backend = Some(backend);
        let (status, headers, body) = call_handler(
            state,
            Some("gw_reuse"),
            chat_body("zeroclaw/test-agent", false),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "x-session-key");
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains("other-agent"), "msg = {msg}");
        assert!(msg.contains("test-agent"), "msg = {msg}");
        assert_eq!(headers["x-session-key"].to_str().unwrap(), "gw_reuse");
    }

    #[tokio::test]
    async fn accepts_reuse_key_same_agent() {
        let backend = Arc::new(MockBackend {
            messages: std::sync::Mutex::new(vec![]),
            owner: std::sync::Mutex::new(Some("test-agent".to_string())),
            load_count: std::sync::Mutex::new(0),
        });
        let mut state = test_state(chat_config("test-agent"));
        state.session_backend = Some(backend);
        // Single-message + key -> State A loads the (empty) backend, owner
        // matches, so no rejection; the turn is dispatched.
        let (status, headers, body) = call_handler(
            state,
            Some("gw_same"),
            chat_body("zeroclaw/test-agent", false),
        )
        .await;
        // The agent turn fails because `uri` is unreachable (127.0.0.1:1), but
        // the important part is that ownership did NOT reject — a 500 from the
        // turn, not a 400 ownership error.
        assert_ne!(status, StatusCode::BAD_REQUEST);
        assert_eq!(headers["x-session-key"].to_str().unwrap(), "gw_same");
        let _ = body;
    }

    #[tokio::test]
    async fn rejects_nonempty_session_on_backend_without_ownership() {
        // A backend that cannot track ownership (JSONL `SessionStore`; its
        // `claim` returns `Err(Unsupported)`) fails closed on a non-empty
        // session: the reuse cannot be attributed to this caller, so it is
        // refused with a precise error instead of a misleading backfill hint.
        let tmp = tempfile::tempdir().unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(SessionStore::new(tmp.path()).unwrap());
        backend
            .append("gw_legacy", &ChatMessage::assistant("existing"))
            .unwrap();
        let mut state = test_state(chat_config("test-agent"));
        state.session_backend = Some(backend);
        let (status, headers, body) = call_handler(
            state,
            Some("gw_legacy"),
            chat_body("zeroclaw/test-agent", false),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("ownership")
        );
        assert_eq!(headers["x-session-key"].to_str().unwrap(), "gw_legacy");
    }

    #[tokio::test]
    async fn allows_fresh_session_on_backend_without_ownership() {
        // Same backend, fresh (empty) session: claim is `Unsupported` but
        // there is nothing to attribute, so the request proceeds instead of
        // failing closed on a phantom non-empty history.
        let tmp = tempfile::tempdir().unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let mut state = test_state(chat_config("test-agent"));
        state.session_backend = Some(backend);
        // Single message + fresh key -> State A loads an empty backend, claim
        // degrades, and the turn is dispatched (fails on the unreachable
        // fixture URI, not on ownership).
        let (status, headers, _body) = call_handler(
            state,
            Some("gw_fresh"),
            chat_body("zeroclaw/test-agent", false),
        )
        .await;
        assert_ne!(status, StatusCode::BAD_REQUEST);
        assert_eq!(headers["x-session-key"].to_str().unwrap(), "gw_fresh");
    }

    // ── Full-turn streaming + blocking ───────────────────────────────────

    async fn serve_fixture(
        router: axum::Router,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local provider fixture");
        let addr = listener.local_addr().expect("fixture address");
        let handle = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, router).await.expect("fixture serves");
        });
        (addr, handle)
    }

    /// Anthropic-shaped provider fixture: SSE for stream=true, JSON otherwise.
    fn anthropic_fixture(stream_body: String, json_body: serde_json::Value) -> axum::Router {
        axum::Router::new().route(
            "/v1/messages",
            post(move |Json(request): Json<serde_json::Value>| async move {
                if request["stream"].as_bool() == Some(true) {
                    ([(header::CONTENT_TYPE, "text/event-stream")], stream_body).into_response()
                } else {
                    Json(json_body).into_response()
                }
            }),
        )
    }

    const OK_STREAM: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":1}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";

    const OK_JSON: &str = r#"{"id":"msg_test","type":"message","role":"assistant","model":"claude-test","content":[{"type":"text","text":"Hello"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":5}}"#;

    /// Config for a full turn against a local Anthropic-shaped mock at
    /// `mock_uri`. Mirrors the WebSocket fixture's workspace/runtime-profile
    /// wiring so `Agent` construction and the turn succeed.
    fn chat_turn_config(mock_uri: &str, timeout_secs: u64) -> (Config, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("temporary gateway workspace");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("gateway workspace");
        let mut config = Config {
            data_dir: workspace.clone(),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.memory.backend = "none".to_string();
        config.reliability.provider_retries = 0;
        config.gateway.long_running_request_timeout_secs = timeout_secs;
        config.providers.models.anthropic.insert(
            "fixture".to_string(),
            AnthropicModelProviderConfig {
                base: ModelProviderConfig {
                    api_key: Some("test-key".to_string()),
                    uri: Some(mock_uri.to_string()),
                    model: Some("claude-test".to_string()),
                    ..Default::default()
                },
            },
        );
        config
            .risk_profiles
            .insert("fixture".to_string(), RiskProfileConfig::default());
        config
            .runtime_profiles
            .insert("fixture".to_string(), RuntimeProfileConfig::default());
        config.agents.insert(
            "test-agent".to_string(),
            AliasedAgentConfig {
                model_provider: "anthropic.fixture".into(),
                risk_profile: "fixture".into(),
                runtime_profile: "fixture".into(),
                workspace: AgentWorkspaceConfig {
                    path: Some(workspace),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        (config, tmp)
    }

    fn hang_router() -> axum::Router {
        axum::Router::new().route(
            "/v1/messages",
            post(|| async { std::future::pending::<axum::response::Response>().await }),
        )
    }

    fn failing_router() -> axum::Router {
        axum::Router::new().route(
            "/v1/messages",
            post(|Json(_): Json<serde_json::Value>| async {
                (StatusCode::INTERNAL_SERVER_ERROR, "provider boom").into_response()
            }),
        )
    }

    fn ok_json_body() -> serde_json::Value {
        serde_json::from_str(OK_JSON).unwrap()
    }

    #[tokio::test]
    async fn stream_success_invariants() {
        let (addr, server) =
            serve_fixture(anthropic_fixture(OK_STREAM.to_string(), ok_json_body())).await;
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 10);
        let state = test_state(config);
        let (status, headers, sse) =
            call_stream(state, None, chat_body("zeroclaw/test-agent", true)).await;
        server.abort();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers["content-type"].to_str().unwrap(),
            "text/event-stream"
        );
        assert!(
            headers["x-session-key"]
                .to_str()
                .unwrap()
                .starts_with("gw_"),
            "a fresh key is generated and echoed"
        );

        let frames = sse_data_frames(&sse);
        assert!(
            frames.len() >= 3,
            "role chunk + delta + finish + [DONE]: {frames:?}"
        );

        // First chunk carries the role with empty content.
        let first: serde_json::Value = serde_json::from_str(&frames[0]).unwrap();
        assert_eq!(first["object"], "chat.completion.chunk");
        assert_eq!(first["model"], "zeroclaw/test-agent");
        assert_eq!(first["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(first["choices"][0]["delta"]["content"], "");

        // Content deltas concatenate to the model's text.
        let content: String = frames
            .iter()
            .filter_map(|f| serde_json::from_str::<serde_json::Value>(f).ok())
            .filter_map(|v| {
                v["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(content, "Hello");

        // Terminal finish chunk then the [DONE] sentinel.
        assert_eq!(frames[frames.len() - 1], "[DONE]");
        let finish: serde_json::Value = serde_json::from_str(&frames[frames.len() - 2]).unwrap();
        assert_eq!(finish["choices"][0]["finish_reason"], "stop");
        assert_eq!(finish["choices"][0]["delta"], json!({}));
    }

    #[tokio::test]
    async fn stream_usage_only_when_include_usage() {
        let (addr, server) =
            serve_fixture(anthropic_fixture(OK_STREAM.to_string(), ok_json_body())).await;
        let uri = format!("http://{addr}");

        let has_usage_chunk = |sse: &str| {
            sse_data_frames(sse).iter().any(|f| {
                serde_json::from_str::<serde_json::Value>(f)
                    .ok()
                    .is_some_and(|v| v["choices"].as_array().is_some_and(|c| c.is_empty()))
            })
        };

        // include_usage=true -> one usage chunk with the aggregated counts.
        let mut with_usage = chat_body("zeroclaw/test-agent", true);
        with_usage["stream_options"] = json!({"include_usage": true});
        let (config, _tmp) = chat_turn_config(&uri, 10);
        let (_, _, sse) = call_stream(test_state(config), None, with_usage).await;
        assert!(has_usage_chunk(&sse), "usage chunk missing: {sse}");
        let usage_frame = sse_data_frames(&sse)
            .into_iter()
            .find_map(|f| {
                let v: serde_json::Value = serde_json::from_str(&f).ok()?;
                (v["choices"].as_array().is_some_and(|c| c.is_empty())).then_some(v)
            })
            .unwrap();
        assert_eq!(usage_frame["usage"]["prompt_tokens"], 1);
        assert_eq!(usage_frame["usage"]["completion_tokens"], 5);

        // include_usage unset (default) -> no usage chunk.
        let (config2, _tmp2) = chat_turn_config(&uri, 10);
        let (_, _, sse2) = call_stream(
            test_state(config2),
            None,
            chat_body("zeroclaw/test-agent", true),
        )
        .await;
        assert!(
            !has_usage_chunk(&sse2),
            "usage chunk must be absent by default"
        );

        server.abort();
    }

    #[tokio::test]
    async fn stream_suppresses_thinking_text() {
        const THINKING_STREAM: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_t\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":1}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"secret plan\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"more reasoning\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":1}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";
        let (addr, server) = serve_fixture(anthropic_fixture(
            THINKING_STREAM.to_string(),
            ok_json_body(),
        ))
        .await;
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 10);
        let (_, _, sse) = call_stream(
            test_state(config),
            None,
            chat_body("zeroclaw/test-agent", true),
        )
        .await;
        server.abort();

        assert!(
            !sse.contains("secret plan") && !sse.contains("more reasoning"),
            "thinking text must never reach the SSE stream: {sse}"
        );
        let content: String = sse_data_frames(&sse)
            .iter()
            .filter_map(|f| {
                serde_json::from_str::<serde_json::Value>(f)
                    .ok()
                    .and_then(|v| {
                        v["choices"][0]["delta"]["content"]
                            .as_str()
                            .map(str::to_string)
                    })
            })
            .collect();
        assert_eq!(content, "Hello");
    }

    #[tokio::test]
    async fn stream_timeout_emits_error_event() {
        let (addr, server) = serve_fixture(hang_router()).await;
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 1);
        let (status, _, sse) = call_stream(
            test_state(config),
            None,
            chat_body("zeroclaw/test-agent", true),
        )
        .await;
        server.abort();

        // The SSE response starts immediately; the deadline is signalled in-band.
        assert_eq!(status, StatusCode::OK);
        let frames = sse_data_frames(&sse);
        let error_frame = frames
            .iter()
            .find(|f| f.contains("\"error\""))
            .unwrap_or_else(|| panic!("timeout error envelope missing: {frames:?}"));
        let v: serde_json::Value = serde_json::from_str(error_frame).unwrap();
        assert_eq!(v["error"]["type"], "timeout");
        assert_eq!(v["error"]["status"], 408);
        assert_eq!(frames[frames.len() - 1], "[DONE]");
    }

    #[tokio::test]
    async fn stream_error_emits_internal_error() {
        let (addr, server) = serve_fixture(failing_router()).await;
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 10);
        let (status, _, sse) = call_stream(
            test_state(config),
            None,
            chat_body("zeroclaw/test-agent", true),
        )
        .await;
        server.abort();

        assert_eq!(status, StatusCode::OK);
        let frames = sse_data_frames(&sse);
        let error_frame = frames
            .iter()
            .find(|f| f.contains("\"error\""))
            .unwrap_or_else(|| panic!("internal_error envelope missing: {frames:?}"));
        let v: serde_json::Value = serde_json::from_str(error_frame).unwrap();
        assert_eq!(v["error"]["type"], "internal_error");
        assert_eq!(v["error"]["status"], 500);
        assert_eq!(frames[frames.len() - 1], "[DONE]");
    }

    #[tokio::test]
    async fn blocking_success_response() {
        let (addr, server) =
            serve_fixture(anthropic_fixture(OK_STREAM.to_string(), ok_json_body())).await;
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 10);
        let (status, headers, body) = call_handler(
            test_state(config),
            None,
            chat_body("zeroclaw/test-agent", false),
        )
        .await;
        server.abort();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["choices"][0]["message"]["role"], "assistant");
        assert_eq!(body["choices"][0]["message"]["content"], "Hello");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert!(
            body["choices"][0]["message"].get("tool_calls").is_none(),
            "blocking responses never carry tool_calls: {body}"
        );
        assert_eq!(body["usage"]["prompt_tokens"], 1);
        assert_eq!(body["usage"]["completion_tokens"], 5);
        assert!(
            headers["x-session-key"]
                .to_str()
                .unwrap()
                .starts_with("gw_")
        );
    }

    #[tokio::test]
    async fn blocking_timeout_408() {
        let (addr, server) = serve_fixture(hang_router()).await;
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 1);
        let (status, _, body) = call_handler(
            test_state(config),
            None,
            chat_body("zeroclaw/test-agent", false),
        )
        .await;
        server.abort();
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(body["error"]["type"], "timeout");
        assert_eq!(body["error"]["status"], 408);
    }

    #[tokio::test]
    async fn blocking_error_500() {
        let (addr, server) = serve_fixture(failing_router()).await;
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 10);
        let (status, _, body) = call_handler(
            test_state(config),
            None,
            chat_body("zeroclaw/test-agent", false),
        )
        .await;
        server.abort();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["type"], "internal_error");
        assert_eq!(body["error"]["status"], 500);
    }

    #[tokio::test]
    async fn fresh_key_sets_ownership_and_echoes() {
        let (addr, server) =
            serve_fixture(anthropic_fixture(OK_STREAM.to_string(), ok_json_body())).await;
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 10);
        let backend = Arc::new(MockBackend {
            messages: std::sync::Mutex::new(vec![]),
            owner: std::sync::Mutex::new(None),
            load_count: std::sync::Mutex::new(0),
        });
        let state_backend: Arc<dyn SessionBackend> = backend.clone();
        let mut state = test_state(config);
        state.session_backend = Some(state_backend);
        let (status, headers, _) =
            call_handler(state, None, chat_body("zeroclaw/test-agent", false)).await;
        server.abort();

        assert_eq!(status, StatusCode::OK);
        let key = headers["x-session-key"].to_str().unwrap().to_string();
        assert!(key.starts_with("gw_"), "fresh key echoed: {key}");
        assert_eq!(
            backend.owner.lock().unwrap().as_deref(),
            Some("test-agent"),
            "a fresh session claims its owning agent"
        );
    }

    // ── Session-key hardening (post-review) ──────────────────────────────

    #[tokio::test]
    async fn rejects_non_utf8_session_key() {
        let state = test_state(chat_config("test-agent"));
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-session-key",
            HeaderValue::from_bytes(&[0xFF, 0xFE]).unwrap(),
        );
        let response = handle_chat_completions(
            State(state),
            ConnectInfo("127.0.0.1:8080".parse().unwrap()),
            headers,
            Bytes::from(chat_body("zeroclaw/test-agent", false).to_string()),
        )
        .await;
        let (status, body) = response_json(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "x-session-key");
    }

    #[test]
    fn http_memory_scope_matches_ws() {
        // The WebSocket transport scopes memory by the raw session id (no
        // `gw_` prefix); HTTP strips the prefix so both land in the same
        // bucket for a shared session.
        let ws_session_id = "my-session-1";
        let http_key = format!("gw_{ws_session_id}");
        assert_eq!(
            http_memory_scope(&http_key),
            sanitize_session_key(ws_session_id)
        );
        // A key without the prefix degrades to the raw value.
        assert_eq!(
            http_memory_scope("naked-key"),
            sanitize_session_key("naked-key")
        );
        // Non-ASCII is sanitized identically on both sides.
        assert_eq!(
            http_memory_scope("gw_slack_C1_user one"),
            sanitize_session_key("slack_C1_user one")
        );
    }

    // ── History-precedence three states ──────────────────────────────────

    /// Fixture that records every request body so tests can assert what the
    /// agent actually sent (e.g. whether backend history was seeded).
    fn recording_fixture(
        stream_body: String,
        json_body: serde_json::Value,
        requests: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) -> axum::Router {
        axum::Router::new().route(
            "/v1/messages",
            post(move |Json(request): Json<serde_json::Value>| async move {
                requests.lock().unwrap().push(request.clone());
                if request["stream"].as_bool() == Some(true) {
                    ([(header::CONTENT_TYPE, "text/event-stream")], stream_body).into_response()
                } else {
                    Json(json_body).into_response()
                }
            }),
        )
    }

    fn request_message_contents(
        requests: &Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) -> Vec<String> {
        let guard = requests.lock().unwrap();
        guard
            .first()
            .map(|req| {
                req["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| {
                        let c = &m["content"];
                        if let Some(s) = c.as_str() {
                            Some(s.to_string())
                        } else if let Some(arr) = c.as_array() {
                            // Anthropic-shaped body: `content` is a block array
                            // (`[{type:"text",text:"..."}]`), not a bare string.
                            Some(
                                arr.iter()
                                    .filter_map(|b| b["text"].as_str())
                                    .collect::<Vec<_>>()
                                    .join(""),
                            )
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn loads_backend_history_single_message() {
        // State A: one message + key -> load the backend transcript and seed it.
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (addr, server) = serve_fixture(recording_fixture(
            OK_STREAM.to_string(),
            ok_json_body(),
            Arc::clone(&requests),
        ))
        .await;

        let backend = Arc::new(MockBackend {
            messages: std::sync::Mutex::new(vec![
                ChatMessage::user("earlier"),
                ChatMessage::assistant("existing"),
            ]),
            owner: std::sync::Mutex::new(Some("test-agent".to_string())),
            load_count: std::sync::Mutex::new(0),
        });
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 10);
        let mut state = test_state(config);
        state.session_backend = Some(Arc::clone(&backend) as Arc<dyn SessionBackend>);
        let (status, _, _) =
            call_handler(state, Some("gw_a"), chat_body("zeroclaw/test-agent", false)).await;
        server.abort();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            *backend.load_count.lock().unwrap(),
            1,
            "single message + key must load the backend transcript"
        );
        let contents = request_message_contents(&requests);
        assert!(
            contents.iter().any(|c| c.contains("existing")),
            "backend assistant reply must be seeded into the turn: {contents:?}"
        );
        assert!(
            contents.iter().any(|c| c.contains("earlier")),
            "backend user turn must be seeded into the turn: {contents:?}"
        );
        assert!(
            contents.iter().any(|c| c.contains("hello")),
            "the current user message must be present: {contents:?}"
        );
    }

    #[tokio::test]
    async fn authoritative_multi_message_skips_load() {
        // State B: more than one message -> the request is authoritative; no
        // backend load, the request's own history is seeded instead.
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (addr, server) = serve_fixture(recording_fixture(
            OK_STREAM.to_string(),
            ok_json_body(),
            Arc::clone(&requests),
        ))
        .await;

        let backend = Arc::new(MockBackend {
            messages: std::sync::Mutex::new(vec![
                ChatMessage::user("earlier"),
                ChatMessage::assistant("existing"),
            ]),
            owner: std::sync::Mutex::new(Some("test-agent".to_string())),
            load_count: std::sync::Mutex::new(0),
        });
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 10);
        let mut state = test_state(config);
        state.session_backend = Some(Arc::clone(&backend) as Arc<dyn SessionBackend>);

        let mut body = chat_body("zeroclaw/test-agent", false);
        body["messages"] = json!([
            {"role": "user", "content": "q1"},
            {"role": "user", "content": "hello"},
        ]);
        let (status, _, _) = call_handler(state, Some("gw_b"), body).await;
        server.abort();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            *backend.load_count.lock().unwrap(),
            0,
            ">1 message means the request is authoritative; backend must not load"
        );
        let contents = request_message_contents(&requests);
        assert!(
            contents.iter().any(|c| c.contains("q1")),
            "request-provided history must be seeded: {contents:?}"
        );
        assert!(
            !contents
                .iter()
                .any(|c| c.contains("existing") || c.contains("earlier")),
            "backend history must NOT leak into an authoritative request: {contents:?}"
        );
    }

    #[tokio::test]
    async fn new_key_skips_load() {
        // State C: no key -> fresh `gw_{uuid}`; nothing to load.
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (addr, server) = serve_fixture(recording_fixture(
            OK_STREAM.to_string(),
            ok_json_body(),
            Arc::clone(&requests),
        ))
        .await;

        let backend = Arc::new(MockBackend {
            messages: std::sync::Mutex::new(vec![
                ChatMessage::user("earlier"),
                ChatMessage::assistant("existing"),
            ]),
            owner: std::sync::Mutex::new(None),
            load_count: std::sync::Mutex::new(0),
        });
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 10);
        let mut state = test_state(config);
        state.session_backend = Some(Arc::clone(&backend) as Arc<dyn SessionBackend>);
        let (status, _, _) =
            call_handler(state, None, chat_body("zeroclaw/test-agent", false)).await;
        server.abort();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            *backend.load_count.lock().unwrap(),
            0,
            "a fresh key has no backend transcript to load"
        );
        let contents = request_message_contents(&requests);
        assert!(
            !contents
                .iter()
                .any(|c| c.contains("existing") || c.contains("earlier")),
            "backend history must not leak into a fresh session: {contents:?}"
        );
    }

    #[tokio::test]
    async fn stream_suppresses_tool_call_chunks() {
        // A provider stream carrying a tool_use block must never surface a
        // `tool_calls` chunk on the HTTP transport: tool execution is
        // transparent and the transport suppresses every non-usage/non-text
        // event.
        const TOOL_STREAM: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_t\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":1}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"web_search\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\\\"rust\\\"}\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":1}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n";
        let (addr, server) =
            serve_fixture(anthropic_fixture(TOOL_STREAM.to_string(), ok_json_body())).await;
        let (config, _tmp) = chat_turn_config(&format!("http://{addr}"), 10);
        let (_, _, sse) = call_stream(
            test_state(config),
            None,
            chat_body("zeroclaw/test-agent", true),
        )
        .await;
        server.abort();

        let frames = sse_data_frames(&sse);
        assert!(
            frames.iter().all(|f| !f.contains("\"tool_calls\"")),
            "tool call activity must never serialize to a chunk: {frames:?}"
        );
        assert!(
            !sse.contains("web_search") && !sse.contains("rust"),
            "tool names/arguments must not leak into the stream: {sse}"
        );
        assert_eq!(frames[frames.len() - 1], "[DONE]");
    }

    // ── Tool whitelist + tool_choice ───────────────────────────────

    #[tokio::test]
    async fn rejects_tool_choice_required() {
        let mut v = base_request();
        v["tool_choice"] = json!("required");
        assert_rejected(v, "tool_choice").await;
    }

    #[tokio::test]
    async fn rejects_tool_choice_named_function() {
        // A specific-function object is a recognized-but-unsupported shape: it
        // gets `unsupported_parameter` (parameter supported, value not),
        // unlike malformed shapes which stay `invalid_request_error`.
        let mut v = base_request();
        v["tool_choice"] = json!({"type": "function", "function": {"name": "web_search"}});
        let r = parse_request(v);
        let err = run_validators(&r).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "unsupported_parameter");
        assert_eq!(body["error"]["param"], "tool_choice");
    }

    #[tokio::test]
    async fn rejects_tool_choice_malformed() {
        for val in [json!(1), json!([1]), json!(true)] {
            let mut v = base_request();
            v["tool_choice"] = val;
            assert_rejected(v, "tool_choice").await;
        }
    }

    #[tokio::test]
    async fn tool_choice_null_means_absent() {
        // `null` deserializes to `None`, identical to an absent field: the
        // default `auto` mode with the full agent tool set.
        let mut v = base_request();
        v["tool_choice"] = json!(null);
        let r = parse_request(v);
        assert!(r.tool_choice.is_none(), "null must behave as absent");
        run_validators(&r).expect("null tool_choice must pass the shape gate");
    }

    #[tokio::test]
    async fn accepts_tool_choice_auto_and_none() {
        for val in [json!("auto"), json!("none")] {
            let mut v = base_request();
            v["tool_choice"] = val;
            let r = parse_request(v);
            run_validators(&r).expect("auto/none must pass the shape gate");
        }
    }

    #[tokio::test]
    async fn rejects_tool_kind_non_function() {
        let mut v = base_request();
        v["tools"] = json!([{"type": "web_search", "function": {"name": "x"}}]);
        assert_rejected(v, "tools").await;
    }

    #[tokio::test]
    async fn rejects_tool_with_empty_name() {
        let mut v = base_request();
        v["tools"] = json!([{"type": "function", "function": {"name": "  "}}]);
        assert_rejected(v, "tools").await;
    }

    fn configured_tools() -> HashMap<String, ToolSpec> {
        let mut map = HashMap::new();
        for (name, desc) in [("alpha", "Alpha does A"), ("beta", "Beta does B")] {
            map.insert(
                name.to_string(),
                ToolSpec::new(name, desc, json!({"type": "object"})),
            );
        }
        map
    }

    fn tool_requests(names: &[&str]) -> Vec<ChatCompletionTool> {
        names
            .iter()
            .map(|n| ChatCompletionTool {
                kind: "function".into(),
                function: ToolFunction {
                    name: n.to_string(),
                    // Client-supplied schema deliberately differs from the
                    // authoritative spec so `authoritative_specs_for` emits a
                    // WARN audit and still returns the server spec.
                    description: Some("client description".into()),
                    parameters: json!({"type": "object", "properties": {"x": {"type": "string"}}}),
                },
            })
            .collect()
    }

    #[test]
    fn parse_tool_choice_auto_and_none() {
        assert_eq!(parse_tool_choice(&None), ToolChoiceMode::Auto);
        assert_eq!(
            parse_tool_choice(&Some(json!("auto"))),
            ToolChoiceMode::Auto
        );
        assert_eq!(
            parse_tool_choice(&Some(json!("none"))),
            ToolChoiceMode::None
        );
    }

    #[test]
    fn tool_choice_none_returns_empty_override() {
        let specs = resolve_tool_specs(&Some(json!("none")), &None, &configured_tools())
            .expect("none must resolve")
            .expect("none must yield an override");
        assert!(specs.is_empty(), "tool_choice=none disables every tool");
    }

    #[test]
    fn tool_choice_auto_without_tools_returns_none() {
        let specs = resolve_tool_specs(&None, &None, &configured_tools())
            .expect("auto + no tools must resolve");
        assert!(specs.is_none(), "default tool set, not an override");
    }

    #[tokio::test]
    async fn empty_tools_400() {
        let err = resolve_tool_specs(&None, &Some(Vec::new()), &configured_tools()).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["param"], "tools");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("must not be empty")
        );
    }

    #[tokio::test]
    async fn unknown_tool_400() {
        let tools = Some(tool_requests(&["nope"]));
        let err = resolve_tool_specs(&None, &tools, &configured_tools()).unwrap_err();
        let (status, body) = response_json(err.into_response()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["param"], "tools");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Unknown tool(s): nope")
        );
    }

    #[test]
    fn authoritative_spec_wins_and_preserves_request_order() {
        // Client asks [beta, alpha] with forged schemas; the response must
        // come back in the same order but with the authoritative specs.
        let tools = Some(tool_requests(&["beta", "alpha"]));
        let specs = resolve_tool_specs(&None, &tools, &configured_tools())
            .expect("known names must resolve")
            .expect("allow-list override");
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["beta", "alpha"], "request order preserved");
        assert_eq!(specs[0].description, "Beta does B");
        assert_eq!(specs[0].parameters.as_ref(), &json!({"type": "object"}));
    }

    #[test]
    fn name_collision_selects_server_spec() {
        let tools = Some(tool_requests(&["alpha"]));
        let specs = resolve_tool_specs(&None, &tools, &configured_tools())
            .expect("known name must resolve")
            .expect("override");
        assert_eq!(
            specs[0].description, "Alpha does A",
            "server spec wins over the client-supplied description"
        );
        assert_eq!(
            specs[0].parameters.as_ref(),
            &json!({"type": "object"}),
            "server schema wins over the client-supplied schema"
        );
    }

    // ── Router mount + 413 rewrite ──────────────────────────────────────

    #[tokio::test]
    async fn chat_completions_route_absent_when_disabled() {
        // A standalone disabled router carries no route; a merged-in empty
        // router yields the gateway's existing 405 for unknown POST paths.
        let router = build_chat_completions_router(test_state(Config::default()), false);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn chat_completions_route_mounted_when_enabled() {
        let router = build_chat_completions_router(test_state(Config::default()), true);
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    // The handler's `ConnectInfo` extractor requires the
                    // extension; axum's `oneshot` does not populate it.
                    .extension(ConnectInfo(addr))
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // The handler runs (route mounted): its first step — body serde parse —
        // fails on the missing `messages`, so the OpenAI-compatible 400 envelope
        // comes back instead of 404/405.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rewrite_payload_too_large_returns_json_envelope() {
        let response = rewrite_payload_too_large(
            Response::builder()
                .status(StatusCode::PAYLOAD_TOO_LARGE)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let (_, body) = response_json(response).await;
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["status"], 413);
        assert_eq!(
            body["error"]["message"].as_str().unwrap(),
            format!(
                "Request body exceeds the {} byte limit",
                crate::MAX_BODY_SIZE
            )
        );
    }

    #[tokio::test]
    async fn rewrite_payload_too_large_passes_through_other_statuses() {
        let response = rewrite_payload_too_large(
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
