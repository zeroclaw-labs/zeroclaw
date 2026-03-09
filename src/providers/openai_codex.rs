use crate::auth::openai_oauth::extract_account_id_from_jwt;
use crate::auth::AuthService;
use crate::multimodal;
use crate::providers::traits::{
    ChatMessage, ChatRequest, ChatResponse, Provider, ProviderCapabilities, TokenUsage, ToolCall,
    ToolsPayload,
};
use crate::providers::ProviderRuntimeOptions;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Internal response from Codex API carrying both text and optional native tool calls.
struct CodexResponse {
    text: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<TokenUsage>,
    /// The server-assigned response ID, used for `previous_response_id` incremental sends.
    response_id: Option<String>,
}

const DEFAULT_CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_RESPONSES_URL_ENV: &str = "ZEROCLAW_CODEX_RESPONSES_URL";
const CODEX_BASE_URL_ENV: &str = "ZEROCLAW_CODEX_BASE_URL";
const DEFAULT_CODEX_INSTRUCTIONS: &str =
    "You are ZeroClaw, a concise and helpful coding assistant.";

/// Transport mode for the OpenAI Codex Responses API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    /// Try WebSocket first, fall back to SSE on failure.
    Auto,
    /// Always use a persistent WebSocket connection.
    WebSocket,
    /// Always use HTTP POST + SSE streaming (original behavior).
    Sse,
}

impl Transport {
    fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "websocket" | "ws" => Self::WebSocket,
            "sse" | "http" => Self::Sse,
            _ => Self::Auto,
        }
    }
}

/// Type alias for the WebSocket stream to avoid long generic signatures.
type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Manages a persistent WebSocket connection to the OpenAI Responses API.
/// Maximum age of a WebSocket connection before forcing a reconnect (5 minutes).
const WS_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(300);

struct WsConnectionManager {
    conn: tokio::sync::Mutex<Option<WsStream>>,
    url: String,
    connected_at: tokio::sync::Mutex<Option<std::time::Instant>>,
}

impl WsConnectionManager {
    fn new(url: String) -> Self {
        Self {
            conn: tokio::sync::Mutex::new(None),
            url,
            connected_at: tokio::sync::Mutex::new(None),
        }
    }

    /// Convert an HTTPS URL to its WSS equivalent.
    fn to_ws_url(url: &str) -> String {
        if url.starts_with("https://") {
            format!("wss://{}", &url["https://".len()..])
        } else if url.starts_with("http://") {
            format!("ws://{}", &url["http://".len()..])
        } else {
            url.to_string()
        }
    }

    /// Establish a new WebSocket connection with auth headers.
    async fn connect(&self, bearer_token: &str, account_id: Option<&str>) -> anyhow::Result<()> {
        let ws_url = Self::to_ws_url(&self.url);
        tracing::info!(url = %ws_url, "Opening WebSocket connection to OpenAI Codex");

        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = ws_url
            .into_client_request()
            .map_err(|e| anyhow::anyhow!("Failed to build WS request: {e}"))?;

        let headers = request.headers_mut();
        headers.insert(
            "Authorization",
            format!("Bearer {bearer_token}")
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid auth header: {e}"))?,
        );
        headers.insert(
            "OpenAI-Beta",
            "responses-websocket=v1"
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid OpenAI-Beta header"))?,
        );
        if let Some(acct) = account_id {
            headers.insert(
                "chatgpt-account-id",
                acct.parse()
                    .map_err(|e| anyhow::anyhow!("Invalid account-id header: {e}"))?,
            );
        }

        let (ws_stream, _response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| anyhow::anyhow!("WebSocket connection failed: {e}"))?;

        tracing::info!("WebSocket connection established");
        let mut guard = self.conn.lock().await;
        *guard = Some(ws_stream);
        *self.connected_at.lock().await = Some(std::time::Instant::now());
        Ok(())
    }

