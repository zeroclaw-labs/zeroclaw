use crate::auth::openai_oauth::extract_account_id_from_jwt;
use crate::auth::AuthService;
use crate::multimodal;
use crate::providers::traits::{
    ChatMessage, ChatRequest, ChatResponse, Provider, ProviderCapabilities, TokenUsage,
};
use crate::providers::ProviderRuntimeOptions;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{
            header::{AUTHORIZATION, USER_AGENT},
            HeaderValue as WsHeaderValue,
        },
        Message as WsMessage,
    },
};

const DEFAULT_CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_RESPONSES_URL_ENV: &str = "ZEROCLAW_CODEX_RESPONSES_URL";
const CODEX_BASE_URL_ENV: &str = "ZEROCLAW_CODEX_BASE_URL";
const CODEX_QUOTA_ENDPOINTS: [&str; 5] = [
    "https://chatgpt.com/backend-api/codex/usage",
    "https://chatgpt.com/backend-api/codex/rate_limits",
    "https://chatgpt.com/backend-api/codex/limits",
    "https://chatgpt.com/backend-api/usage_limits",
    "https://chatgpt.com/backend-api/limits",
];
const CODEX_TRANSPORT_ENV: &str = "ZEROCLAW_CODEX_TRANSPORT";
const CODEX_PROVIDER_TRANSPORT_ENV: &str = "ZEROCLAW_PROVIDER_TRANSPORT";
const CODEX_RESPONSES_WEBSOCKET_ENV_LEGACY: &str = "ZEROCLAW_RESPONSES_WEBSOCKET";
const DEFAULT_CODEX_INSTRUCTIONS: &str =
    "You are ZeroClaw, a concise and helpful coding assistant.";
const CODEX_WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const CODEX_WS_SEND_TIMEOUT: Duration = Duration::from_secs(15);
const CODEX_WS_READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexTransport {
    Auto,
    WebSocket,
    Sse,
}

#[derive(Debug)]
enum WebsocketRequestError {
    TransportUnavailable(anyhow::Error),
    Stream(anyhow::Error),
}

impl WebsocketRequestError {
    fn transport_unavailable<E>(error: E) -> Self
    where
        E: Into<anyhow::Error>,
    {
        Self::TransportUnavailable(error.into())
    }

    fn stream<E>(error: E) -> Self
    where
        E: Into<anyhow::Error>,
    {
        Self::Stream(error.into())
    }
}

impl std::fmt::Display for WebsocketRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TransportUnavailable(error) | Self::Stream(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for WebsocketRequestError {}

pub struct OpenAiCodexProvider {
    auth: AuthService,
    auth_profile_override: Option<String>,
    responses_url: String,
    transport: CodexTransport,
    custom_endpoint: bool,
    gateway_api_key: Option<String>,
    reasoning_level: Option<String>,
    client: Client,
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<ResponsesInput>,
    instructions: String,
    store: bool,
    stream: bool,
    text: ResponsesTextOptions,
    reasoning: ResponsesReasoningOptions,
    include: Vec<String>,
    tool_choice: String,
    parallel_tool_calls: bool,
}

#[derive(Debug, Serialize)]
struct ResponsesInput {
    role: String,
    content: Vec<ResponsesInputContent>,
}

#[derive(Debug, Serialize)]
struct ResponsesInputContent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResponsesTextOptions {
    verbosity: String,
}

#[derive(Debug, Serialize)]
struct ResponsesReasoningOptions {
    effort: String,
    summary: String,
}

#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output: Vec<ResponsesOutput>,
    #[serde(default)]
    output_text: Option<String>,
}

struct CodexResponseMetadata {
    text: String,
    rate_limit_summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponsesOutput {
    #[serde(default)]
    content: Vec<ResponsesContent>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContent {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

impl OpenAiCodexProvider {
    pub fn new(
        options: &ProviderRuntimeOptions,
        gateway_api_key: Option<&str>,
    ) -> anyhow::Result<Self> {
        let state_dir = options
            .zeroclaw_dir
            .clone()
            .unwrap_or_else(default_zeroclaw_dir);
        let auth = AuthService::new(&state_dir, options.secrets_encrypt);
        let responses_url = resolve_responses_url(options)?;

        Ok(Self {
            auth,
            auth_profile_override: options.auth_profile_override.clone(),
            custom_endpoint: !is_default_responses_url(&responses_url),
            responses_url,
            transport: resolve_transport_mode(options)?,
            gateway_api_key: gateway_api_key.map(ToString::to_string),
            reasoning_level: normalize_reasoning_level(
                options.reasoning_level.as_deref(),
                "provider.reasoning_level",
            ),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        })
    }
}

fn default_zeroclaw_dir() -> PathBuf {
    directories::UserDirs::new().map_or_else(
        || PathBuf::from(".zeroclaw"),
        |dirs| dirs.home_dir().join(".zeroclaw"),
    )
}

fn build_responses_url(base_or_endpoint: &str) -> anyhow::Result<String> {
    let candidate = base_or_endpoint.trim();
    if candidate.is_empty() {
        anyhow::bail!("OpenAI Codex endpoint override cannot be empty");
    }

    let mut parsed = reqwest::Url::parse(candidate)
        .map_err(|_| anyhow::anyhow!("OpenAI Codex endpoint override must be a valid URL"))?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => anyhow::bail!("OpenAI Codex endpoint override must use http:// or https://"),
    }

    let path = parsed.path().trim_end_matches('/');
    if !path.ends_with("/responses") {
        let with_suffix = if path.is_empty() || path == "/" {
            "/responses".to_string()
        } else {
            format!("{path}/responses")
        };
        parsed.set_path(&with_suffix);
    }

    parsed.set_query(None);
    parsed.set_fragment(None);

    Ok(parsed.to_string())
}

fn resolve_responses_url(options: &ProviderRuntimeOptions) -> anyhow::Result<String> {
    if let Some(endpoint) = std::env::var(CODEX_RESPONSES_URL_ENV)
        .ok()
        .and_then(|value| first_nonempty(Some(&value)))
    {
        return build_responses_url(&endpoint);
    }

    if let Some(base_url) = std::env::var(CODEX_BASE_URL_ENV)
        .ok()
        .and_then(|value| first_nonempty(Some(&value)))
    {
        return build_responses_url(&base_url);
    }

    if let Some(api_url) = options
        .provider_api_url
        .as_deref()
        .and_then(|value| first_nonempty(Some(value)))
    {
        return build_responses_url(&api_url);
    }

    Ok(DEFAULT_CODEX_RESPONSES_URL.to_string())
}

fn canonical_endpoint(url: &str) -> Option<(String, String, u16, String)> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let port = parsed.port_or_known_default()?;
    let path = parsed.path().trim_end_matches('/').to_string();
    Some((parsed.scheme().to_ascii_lowercase(), host, port, path))
}

fn is_default_responses_url(url: &str) -> bool {
    canonical_endpoint(url) == canonical_endpoint(DEFAULT_CODEX_RESPONSES_URL)
}

fn first_nonempty(text: Option<&str>) -> Option<String> {
    text.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn parse_transport_override(
    raw: Option<&str>,
    source: &str,
) -> anyhow::Result<Option<CodexTransport>> {
    let Some(raw_value) = raw else {
        return Ok(None);
    };
    let value = raw_value.trim();
    if value.is_empty() {
        return Ok(None);
    }

    let normalized = value.to_ascii_lowercase().replace(['-', '_'], "");
    match normalized.as_str() {
        "auto" => Ok(Some(CodexTransport::Auto)),
        "websocket" | "ws" => Ok(Some(CodexTransport::WebSocket)),
        "sse" | "http" => Ok(Some(CodexTransport::Sse)),
        _ => anyhow::bail!(
            "Invalid OpenAI Codex transport override '{value}' from {source}; expected one of: auto, websocket, sse"
        ),
    }
}

fn parse_legacy_websocket_flag(raw: &str) -> Option<CodexTransport> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "on" | "yes" => Some(CodexTransport::WebSocket),
        "0" | "false" | "off" | "no" => Some(CodexTransport::Sse),
        _ => None,
    }
}