    /// Ensure the connection is alive, reconnecting with exponential backoff if needed.
    /// Returns `true` if a new connection was established (reconnected), `false` if reused.
    async fn ensure_connected(
        &self,
        bearer_token: &str,
        account_id: Option<&str>,
    ) -> anyhow::Result<bool> {
        // Check if we already have a live, non-stale connection.
        {
            let guard = self.conn.lock().await;
            if guard.is_some() {
                let age_ok = self
                    .connected_at
                    .lock()
                    .await
                    .map_or(false, |t| t.elapsed() < WS_MAX_AGE);
                if age_ok {
                    return Ok(false);
                }
                // Connection too old — drop and reconnect.
                tracing::info!("WebSocket connection expired, reconnecting");
                drop(guard);
                self.invalidate().await;
            }
        }

        // Reconnect with exponential backoff: 1s, 2s, 4s, 8s, 16s
        let mut delay = std::time::Duration::from_secs(1);
        const MAX_RETRIES: u32 = 5;

        for attempt in 1..=MAX_RETRIES {
            match self.connect(bearer_token, account_id).await {
                Ok(()) => return Ok(true),
                Err(e) => {
                    if attempt == MAX_RETRIES {
                        return Err(anyhow::anyhow!(
                            "WebSocket connection failed after {MAX_RETRIES} attempts: {e}"
                        ));
                    }
                    tracing::warn!(
                        attempt,
                        error = %e,
                        delay_secs = delay.as_secs(),
                        "WebSocket connect failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        }
        unreachable!()
    }

    /// Send a request over WebSocket and receive streaming events until completion.
    /// Returns text and any native function calls from the response.
    async fn send_and_receive(
        &self,
        request: Value,
        delta_tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> anyhow::Result<CodexResponse> {
        let mut guard = self.conn.lock().await;
        let ws = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("WebSocket not connected"))?;

        // Build response.create envelope — fields must be at the top level
        // (not nested under a "response" key). This matches the OpenAI
        // Responses API WebSocket spec.
        let mut envelope = if request.is_object() {
            request.clone()
        } else {
            serde_json::json!({})
        };
        envelope["type"] = serde_json::json!("response.create");
        if envelope.get("store").is_none() {
            envelope["store"] = serde_json::json!(false);
        }
        let payload = serde_json::to_string(&envelope)?;
        tracing::info!(len = payload.len(), "Sending WebSocket request");

        ws.send(WsMessage::Text(payload.into()))
            .await
            .map_err(|e| anyhow::anyhow!("WebSocket send failed: {e}"))?;

        // Receive events until response.completed / response.failed / error
        let mut saw_delta = false;
        let mut delta_accumulator = String::new();
        let mut fallback_text: Option<String> = None;
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<TokenUsage> = None;
        let mut response_id: Option<String> = None;

        const MSG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

        loop {
            let msg = tokio::time::timeout(MSG_TIMEOUT, ws.next())
                .await
                .map_err(|_| {
                    anyhow::anyhow!("WebSocket read timed out after {}s", MSG_TIMEOUT.as_secs())
                })?;

            let msg = match msg {
                Some(Ok(m)) => m,
                Some(Err(e)) => {
                    *guard = None;
                    return Err(anyhow::anyhow!("WebSocket read error: {e}"));
                }
                None => {
                    *guard = None;
                    break;
                }
            };

            match msg {
                WsMessage::Text(text) => {
                    let event: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    let event_type = event.get("type").and_then(Value::as_str);

                    if let Some(message) = extract_stream_error_message(&event) {
                        if message.contains("not found") && message.contains("previous_response") {
                            return Err(anyhow::anyhow!("PREV_RESP_NOT_FOUND:{message}"));
                        }
                        return Err(anyhow::anyhow!("OpenAI Codex WebSocket error: {message}"));
                    }

                    if let Some(text) = extract_stream_event_text(&event, saw_delta) {
                        if event_type == Some("response.output_text.delta") {
                            saw_delta = true;
                            delta_accumulator.push_str(&text);
                            if let Some(tx) = &delta_tx {
                                let _ = tx.try_send(text);
                            }
                        } else if fallback_text.is_none() {
                            fallback_text = Some(text);
                        }
                    }

                    match event_type {
                        Some("response.completed" | "response.done") => {
                            // Extract function_call items, usage, and response ID from the completed response
                            if let Some(response) = event.get("response") {
                                tool_calls = extract_function_calls_from_response(response);
                                usage = extract_usage_from_response(response);
                                response_id =
                                    response.get("id").and_then(Value::as_str).map(String::from);
                            }
                            break;
                        }
                        Some("response.failed") => {
                            if saw_delta && !delta_accumulator.is_empty() {
                                break;
                            }
                            return Err(anyhow::anyhow!(
                                "OpenAI Codex response failed (no details)"
                            ));
                        }
                        _ => {}
                    }
                }
                WsMessage::Close(_) => {
                    *guard = None;
                    break;
                }
                WsMessage::Ping(data) => {
                    let _ = ws.send(WsMessage::Pong(data)).await;
                }
                _ => {}
            }
        }

        let text = if saw_delta {
            nonempty_preserve(Some(&delta_accumulator)).unwrap_or_default()
        } else {
            fallback_text.unwrap_or_default()
        };

        Ok(CodexResponse {
            text,
            tool_calls,
            usage,
            response_id,
        })
    }

    /// Discard the current connection so the next call reconnects.
    async fn invalidate(&self) {
        let mut guard = self.conn.lock().await;
        *guard = None;
        *self.connected_at.lock().await = None;
    }
}

/// Resolve the transport mode from the `ZEROCLAW_TRANSPORT` env var.
fn resolve_transport() -> Transport {
    std::env::var("ZEROCLAW_TRANSPORT")
        .ok()
        .map(|v| Transport::from_str_lossy(&v))
        .unwrap_or(Transport::Auto)
}

pub struct OpenAiCodexProvider {
    auth: AuthService,
    auth_profile_override: Option<String>,
    responses_url: String,
    custom_endpoint: bool,
    gateway_api_key: Option<String>,
    client: Client,
    /// Optional real-time streaming delta sender (set by agent loop).
    on_delta: std::sync::Mutex<Option<tokio::sync::mpsc::Sender<String>>>,
    /// WebSocket connection manager for persistent connections.
    ws_manager: WsConnectionManager,
    /// Transport mode (Auto/WebSocket/Sse).
    transport: Transport,
    /// Tracks the last response ID for incremental (delta-only) sends.
    /// Shared across both WS and SSE transport paths.
    previous_response_id: tokio::sync::Mutex<Option<String>>,
    /// Number of input items sent in the last request, used to compute
    /// the delta for incremental sends.
    last_input_count: tokio::sync::Mutex<usize>,
    /// When `false`, always sends full context (never uses `previous_response_id`).
    /// Subagents set this to `false` to avoid cross-session race conditions.
    incremental_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<serde_json::Value>,
    instructions: String,
    store: bool,
    stream: bool,
    text: ResponsesTextOptions,
    reasoning: ResponsesReasoningOptions,
    include: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ResponsesTextOptions {
    verbosity: String,
}

#[derive(Debug, Clone, Serialize)]
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

        let transport = resolve_transport();
        let ws_manager = WsConnectionManager::new(responses_url.clone());

        Ok(Self {
            auth,
            auth_profile_override: options.auth_profile_override.clone(),
            custom_endpoint: !is_default_responses_url(&responses_url),
            responses_url,
            gateway_api_key: gateway_api_key.map(ToString::to_string),
            client: Client::builder()
                // No total-lifecycle timeout — SSE streams can run for
                // minutes.  Per-chunk timeouts are applied inside
                // decode_responses_body via tokio::time::timeout.
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
            on_delta: std::sync::Mutex::new(None),
            ws_manager,
            transport,
            previous_response_id: tokio::sync::Mutex::new(None),
            last_input_count: tokio::sync::Mutex::new(0),
            incremental_enabled: true,
        })
    }

    /// Create a provider that never uses incremental sends (`previous_response_id`).
    /// Each request sends the full conversation context, avoiding cross-session
    /// race conditions when multiple agents share the same OpenAI account.
    pub fn new_non_incremental(
        options: &ProviderRuntimeOptions,
        gateway_api_key: Option<&str>,
    ) -> anyhow::Result<Self> {
        let mut provider = Self::new(options, gateway_api_key)?;
        provider.incremental_enabled = false;
        Ok(provider)
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

fn resolve_instructions(system_prompt: Option<&str>) -> String {
    first_nonempty(system_prompt).unwrap_or_else(|| DEFAULT_CODEX_INSTRUCTIONS.to_string())
}

fn normalize_model_id(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

fn build_responses_input(messages: &[ChatMessage]) -> (String, Vec<serde_json::Value>) {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut input: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => system_parts.push(&msg.content),
            "user" => {
                let (cleaned_text, image_refs) = multimodal::parse_image_markers(&msg.content);

                let mut content_items: Vec<serde_json::Value> = Vec::new();

                // Add text if present
                if !cleaned_text.trim().is_empty() {
                    content_items.push(serde_json::json!({
                        "type": "input_text",
                        "text": cleaned_text,
                    }));
                }

                // Add images
                for image_ref in image_refs {
                    content_items.push(serde_json::json!({
                        "type": "input_image",
                        "image_url": image_ref,
                    }));
                }

                // If no content at all, add empty text
                if content_items.is_empty() {
                    content_items.push(serde_json::json!({
                        "type": "input_text",
                        "text": "",
                    }));
                }

                input.push(serde_json::json!({
                    "role": "user",
                    "content": content_items,
                }));
            }
            "assistant" => {
                // Check if this is a native tool call history message (JSON with tool_calls)
                if let Ok(parsed) = serde_json::from_str::<Value>(&msg.content) {
                    if let Some(tool_calls_arr) = parsed.get("tool_calls").and_then(Value::as_array)
                    {
                        // Emit assistant text as output_text if present
                        if let Some(text) = parsed.get("content").and_then(Value::as_str) {
                            if !text.is_empty() {
                                input.push(serde_json::json!({
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{"type": "output_text", "text": text}],
                                }));
                            }
                        }
                        // Emit each tool call as a function_call input item
                        for tc in tool_calls_arr {
                            let call_id = tc.get("id").and_then(Value::as_str).unwrap_or_default();
                            let name = tc.get("name").and_then(Value::as_str).unwrap_or_default();
                            let arguments =
                                tc.get("arguments").and_then(Value::as_str).unwrap_or("{}");
                            input.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "name": name,
                                "arguments": arguments,
                            }));
                        }
                        continue;
                    }
                }
                // Plain assistant text
                input.push(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": msg.content}],
                }));
            }
            "tool" => {
                // Tool result message: parse JSON with tool_call_id and content
                if let Ok(parsed) = serde_json::from_str::<Value>(&msg.content) {
                    let call_id = parsed
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let output = parsed.get("content").and_then(Value::as_str).unwrap_or("");
                    input.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                }
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
            "xhigh" => "high".to_string(),
            _ => "high".to_string(),
        };
    }
    if (id.starts_with("gpt-5.2") || id.starts_with("gpt-5.3") || id.starts_with("gpt-5.4"))
        && effort == "minimal"
    {
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

fn resolve_reasoning_effort(model_id: &str) -> String {
    let raw = std::env::var("ZEROCLAW_CODEX_REASONING_EFFORT")
        .ok()
        .and_then(|value| first_nonempty(Some(&value)))
        .unwrap_or_else(|| "xhigh".to_string())
        .to_ascii_lowercase();
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

/// Decode an OpenAI Codex Responses API response by streaming SSE chunks
/// incrementally.  This avoids holding the entire response in memory and —
/// critically — prevents the global reqwest timeout from killing long-running
/// model generations, because `chunk()` yields data as soon as the server
/// sends it.
async fn decode_responses_body(
    mut response: reqwest::Response,
    delta_tx: Option<tokio::sync::mpsc::Sender<String>>,
) -> anyhow::Result<CodexResponse> {
    // ── Incremental SSE streaming ────────────────────────────────
    let mut pending = String::new();
    let mut saw_delta = false;
    let mut delta_accumulator = String::new();
    let mut fallback_text: Option<String> = None;
    let mut is_sse = false;
    let mut json_buf = String::new();
    let mut completed_response: Option<Value> = None;

    // Per-chunk read timeout (5 minutes).  Matches OpenClaw's undici
    // bodyTimeout approach: each individual chunk has time to arrive, but
    // the overall SSE stream can run indefinitely.
    const CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

    while let Some(chunk) = tokio::time::timeout(CHUNK_TIMEOUT, response.chunk())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "OpenAI Codex SSE chunk read timed out after {}s",
                CHUNK_TIMEOUT.as_secs()
            )
        })??
    {
        let chunk_str = String::from_utf8_lossy(&chunk);

        // Detect whether this is an SSE stream on the first chunk.
        if !is_sse && pending.is_empty() && json_buf.is_empty() {
            let trimmed = chunk_str.trim_start();
            if trimmed.starts_with("event:") || trimmed.starts_with("data:") {
                is_sse = true;
            }
        }

        if !is_sse {
            // Not SSE — accumulate raw JSON.
            json_buf.push_str(&chunk_str);
            continue;
        }

        pending.push_str(&chunk_str);

        // Process every complete SSE event block (separated by \n\n).
        while let Some(idx) = pending.find("\n\n") {
            let block = pending[..idx].to_string();
            pending = pending[idx + 2..].to_string();
            process_sse_block(
                &block,
                &mut saw_delta,
                &mut delta_accumulator,
                &mut fallback_text,
                &delta_tx,
                &mut completed_response,
            )?;
        }
    }

    // Flush remaining pending data.
    if is_sse && !pending.trim().is_empty() {
        process_sse_block(
            &pending,
            &mut saw_delta,
            &mut delta_accumulator,
            &mut fallback_text,
            &delta_tx,
            &mut completed_response,
        )?;
    }

    let (tool_calls, usage, response_id) = completed_response
        .as_ref()
        .map(|r| {
            let tc = extract_function_calls_from_response(r);
            let u = extract_usage_from_response(r);
            let rid = r.get("id").and_then(Value::as_str).map(String::from);
            (tc, u, rid)
        })
        .unwrap_or_default();

    if is_sse {
        if saw_delta {
            let text = nonempty_preserve(Some(&delta_accumulator))
                .ok_or_else(|| anyhow::anyhow!("No response from OpenAI Codex (empty delta)"))?;
            return Ok(CodexResponse {
                text,
                tool_calls,
                usage,
                response_id,
            });
        }
        let text =
            fallback_text.ok_or_else(|| anyhow::anyhow!("No response from OpenAI Codex stream"))?;
        return Ok(CodexResponse {
            text,
            tool_calls,
            usage,
            response_id,
        });
    }

    // ── Non-SSE JSON fallback ────────────────────────────────────
    let parsed: ResponsesResponse = serde_json::from_str(&json_buf).map_err(|err| {
        anyhow::anyhow!(
            "OpenAI Codex JSON parse failed: {err}. Payload: {}",
            super::sanitize_api_error(&json_buf)
        )
    })?;
    let text = extract_responses_text(&parsed)
        .ok_or_else(|| anyhow::anyhow!("No response from OpenAI Codex"))?;
    Ok(CodexResponse {
        text,
        tool_calls,
        usage,
        response_id: None,
    })
}