fn resolve_transport_mode(options: &ProviderRuntimeOptions) -> anyhow::Result<CodexTransport> {
    if let Some(mode) = parse_transport_override(
        options.provider_transport.as_deref(),
        "provider.transport runtime override",
    )? {
        return Ok(mode);
    }

    if let Ok(value) = std::env::var(CODEX_TRANSPORT_ENV) {
        if let Some(mode) = parse_transport_override(Some(&value), CODEX_TRANSPORT_ENV)? {
            return Ok(mode);
        }
    }

    if let Ok(value) = std::env::var(CODEX_PROVIDER_TRANSPORT_ENV) {
        if let Some(mode) = parse_transport_override(Some(&value), CODEX_PROVIDER_TRANSPORT_ENV)? {
            return Ok(mode);
        }
    }

    if let Some(mode) = std::env::var(CODEX_RESPONSES_WEBSOCKET_ENV_LEGACY)
        .ok()
        .and_then(|value| parse_legacy_websocket_flag(&value))
    {
        tracing::warn!(
            env = CODEX_RESPONSES_WEBSOCKET_ENV_LEGACY,
            "Using deprecated websocket toggle env for OpenAI Codex transport"
        );
        return Ok(mode);
    }

    Ok(CodexTransport::Auto)
}

fn resolve_instructions(system_prompt: Option<&str>) -> String {
    first_nonempty(system_prompt).unwrap_or_else(|| DEFAULT_CODEX_INSTRUCTIONS.to_string())
}

fn normalize_model_id(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

fn build_responses_input(messages: &[ChatMessage]) -> (String, Vec<ResponsesInput>) {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut input: Vec<ResponsesInput> = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => system_parts.push(&msg.content),
            "user" => {
                let (cleaned_text, image_refs) = multimodal::parse_image_markers(&msg.content);
                let mut content = Vec::new();
                if !cleaned_text.trim().is_empty() {
                    content.push(ResponsesInputContent {
                        kind: "input_text".to_string(),
                        text: Some(cleaned_text),
                        image_url: None,
                    });
                }
                for image_ref in image_refs {
                    content.push(ResponsesInputContent {
                        kind: "input_image".to_string(),
                        text: None,
                        image_url: Some(image_ref),
                    });
                }
                if content.is_empty() {
                    content.push(ResponsesInputContent {
                        kind: "input_text".to_string(),
                        text: Some(msg.content.clone()),
                        image_url: None,
                    });
                }
                input.push(ResponsesInput {
                    role: "user".to_string(),
                    content,
                });
            }
            "assistant" => {
                input.push(ResponsesInput {
                    role: "assistant".to_string(),
                    content: vec![ResponsesInputContent {
                        kind: "output_text".to_string(),
                        text: Some(msg.content.clone()),
                        image_url: None,
                    }],
                });
            }
            _ => {}
        }
    }

    let instructions = if system_parts.is_empty() {
        DEFAULT_CODEX_INSTRUCTIONS.to_string()
    } else {
        system_parts.join("\n\n")
    };

    (instructions, input)
}

fn clamp_reasoning_effort(model: &str, effort: &str) -> String {
    let id = normalize_model_id(model);
    // gpt-5-codex currently supports only low|medium|high.
    if id == "gpt-5-codex" {
        return match effort {
            "low" | "medium" | "high" => effort.to_string(),
            "minimal" => "low".to_string(),
            _ => "high".to_string(),
        };
    }
    if (id.starts_with("gpt-5.2") || id.starts_with("gpt-5.3")) && effort == "minimal" {
        return "low".to_string();
    }
    if id.starts_with("gpt-5-codex") && effort == "xhigh" {
        return "high".to_string();
    }
    if id == "gpt-5.1" && effort == "xhigh" {
        return "high".to_string();
    }
    if id == "gpt-5.1-codex-mini" {
        return if effort == "high" || effort == "xhigh" {
            "high".to_string()
        } else {
            "medium".to_string()
        };
    }
    effort.to_string()
}

fn normalize_reasoning_level(raw: Option<&str>, source: &str) -> Option<String> {
    let value = raw?.trim();
    if value.is_empty() {
        return None;
    }
    let normalized = value.to_ascii_lowercase().replace(['-', '_'], "");
    match normalized.as_str() {
        "minimal" | "low" | "medium" | "high" | "xhigh" => Some(normalized),
        _ => {
            tracing::warn!(
                reasoning_level = %value,
                source,
                "Ignoring invalid reasoning level override"
            );
            None
        }
    }
}

fn resolve_reasoning_effort(model_id: &str, override_level: Option<&str>) -> String {
    let override_level = normalize_reasoning_level(override_level, "provider.reasoning_level");
    let env_level = std::env::var("ZEROCLAW_CODEX_REASONING_EFFORT")
        .ok()
        .and_then(|value| {
            normalize_reasoning_level(Some(&value), "ZEROCLAW_CODEX_REASONING_EFFORT")
        });
    let raw = override_level
        .or(env_level)
        .unwrap_or_else(|| "xhigh".to_string());
    clamp_reasoning_effort(model_id, &raw)
}

fn nonempty_preserve(text: Option<&str>) -> Option<String> {
    text.and_then(|value| {
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    })
}

fn extract_responses_text(response: &ResponsesResponse) -> Option<String> {
    if let Some(text) = first_nonempty(response.output_text.as_deref()) {
        return Some(text);
    }

    for item in &response.output {
        for content in &item.content {
            if content.kind.as_deref() == Some("output_text") {
                if let Some(text) = first_nonempty(content.text.as_deref()) {
                    return Some(text);
                }
            }
        }
    }

    for item in &response.output {
        for content in &item.content {
            if let Some(text) = first_nonempty(content.text.as_deref()) {
                return Some(text);
            }
        }
    }

    None
}

fn extract_stream_event_text(event: &Value, saw_delta: bool) -> Option<String> {
    let event_type = event.get("type").and_then(Value::as_str);
    match event_type {
        Some("response.output_text.delta") => {
            nonempty_preserve(event.get("delta").and_then(Value::as_str))
        }
        Some("response.output_text.done") if !saw_delta => {
            nonempty_preserve(event.get("text").and_then(Value::as_str))
        }
        Some("response.completed" | "response.done") => event
            .get("response")
            .and_then(|value| serde_json::from_value::<ResponsesResponse>(value.clone()).ok())
            .and_then(|response| extract_responses_text(&response)),
        _ => None,
    }
}