/// Process a single SSE event block (the text between `\n\n` boundaries).
fn process_sse_block(
    block: &str,
    saw_delta: &mut bool,
    delta_accumulator: &mut String,
    fallback_text: &mut Option<String>,
    delta_tx: &Option<tokio::sync::mpsc::Sender<String>>,
    completed_response: &mut Option<Value>,
) -> anyhow::Result<()> {
    let data_lines: Vec<String> = block
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

    // Try parsing as a single JSON object first.
    if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
        return process_sse_event(
            event,
            saw_delta,
            delta_accumulator,
            fallback_text,
            delta_tx,
            completed_response,
        );
    }

    // Fall back to per-line parsing.
    for line in data_lines {
        let line = line.trim();
        if line.is_empty() || line == "[DONE]" {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Value>(line) {
            process_sse_event(
                event,
                saw_delta,
                delta_accumulator,
                fallback_text,
                delta_tx,
                completed_response,
            )?;
        }
    }

    Ok(())
}

/// Process a single parsed SSE JSON event.
fn process_sse_event(
    event: Value,
    saw_delta: &mut bool,
    delta_accumulator: &mut String,
    fallback_text: &mut Option<String>,
    delta_tx: &Option<tokio::sync::mpsc::Sender<String>>,
    completed_response: &mut Option<Value>,
) -> anyhow::Result<()> {
    if let Some(message) = extract_stream_error_message(&event) {
        return Err(anyhow::anyhow!("OpenAI Codex stream error: {message}"));
    }
    if let Some(text) = extract_stream_event_text(&event, *saw_delta) {
        let event_type = event.get("type").and_then(Value::as_str);
        if event_type == Some("response.output_text.delta") {
            *saw_delta = true;
            delta_accumulator.push_str(&text);
            // Forward delta to channel for real-time streaming
            if let Some(tx) = delta_tx {
                let _ = tx.try_send(text);
            }
        } else if fallback_text.is_none() {
            *fallback_text = Some(text);
        }
    }
    // Capture the completed response for function call / usage extraction
    let event_type = event.get("type").and_then(Value::as_str);
    if matches!(event_type, Some("response.completed" | "response.done")) {
        if let Some(response) = event.get("response") {
            *completed_response = Some(response.clone());
        }
    }
    Ok(())
}