fn parse_sse_text(body: &str) -> anyhow::Result<Option<String>> {
    let mut saw_delta = false;
    let mut delta_accumulator = String::new();
    let mut fallback_text = None;
    let mut buffer = body.to_string();

    let mut process_event = |event: Value| -> anyhow::Result<()> {
        if let Some(message) = extract_stream_error_message(&event) {
            return Err(anyhow::anyhow!("OpenAI Codex stream error: {message}"));
        }
        if let Some(text) = extract_stream_event_text(&event, saw_delta) {
            let event_type = event.get("type").and_then(Value::as_str);
            if event_type == Some("response.output_text.delta") {
                saw_delta = true;
                delta_accumulator.push_str(&text);
            } else if fallback_text.is_none() {
                fallback_text = Some(text);
            }
        }
        Ok(())
    };

    let mut process_chunk = |chunk: &str| -> anyhow::Result<()> {
        let data_lines: Vec<String> = chunk
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.trim().to_string())
            .collect();
        if data_lines.is_empty() {
            return Ok(());
        }

        let joined = data_lines.join("\n");
        let trimmed = joined.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return Ok(());
        }

        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
            return process_event(event);
        }

        for line in data_lines {
            let line = line.trim();
            if line.is_empty() || line == "[DONE]" {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<Value>(line) {
                process_event(event)?;
            }
        }

        Ok(())
    };

    loop {
        let Some(idx) = buffer.find("\n\n") else {
            break;
        };

        let chunk = buffer[..idx].to_string();
        buffer = buffer[idx + 2..].to_string();
        process_chunk(&chunk)?;
    }

    if !buffer.trim().is_empty() {
        process_chunk(&buffer)?;
    }

    if saw_delta {
        return Ok(nonempty_preserve(Some(&delta_accumulator)));
    }

    Ok(fallback_text)
}

fn is_rate_limit_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("limit")
        || key.contains("quota")
        || key.contains("remaining")
        || key.contains("usage")
        || key.contains("weekly")
        || key.contains("5h")
        || key.contains("hour")
        || key.contains("percent")
        || key.contains("pct")
}

fn is_quota_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("weekly")
        || key.contains("week")
        || key.contains("5h")
        || key.contains("five")
        || key.contains("hour")
        || key.contains("remaining")
        || key.contains("limit")
        || key.contains("quota")
        || key.contains("percent")
        || key.contains("reset")
}

fn collect_rate_limit_pairs(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let child_path = if path.is_empty() {
                    k.to_string()
                } else {
                    format!("{path}.{k}")
                };
                if is_rate_limit_key(k) {
                    match v {
                        Value::String(s) if !s.trim().is_empty() => {
                            out.push(format!("{child_path}={}", s.trim()));
                        }
                        Value::Number(n) => out.push(format!("{child_path}={n}")),
                        Value::Bool(b) => out.push(format!("{child_path}={b}")),
                        _ => {}
                    }
                }
                collect_rate_limit_pairs(v, &child_path, out);
            }
        }
        Value::Array(arr) => {
            for (idx, item) in arr.iter().enumerate() {
                let child_path = if path.is_empty() {
                    format!("[{idx}]")
                } else {
                    format!("{path}[{idx}]")
                };
                collect_rate_limit_pairs(item, &child_path, out);
            }
        }
        _ => {}
    }
}

fn parse_sse_rate_limit_summary(body: &str) -> Option<String> {
    let mut pairs = Vec::new();
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = data.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Value>(payload) {
            collect_rate_limit_pairs(&event, "", &mut pairs);
        }
    }
    if pairs.is_empty() {
        None
    } else {
        pairs.sort();
        pairs.dedup();
        Some(pairs.join("; "))
    }
}

fn collect_quota_pairs(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let child_path = if path.is_empty() {
                    k.to_string()
                } else {
                    format!("{path}.{k}")
                };
                if is_quota_key(k) || is_quota_key(&child_path) {
                    match v {
                        Value::String(s) if !s.trim().is_empty() => {
                            out.push(format!("{child_path}={}", s.trim()));
                        }
                        Value::Number(n) => out.push(format!("{child_path}={n}")),
                        Value::Bool(b) => out.push(format!("{child_path}={b}")),
                        _ => {}
                    }
                }
                collect_quota_pairs(v, &child_path, out);
            }
        }
        Value::Array(arr) => {
            for (idx, item) in arr.iter().enumerate() {
                let child_path = if path.is_empty() {
                    format!("[{idx}]")
                } else {
                    format!("{path}[{idx}]")
                };
                collect_quota_pairs(item, &child_path, out);
            }
        }
        _ => {}
    }
}

fn summarize_quota_payload(value: &Value) -> Option<String> {
    if let Some(summary) = summarize_codex_usage_payload(value) {
        return Some(summary);
    }

    let mut pairs = Vec::new();
    collect_quota_pairs(value, "", &mut pairs);
    if pairs.is_empty() {
        None
    } else {
        pairs.sort();
        pairs.dedup();
        Some(pairs.join("; "))
    }
}

fn summarize_codex_usage_window(label: &str, window: &Value) -> Option<String> {
    let used_percent = window.get("used_percent").and_then(Value::as_u64)?;
    let remaining_percent = 100_u64.saturating_sub(used_percent.min(100));
    let reset_after = window
        .get("reset_after_seconds")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let reset_at = window
        .get("reset_at")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let reset_after_human = humanize_reset_after_seconds(reset_after);
    let reset_at_human = humanize_unix_timestamp_utc(reset_at);
    Some(format!(
        "{label} remaining={remaining_percent}% (used={used_percent}%, reset in {reset_after_human}, reset at {reset_at_human})"
    ))
}

fn humanize_reset_after_seconds(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".to_string();
    }
    let mut remaining = seconds;
    let days = remaining / 86_400;
    remaining %= 86_400;
    let hours = remaining / 3_600;
    remaining %= 3_600;
    let minutes = remaining / 60;
    let secs = remaining % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if secs > 0 && parts.is_empty() {
        parts.push(format!("{secs}s"));
    }
    if parts.is_empty() {
        "now".to_string()
    } else {
        parts.join(" ")
    }
}