/// Extract native function_call items from a completed response object.
fn extract_function_calls_from_response(response: &Value) -> Vec<ToolCall> {
    let mut tool_calls = Vec::new();
    let output = match response.get("output").and_then(Value::as_array) {
        Some(arr) => arr,
        None => return tool_calls,
    };
    for item in output {
        let item_type = item.get("type").and_then(Value::as_str);
        if item_type == Some("function_call") {
            let id = item
                .get("call_id")
                .and_then(Value::as_str)
                .or_else(|| item.get("id").and_then(Value::as_str))
                .unwrap_or_default()
                .to_string();
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}")
                .to_string();
            tool_calls.push(ToolCall {
                id,
                name,
                arguments,
            });
        }
    }
    tool_calls
}

/// Extract usage/token information from a completed response object.
fn extract_usage_from_response(response: &Value) -> Option<TokenUsage> {
    let usage = response.get("usage")?;
    let input = usage.get("input_tokens").and_then(Value::as_u64);
    let output = usage.get("output_tokens").and_then(Value::as_u64);
    if input.is_none() && output.is_none() {
        return None;
    }
    Some(TokenUsage {
        input_tokens: input,
        output_tokens: output,
    })
}

impl OpenAiCodexProvider {
    async fn send_responses_request(
        &self,
        input: Vec<serde_json::Value>,
        instructions: String,
        model: &str,
        tools: Option<Vec<serde_json::Value>>,
    ) -> anyhow::Result<CodexResponse> {
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

        // ── Incremental send logic (previous_response_id) ────────
        // If we have a previous response ID and the input has grown,
        // only send the new tool result items instead of the full context.
        // Keep a clone of full input for SSE fallback (SSE does not support previous_response_id).
        let input_clone = input.clone();
        let full_input_count = input.len();
        let prev_rid = self.previous_response_id.lock().await.clone();
        let last_count = *self.last_input_count.lock().await;

        let (effective_input, effective_prev_id) = if self.incremental_enabled {
            if let Some(ref rid) = prev_rid {
                if last_count > 0 && input.len() > last_count {
                    // Extract only the new items (tool results) since last request
                    let new_items: Vec<serde_json::Value> = input[last_count..]
                        .iter()
                        .filter(|item| {
                            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                        })
                        .cloned()
                        .collect();

                    if new_items.is_empty() {
                        // No tool results in new items — new conversation turn, send full context
                        tracing::info!("No tool results in new items, sending full context");
                        (input, None)
                    } else {
                        tracing::info!(
                            previous_response_id = %rid,
                            full_input_count = input.len(),
                            incremental_count = new_items.len(),
                            "Using incremental send with previous_response_id"
                        );
                        (new_items, Some(rid.clone()))
                    }
                } else {
                    // No growth or first request — send full context
                    (input, None)
                }
            } else {
                (input, None)
            }
        } else {
            // Incremental disabled — always send full context
            (input, None)
        };

        let has_tools = tools.as_ref().map_or(false, |t| !t.is_empty());
        let request = ResponsesRequest {
            model: normalized_model.to_string(),
            input: effective_input,
            instructions,
            store: false,
            stream: true,
            text: ResponsesTextOptions {
                verbosity: "medium".to_string(),
            },
            reasoning: ResponsesReasoningOptions {
                effort: resolve_reasoning_effort(normalized_model),
                summary: "auto".to_string(),
            },
            include: vec!["reasoning.encrypted_content".to_string()],
            tools,
            tool_choice: if has_tools {
                Some("auto".to_string())
            } else {
                None
            },
            parallel_tool_calls: if has_tools { Some(true) } else { None },
            previous_response_id: effective_prev_id,
        };

        let bearer_token = if use_gateway_api_key_auth {
            self.gateway_api_key.as_deref().unwrap_or_default()
        } else {
            access_token.as_deref().unwrap_or_default()
        };

        let delta_tx = self.on_delta.lock().ok().and_then(|g| g.clone());

        // ── WebSocket transport path ───────────────────────────────
        let use_ws = match self.transport {
            Transport::WebSocket => true,
            Transport::Sse => false,
            Transport::Auto => true, // try WS first, fall back to SSE
        };

        if use_ws {
            let ws_result = async {
                let reconnected = self.ws_manager
                    .ensure_connected(bearer_token, account_id.as_deref())
                    .await?;

                // If we reconnected, the new WS session doesn't know old response IDs.
                // Clear incremental state and send full context.
                let ws_request = if reconnected && request.previous_response_id.is_some() {
                    tracing::info!("WebSocket reconnected, clearing previous_response_id and sending full context");
                    *self.previous_response_id.lock().await = None;
                    let full_request = ResponsesRequest {
                        input: input_clone.clone(),
                        previous_response_id: None,
                        ..request.clone()
                    };
                    serde_json::to_value(&full_request)?
                } else {
                    serde_json::to_value(&request)?
                };

                tracing::info!(
                    url = %self.responses_url,
                    model = %normalized_model,
                    transport = "websocket",
                    "Sending OpenAI Codex request via WebSocket"
                );

                self.ws_manager
                    .send_and_receive(ws_request, delta_tx.clone())
                    .await
            }
            .await;

            match ws_result {
                Ok(codex_resp) => {
                    // Detect empty/broken response (no text, no tool calls, no ID)
                    // and treat as WS error to trigger SSE fallback.
                    if codex_resp.text.is_empty()
                        && codex_resp.tool_calls.is_empty()
                        && codex_resp.response_id.is_none()
                    {
                        if self.transport == Transport::WebSocket {
                            return Err(anyhow::anyhow!(
                                "WebSocket returned empty response (no text, no tool calls)"
                            ));
                        }
                        tracing::warn!(
                            "WebSocket returned empty response, invalidating connection and falling back to SSE"
                        );
                        self.ws_manager.invalidate().await;
                        // Clear stale incremental state
                        *self.previous_response_id.lock().await = None;
                        *self.last_input_count.lock().await = 0;
                    } else {
                        tracing::info!(
                            len = codex_resp.text.len(),
                            tool_calls = codex_resp.tool_calls.len(),
                            response_id = ?codex_resp.response_id,
                            "WebSocket response completed"
                        );
                        // Update incremental state for next call
                        if self.incremental_enabled {
                            *self.previous_response_id.lock().await =
                                codex_resp.response_id.clone();
                            *self.last_input_count.lock().await = full_input_count;
                        }
                        return Ok(codex_resp);
                    }
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.starts_with("PREV_RESP_NOT_FOUND:") && self.incremental_enabled {
                        tracing::warn!("previous_response_not_found — retrying with full context");
                        *self.previous_response_id.lock().await = None;
                        *self.last_input_count.lock().await = 0;

                        let retry_request = ResponsesRequest {
                            input: input_clone.clone(),
                            previous_response_id: None,
                            ..request.clone()
                        };
                        let retry_payload = serde_json::to_value(&retry_request)?;
                        match self
                            .ws_manager
                            .send_and_receive(retry_payload, delta_tx.clone())
                            .await
                        {
                            Ok(codex_resp) => {
                                if self.incremental_enabled {
                                    *self.previous_response_id.lock().await =
                                        codex_resp.response_id.clone();
                                    *self.last_input_count.lock().await = full_input_count;
                                }
                                return Ok(codex_resp);
                            }
                            Err(retry_err) => {
                                if self.transport == Transport::WebSocket {
                                    return Err(retry_err);
                                }
                                tracing::warn!(error = %retry_err, "WS retry failed, falling back to SSE");
                                self.ws_manager.invalidate().await;
                            }
                        }
                    } else {
                        if self.transport == Transport::WebSocket {
                            // Strict WS mode — do not fall back.
                            return Err(e);
                        }
                        // Auto mode — fall back to SSE.
                        tracing::warn!(
                            error = %e,
                            "WebSocket request failed, falling back to SSE"
                        );
                        self.ws_manager.invalidate().await;
                        // Clear stale incremental state before SSE fallback
                        *self.previous_response_id.lock().await = None;
                        *self.last_input_count.lock().await = 0;
                    }
                }
            }
        }

        // ── SSE transport path (original) ──────────────────────────
        // SSE endpoint does not support previous_response_id — always send full context.
        let sse_request = ResponsesRequest {
            input: input_clone,
            previous_response_id: None,
            ..request
        };

        let mut request_builder = self
            .client
            .post(&self.responses_url)
            .header("Authorization", format!("Bearer {bearer_token}"))
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "pi")
            .header("accept", "text/event-stream")
            .header("Content-Type", "application/json");

        if let Some(account_id) = account_id.as_deref() {
            request_builder = request_builder.header("chatgpt-account-id", account_id);
        }

        if use_gateway_api_key_auth {
            if let Some(access_token) = access_token.as_deref() {
                request_builder = request_builder.header("x-openai-access-token", access_token);
            }
            if let Some(account_id) = account_id.as_deref() {
                request_builder = request_builder.header("x-openai-account-id", account_id);
            }
        }

        tracing::info!(
            url = %self.responses_url,
            model = %normalized_model,
            transport = "sse",
            "Sending OpenAI Codex request"
        );

        let response = request_builder.json(&sse_request).send().await?;

        tracing::info!(
            status = %response.status(),
            "OpenAI Codex response received"
        );

        if !response.status().is_success() {
            return Err(super::api_error("OpenAI Codex", response).await);
        }

        tracing::info!(has_delta_tx = delta_tx.is_some(), "Starting SSE decode");
        let result = decode_responses_body(response, delta_tx).await;
        match &result {
            Ok(resp) => {
                tracing::info!(
                    len = resp.text.len(),
                    tool_calls = resp.tool_calls.len(),
                    response_id = ?resp.response_id,
                    "SSE decode completed"
                );
                // Update incremental state for next call
                if self.incremental_enabled {
                    *self.previous_response_id.lock().await = resp.response_id.clone();
                    *self.last_input_count.lock().await = full_input_count;
                }
            }
            Err(e) => tracing::error!(error = %e, "SSE decode failed"),
        }
        result
    }
}