fn humanize_unix_timestamp_utc(timestamp: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

fn summarize_codex_usage_payload(value: &Value) -> Option<String> {
    let rate_limit = value.get("rate_limit")?;
    let mut parts = Vec::new();
    if let Some(window) = rate_limit.get("primary_window") {
        if let Some(summary) = summarize_codex_usage_window("5h", window) {
            parts.push(summary);
        }
    }
    if let Some(window) = rate_limit.get("secondary_window") {
        if let Some(summary) = summarize_codex_usage_window("weekly", window) {
            parts.push(summary);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

fn extract_stream_error_message(event: &Value) -> Option<String> {
    let event_type = event.get("type").and_then(Value::as_str);

    if event_type == Some("error") {
        return first_nonempty(
            event
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| event.get("code").and_then(Value::as_str))
                .or_else(|| {
                    event
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                }),
        );
    }

    if event_type == Some("response.failed") {
        return first_nonempty(
            event
                .get("response")
                .and_then(|response| response.get("error"))
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str),
        );
    }

    None
}

async fn decode_responses_body(response: reqwest::Response) -> anyhow::Result<String> {
    let status = response.status();
    // Use bytes() so an invalid-UTF-8 or truncated body doesn't hide the content.
    // from_utf8_lossy replaces bad bytes with U+FFFD rather than erroring.
    let bytes = response.bytes().await?;
    let body = String::from_utf8_lossy(&bytes);
    tracing::debug!(
        status = %status,
        body_bytes = bytes.len(),
        "OpenAI Codex response received"
    );

    if let Some(text) = parse_sse_text(&body)? {
        return Ok(text);
    }

    let body_trimmed = body.trim_start();
    let looks_like_sse = body_trimmed.starts_with("event:") || body_trimmed.starts_with("data:");
    if looks_like_sse {
        return Err(anyhow::anyhow!(
            "No response from OpenAI Codex stream payload: {}",
            super::sanitize_api_error(&body)
        ));
    }

    let parsed: ResponsesResponse = serde_json::from_str(&body).map_err(|err| {
        anyhow::anyhow!(
            "OpenAI Codex JSON parse failed: {err}. Payload: {}",
            super::sanitize_api_error(&body)
        )
    })?;
    extract_responses_text(&parsed).ok_or_else(|| anyhow::anyhow!("No response from OpenAI Codex"))
}

async fn decode_responses_body_with_metadata(
    response: reqwest::Response,
) -> anyhow::Result<CodexResponseMetadata> {
    let status = response.status();
    let bytes = response.bytes().await?;
    let body = String::from_utf8_lossy(&bytes);
    tracing::debug!(
        status = %status,
        body_bytes = bytes.len(),
        "OpenAI Codex response received"
    );
    let sse_rate_limit_summary = parse_sse_rate_limit_summary(&body);

    let text = if let Some(text) = parse_sse_text(&body)? {
        text
    } else {
        let body_trimmed = body.trim_start();
        let looks_like_sse =
            body_trimmed.starts_with("event:") || body_trimmed.starts_with("data:");
        if looks_like_sse {
            return Err(anyhow::anyhow!(
                "No response from OpenAI Codex stream payload: {}",
                super::sanitize_api_error(&body)
            ));
        }

        let parsed: ResponsesResponse = serde_json::from_str(&body).map_err(|err| {
            anyhow::anyhow!(
                "OpenAI Codex JSON parse failed: {err}. Payload: {}",
                super::sanitize_api_error(&body)
            )
        })?;
        extract_responses_text(&parsed)
            .ok_or_else(|| anyhow::anyhow!("No response from OpenAI Codex"))?
    };

    Ok(CodexResponseMetadata {
        text,
        rate_limit_summary: sse_rate_limit_summary,
    })
}

fn codex_rate_limit_summary_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let mut pairs = Vec::new();
    for (name, value) in headers {
        let key = name.as_str().to_ascii_lowercase();
        let include = key.contains("ratelimit")
            || key.contains("rate-limit")
            || key.contains("weekly")
            || key.contains("5h")
            || key.contains("5-hour")
            || key.contains("limit")
            || key.contains("quota")
            || key.contains("usage");
        if !include {
            continue;
        }
        if let Ok(text) = value.to_str() {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                pairs.push(format!("{key}={trimmed}"));
            }
        }
    }
    if pairs.is_empty() {
        None
    } else {
        Some(pairs.join("; "))
    }
}

impl OpenAiCodexProvider {
    async fn fetch_codex_quota_summary(
        &self,
        access_token: &str,
        account_id: &str,
    ) -> Option<String> {
        for endpoint in CODEX_QUOTA_ENDPOINTS {
            let response = match self
                .client
                .get(endpoint)
                .header("Authorization", format!("Bearer {access_token}"))
                .header("chatgpt-account-id", account_id)
                .header("originator", "pi")
                .header("accept", "application/json")
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(_) => continue,
            };

            if !response.status().is_success() {
                continue;
            }

            let body = match response.text().await {
                Ok(body) => body,
                Err(_) => continue,
            };
            let Ok(json) = serde_json::from_str::<Value>(&body) else {
                continue;
            };
            if let Some(summary) = summarize_quota_payload(&json) {
                return Some(summary);
            }
        }