#[async_trait]
impl Provider for OpenAiCodexProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: true,
        }
    }

    fn set_on_delta(&self, tx: tokio::sync::mpsc::Sender<String>) {
        if let Ok(mut guard) = self.on_delta.lock() {
            *guard = Some(tx);
        }
    }

    fn clear_on_delta(&self) {
        if let Ok(mut guard) = self.on_delta.lock() {
            *guard = None;
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<String> {
        // Fresh conversation — clear incremental state
        *self.previous_response_id.lock().await = None;
        *self.last_input_count.lock().await = 0;

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
        let resp = self
            .send_responses_request(input, instructions, model, None)
            .await?;
        Ok(resp.text)
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<String> {
        // Fresh conversation — clear incremental state
        *self.previous_response_id.lock().await = None;
        *self.last_input_count.lock().await = 0;

        // Normalize image markers: convert file paths to data URIs
        let config = crate::config::MultimodalConfig::default();
        let prepared = crate::multimodal::prepare_messages_for_provider(messages, &config).await?;

        let (instructions, input) = build_responses_input(&prepared.messages);
        let resp = self
            .send_responses_request(input, instructions, model, None)
            .await?;
        Ok(resp.text)
    }

    fn convert_tools(&self, tools: &[crate::tools::traits::ToolSpec]) -> ToolsPayload {
        let openai_tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|spec| {
                serde_json::json!({
                    "type": "function",
                    "name": spec.name,
                    "description": spec.description,
                    "parameters": spec.parameters,
                })
            })
            .collect();
        ToolsPayload::OpenAI {
            tools: openai_tools,
        }
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let config = crate::config::MultimodalConfig::default();
        let prepared =
            crate::multimodal::prepare_messages_for_provider(request.messages, &config).await?;
        let (instructions, input) = build_responses_input(&prepared.messages);

        // Convert tool specs to OpenAI function tool definitions
        let tools = request.tools.and_then(|specs| {
            if specs.is_empty() {
                return None;
            }
            let openai_tools: Vec<serde_json::Value> = specs
                .iter()
                .map(|spec| {
                    serde_json::json!({
                        "type": "function",
                        "name": spec.name,
                        "description": spec.description,
                        "parameters": spec.parameters,
                    })
                })
                .collect();
            Some(openai_tools)
        });

        let resp = self
            .send_responses_request(input, instructions, model, tools)
            .await?;

        let stop_reason = if resp.tool_calls.is_empty() {
            "stop"
        } else {
            "toolUse"
        };
        tracing::debug!(
            stop_reason,
            tool_calls = resp.tool_calls.len(),
            "Chat response completed"
        );

        Ok(ChatResponse {
            text: if resp.text.is_empty() {
                None
            } else {
                Some(resp.text)
            },
            tool_calls: resp.tool_calls,
            usage: resp.usage,
            reasoning_content: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(input[0]["role"], "user");
        let content = input[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);

        // First content = text
        assert_eq!(content[0]["type"], "input_text");
        assert!(content[0]["text"]
            .as_str()
            .unwrap()
            .contains("Describe this"));

        // Second content = image
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "data:image/png;base64,abc");
    }

    #[test]
    fn build_responses_input_preserves_text_only_messages() {
        let messages = vec![ChatMessage::user("Hello without images")];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        let content = input[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "Hello without images");
    }

    #[test]
    fn build_responses_input_handles_multiple_images() {
        let messages = vec![ChatMessage::user(
            "Compare these: [IMAGE:data:image/png;base64,img1] and [IMAGE:data:image/jpeg;base64,img2]",
        )];
        let (_, input) = build_responses_input(&messages);

        assert_eq!(input.len(), 1);
        let content = input[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3); // text + 2 images

        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[2]["type"], "input_image");
    }

    #[test]
    fn capabilities_includes_vision() {
        let options = ProviderRuntimeOptions {
            provider_api_url: None,
            zeroclaw_dir: None,
            secrets_encrypt: false,
            auth_profile_override: None,
            reasoning_enabled: None,
            disable_incremental: false,
        };
        let provider =
            OpenAiCodexProvider::new(&options, None).expect("provider should initialize");
        let caps = provider.capabilities();

        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn transport_from_str_lossy_parses_known_values() {
        assert_eq!(Transport::from_str_lossy("websocket"), Transport::WebSocket);
        assert_eq!(Transport::from_str_lossy("ws"), Transport::WebSocket);
        assert_eq!(Transport::from_str_lossy("WS"), Transport::WebSocket);
        assert_eq!(Transport::from_str_lossy("sse"), Transport::Sse);
        assert_eq!(Transport::from_str_lossy("http"), Transport::Sse);
        assert_eq!(Transport::from_str_lossy("SSE"), Transport::Sse);
        assert_eq!(Transport::from_str_lossy("auto"), Transport::Auto);
        assert_eq!(Transport::from_str_lossy("anything"), Transport::Auto);
        assert_eq!(Transport::from_str_lossy(""), Transport::Auto);
    }

    #[test]
    fn ws_connection_manager_converts_urls() {
        assert_eq!(
            WsConnectionManager::to_ws_url("https://api.openai.com/v1/responses"),
            "wss://api.openai.com/v1/responses"
        );
        assert_eq!(
            WsConnectionManager::to_ws_url("http://localhost:8080/v1/responses"),
            "ws://localhost:8080/v1/responses"
        );
        assert_eq!(
            WsConnectionManager::to_ws_url("wss://already.ws/path"),
            "wss://already.ws/path"
        );
    }

    #[test]
    fn resolve_transport_defaults_to_auto() {
        let _guard = EnvGuard::set("ZEROCLAW_TRANSPORT", None);
        assert_eq!(resolve_transport(), Transport::Auto);
    }

    #[test]
    fn resolve_transport_reads_env_var() {
        let _guard = EnvGuard::set("ZEROCLAW_TRANSPORT", Some("websocket"));
        assert_eq!(resolve_transport(), Transport::WebSocket);
    }

    #[test]
    fn resolve_transport_reads_sse_from_env() {
        let _guard = EnvGuard::set("ZEROCLAW_TRANSPORT", Some("sse"));
        assert_eq!(resolve_transport(), Transport::Sse);
    }

    #[test]
    fn constructor_defaults_to_auto_transport() {
        let _guard = EnvGuard::set("ZEROCLAW_TRANSPORT", None);
        let options = ProviderRuntimeOptions::default();
        let provider =
            OpenAiCodexProvider::new(&options, None).expect("provider should initialize");
        assert_eq!(provider.transport, Transport::Auto);
    }
}