        None
    }

    fn responses_websocket_url(&self, model: &str) -> anyhow::Result<String> {
        let mut url = reqwest::Url::parse(&self.responses_url)?;
        let next_scheme: &'static str = match url.scheme() {
            "https" | "wss" => "wss",
            "http" | "ws" => "ws",
            other => {
                anyhow::bail!(
                    "OpenAI Codex websocket transport does not support URL scheme: {}",
                    other
                );
            }
        };

        url.set_scheme(next_scheme)
            .map_err(|()| anyhow::anyhow!("failed to set websocket URL scheme"))?;

        if !url.query_pairs().any(|(k, _)| k == "model") {
            url.query_pairs_mut().append_pair("model", model);
        }

        Ok(url.into())
    }

    fn apply_auth_headers_ws(
        &self,
        request: &mut tokio_tungstenite::tungstenite::http::Request<()>,
        bearer_token: &str,
        account_id: Option<&str>,
        access_token: Option<&str>,
        use_gateway_api_key_auth: bool,
    ) -> anyhow::Result<()> {
        let headers = request.headers_mut();
        headers.insert(
            AUTHORIZATION,
            WsHeaderValue::from_str(&format!("Bearer {bearer_token}"))?,
        );
        headers.insert(
            "OpenAI-Beta",
            WsHeaderValue::from_static("responses=experimental"),
        );
        headers.insert("originator", WsHeaderValue::from_static("pi"));
        headers.insert("accept", WsHeaderValue::from_static("text/event-stream"));
        headers.insert(USER_AGENT, WsHeaderValue::from_static("zeroclaw"));

        if let Some(account_id) = account_id {
            headers.insert("chatgpt-account-id", WsHeaderValue::from_str(account_id)?);
        }

        if use_gateway_api_key_auth {
            if let Some(access_token) = access_token {
                headers.insert(
                    "x-openai-access-token",
                    WsHeaderValue::from_str(access_token)?,
                );
            }
            if let Some(account_id) = account_id {
                headers.insert("x-openai-account-id", WsHeaderValue::from_str(account_id)?);
            }
        }

        Ok(())
    }

    async fn send_responses_websocket_request(
        &self,
        request: &ResponsesRequest,
        model: &str,
        bearer_token: &str,
        account_id: Option<&str>,
        access_token: Option<&str>,
        use_gateway_api_key_auth: bool,
    ) -> Result<String, WebsocketRequestError> {
        let ws_url = self
            .responses_websocket_url(model)
            .map_err(WebsocketRequestError::transport_unavailable)?;
        let mut ws_request = ws_url.into_client_request().map_err(|error| {
            WebsocketRequestError::transport_unavailable(anyhow::anyhow!(
                "invalid websocket request URL: {error}"
            ))
        })?;
        self.apply_auth_headers_ws(
            &mut ws_request,
            bearer_token,
            account_id,
            access_token,
            use_gateway_api_key_auth,
        )
        .map_err(WebsocketRequestError::transport_unavailable)?;

        let payload = serde_json::json!({
            "type": "response.create",
            "model": &request.model,
            "input": &request.input,
            "instructions": &request.instructions,
            "store": request.store,
            "text": &request.text,
            "reasoning": &request.reasoning,
            "include": &request.include,
            "tool_choice": &request.tool_choice,
            "parallel_tool_calls": request.parallel_tool_calls,
        });

        let (mut ws_stream, _) = timeout(CODEX_WS_CONNECT_TIMEOUT, connect_async(ws_request))
            .await
            .map_err(|_| {
                WebsocketRequestError::transport_unavailable(anyhow::anyhow!(
                    "OpenAI Codex websocket connect timed out after {}s",
                    CODEX_WS_CONNECT_TIMEOUT.as_secs()
                ))
            })?
            .map_err(WebsocketRequestError::transport_unavailable)?;
        timeout(
            CODEX_WS_SEND_TIMEOUT,
            ws_stream.send(WsMessage::Text(
                serde_json::to_string(&payload)
                    .map_err(WebsocketRequestError::transport_unavailable)?
                    .into(),
            )),
        )
        .await
        .map_err(|_| {
            WebsocketRequestError::transport_unavailable(anyhow::anyhow!(
                "OpenAI Codex websocket send timed out after {}s",
                CODEX_WS_SEND_TIMEOUT.as_secs()
            ))
        })?
        .map_err(WebsocketRequestError::transport_unavailable)?;

        let mut saw_delta = false;
        let mut delta_accumulator = String::new();
        let mut fallback_text: Option<String> = None;
        let mut timed_out = false;

        loop {
            let frame = match timeout(CODEX_WS_READ_TIMEOUT, ws_stream.next()).await {
                Ok(frame) => frame,
                Err(_) => {
                    let _ = ws_stream.close(None).await;
                    if saw_delta || fallback_text.is_some() {
                        timed_out = true;
                        break;
                    }
                    return Err(WebsocketRequestError::stream(anyhow::anyhow!(
                        "OpenAI Codex websocket stream timed out after {}s waiting for events",
                        CODEX_WS_READ_TIMEOUT.as_secs()
                    )));
                }
            };

            let Some(frame) = frame else {
                break;
            };
            let frame = frame.map_err(WebsocketRequestError::stream)?;
            let event: Value = match frame {
                WsMessage::Text(text) => {
                    serde_json::from_str(text.as_ref()).map_err(WebsocketRequestError::stream)?
                }
                WsMessage::Binary(binary) => {
                    let text = String::from_utf8(binary.to_vec()).map_err(|error| {
                        WebsocketRequestError::stream(anyhow::anyhow!(
                            "invalid UTF-8 websocket frame from OpenAI Codex: {error}"
                        ))
                    })?;
                    serde_json::from_str(&text).map_err(WebsocketRequestError::stream)?
                }
                WsMessage::Ping(payload) => {
                    ws_stream
                        .send(WsMessage::Pong(payload))
                        .await
                        .map_err(WebsocketRequestError::stream)?;
                    continue;
                }
                WsMessage::Close(_) => break,
                _ => continue,
            };

            if let Some(message) = extract_stream_error_message(&event) {
                return Err(WebsocketRequestError::stream(anyhow::anyhow!(
                    "OpenAI Codex websocket stream error: {message}"
                )));
            }

            if let Some(text) = extract_stream_event_text(&event, saw_delta) {
                let event_type = event.get("type").and_then(Value::as_str);
                if event_type == Some("response.output_text.delta") {
                    saw_delta = true;
                    delta_accumulator.push_str(&text);
                } else if fallback_text.is_none() {
                    fallback_text = Some(text);
                }
            }

            let event_type = event.get("type").and_then(Value::as_str);
            if event_type == Some("response.completed") || event_type == Some("response.done") {
                if let Some(response_value) = event.get("response").cloned() {
                    if let Ok(parsed) = serde_json::from_value::<ResponsesResponse>(response_value)
                    {
                        if let Some(text) = extract_responses_text(&parsed) {
                            let _ = ws_stream.close(None).await;
                            return Ok(text);
                        }
                    }
                }

                if saw_delta {
                    let _ = ws_stream.close(None).await;
                    return nonempty_preserve(Some(&delta_accumulator)).ok_or_else(|| {
                        WebsocketRequestError::stream(anyhow::anyhow!(
                            "No response from OpenAI Codex"
                        ))
                    });
                }
                if let Some(text) = fallback_text.clone() {
                    let _ = ws_stream.close(None).await;
                    return Ok(text);
                }
            }
        }

        if saw_delta {
            return nonempty_preserve(Some(&delta_accumulator)).ok_or_else(|| {
                WebsocketRequestError::stream(anyhow::anyhow!("No response from OpenAI Codex"))
            });
        }
        if let Some(text) = fallback_text {
            return Ok(text);
        }
        if timed_out {
            return Err(WebsocketRequestError::stream(anyhow::anyhow!(
                "No response from OpenAI Codex websocket stream before timeout"
            )));
        }

        Err(WebsocketRequestError::transport_unavailable(anyhow::anyhow!(
            "No response from OpenAI Codex websocket stream"
        )))
    }

    async fn send_responses_sse_request(
        &self,
        request: &ResponsesRequest,
        bearer_token: &str,
        account_id: Option<&str>,
        access_token: Option<&str>,
        use_gateway_api_key_auth: bool,
    ) -> anyhow::Result<CodexResponseMetadata> {
        let mut request_builder = self
            .client
            .post(&self.responses_url)
            .header("Authorization", format!("Bearer {bearer_token}"))
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "pi")
            .header("accept", "text/event-stream")
            .header("Content-Type", "application/json");

        if let Some(account_id) = account_id {
            request_builder = request_builder.header("chatgpt-account-id", account_id);
        }

        if use_gateway_api_key_auth {
            if let Some(access_token) = access_token {
                request_builder = request_builder.header("x-openai-access-token", access_token);
            }
            if let Some(account_id) = account_id {
                request_builder = request_builder.header("x-openai-account-id", account_id);
            }
        }

        let response = request_builder.json(request).send().await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenAI Codex", response).await);
        }

        decode_responses_body_with_metadata(response).await
    }

    async fn send_responses_request(
        &self,
        input: Vec<ResponsesInput>,
        instructions: String,
        model: &str,
    ) -> anyhow::Result<CodexResponseMetadata> {
        let use_gateway_api_key_auth = self.custom_endpoint && self.gateway_api_key.is_some();
        let profile = match self
            .auth
            .get_profile("openai-codex", self.auth_profile_override.as_deref())
            .await
        {
            Ok(profile) => profile,
            Err(err) if use_gateway_api_key_auth => {
                tracing::warn!(
                    error = %err,
                    "failed to load OpenAI Codex profile; continuing with custom endpoint API key mode"
                );
                None
            }
            Err(err) => return Err(err),
        };
        let oauth_access_token = match self
            .auth
            .get_valid_openai_access_token(self.auth_profile_override.as_deref())
            .await
        {
            Ok(token) => token,
            Err(err) if use_gateway_api_key_auth => {
                tracing::warn!(
                    error = %err,
                    "failed to refresh OpenAI token; continuing with custom endpoint API key mode"
                );
                None
            }
            Err(err) => return Err(err),
        };

        let account_id = profile.and_then(|profile| profile.account_id).or_else(|| {
            oauth_access_token
                .as_deref()
                .and_then(extract_account_id_from_jwt)
        });
        let access_token = if use_gateway_api_key_auth {
            oauth_access_token
        } else {
            Some(oauth_access_token.ok_or_else(|| {
                anyhow::anyhow!(
                    "OpenAI Codex auth profile not found. Run `zeroclaw auth login --provider openai-codex`."
                )
            })?)
        };
        let account_id = if use_gateway_api_key_auth {
            account_id
        } else {
            Some(account_id.ok_or_else(|| {
                anyhow::anyhow!(
                    "OpenAI Codex account id not found in auth profile/token. Run `zeroclaw auth login --provider openai-codex` again."
                )
            })?)
        };
        let normalized_model = normalize_model_id(model);

        let request = ResponsesRequest {
            model: normalized_model.to_string(),
            input,
            instructions,
            store: false,
            stream: true,
            text: ResponsesTextOptions {
                verbosity: "medium".to_string(),
            },
            reasoning: ResponsesReasoningOptions {
                effort: resolve_reasoning_effort(normalized_model, self.reasoning_level.as_deref()),
                summary: "auto".to_string(),
            },
            include: vec!["reasoning.encrypted_content".to_string()],
            tool_choice: "auto".to_string(),
            parallel_tool_calls: true,
        };

        let bearer_token = if use_gateway_api_key_auth {
            self.gateway_api_key.as_deref().unwrap_or_default()
        } else {
            access_token.as_deref().unwrap_or_default()
        };

        match self.transport {
            CodexTransport::WebSocket => self
                .send_responses_websocket_request(
                    &request,
                    normalized_model,
                    bearer_token,
                    account_id.as_deref(),
                    access_token.as_deref(),
                    use_gateway_api_key_auth,
                )
                .await
                .map(|text| CodexResponseMetadata { text, rate_limit_summary: None })
                .map_err(Into::into),
            CodexTransport::Sse => {
                self.send_responses_sse_request(
                    &request,
                    bearer_token,
                    account_id.as_deref(),
                    access_token.as_deref(),
                    use_gateway_api_key_auth,
                )
                .await
            }
            CodexTransport::Auto => {
                match self
                    .send_responses_websocket_request(
                        &request,
                        normalized_model,
                        bearer_token,
                        account_id.as_deref(),
                        access_token.as_deref(),
                        use_gateway_api_key_auth,
                    )
                    .await
                {
                    Ok(text) => Ok(CodexResponseMetadata { text, rate_limit_summary: None }),
                    Err(WebsocketRequestError::TransportUnavailable(error)) => {
                        tracing::warn!(
                            error = %error,
                            "OpenAI Codex websocket request failed; falling back to SSE"
                        );
                        self.send_responses_sse_request(
                            &request,
                            bearer_token,
                            account_id.as_deref(),
                            access_token.as_deref(),
                            use_gateway_api_key_auth,
                        )
                        .await
                    }
                    Err(WebsocketRequestError::Stream(error)) => Err(error),
                }
            }
        }

    }
}

#[async_trait]
impl Provider for OpenAiCodexProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: false,
            vision: true,
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<String> {
        // Build temporary messages array
        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(ChatMessage::system(sys));
        }
        messages.push(ChatMessage::user(message));

        // Normalize images: convert file paths to data URIs
        let config = crate::config::MultimodalConfig::default();
        let prepared = crate::multimodal::prepare_messages_for_provider(&messages, &config).await?;

        let (instructions, input) = build_responses_input(&prepared.messages);
        self.send_responses_request(input, instructions, model)
            .await
            .map(|result| result.text)
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<String> {
        // Normalize image markers: convert file paths to data URIs
        let config = crate::config::MultimodalConfig::default();
        let prepared = crate::multimodal::prepare_messages_for_provider(messages, &config).await?;

        let (instructions, input) = build_responses_input(&prepared.messages);
        self.send_responses_request(input, instructions, model)
            .await
            .map(|result| result.text)
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let (instructions, input) = build_responses_input(request.messages);
        let result = self
            .send_responses_request(input, instructions, model)
            .await?;
        Ok(ChatResponse {
            text: Some(result.text),
            tool_calls: Vec::new(),
            usage: result.rate_limit_summary.map(|summary| TokenUsage {
                input_tokens: None,
                output_tokens: None,
                token_limit: None,
                token_remaining: None,
                rate_limit_summary: Some(summary),
            }),
            reasoning_content: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderRuntimeOptions;
    use std::sync::{Mutex, OnceLock};

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var(key).ok();
            match value {
                Some(next) => std::env::set_var(key, next),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(original) = self.original.as_deref() {
                std::env::set_var(self.key, original);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    #[test]
    fn extracts_output_text_first() {
        let response = ResponsesResponse {
            output: vec![],
            output_text: Some("hello".into()),
        };
        assert_eq!(extract_responses_text(&response).as_deref(), Some("hello"));
    }

    #[test]
    fn extracts_nested_output_text() {
        let response = ResponsesResponse {
            output: vec![ResponsesOutput {
                content: vec![ResponsesContent {
                    kind: Some("output_text".into()),
                    text: Some("nested".into()),
                }],
            }],
            output_text: None,
        };
        assert_eq!(extract_responses_text(&response).as_deref(), Some("nested"));
    }

    #[test]
    fn build_responses_input_converts_image_markers_to_input_image_blocks() {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "Describe this\n\n[IMAGE:data:image/png;base64,abcd]".to_string(),
        }];

        let (_, input) = build_responses_input(&messages);
        assert_eq!(input.len(), 1);
        assert_eq!(input[0].content.len(), 2);
        assert_eq!(input[0].content[0].kind, "input_text");
        assert_eq!(input[0].content[1].kind, "input_image");
        assert_eq!(
            input[0].content[1].image_url.as_deref(),
            Some("data:image/png;base64,abcd")
        );
    }

    #[test]
    fn default_state_dir_is_non_empty() {
        let path = default_zeroclaw_dir();
        assert!(!path.as_os_str().is_empty());
    }

    #[test]
    fn build_responses_url_appends_suffix_for_base_url() {
        assert_eq!(
            build_responses_url("https://api.tonsof.blue/v1").unwrap(),
            "https://api.tonsof.blue/v1/responses"
        );
    }

    #[test]
    fn build_responses_url_keeps_existing_responses_endpoint() {
        assert_eq!(
            build_responses_url("https://api.tonsof.blue/v1/responses").unwrap(),
            "https://api.tonsof.blue/v1/responses"
        );
    }

    #[test]
    fn resolve_responses_url_prefers_explicit_endpoint_env() {
        let _env_lock = env_lock();
        let _endpoint_guard = EnvGuard::set(
            CODEX_RESPONSES_URL_ENV,
            Some("https://env.example.com/v1/responses"),
        );
        let _base_guard = EnvGuard::set(CODEX_BASE_URL_ENV, Some("https://base.example.com/v1"));

        let options = ProviderRuntimeOptions::default();
        assert_eq!(
            resolve_responses_url(&options).unwrap(),
            "https://env.example.com/v1/responses"
        );
    }

    #[test]
    fn resolve_responses_url_uses_provider_api_url_override() {
        let _env_lock = env_lock();
        let _endpoint_guard = EnvGuard::set(CODEX_RESPONSES_URL_ENV, None);
        let _base_guard = EnvGuard::set(CODEX_BASE_URL_ENV, None);

        let options = ProviderRuntimeOptions {
            provider_api_url: Some("https://proxy.example.com/v1".to_string()),
            ..ProviderRuntimeOptions::default()
        };

        assert_eq!(
            resolve_responses_url(&options).unwrap(),
            "https://proxy.example.com/v1/responses"
        );
    }

    #[test]
    fn resolve_transport_mode_defaults_to_auto() {
        let _env_lock = env_lock();
        let _transport_guard = EnvGuard::set(CODEX_TRANSPORT_ENV, None);
        let _legacy_guard = EnvGuard::set(CODEX_RESPONSES_WEBSOCKET_ENV_LEGACY, None);
        let _provider_guard = EnvGuard::set("ZEROCLAW_PROVIDER_TRANSPORT", None);

        assert_eq!(
            resolve_transport_mode(&ProviderRuntimeOptions::default()).unwrap(),
            CodexTransport::Auto
        );
    }

    #[test]
    fn resolve_transport_mode_accepts_runtime_override() {
        let _env_lock = env_lock();
        let _transport_guard = EnvGuard::set(CODEX_TRANSPORT_ENV, Some("sse"));

        let options = ProviderRuntimeOptions {
            provider_transport: Some("websocket".to_string()),
            ..ProviderRuntimeOptions::default()
        };

        assert_eq!(
            resolve_transport_mode(&options).unwrap(),
            CodexTransport::WebSocket
        );
    }

    #[test]
    fn resolve_transport_mode_legacy_bool_env_is_supported() {
        let _env_lock = env_lock();
        let _transport_guard = EnvGuard::set(CODEX_TRANSPORT_ENV, None);
        let _provider_guard = EnvGuard::set("ZEROCLAW_PROVIDER_TRANSPORT", None);
        let _legacy_guard = EnvGuard::set(CODEX_RESPONSES_WEBSOCKET_ENV_LEGACY, Some("false"));

        assert_eq!(
            resolve_transport_mode(&ProviderRuntimeOptions::default()).unwrap(),
            CodexTransport::Sse
        );
    }

    #[test]
    fn resolve_transport_mode_rejects_invalid_runtime_override() {
        let _env_lock = env_lock();
        let _transport_guard = EnvGuard::set(CODEX_TRANSPORT_ENV, None);
        let _provider_guard = EnvGuard::set("ZEROCLAW_PROVIDER_TRANSPORT", None);
        let _legacy_guard = EnvGuard::set(CODEX_RESPONSES_WEBSOCKET_ENV_LEGACY, None);

        let options = ProviderRuntimeOptions {
            provider_transport: Some("udp".to_string()),
            ..ProviderRuntimeOptions::default()
        };

        let err =
            resolve_transport_mode(&options).expect_err("invalid runtime transport must fail");
        assert!(err
            .to_string()
            .contains("Invalid OpenAI Codex transport override 'udp'"));
    }

    #[test]
    fn websocket_url_uses_ws_scheme_and_model_query() {
        let _env_lock = env_lock();
        let _endpoint_guard = EnvGuard::set(CODEX_RESPONSES_URL_ENV, None);
        let _base_guard = EnvGuard::set(CODEX_BASE_URL_ENV, None);

        let options = ProviderRuntimeOptions::default();
        let provider = OpenAiCodexProvider::new(&options, None).expect("provider should init");
        let ws_url = provider
            .responses_websocket_url("gpt-5.3-codex")
            .expect("websocket URL should be derived");

        assert_eq!(
            ws_url,
            "wss://chatgpt.com/backend-api/codex/responses?model=gpt-5.3-codex"
        );
    }

    #[test]
    fn default_responses_url_detector_handles_equivalent_urls() {
        assert!(is_default_responses_url(DEFAULT_CODEX_RESPONSES_URL));
        assert!(is_default_responses_url(
            "https://chatgpt.com/backend-api/codex/responses/"
        ));
        assert!(!is_default_responses_url(
            "https://api.tonsof.blue/v1/responses"
        ));
    }

    #[test]
    fn constructor_enables_custom_endpoint_key_mode() {
        let _env_lock = env_lock();
        let _endpoint_guard = EnvGuard::set(CODEX_RESPONSES_URL_ENV, None);
        let _base_guard = EnvGuard::set(CODEX_BASE_URL_ENV, None);

        let options = ProviderRuntimeOptions {
            provider_api_url: Some("https://api.tonsof.blue/v1".to_string()),
            ..ProviderRuntimeOptions::default()
        };

        let provider = OpenAiCodexProvider::new(&options, Some("test-key")).unwrap();
        assert!(provider.custom_endpoint);
        assert_eq!(provider.gateway_api_key.as_deref(), Some("test-key"));
    }

    #[test]
    fn resolve_instructions_uses_default_when_missing() {
        assert_eq!(
            resolve_instructions(None),
            DEFAULT_CODEX_INSTRUCTIONS.to_string()
        );
    }

    #[test]
    fn resolve_instructions_uses_default_when_blank() {
        assert_eq!(
            resolve_instructions(Some("   ")),
            DEFAULT_CODEX_INSTRUCTIONS.to_string()
        );
    }

    #[test]
    fn resolve_instructions_uses_system_prompt_when_present() {
        assert_eq!(
            resolve_instructions(Some("Be strict")),
            "Be strict".to_string()
        );
    }

    #[test]
    fn clamp_reasoning_effort_adjusts_known_models() {
        assert_eq!(
            clamp_reasoning_effort("gpt-5-codex", "xhigh"),
            "high".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5-codex", "minimal"),
            "low".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5-codex", "medium"),
            "medium".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5.3-codex", "minimal"),
            "low".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5.1", "xhigh"),
            "high".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5-codex", "xhigh"),
            "high".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5.1-codex-mini", "low"),
            "medium".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5.1-codex-mini", "xhigh"),
            "high".to_string()
        );
        assert_eq!(
            clamp_reasoning_effort("gpt-5.3-codex", "xhigh"),
            "xhigh".to_string()
        );
    }

    #[test]
    fn resolve_reasoning_effort_prefers_config_override() {
        let _env_lock = env_lock();
        let _reasoning_guard = EnvGuard::set("ZEROCLAW_CODEX_REASONING_EFFORT", Some("low"));

        assert_eq!(
            resolve_reasoning_effort("gpt-5-codex", Some("xhigh")),
            "high".to_string()
        );
    }

    #[test]
    fn resolve_reasoning_effort_falls_back_to_env_when_override_invalid() {
        let _env_lock = env_lock();
        let _reasoning_guard = EnvGuard::set("ZEROCLAW_CODEX_REASONING_EFFORT", Some("medium"));

        assert_eq!(
            resolve_reasoning_effort("gpt-5-codex", Some("banana")),
            "medium".to_string()
        );
    }

    #[test]
    fn resolve_reasoning_effort_prefers_runtime_override() {
        assert_eq!(
            resolve_reasoning_effort("gpt-5-codex", Some("minimal")),
            "low".to_string()
        );
        assert_eq!(
            resolve_reasoning_effort("gpt-5.3-codex", Some("xhigh")),
            "xhigh".to_string()
        );
    }

    #[test]
    fn parse_sse_text_reads_output_text_delta() {
        let payload = r#"data: {"type":"response.created","response":{"id":"resp_123"}}

data: {"type":"response.output_text.delta","delta":"Hello"}
data: {"type":"response.output_text.delta","delta":" world"}
data: {"type":"response.completed","response":{"output_text":"Hello world"}}
data: [DONE]
"#;

        assert_eq!(
            parse_sse_text(payload).unwrap().as_deref(),
            Some("Hello world")
        );
    }

    #[test]
    fn parse_sse_text_falls_back_to_completed_response() {
        let payload = r#"data: {"type":"response.completed","response":{"output_text":"Done"}}
data: [DONE]
"#;

        assert_eq!(parse_sse_text(payload).unwrap().as_deref(), Some("Done"));
    }

    #[test]
    fn build_responses_input_maps_content_types_by_role() {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: "You are helpful.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Hi".into(),
            },
            ChatMessage {
                role: "assistant".into(),
                content: "Hello!".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Thanks".into(),
            },
        ];
        let (instructions, input) = build_responses_input(&messages);
        assert_eq!(instructions, "You are helpful.");
        assert_eq!(input.len(), 3);

        let json: Vec<Value> = input
            .iter()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect();
        assert_eq!(json[0]["role"], "user");
        assert_eq!(json[0]["content"][0]["type"], "input_text");
        assert_eq!(json[1]["role"], "assistant");
        assert_eq!(json[1]["content"][0]["type"], "output_text");
        assert_eq!(json[2]["role"], "user");
        assert_eq!(json[2]["content"][0]["type"], "input_text");
    }

    #[test]
    fn build_responses_input_uses_default_instructions_without_system() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "Hello".into(),
        }];
        let (instructions, input) = build_responses_input(&messages);
        assert_eq!(instructions, DEFAULT_CODEX_INSTRUCTIONS);
        assert_eq!(input.len(), 1);
    }

    #[test]
    fn build_responses_input_ignores_unknown_roles() {
        let messages = vec![
            ChatMessage {
                role: "tool".into(),
                content: "result".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Go".into(),
            },
        ];
        let (instructions, input) = build_responses_input(&messages);
        assert_eq!(instructions, DEFAULT_CODEX_INSTRUCTIONS);
        assert_eq!(input.len(), 1);
        let json = serde_json::to_value(&input[0]).unwrap();
        assert_eq!(json["role"], "user");
    }

    #[test]
    fn build_responses_input_handles_image_markers() {
        let messages = vec![ChatMessage::user(
            "Describe this\n\n[IMAGE:data:image/png;base64,abc]",
        )];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0].role, "user");
        assert_eq!(input[0].content.len(), 2);

        let json: Vec<Value> = input[0]
            .content
            .iter()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect();

        // First content = text
        assert_eq!(json[0]["type"], "input_text");
        assert!(json[0]["text"].as_str().unwrap().contains("Describe this"));

        // Second content = image
        assert_eq!(json[1]["type"], "input_image");
        assert_eq!(json[1]["image_url"], "data:image/png;base64,abc");
    }

    #[test]
    fn build_responses_input_preserves_text_only_messages() {
        let messages = vec![ChatMessage::user("Hello without images")];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0].content.len(), 1);

        let json = serde_json::to_value(&input[0].content[0]).unwrap();
        assert_eq!(json["type"], "input_text");
        assert_eq!(json["text"], "Hello without images");
    }

    #[test]
    fn build_responses_input_handles_multiple_images() {
        let messages = vec![ChatMessage::user(
            "Compare these: [IMAGE:data:image/png;base64,img1] and [IMAGE:data:image/jpeg;base64,img2]",
        )];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        assert_eq!(input[0].content.len(), 3); // text + 2 images

        let json: Vec<Value> = input[0]
            .content
            .iter()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect();

        assert_eq!(json[0]["type"], "input_text");
        assert_eq!(json[1]["type"], "input_image");
        assert_eq!(json[2]["type"], "input_image");
    }

    #[test]
    fn capabilities_includes_vision() {
        let options = ProviderRuntimeOptions {
            provider_api_url: None,
            provider_transport: None,
            zeroclaw_dir: None,
            secrets_encrypt: false,
            auth_profile_override: None,
            reasoning_enabled: None,
            reasoning_level: None,
            reasoning_effort: None,
            custom_provider_api_mode: None,
            max_tokens_override: None,
            model_support_vision: None,
        };
        let provider =
            OpenAiCodexProvider::new(&options, None).expect("provider should initialize");
        let caps = provider.capabilities();

        assert!(!caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn summarize_quota_payload_prefers_codex_usage_windows() {
        let payload = serde_json::json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 37,
                    "reset_after_seconds": 1498,
                    "reset_at": 1772036101
                },
                "secondary_window": {
                    "used_percent": 98,
                    "reset_after_seconds": 98099,
                    "reset_at": 1772132702
                }
            }
        });

        let summary = summarize_quota_payload(&payload).expect("quota summary");
        assert!(summary.contains("5h remaining=63% (used=37%"));
        assert!(summary.contains("weekly remaining=2% (used=98%"));
    }

    #[tokio::test]
    #[ignore = "Live diagnostic probe for OpenAI Codex account quota endpoints"]
    async fn openai_codex_live_quota_endpoint_probe() {
        if std::env::var("ZEROCLAW_LIVE_QUOTA_PROBE").ok().as_deref() != Some("1") {
            eprintln!("Skipping live probe. Set ZEROCLAW_LIVE_QUOTA_PROBE=1 to run.");
            return;
        }

        let options = ProviderRuntimeOptions::default();
        let state_dir = options
            .zeroclaw_dir
            .clone()
            .unwrap_or_else(default_zeroclaw_dir);
        let auth = AuthService::new(&state_dir, options.secrets_encrypt);
        let profile = auth
            .get_profile("openai-codex", options.auth_profile_override.as_deref())
            .await
            .unwrap()
            .unwrap();
        let access_token = auth
            .get_valid_openai_access_token(options.auth_profile_override.as_deref())
            .await
            .unwrap()
            .unwrap();
        let account_id = profile
            .account_id
            .or_else(|| extract_account_id_from_jwt(&access_token))
            .expect("account id");

        let endpoints = [
            ("GET", "https://chatgpt.com/backend-api/codex/rate_limits"),
            ("GET", "https://chatgpt.com/backend-api/codex/limits"),
            ("GET", "https://chatgpt.com/backend-api/codex/usage"),
            ("GET", "https://chatgpt.com/backend-api/usage_limits"),
            ("GET", "https://chatgpt.com/backend-api/limits"),
            (
                "GET",
                "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27",
            ),
            (
                "GET",
                "https://chatgpt.com/backend-api/accounts/check/v4-2025-02-27",
            ),
            ("GET", "https://chatgpt.com/backend-api/accounts/check"),
            ("GET", "https://chatgpt.com/backend-api/accounts"),
            ("GET", "https://chatgpt.com/backend-api/me"),
            (
                "GET",
                "https://chatgpt.com/backend-api/settings/beta_features",
            ),
            ("GET", "https://chatgpt.com/backend-api/models"),
            ("GET", "https://chatgpt.com/backend-api/codex/client_state"),
            ("GET", "https://chatgpt.com/backend-api/codex/session"),
            ("GET", "https://chatgpt.com/backend-api/codex/entitlements"),
            ("GET", "https://chatgpt.com/backend-api/codex/availability"),
        ];

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mut codex_usage_verified = false;
        for (method, url) in endpoints {
            let request = match method {
                "GET" => client.get(url),
                _ => unreachable!(),
            };
            let response = request
                .header("Authorization", format!("Bearer {access_token}"))
                .header("chatgpt-account-id", account_id.clone())
                .header("accept", "application/json")
                .send()
                .await
                .unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.text().await.unwrap_or_default();
            let json_keys = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .as_object()
                        .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                })
                .unwrap_or_default();
            eprintln!(
                "LIVE_PROBE url={url} status={} root_keys={:?}",
                status.as_u16(),
                json_keys
            );
            if let Some(summary) = summarize_quota_payload(
                &serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
            ) {
                eprintln!("LIVE_PROBE quota_summary url={url} summary={summary}");
                if url.ends_with("/codex/usage") && summary.contains("weekly remaining=") {
                    codex_usage_verified = true;
                }
            }
            let interesting_headers: Vec<String> = headers
                .iter()
                .filter_map(|(name, value)| {
                    let key = name.as_str().to_ascii_lowercase();
                    if key.contains("ratelimit")
                        || key.contains("rate-limit")
                        || key.contains("quota")
                        || key.contains("limit")
                        || key.contains("remaining")
                    {
                        Some(format!("{key}={}", value.to_str().unwrap_or("<binary>")))
                    } else {
                        None
                    }
                })
                .collect();
            if !interesting_headers.is_empty() {
                eprintln!(
                    "LIVE_PROBE headers url={url} headers={}",
                    interesting_headers.join("; ")
                );
            }
        }

        assert!(
            codex_usage_verified,
            "expected /backend-api/codex/usage to expose weekly/5h quota summary"
        );
    }
}
