//! Generic OpenAI-compatible provider.
//! Most LLM APIs follow the same `/v1/chat/completions` format.
//! This module provides a single implementation that works for all of them.

use crate::multimodal;
use crate::providers::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, StreamChunk, StreamError, StreamOptions, StreamResult, TokenUsage,
    ToolCall as ProviderToolCall,
};
use async_trait::async_trait;
use futures_util::{stream, SinkExt, StreamExt};
use reqwest::{
    header::{HeaderMap, HeaderValue, USER_AGENT},
    Client,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::header::{HeaderName, AUTHORIZATION},
        http::HeaderValue as WsHeaderValue,
        Message as WsMessage,
    },
};

/// A provider that speaks the OpenAI-compatible chat completions API.
/// Used by: Venice, Vercel AI Gateway, Cloudflare AI Gateway, Moonshot,
/// Synthetic, `OpenCode` Zen, `Z.AI`, `GLM`, `MiniMax`, Bedrock, Qianfan, Groq, Mistral, `xAI`, etc.
#[allow(clippy::struct_excessive_bools)]
pub struct OpenAiCompatibleProvider {
    pub(crate) name: String,
    pub(crate) base_url: String,
    pub(crate) credential: Option<String>,
    pub(crate) auth_header: AuthStyle,
    supports_vision: bool,
    /// When false, do not fall back to /v1/responses on chat completions 404.
    /// GLM/Zhipu does not support the responses API.
    supports_responses_fallback: bool,
    user_agent: Option<String>,
    /// When true, collect all `system` messages and prepend their content
    /// to the first `user` message, then drop the system messages.
    /// Required for providers that reject `role: system` (e.g. MiniMax).
    merge_system_into_user: bool,
    /// Whether this provider supports OpenAI-style native tool calling.
    /// When false, tools are injected into the system prompt as text.
    native_tool_calling: bool,
    /// Selects the primary protocol for this compatible endpoint.
    api_mode: CompatibleApiMode,
    /// Optional max token cap propagated to outbound requests.
    max_tokens_override: Option<u32>,
}

/// How the provider expects the API key to be sent.
#[derive(Debug, Clone)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `x-api-key: <key>` (used by some Chinese providers)
    XApiKey,
    /// Custom header name
    Custom(String),
}

/// API mode for OpenAI-compatible endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibleApiMode {
    /// Default mode: call chat-completions first and optionally fallback.
    OpenAiChatCompletions,
    /// Responses-first mode: call `/responses` directly.
    OpenAiResponses,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
    ) -> Self {
        Self::new_with_options(
            name,
            base_url,
            credential,
            auth_style,
            false,
            true,
            None,
            false,
            CompatibleApiMode::OpenAiChatCompletions,
            None,
        )
    }

    pub fn new_with_vision(
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
        supports_vision: bool,
    ) -> Self {
        Self::new_with_options(
            name,
            base_url,
            credential,
            auth_style,
            supports_vision,
            true,
            None,
            false,
            CompatibleApiMode::OpenAiChatCompletions,
            None,
        )
    }

    /// Same as `new` but skips the /v1/responses fallback on 404.
    /// Use for providers (e.g. GLM) that only support chat completions.
    pub fn new_no_responses_fallback(
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
    ) -> Self {
        Self::new_with_options(
            name,
            base_url,
            credential,
            auth_style,
            false,
            false,
            None,
            false,
            CompatibleApiMode::OpenAiChatCompletions,
            None,
        )
    }

    /// Create a provider with a custom User-Agent header.
    ///
    /// Some providers (for example Kimi Code) require a specific User-Agent
    /// for request routing and policy enforcement.
    pub fn new_with_user_agent(
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
        user_agent: &str,
    ) -> Self {
        Self::new_with_options(
            name,
            base_url,
            credential,
            auth_style,
            false,
            true,
            Some(user_agent),
            false,
            CompatibleApiMode::OpenAiChatCompletions,
            None,
        )
    }

    pub fn new_with_user_agent_and_vision(
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
        user_agent: &str,
        supports_vision: bool,
    ) -> Self {
        Self::new_with_options(
            name,
            base_url,
            credential,
            auth_style,
            supports_vision,
            true,
            Some(user_agent),
            false,
            CompatibleApiMode::OpenAiChatCompletions,
            None,
        )
    }

    /// For providers that do not support `role: system` (e.g. MiniMax).
    /// System prompt content is prepended to the first user message instead.
    pub fn new_merge_system_into_user(
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
    ) -> Self {
        Self::new_with_options(
            name,
            base_url,
            credential,
            auth_style,
            false,
            false,
            None,
            true,
            CompatibleApiMode::OpenAiChatCompletions,
            None,
        )
    }

    /// Constructor used by `custom:` providers to choose explicit protocol mode.
    pub fn new_custom_with_mode(
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
        supports_vision: bool,
        api_mode: CompatibleApiMode,
        max_tokens_override: Option<u32>,
    ) -> Self {
        Self::new_with_options(
            name,
            base_url,
            credential,
            auth_style,
            supports_vision,
            true,
            None,
            false,
            api_mode,
            max_tokens_override,
        )
    }

    fn new_with_options(
        name: &str,
        base_url: &str,
        credential: Option<&str>,
        auth_style: AuthStyle,
        supports_vision: bool,
        supports_responses_fallback: bool,
        user_agent: Option<&str>,
        merge_system_into_user: bool,
        api_mode: CompatibleApiMode,
        max_tokens_override: Option<u32>,
    ) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            credential: credential.map(ToString::to_string),
            auth_header: auth_style,
            supports_vision,
            supports_responses_fallback,
            user_agent: user_agent.map(ToString::to_string),
            merge_system_into_user,
            native_tool_calling: !merge_system_into_user,
            api_mode,
            max_tokens_override: max_tokens_override.filter(|value| *value > 0),
        }
    }

    /// Collect all `system` role messages, concatenate their content,
    /// and prepend to the first `user` message. Drop all system messages.
    /// Used for providers (e.g. MiniMax) that reject `role: system`.
    fn flatten_system_messages(messages: &[ChatMessage]) -> Vec<ChatMessage> {
        let system_content: String = messages
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        if system_content.is_empty() {
            return messages.to_vec();
        }

        let mut result: Vec<ChatMessage> = messages
            .iter()
            .filter(|m| m.role != "system")
            .cloned()
            .collect();

        if let Some(first_user) = result.iter_mut().find(|m| m.role == "user") {
            first_user.content = format!("{system_content}\n\n{}", first_user.content);
        } else {
            // No user message found: insert a synthetic user message with system content
            result.insert(0, ChatMessage::user(&system_content));
        }

        result
    }

    fn http_client(&self) -> Client {
        if let Some(ua) = self.user_agent.as_deref() {
            let mut headers = HeaderMap::new();
            if let Ok(value) = HeaderValue::from_str(ua) {
                headers.insert(USER_AGENT, value);
            }

            let builder = Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .default_headers(headers);
            let builder =
                crate::config::apply_runtime_proxy_to_builder(builder, "provider.compatible");

            return builder.build().unwrap_or_else(|error| {
                tracing::warn!("Failed to build proxied timeout client with user-agent: {error}");
                Client::new()
            });
        }

        crate::config::build_runtime_proxy_client_with_timeouts("provider.compatible", 120, 10)
    }

    /// Build the full URL for chat completions, detecting if base_url already includes the path.
    /// This allows custom providers with non-standard endpoints (e.g., VolcEngine ARK uses
    /// `/api/coding/v3/chat/completions` instead of `/v1/chat/completions`).
    fn chat_completions_url(&self) -> String {
        let has_full_endpoint = reqwest::Url::parse(&self.base_url)
            .map(|url| {
                url.path()
                    .trim_end_matches('/')
                    .ends_with("/chat/completions")
            })
            .unwrap_or_else(|_| {
                self.base_url
                    .trim_end_matches('/')
                    .ends_with("/chat/completions")
            });

        if has_full_endpoint {
            self.base_url.clone()
        } else {
            format!("{}/chat/completions", self.base_url)
        }
    }

    fn path_ends_with(&self, suffix: &str) -> bool {
        if let Ok(url) = reqwest::Url::parse(&self.base_url) {
            return url.path().trim_end_matches('/').ends_with(suffix);
        }

        self.base_url.trim_end_matches('/').ends_with(suffix)
    }

    fn has_explicit_api_path(&self) -> bool {
        let Ok(url) = reqwest::Url::parse(&self.base_url) else {
            return false;
        };

        let path = url.path().trim_end_matches('/');
        !path.is_empty() && path != "/"
    }

    /// Build the full URL for responses API, detecting if base_url already includes the path.
    fn responses_url(&self) -> String {
        if self.path_ends_with("/responses") {
            return self.base_url.clone();
        }

        let normalized_base = self.base_url.trim_end_matches('/');

        // If chat endpoint is explicitly configured, derive sibling responses endpoint.
        if let Some(prefix) = normalized_base.strip_suffix("/chat/completions") {
            return format!("{prefix}/responses");
        }

        // If an explicit API path already exists (e.g. /v1, /openai, /api/coding/v3),
        // append responses directly to avoid duplicate /v1 segments.
        if self.has_explicit_api_path() {
            format!("{normalized_base}/responses")
        } else {
            format!("{normalized_base}/v1/responses")
        }
    }

    fn tool_specs_to_openai_format(tools: &[crate::tools::ToolSpec]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters
                    }
                })
            })
            .collect()
    }
}

#[derive(Debug, Serialize)]
struct ApiChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: MessageContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<MessagePart>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MessagePart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlPart },
}

#[derive(Debug, Serialize)]
struct ImageUrlPart {
    url: String,
}

#[derive(Debug, Deserialize)]
struct ApiChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<UsageInfo>,
}

#[derive(Debug, Deserialize)]
struct UsageInfo {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

/// Remove `<think>...</think>` blocks from model output.
/// Some reasoning models (e.g. MiniMax) embed their chain-of-thought inline
/// in the `content` field rather than a separate `reasoning_content` field.
/// The resulting `<think>` tags must be stripped before returning to the user.
fn strip_think_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        if let Some(start) = rest.find("<think>") {
            result.push_str(&rest[..start]);
            if let Some(end) = rest[start..].find("</think>") {
                rest = &rest[start + end + "</think>".len()..];
            } else {
                // Unclosed tag: drop the rest to avoid leaking partial reasoning.
                break;
            }
        } else {
            result.push_str(rest);
            break;
        }
    }
    result.trim().to_string()
}

#[derive(Debug, Deserialize, Serialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning/thinking models (e.g. Qwen3, GLM-4) may return their output
    /// in `reasoning_content` instead of `content`. Used as automatic fallback.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

impl ResponseMessage {
    /// Extract text content, falling back to `reasoning_content` when `content`
    /// is missing or empty. Reasoning/thinking models (Qwen3, GLM-4, etc.)
    /// often return their output solely in `reasoning_content`.
    /// Strips `<think>...</think>` blocks that some models (e.g. MiniMax) embed
    /// inline in `content` instead of using a separate field.
    fn effective_content(&self) -> String {
        if let Some(content) = self.content.as_ref().filter(|c| !c.is_empty()) {
            let stripped = strip_think_tags(content);
            if !stripped.is_empty() {
                return stripped;
            }
        }

        self.reasoning_content
            .as_ref()
            .map(|c| strip_think_tags(c))
            .filter(|c| !c.is_empty())
            .unwrap_or_default()
    }

    fn effective_content_optional(&self) -> Option<String> {
        if let Some(content) = self.content.as_ref().filter(|c| !c.is_empty()) {
            let stripped = strip_think_tags(content);
            if !stripped.is_empty() {
                return Some(stripped);
            }
        }

        self.reasoning_content
            .as_ref()
            .map(|c| strip_think_tags(c))
            .filter(|c| !c.is_empty())
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type")]
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    function: Option<Function>,

    // Compatibility: Some providers (e.g., older GLM) may use 'name' directly
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,

    // Compatibility: DeepSeek sometimes wraps arguments differently
    #[serde(rename = "parameters", default)]
    parameters: Option<serde_json::Value>,
}

impl ToolCall {
    /// Extract function name with fallback logic for various provider formats
    fn function_name(&self) -> Option<String> {
        // Standard OpenAI format: tool_calls[].function.name
        if let Some(ref func) = self.function {
            if let Some(ref name) = func.name {
                return Some(name.clone());
            }
        }
        // Fallback: direct name field
        self.name.clone()
    }

    /// Extract arguments with fallback logic and type conversion
    fn function_arguments(&self) -> Option<String> {
        // Standard OpenAI format: tool_calls[].function.arguments (string)
        if let Some(ref func) = self.function {
            if let Some(ref args) = func.arguments {
                return Some(args.clone());
            }
        }
        // Fallback: direct arguments field
        if let Some(ref args) = self.arguments {
            return Some(args.clone());
        }
        // Compatibility: Some providers return parameters as object instead of string
        if let Some(ref params) = self.parameters {
            return serde_json::to_string(params).ok();
        }
        None
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Function {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    messages: Vec<NativeMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Serialize)]
struct NativeMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
    /// Raw reasoning content from thinking models; pass-through for providers
    /// that require it in assistant tool-call history messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<ResponsesInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResponsesInput {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize, Clone)]
struct ResponsesResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    output: Vec<ResponsesOutput>,
    #[serde(default)]
    output_text: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct ResponsesOutput {
    #[serde(rename = "type")]
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    content: Vec<ResponsesContent>,
}

#[derive(Debug, Deserialize, Clone)]
struct ResponsesContent {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResponsesWebSocketCreateEvent {
    #[serde(rename = "type")]
    kind: &'static str,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<String>,
    input: Vec<ResponsesInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

// ---------------------------------------------------------------
// Streaming support (SSE parser)
// ---------------------------------------------------------------

/// Server-Sent Event stream chunk for OpenAI-compatible streaming.
#[derive(Debug, Deserialize)]
struct StreamChunkResponse {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning/thinking models may stream output via `reasoning_content`.
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// Parse SSE (Server-Sent Events) stream from OpenAI-compatible providers.
/// Handles the `data: {...}` format and `[DONE]` sentinel.
fn parse_sse_line(line: &str) -> StreamResult<Option<String>> {
    let line = line.trim();

    // Skip empty lines and comments
    if line.is_empty() || line.starts_with(':') {
        return Ok(None);
    }

    // SSE format: "data: {...}"
    if let Some(data) = line.strip_prefix("data:") {
        let data = data.trim();

        // Check for [DONE] sentinel
        if data == "[DONE]" {
            return Ok(None);
        }

        // Parse JSON delta
        let chunk: StreamChunkResponse = serde_json::from_str(data).map_err(StreamError::Json)?;

        // Extract content from delta
        if let Some(choice) = chunk.choices.first() {
            if let Some(content) = &choice.delta.content {
                if !content.is_empty() {
                    return Ok(Some(content.clone()));
                }
            }
            // Fallback to reasoning_content for thinking models
            if let Some(reasoning) = &choice.delta.reasoning_content {
                return Ok(Some(reasoning.clone()));
            }
        }
    }

    Ok(None)
}

/// Convert SSE byte stream to text chunks.
fn sse_bytes_to_chunks(
    response: reqwest::Response,
    count_tokens: bool,
) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
    // Create a channel to send chunks
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

    tokio::spawn(async move {
        // Buffer for incomplete lines
        let mut buffer = String::new();

        // Get response body as bytes stream
        match response.error_for_status_ref() {
            Ok(_) => {}
            Err(e) => {
                let _ = tx.send(Err(StreamError::Http(e))).await;
                return;
            }
        }

        let mut bytes_stream = response.bytes_stream();

        while let Some(item) = bytes_stream.next().await {
            match item {
                Ok(bytes) => {
                    // Convert bytes to string and process line by line
                    let text = match String::from_utf8(bytes.to_vec()) {
                        Ok(t) => t,
                        Err(e) => {
                            let _ = tx
                                .send(Err(StreamError::InvalidSse(format!(
                                    "Invalid UTF-8: {}",
                                    e
                                ))))
                                .await;
                            break;
                        }
                    };

                    buffer.push_str(&text);

                    // Process complete lines
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer.drain(..=pos).collect::<String>();
                        buffer = buffer[pos + 1..].to_string();

                        match parse_sse_line(&line) {
                            Ok(Some(content)) => {
                                let mut chunk = StreamChunk::delta(content);
                                if count_tokens {
                                    chunk = chunk.with_token_estimate();
                                }
                                if tx.send(Ok(chunk)).await.is_err() {
                                    return; // Receiver dropped
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e))).await;
                    break;
                }
            }
        }

        // Send final chunk
        let _ = tx.send(Ok(StreamChunk::final_chunk())).await;
    });

    // Convert channel receiver to stream
    stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|chunk| (chunk, rx))
    })
    .boxed()
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

fn normalize_responses_role(role: &str) -> &'static str {
    match role {
        "assistant" | "tool" => "assistant",
        _ => "user",
    }
}

fn build_responses_prompt(messages: &[ChatMessage]) -> (Option<String>, Vec<ResponsesInput>) {
    let mut instructions_parts = Vec::new();
    let mut input = Vec::new();

    for message in messages {
        if message.content.trim().is_empty() {
            continue;
        }

        if message.role == "system" {
            instructions_parts.push(message.content.clone());
            continue;
        }

        input.push(ResponsesInput {
            role: normalize_responses_role(&message.role).to_string(),
            content: message.content.clone(),
        });
    }

    let instructions = if instructions_parts.is_empty() {
        None
    } else {
        Some(instructions_parts.join("\n\n"))
    };

    (instructions, input)
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

fn extract_responses_tool_calls(response: &ResponsesResponse) -> Vec<ProviderToolCall> {
    response
        .output
        .iter()
        .filter(|item| item.kind.as_deref() == Some("function_call"))
        .filter_map(|item| {
            let name = item.name.clone()?;
            let arguments = item.arguments.clone().unwrap_or_else(|| "{}".to_string());
            Some(ProviderToolCall {
                id: item
                    .call_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name,
                arguments,
            })
        })
        .collect()
}

fn parse_responses_chat_response(response: ResponsesResponse) -> ProviderChatResponse {
    let text = extract_responses_text(&response);
    let tool_calls = extract_responses_tool_calls(&response);
    ProviderChatResponse {
        text,
        tool_calls,
        usage: None,
        reasoning_content: None,
    }
}

fn extract_responses_stream_error_message(event: &Value) -> Option<String> {
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

fn extract_responses_stream_text_event(event: &Value, saw_delta: bool) -> Option<String> {
    let event_type = event.get("type").and_then(Value::as_str);
    match event_type {
        Some("response.output_text.delta") => {
            first_nonempty(event.get("delta").and_then(Value::as_str))
        }
        Some("response.output_text.done") if !saw_delta => {
            first_nonempty(event.get("text").and_then(Value::as_str))
        }
        Some("response.completed" | "response.done") => event
            .get("response")
            .and_then(|value| serde_json::from_value::<ResponsesResponse>(value.clone()).ok())
            .and_then(|response| extract_responses_text(&response)),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct ResponsesWebSocketAccumulator {
    saw_delta: bool,
    delta_accumulator: String,
    fallback_text: Option<String>,
    latest_response_id: Option<String>,
    output_items: Vec<ResponsesOutput>,
}

impl ResponsesWebSocketAccumulator {
    fn final_text(&self) -> Option<String> {
        if self.saw_delta {
            first_nonempty(Some(&self.delta_accumulator))
        } else {
            self.fallback_text.clone()
        }
    }

    fn fallback_response(&self) -> Option<ResponsesResponse> {
        let output_text = self.final_text();
        if output_text.is_none() && self.output_items.is_empty() {
            return None;
        }

        Some(ResponsesResponse {
            id: self.latest_response_id.clone(),
            output: self.output_items.clone(),
            output_text,
        })
    }

    fn record_output_item(&mut self, event: &Value) {
        let Some(event_type) = event.get("type").and_then(Value::as_str) else {
            return;
        };

        if event_type != "response.output_item.done" {
            return;
        }

        let item = event
            .get("item")
            .or_else(|| event.get("output_item"))
            .cloned();
        if let Some(item) = item {
            if let Ok(parsed) = serde_json::from_value::<ResponsesOutput>(item) {
                self.output_items.push(parsed);
            }
        }
    }

    fn apply_event(&mut self, event: &Value) -> anyhow::Result<Option<ResponsesResponse>> {
        if let Some(message) = extract_responses_stream_error_message(event) {
            anyhow::bail!("{}", message.trim());
        }

        self.record_output_item(event);

        let event_type = event.get("type").and_then(Value::as_str);
        if let Some(id) = event
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(Value::as_str)
        {
            self.latest_response_id = Some(id.to_string());
        }

        if let Some(text) = extract_responses_stream_text_event(event, self.saw_delta) {
            if event_type == Some("response.output_text.delta") {
                self.saw_delta = true;
                self.delta_accumulator.push_str(&text);
            } else if self.fallback_text.is_none() {
                self.fallback_text = Some(text);
            }
        }

        if event_type == Some("response.completed") || event_type == Some("response.done") {
            if let Some(value) = event.get("response").cloned() {
                if let Ok(mut parsed) = serde_json::from_value::<ResponsesResponse>(value) {
                    if parsed.output_text.is_none() {
                        parsed.output_text = self.final_text();
                    }
                    if parsed.output.is_empty() && !self.output_items.is_empty() {
                        parsed.output = self.output_items.clone();
                    }
                    return Ok(Some(parsed));
                }
            }
            return Ok(self.fallback_response());
        }

        Ok(None)
    }
}

fn compact_sanitized_body_snippet(body: &str) -> String {
    super::sanitize_api_error(body)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_chat_response_body(provider_name: &str, body: &str) -> anyhow::Result<ApiChatResponse> {
    serde_json::from_str::<ApiChatResponse>(body).map_err(|error| {
        let snippet = compact_sanitized_body_snippet(body);
        anyhow::anyhow!(
            "{provider_name} API returned an unexpected chat-completions payload: {error}; body={snippet}"
        )
    })
}

fn parse_responses_response_body(
    provider_name: &str,
    body: &str,
) -> anyhow::Result<ResponsesResponse> {
    serde_json::from_str::<ResponsesResponse>(body).map_err(|error| {
        let snippet = compact_sanitized_body_snippet(body);
        anyhow::anyhow!(
            "{provider_name} Responses API returned an unexpected payload: {error}; body={snippet}"
        )
    })
}

impl OpenAiCompatibleProvider {
    fn should_use_responses_mode(&self) -> bool {
        self.api_mode == CompatibleApiMode::OpenAiResponses
    }

    fn effective_max_tokens(&self) -> Option<u32> {
        self.max_tokens_override.filter(|value| *value > 0)
    }

    fn should_try_responses_websocket(&self) -> bool {
        if let Ok(raw) = std::env::var("ZEROCLAW_RESPONSES_WEBSOCKET") {
            let normalized = raw.trim().to_ascii_lowercase();
            if matches!(normalized.as_str(), "0" | "false" | "off" | "no") {
                return false;
            }
            if matches!(normalized.as_str(), "1" | "true" | "on" | "yes") {
                return true;
            }
        }

        reqwest::Url::parse(&self.responses_url())
            .ok()
            .and_then(|url| {
                url.host_str()
                    .map(|host| host.eq_ignore_ascii_case("api.openai.com"))
            })
            .unwrap_or(false)
    }

    fn responses_websocket_url(&self, model: &str) -> anyhow::Result<String> {
        let mut url = reqwest::Url::parse(&self.responses_url())?;
        let next_scheme: &'static str = match url.scheme() {
            "https" | "wss" => "wss",
            "http" | "ws" => "ws",
            other => {
                anyhow::bail!(
                    "{} Responses API websocket transport does not support URL scheme: {}",
                    self.name,
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

    fn apply_auth_header_ws(
        &self,
        request: &mut tokio_tungstenite::tungstenite::http::Request<()>,
        credential: &str,
    ) -> anyhow::Result<()> {
        let headers = request.headers_mut();
        match &self.auth_header {
            AuthStyle::Bearer => {
                let value = WsHeaderValue::from_str(&format!("Bearer {credential}"))?;
                headers.insert(AUTHORIZATION, value);
            }
            AuthStyle::XApiKey => {
                headers.insert("x-api-key", WsHeaderValue::from_str(credential)?);
            }
            AuthStyle::Custom(header) => {
                let name = HeaderName::from_bytes(header.as_bytes())?;
                headers.insert(name, WsHeaderValue::from_str(credential)?);
            }
        }

        if let Some(ua) = self.user_agent.as_deref() {
            headers.insert(USER_AGENT, WsHeaderValue::from_str(ua)?);
        }

        Ok(())
    }

    async fn send_responses_websocket_request(
        &self,
        credential: &str,
        messages: &[ChatMessage],
        model: &str,
        tools: Option<Vec<Value>>,
    ) -> anyhow::Result<ResponsesResponse> {
        let (instructions, input) = build_responses_prompt(messages);
        if input.is_empty() {
            anyhow::bail!(
                "{} Responses API websocket mode requires at least one non-system message",
                self.name
            );
        }

        let tools = tools.filter(|items| !items.is_empty());
        let payload = ResponsesWebSocketCreateEvent {
            kind: "response.create",
            model: model.to_string(),
            previous_response_id: None,
            input,
            instructions,
            store: Some(false),
            max_output_tokens: self.effective_max_tokens(),
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
        };

        let ws_url = self.responses_websocket_url(model)?;
        let mut request = ws_url
            .into_client_request()
            .map_err(|error| anyhow::anyhow!("invalid websocket request URL: {error}"))?;
        self.apply_auth_header_ws(&mut request, credential)?;

        let (mut ws_stream, _) = connect_async(request).await?;
        ws_stream
            .send(WsMessage::Text(serde_json::to_string(&payload)?.into()))
            .await?;

        let mut accumulator = ResponsesWebSocketAccumulator::default();
        while let Some(frame) = ws_stream.next().await {
            let frame = frame?;
            match frame {
                WsMessage::Text(text) => {
                    let event: Value = serde_json::from_str(text.as_ref())?;
                    if let Some(response) = accumulator.apply_event(&event)? {
                        let _ = ws_stream.close(None).await;
                        return Ok(response);
                    }
                }
                WsMessage::Binary(binary) => {
                    let text = String::from_utf8(binary.to_vec()).map_err(|error| {
                        anyhow::anyhow!("invalid UTF-8 websocket frame from Responses API: {error}")
                    })?;
                    let event: Value = serde_json::from_str(&text)?;
                    if let Some(response) = accumulator.apply_event(&event)? {
                        let _ = ws_stream.close(None).await;
                        return Ok(response);
                    }
                }
                WsMessage::Ping(payload) => {
                    ws_stream.send(WsMessage::Pong(payload)).await?;
                }
                WsMessage::Close(_) => break,
                _ => {}
            }
        }

        if let Some(response) = accumulator.fallback_response() {
            return Ok(response);
        }

        anyhow::bail!("No response from {} Responses websocket stream", self.name)
    }

    async fn send_responses_http_request(
        &self,
        credential: &str,
        messages: &[ChatMessage],
        model: &str,
        tools: Option<Vec<Value>>,
    ) -> anyhow::Result<ResponsesResponse> {
        let (instructions, input) = build_responses_prompt(messages);
        if input.is_empty() {
            anyhow::bail!(
                "{} Responses API fallback requires at least one non-system message",
                self.name
            );
        }

        let tools = tools.filter(|items| !items.is_empty());
        let request = ResponsesRequest {
            model: model.to_string(),
            input,
            instructions,
            max_output_tokens: self.effective_max_tokens(),
            stream: Some(false),
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
        };

        let url = self.responses_url();

        let response = self
            .apply_auth_header(self.http_client().post(&url).json(&request), credential)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("{} Responses API error: {error}", self.name);
        }

        let body = response.text().await?;
        parse_responses_response_body(&self.name, &body)
    }

    async fn send_responses_request(
        &self,
        credential: &str,
        messages: &[ChatMessage],
        model: &str,
        tools: Option<Vec<Value>>,
    ) -> anyhow::Result<ResponsesResponse> {
        if self.should_try_responses_websocket() {
            match self
                .send_responses_websocket_request(credential, messages, model, tools.clone())
                .await
            {
                Ok(response) => return Ok(response),
                Err(error) => {
                    tracing::warn!(
                        provider = %self.name,
                        error = %error,
                        "Responses websocket request failed; falling back to HTTP"
                    );
                }
            }
        }

        self.send_responses_http_request(credential, messages, model, tools)
            .await
    }

    fn apply_auth_header(
        &self,
        req: reqwest::RequestBuilder,
        credential: &str,
    ) -> reqwest::RequestBuilder {
        match &self.auth_header {
            AuthStyle::Bearer => req.header("Authorization", format!("Bearer {credential}")),
            AuthStyle::XApiKey => req.header("x-api-key", credential),
            AuthStyle::Custom(header) => req.header(header, credential),
        }
    }

    async fn chat_via_responses(
        &self,
        credential: &str,
        messages: &[ChatMessage],
        model: &str,
    ) -> anyhow::Result<String> {
        let responses = self
            .send_responses_request(credential, messages, model, None)
            .await?;
        extract_responses_text(&responses)
            .ok_or_else(|| anyhow::anyhow!("No response from {} Responses API", self.name))
    }

    async fn chat_via_responses_chat(
        &self,
        credential: &str,
        messages: &[ChatMessage],
        model: &str,
        tools: Option<Vec<Value>>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let responses = self
            .send_responses_request(credential, messages, model, tools)
            .await?;
        let parsed = parse_responses_chat_response(responses);
        if parsed.text.is_none() && parsed.tool_calls.is_empty() {
            anyhow::bail!("No response from {} Responses API", self.name);
        }
        Ok(parsed)
    }

    fn convert_tool_specs(
        tools: Option<&[crate::tools::ToolSpec]>,
    ) -> Option<Vec<serde_json::Value>> {
        tools.map(|items| {
            items
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect()
        })
    }

    fn to_message_content(
        role: &str,
        content: &str,
        allow_user_image_parts: bool,
    ) -> MessageContent {
        if role != "user" || !allow_user_image_parts {
            return MessageContent::Text(content.to_string());
        }

        let (cleaned_text, image_refs) = multimodal::parse_image_markers(content);
        if image_refs.is_empty() {
            return MessageContent::Text(content.to_string());
        }

        let mut parts = Vec::with_capacity(image_refs.len() + 1);
        let trimmed_text = cleaned_text.trim();
        if !trimmed_text.is_empty() {
            parts.push(MessagePart::Text {
                text: trimmed_text.to_string(),
            });
        }

        for image_ref in image_refs {
            parts.push(MessagePart::ImageUrl {
                image_url: ImageUrlPart { url: image_ref },
            });
        }

        MessageContent::Parts(parts)
    }

    fn convert_messages_for_native(
        messages: &[ChatMessage],
        allow_user_image_parts: bool,
    ) -> Vec<NativeMessage> {
        messages
            .iter()
            .map(|message| {
                if message.role == "assistant" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                    {
                        if let Some(tool_calls_value) = value.get("tool_calls") {
                            if let Ok(parsed_calls) =
                                serde_json::from_value::<Vec<ProviderToolCall>>(
                                    tool_calls_value.clone(),
                                )
                            {
                                let tool_calls = parsed_calls
                                    .into_iter()
                                    .map(|tc| ToolCall {
                                        id: Some(tc.id),
                                        kind: Some("function".to_string()),
                                        function: Some(Function {
                                            name: Some(tc.name),
                                            arguments: Some(tc.arguments),
                                        }),
                                        name: None,
                                        arguments: None,
                                        parameters: None,
                                    })
                                    .collect::<Vec<_>>();

                                let content = value
                                    .get("content")
                                    .and_then(serde_json::Value::as_str)
                                    .map(|value| MessageContent::Text(value.to_string()));

                                let reasoning_content = value
                                    .get("reasoning_content")
                                    .and_then(serde_json::Value::as_str)
                                    .map(ToString::to_string);

                                return NativeMessage {
                                    role: "assistant".to_string(),
                                    content,
                                    tool_call_id: None,
                                    tool_calls: Some(tool_calls),
                                    reasoning_content,
                                };
                            }
                        }
                    }
                }

                if message.role == "tool" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content) {
                        let tool_call_id = value
                            .get("tool_call_id")
                            .and_then(serde_json::Value::as_str)
                            .map(ToString::to_string);
                        let content = value
                            .get("content")
                            .and_then(serde_json::Value::as_str)
                            .map(|value| MessageContent::Text(value.to_string()))
                            .or_else(|| Some(MessageContent::Text(message.content.clone())));

                        return NativeMessage {
                            role: "tool".to_string(),
                            content,
                            tool_call_id,
                            tool_calls: None,
                            reasoning_content: None,
                        };
                    }
                }

                NativeMessage {
                    role: message.role.clone(),
                    content: Some(Self::to_message_content(
                        &message.role,
                        &message.content,
                        allow_user_image_parts,
                    )),
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning_content: None,
                }
            })
            .collect()
    }

    fn with_prompt_guided_tool_instructions(
        messages: &[ChatMessage],
        tools: Option<&[crate::tools::ToolSpec]>,
    ) -> Vec<ChatMessage> {
        let Some(tools) = tools else {
            return messages.to_vec();
        };

        if tools.is_empty() {
            return messages.to_vec();
        }

        let instructions = crate::providers::traits::build_tool_instructions_text(tools);
        let mut modified_messages = messages.to_vec();

        if let Some(system_message) = modified_messages.iter_mut().find(|m| m.role == "system") {
            if !system_message.content.is_empty() {
                system_message.content.push_str("\n\n");
            }
            system_message.content.push_str(&instructions);
        } else {
            modified_messages.insert(0, ChatMessage::system(instructions));
        }

        modified_messages
    }

    fn parse_native_response(message: ResponseMessage) -> ProviderChatResponse {
        let text = message.effective_content_optional();
        let reasoning_content = message.reasoning_content.clone();
        let tool_calls = message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tc| {
                let name = tc.function_name()?;
                let arguments = tc.function_arguments().unwrap_or_else(|| "{}".to_string());
                let normalized_arguments =
                    if serde_json::from_str::<serde_json::Value>(&arguments).is_ok() {
                        arguments
                    } else {
                        tracing::warn!(
                            function = %name,
                            arguments = %arguments,
                            "Invalid JSON in native tool-call arguments, using empty object"
                        );
                        "{}".to_string()
                    };
                Some(ProviderToolCall {
                    id: tc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    name,
                    arguments: normalized_arguments,
                })
            })
            .collect::<Vec<_>>();

        ProviderChatResponse {
            text,
            tool_calls,
            usage: None,
            reasoning_content,
        }
    }

    fn is_native_tool_schema_unsupported(status: reqwest::StatusCode, error: &str) -> bool {
        if !matches!(
            status,
            reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNPROCESSABLE_ENTITY
        ) {
            return false;
        }

        let lower = error.to_lowercase();
        [
            "unknown parameter: tools",
            "unsupported parameter: tools",
            "unrecognized field `tools`",
            "does not support tools",
            "function calling is not supported",
            "tool_choice",
        ]
        .iter()
        .any(|hint| lower.contains(hint))
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn capabilities(&self) -> crate::providers::traits::ProviderCapabilities {
        crate::providers::traits::ProviderCapabilities {
            // Providers that require system-prompt merging (e.g. MiniMax) also
            // reject OpenAI-style `tools` in the request body. Fall back to
            // prompt-guided tool calling for those providers.
            native_tool_calling: self.native_tool_calling,
            vision: self.supports_vision,
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} API key not set. Run `zeroclaw onboard` or set the appropriate env var.",
                self.name
            )
        })?;

        let mut messages = Vec::new();

        if self.merge_system_into_user {
            let content = match system_prompt {
                Some(sys) => format!("{sys}\n\n{message}"),
                None => message.to_string(),
            };
            messages.push(Message {
                role: "user".to_string(),
                content: Self::to_message_content("user", &content, !self.merge_system_into_user),
            });
        } else {
            if let Some(sys) = system_prompt {
                messages.push(Message {
                    role: "system".to_string(),
                    content: MessageContent::Text(sys.to_string()),
                });
            }
            messages.push(Message {
                role: "user".to_string(),
                content: Self::to_message_content("user", message, true),
            });
        }

        let request = ApiChatRequest {
            model: model.to_string(),
            messages,
            temperature,
            max_tokens: self.effective_max_tokens(),
            stream: Some(false),
            tools: None,
            tool_choice: None,
        };

        let url = self.chat_completions_url();

        let mut fallback_messages = Vec::new();
        if let Some(system_prompt) = system_prompt {
            fallback_messages.push(ChatMessage::system(system_prompt));
        }
        fallback_messages.push(ChatMessage::user(message));
        let fallback_messages = if self.merge_system_into_user {
            Self::flatten_system_messages(&fallback_messages)
        } else {
            fallback_messages
        };

        if self.should_use_responses_mode() {
            return self
                .chat_via_responses(credential, &fallback_messages, model)
                .await;
        }

        let response = match self
            .apply_auth_header(self.http_client().post(&url).json(&request), credential)
            .send()
            .await
        {
            Ok(response) => response,
            Err(chat_error) => {
                if self.supports_responses_fallback {
                    let sanitized = super::sanitize_api_error(&chat_error.to_string());
                    return self
                        .chat_via_responses(credential, &fallback_messages, model)
                        .await
                        .map_err(|responses_err| {
                            anyhow::anyhow!(
                                "{} chat completions transport error: {sanitized} (responses fallback failed: {responses_err})",
                                self.name
                            )
                        });
                }

                return Err(chat_error.into());
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await?;
            let sanitized = super::sanitize_api_error(&error);

            if status == reqwest::StatusCode::NOT_FOUND && self.supports_responses_fallback {
                return self
                    .chat_via_responses(credential, &fallback_messages, model)
                    .await
                    .map_err(|responses_err| {
                        anyhow::anyhow!(
                            "{} API error ({status}): {sanitized} (chat completions unavailable; responses fallback failed: {responses_err})",
                            self.name
                        )
                    });
            }

            anyhow::bail!("{} API error ({status}): {sanitized}", self.name);
        }

        let body = response.text().await?;
        let chat_response = parse_chat_response_body(&self.name, &body)?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| {
                // If tool_calls are present, serialize the full message as JSON
                // so parse_tool_calls can handle the OpenAI-style format
                if c.message.tool_calls.is_some()
                    && c.message
                        .tool_calls
                        .as_ref()
                        .map_or(false, |t| !t.is_empty())
                {
                    serde_json::to_string(&c.message)
                        .unwrap_or_else(|_| c.message.effective_content())
                } else {
                    // No tool calls, return content (with reasoning_content fallback)
                    c.message.effective_content()
                }
            })
            .ok_or_else(|| anyhow::anyhow!("No response from {}", self.name))
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} API key not set. Run `zeroclaw onboard` or set the appropriate env var.",
                self.name
            )
        })?;

        let effective_messages = if self.merge_system_into_user {
            Self::flatten_system_messages(messages)
        } else {
            messages.to_vec()
        };
        let api_messages: Vec<Message> = effective_messages
            .iter()
            .map(|m| Message {
                role: m.role.clone(),
                content: Self::to_message_content(
                    &m.role,
                    &m.content,
                    !self.merge_system_into_user,
                ),
            })
            .collect();

        let request = ApiChatRequest {
            model: model.to_string(),
            messages: api_messages,
            temperature,
            max_tokens: self.effective_max_tokens(),
            stream: Some(false),
            tools: None,
            tool_choice: None,
        };

        if self.should_use_responses_mode() {
            return self
                .chat_via_responses(credential, &effective_messages, model)
                .await;
        }

        let url = self.chat_completions_url();
        let response = match self
            .apply_auth_header(self.http_client().post(&url).json(&request), credential)
            .send()
            .await
        {
            Ok(response) => response,
            Err(chat_error) => {
                if self.supports_responses_fallback {
                    let sanitized = super::sanitize_api_error(&chat_error.to_string());
                    return self
                        .chat_via_responses(credential, &effective_messages, model)
                        .await
                        .map_err(|responses_err| {
                            anyhow::anyhow!(
                                "{} chat completions transport error: {sanitized} (responses fallback failed: {responses_err})",
                                self.name
                            )
                        });
                }

                return Err(chat_error.into());
            }
        };

        if !response.status().is_success() {
            let status = response.status();

            // Mirror chat_with_system: 404 may mean this provider uses the Responses API
            if status == reqwest::StatusCode::NOT_FOUND && self.supports_responses_fallback {
                return self
                    .chat_via_responses(credential, &effective_messages, model)
                    .await
                    .map_err(|responses_err| {
                        anyhow::anyhow!(
                            "{} API error (chat completions unavailable; responses fallback failed: {responses_err})",
                            self.name
                        )
                    });
            }

            return Err(super::api_error(&self.name, response).await);
        }

        let body = response.text().await?;
        let chat_response = parse_chat_response_body(&self.name, &body)?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| {
                // If tool_calls are present, serialize the full message as JSON
                // so parse_tool_calls can handle the OpenAI-style format
                if c.message.tool_calls.is_some()
                    && c.message
                        .tool_calls
                        .as_ref()
                        .map_or(false, |t| !t.is_empty())
                {
                    serde_json::to_string(&c.message)
                        .unwrap_or_else(|_| c.message.effective_content())
                } else {
                    // No tool calls, return content (with reasoning_content fallback)
                    c.message.effective_content()
                }
            })
            .ok_or_else(|| anyhow::anyhow!("No response from {}", self.name))
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} API key not set. Run `zeroclaw onboard` or set the appropriate env var.",
                self.name
            )
        })?;

        let effective_messages = if self.merge_system_into_user {
            Self::flatten_system_messages(messages)
        } else {
            messages.to_vec()
        };
        let api_messages: Vec<Message> = effective_messages
            .iter()
            .map(|m| Message {
                role: m.role.clone(),
                content: Self::to_message_content(
                    &m.role,
                    &m.content,
                    !self.merge_system_into_user,
                ),
            })
            .collect();

        let request = ApiChatRequest {
            model: model.to_string(),
            messages: api_messages,
            temperature,
            max_tokens: self.effective_max_tokens(),
            stream: Some(false),
            tools: if tools.is_empty() {
                None
            } else {
                Some(tools.to_vec())
            },
            tool_choice: if tools.is_empty() {
                None
            } else {
                Some("auto".to_string())
            },
        };

        if self.should_use_responses_mode() {
            return self
                .chat_via_responses_chat(
                    credential,
                    &effective_messages,
                    model,
                    (!tools.is_empty()).then(|| tools.to_vec()),
                )
                .await;
        }

        let url = self.chat_completions_url();
        let response = match self
            .apply_auth_header(self.http_client().post(&url).json(&request), credential)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(
                    "{} native tool call transport failed: {error}; falling back to history path",
                    self.name
                );
                let text = self.chat_with_history(messages, model, temperature).await?;
                return Ok(ProviderChatResponse {
                    text: Some(text),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                });
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            if status == reqwest::StatusCode::NOT_FOUND && self.supports_responses_fallback {
                return self
                    .chat_via_responses_chat(
                        credential,
                        &effective_messages,
                        model,
                        (!tools.is_empty()).then(|| tools.to_vec()),
                    )
                    .await;
            }
            return Err(super::api_error(&self.name, response).await);
        }

        let body = response.text().await?;
        let chat_response = parse_chat_response_body(&self.name, &body)?;
        let usage = chat_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        });
        let choice = chat_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No response from {}", self.name))?;

        let text = choice.message.effective_content_optional();
        let reasoning_content = choice.message.reasoning_content;
        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tc| {
                let function = tc.function?;
                let name = function.name?;
                let arguments = function.arguments.unwrap_or_else(|| "{}".to_string());
                Some(ProviderToolCall {
                    id: uuid::Uuid::new_v4().to_string(),
                    name,
                    arguments,
                })
            })
            .collect::<Vec<_>>();

        Ok(ProviderChatResponse {
            text,
            tool_calls,
            usage,
            reasoning_content,
        })
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} API key not set. Run `zeroclaw onboard` or set the appropriate env var.",
                self.name
            )
        })?;

        let tools = Self::convert_tool_specs(request.tools);
        let response_tools = tools.clone();
        let effective_messages = if self.merge_system_into_user {
            Self::flatten_system_messages(request.messages)
        } else {
            request.messages.to_vec()
        };
        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages_for_native(
                &effective_messages,
                !self.merge_system_into_user,
            ),
            temperature,
            max_tokens: self.effective_max_tokens(),
            stream: Some(false),
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
        };

        if self.should_use_responses_mode() {
            return self
                .chat_via_responses_chat(
                    credential,
                    &effective_messages,
                    model,
                    response_tools.clone(),
                )
                .await;
        }

        let url = self.chat_completions_url();
        let response = match self
            .apply_auth_header(
                self.http_client().post(&url).json(&native_request),
                credential,
            )
            .send()
            .await
        {
            Ok(response) => response,
            Err(chat_error) => {
                if self.supports_responses_fallback {
                    let sanitized = super::sanitize_api_error(&chat_error.to_string());
                    return self
                        .chat_via_responses_chat(
                            credential,
                            &effective_messages,
                            model,
                            response_tools.clone(),
                        )
                        .await
                        .map_err(|responses_err| {
                            anyhow::anyhow!(
                                "{} native chat transport error: {sanitized} (responses fallback failed: {responses_err})",
                                self.name
                            )
                        });
                }

                return Err(chat_error.into());
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await?;
            let sanitized = super::sanitize_api_error(&error);

            if Self::is_native_tool_schema_unsupported(status, &sanitized) {
                let fallback_messages =
                    Self::with_prompt_guided_tool_instructions(request.messages, request.tools);
                let text = self
                    .chat_with_history(&fallback_messages, model, temperature)
                    .await?;
                return Ok(ProviderChatResponse {
                    text: Some(text),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                });
            }

            if status == reqwest::StatusCode::NOT_FOUND && self.supports_responses_fallback {
                return self
                    .chat_via_responses_chat(
                        credential,
                        &effective_messages,
                        model,
                        response_tools.clone(),
                    )
                    .await
                    .map_err(|responses_err| {
                        anyhow::anyhow!(
                            "{} API error ({status}): {sanitized} (chat completions unavailable; responses fallback failed: {responses_err})",
                            self.name
                        )
                    });
            }

            anyhow::bail!("{} API error ({status}): {sanitized}", self.name);
        }

        let native_response: ApiChatResponse = response.json().await?;
        let usage = native_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        });
        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message)
            .ok_or_else(|| anyhow::anyhow!("No response from {}", self.name))?;

        let mut result = Self::parse_native_response(message);
        result.usage = usage;
        Ok(result)
    }

    fn supports_native_tools(&self) -> bool {
        self.native_tool_calling
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        let credential = match self.credential.as_ref() {
            Some(value) => value.clone(),
            None => {
                let provider_name = self.name.clone();
                return stream::once(async move {
                    Err(StreamError::Provider(format!(
                        "{} API key not set",
                        provider_name
                    )))
                })
                .boxed();
            }
        };

        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: MessageContent::Text(sys.to_string()),
            });
        }
        messages.push(Message {
            role: "user".to_string(),
            content: Self::to_message_content("user", message, !self.merge_system_into_user),
        });

        let request = ApiChatRequest {
            model: model.to_string(),
            messages,
            temperature,
            max_tokens: self.effective_max_tokens(),
            stream: Some(options.enabled),
            tools: None,
            tool_choice: None,
        };

        let url = self.chat_completions_url();
        let client = self.http_client();
        let auth_header = self.auth_header.clone();

        // Use a channel to bridge the async HTTP response to the stream
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

        tokio::spawn(async move {
            // Build request with auth
            let mut req_builder = client.post(&url).json(&request);

            // Apply auth header
            req_builder = match &auth_header {
                AuthStyle::Bearer => {
                    req_builder.header("Authorization", format!("Bearer {}", credential))
                }
                AuthStyle::XApiKey => req_builder.header("x-api-key", &credential),
                AuthStyle::Custom(header) => req_builder.header(header, &credential),
            };

            // Set accept header for streaming
            req_builder = req_builder.header("Accept", "text/event-stream");

            // Send request
            let response = match req_builder.send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e))).await;
                    return;
                }
            };

            // Check status
            if !response.status().is_success() {
                let status = response.status();
                let error = match response.text().await {
                    Ok(e) => e,
                    Err(_) => format!("HTTP error: {}", status),
                };
                let _ = tx
                    .send(Err(StreamError::Provider(format!("{}: {}", status, error))))
                    .await;
                return;
            }

            // Convert to chunk stream and forward to channel
            let mut chunk_stream = sse_bytes_to_chunks(response, options.count_tokens);
            while let Some(chunk) = chunk_stream.next().await {
                if tx.send(chunk).await.is_err() {
                    break; // Receiver dropped
                }
            }
        });

        // Convert channel receiver to stream
        stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|chunk| (chunk, rx))
        })
        .boxed()
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        if let Some(credential) = self.credential.as_ref() {
            // Hit the chat completions URL with a GET to establish the connection pool.
            // The server will likely return 405 Method Not Allowed, which is fine -
            // the goal is TLS handshake and HTTP/2 negotiation.
            let url = self.chat_completions_url();
            let _ = self
                .apply_auth_header(self.http_client().get(&url), credential)
                .send()
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_provider(name: &str, url: &str, key: Option<&str>) -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new(name, url, key, AuthStyle::Bearer)
    }

    #[test]
    fn creates_with_key() {
        let p = make_provider(
            "venice",
            "https://api.venice.ai",
            Some("venice-test-credential"),
        );
        assert_eq!(p.name, "venice");
        assert_eq!(p.base_url, "https://api.venice.ai");
        assert_eq!(p.credential.as_deref(), Some("venice-test-credential"));
    }

    #[test]
    fn creates_without_key() {
        let p = make_provider("test", "https://example.com", None);
        assert!(p.credential.is_none());
    }

    #[test]
    fn strips_trailing_slash() {
        let p = make_provider("test", "https://example.com/", None);
        assert_eq!(p.base_url, "https://example.com");
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = make_provider("Venice", "https://api.venice.ai", None);
        let result = p
            .chat_with_system(None, "hello", "llama-3.3-70b", 0.7)
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Venice API key not set"));
    }

    #[test]
    fn request_serializes_correctly() {
        let req = ApiChatRequest {
            model: "llama-3.3-70b".to_string(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: MessageContent::Text("You are ZeroClaw".to_string()),
                },
                Message {
                    role: "user".to_string(),
                    content: MessageContent::Text("hello".to_string()),
                },
            ],
            temperature: 0.4,
            max_tokens: None,
            stream: Some(false),
            tools: None,
            tool_choice: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("llama-3.3-70b"));
        assert!(json.contains("system"));
        assert!(json.contains("user"));
        // tools/tool_choice should be omitted when None
        assert!(!json.contains("tools"));
        assert!(!json.contains("tool_choice"));
    }

    #[test]
    fn response_deserializes() {
        let json = r#"{"choices":[{"message":{"content":"Hello from Venice!"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0].message.content,
            Some("Hello from Venice!".to_string())
        );
    }

    #[test]
    fn response_empty_choices() {
        let json = r#"{"choices":[]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.choices.is_empty());
    }

    #[test]
    fn parse_chat_response_body_reports_sanitized_snippet() {
        let body = r#"{"choices":"invalid","api_key":"sk-test-secret-value"}"#;
        let err = parse_chat_response_body("custom", body).expect_err("payload should fail");
        let msg = err.to_string();

        assert!(msg.contains("custom API returned an unexpected chat-completions payload"));
        assert!(msg.contains("body="));
        assert!(msg.contains("[REDACTED]"));
        assert!(!msg.contains("sk-test-secret-value"));
    }

    #[test]
    fn parse_responses_response_body_reports_sanitized_snippet() {
        let body = r#"{"output_text":123,"api_key":"sk-another-secret"}"#;
        let err = parse_responses_response_body("custom", body).expect_err("payload should fail");
        let msg = err.to_string();

        assert!(msg.contains("custom Responses API returned an unexpected payload"));
        assert!(msg.contains("body="));
        assert!(msg.contains("[REDACTED]"));
        assert!(!msg.contains("sk-another-secret"));
    }

    #[test]
    fn x_api_key_auth_style() {
        let p = OpenAiCompatibleProvider::new(
            "moonshot",
            "https://api.moonshot.cn",
            Some("ms-key"),
            AuthStyle::XApiKey,
        );
        assert!(matches!(p.auth_header, AuthStyle::XApiKey));
    }

    #[test]
    fn custom_auth_style() {
        let p = OpenAiCompatibleProvider::new(
            "custom",
            "https://api.example.com",
            Some("key"),
            AuthStyle::Custom("X-Custom-Key".into()),
        );
        assert!(matches!(p.auth_header, AuthStyle::Custom(_)));
    }

    #[test]
    fn custom_constructor_applies_responses_mode_and_max_tokens_override() {
        let provider = OpenAiCompatibleProvider::new_custom_with_mode(
            "custom",
            "https://api.example.com",
            Some("key"),
            AuthStyle::Bearer,
            true,
            CompatibleApiMode::OpenAiResponses,
            Some(2048),
        );

        assert!(provider.should_use_responses_mode());
        assert_eq!(provider.effective_max_tokens(), Some(2048));
    }

    #[tokio::test]
    async fn all_compatible_providers_fail_without_key() {
        let providers = vec![
            make_provider("Venice", "https://api.venice.ai", None),
            make_provider("Moonshot", "https://api.moonshot.cn", None),
            make_provider("GLM", "https://open.bigmodel.cn", None),
            make_provider("MiniMax", "https://api.minimaxi.com/v1", None),
            make_provider("Groq", "https://api.groq.com/openai/v1", None),
            make_provider("Mistral", "https://api.mistral.ai", None),
            make_provider("xAI", "https://api.x.ai", None),
            make_provider("Astrai", "https://as-trai.com/v1", None),
        ];

        for p in providers {
            let result = p.chat_with_system(None, "test", "model", 0.7).await;
            assert!(result.is_err(), "{} should fail without key", p.name);
            assert!(
                result.unwrap_err().to_string().contains("API key not set"),
                "{} error should mention key",
                p.name
            );
        }
    }

    #[test]
    fn responses_extracts_top_level_output_text() {
        let json = r#"{"output_text":"Hello from top-level","output":[]}"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            extract_responses_text(&response).as_deref(),
            Some("Hello from top-level")
        );
    }

    #[test]
    fn responses_extracts_nested_output_text() {
        let json =
            r#"{"output":[{"content":[{"type":"output_text","text":"Hello from nested"}]}]}"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            extract_responses_text(&response).as_deref(),
            Some("Hello from nested")
        );
    }

    #[test]
    fn responses_extracts_any_text_as_fallback() {
        let json = r#"{"output":[{"content":[{"type":"message","text":"Fallback text"}]}]}"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            extract_responses_text(&response).as_deref(),
            Some("Fallback text")
        );
    }

    #[test]
    fn responses_extracts_function_call_as_tool_call() {
        let json = r#"{
            "output":[
                {
                    "type":"function_call",
                    "call_id":"call_abc",
                    "name":"shell",
                    "arguments":"{\"command\":\"date\"}"
                }
            ]
        }"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        let parsed = parse_responses_chat_response(response);
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "call_abc");
        assert_eq!(parsed.tool_calls[0].name, "shell");
        assert_eq!(parsed.tool_calls[0].arguments, "{\"command\":\"date\"}");
    }

    #[test]
    fn websocket_url_converts_scheme_and_adds_model_query() {
        let provider = make_provider("custom", "https://api.openai.com/v1", Some("key"));
        let ws_url = provider
            .responses_websocket_url("gpt-5.2")
            .expect("websocket URL should be derived");
        assert_eq!(ws_url, "wss://api.openai.com/v1/responses?model=gpt-5.2");
    }

    #[test]
    fn websocket_url_preserves_existing_model_query() {
        let provider = make_provider(
            "custom",
            "https://api.openai.com/v1/responses?model=gpt-4.1-mini",
            Some("key"),
        );
        let ws_url = provider
            .responses_websocket_url("gpt-5.2")
            .expect("existing query should be preserved");
        assert_eq!(
            ws_url,
            "wss://api.openai.com/v1/responses?model=gpt-4.1-mini"
        );
    }

    #[test]
    fn websocket_accumulator_parses_delta_and_completed_event() {
        let mut acc = ResponsesWebSocketAccumulator::default();

        assert!(acc
            .apply_event(&serde_json::json!({
                "type":"response.created",
                "response":{"id":"resp_123"}
            }))
            .unwrap()
            .is_none());

        assert!(acc
            .apply_event(&serde_json::json!({
                "type":"response.output_text.delta",
                "delta":"Hello"
            }))
            .unwrap()
            .is_none());

        let response = acc
            .apply_event(&serde_json::json!({
                "type":"response.completed",
                "response":{"id":"resp_123","output_text":"Hello world","output":[]}
            }))
            .unwrap()
            .expect("completed event should finalize response");

        assert_eq!(response.id.as_deref(), Some("resp_123"));
        assert_eq!(
            extract_responses_text(&response).as_deref(),
            Some("Hello world")
        );
    }

    #[test]
    fn websocket_accumulator_falls_back_to_output_items() {
        let mut acc = ResponsesWebSocketAccumulator::default();
        assert!(acc
            .apply_event(&serde_json::json!({
                "type":"response.output_item.done",
                "item":{
                    "type":"function_call",
                    "name":"shell",
                    "call_id":"call_xyz",
                    "arguments":"{\"command\":\"pwd\"}"
                }
            }))
            .unwrap()
            .is_none());

        let fallback = acc
            .fallback_response()
            .expect("function-call output item should be retained");
        let parsed = parse_responses_chat_response(fallback);
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "call_xyz");
        assert_eq!(parsed.tool_calls[0].name, "shell");
    }

    #[test]
    fn websocket_accumulator_reports_stream_error() {
        let mut acc = ResponsesWebSocketAccumulator::default();
        let err = acc
            .apply_event(&serde_json::json!({
                "type":"error",
                "error":{"code":"previous_response_not_found","message":"missing response id"}
            }))
            .expect_err("error event should fail");

        assert!(err.to_string().contains("missing response id"));
    }

    #[test]
    fn build_responses_prompt_preserves_multi_turn_history() {
        let messages = vec![
            ChatMessage::system("policy"),
            ChatMessage::user("step 1"),
            ChatMessage::assistant("ack 1"),
            ChatMessage::tool("{\"result\":\"ok\"}"),
            ChatMessage::user("step 2"),
        ];

        let (instructions, input) = build_responses_prompt(&messages);

        assert_eq!(instructions.as_deref(), Some("policy"));
        assert_eq!(input.len(), 4);
        assert_eq!(input[0].role, "user");
        assert_eq!(input[0].content, "step 1");
        assert_eq!(input[1].role, "assistant");
        assert_eq!(input[1].content, "ack 1");
        assert_eq!(input[2].role, "assistant");
        assert_eq!(input[2].content, "{\"result\":\"ok\"}");
        assert_eq!(input[3].role, "user");
        assert_eq!(input[3].content, "step 2");
    }

    #[tokio::test]
    async fn chat_via_responses_requires_non_system_message() {
        let provider = make_provider("custom", "https://api.example.com", Some("test-key"));
        let err = provider
            .chat_via_responses("test-key", &[ChatMessage::system("policy")], "gpt-test")
            .await
            .expect_err("system-only fallback payload should fail");

        assert!(err
            .to_string()
            .contains("requires at least one non-system message"));
    }

    #[test]
    fn tool_call_function_name_falls_back_to_top_level_name() {
        let call: ToolCall = serde_json::from_value(serde_json::json!({
            "name": "memory_recall",
            "arguments": "{\"query\":\"latest roadmap\"}"
        }))
        .unwrap();

        assert_eq!(call.function_name().as_deref(), Some("memory_recall"));
    }

    #[test]
    fn tool_call_function_arguments_falls_back_to_parameters_object() {
        let call: ToolCall = serde_json::from_value(serde_json::json!({
            "name": "shell",
            "parameters": {"command": "pwd"}
        }))
        .unwrap();

        assert_eq!(
            call.function_arguments().as_deref(),
            Some("{\"command\":\"pwd\"}")
        );
    }

    #[test]
    fn tool_call_function_arguments_prefers_nested_function_field() {
        let call: ToolCall = serde_json::from_value(serde_json::json!({
            "name": "ignored_name",
            "arguments": "{\"query\":\"ignored\"}",
            "function": {
                "name": "memory_recall",
                "arguments": "{\"query\":\"preferred\"}"
            }
        }))
        .unwrap();

        assert_eq!(call.function_name().as_deref(), Some("memory_recall"));
        assert_eq!(
            call.function_arguments().as_deref(),
            Some("{\"query\":\"preferred\"}")
        );
    }

    // ----------------------------------------------------------
    // Custom endpoint path tests (Issue #114)
    // ----------------------------------------------------------

    #[test]
    fn chat_completions_url_standard_openai() {
        // Standard OpenAI-compatible providers get /chat/completions appended
        let p = make_provider("openai", "https://api.openai.com/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_trailing_slash() {
        // Trailing slash is stripped, then /chat/completions appended
        let p = make_provider("test", "https://api.example.com/v1/", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_volcengine_ark() {
        // VolcEngine ARK uses custom path - should use as-is
        let p = make_provider(
            "volcengine",
            "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions",
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://ark.cn-beijing.volces.com/api/coding/v3/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_custom_full_endpoint() {
        // Custom provider with full endpoint path
        let p = make_provider(
            "custom",
            "https://my-api.example.com/v2/llm/chat/completions",
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://my-api.example.com/v2/llm/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_requires_exact_suffix_match() {
        let p = make_provider(
            "custom",
            "https://my-api.example.com/v2/llm/chat/completions-proxy",
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://my-api.example.com/v2/llm/chat/completions-proxy/chat/completions"
        );
    }

    #[test]
    fn responses_url_standard() {
        // Standard providers get /v1/responses appended
        let p = make_provider("test", "https://api.example.com", None);
        assert_eq!(p.responses_url(), "https://api.example.com/v1/responses");
    }

    #[test]
    fn responses_url_custom_full_endpoint() {
        // Custom provider with full responses endpoint
        let p = make_provider(
            "custom",
            "https://my-api.example.com/api/v2/responses",
            None,
        );
        assert_eq!(
            p.responses_url(),
            "https://my-api.example.com/api/v2/responses"
        );
    }

    #[test]
    fn responses_url_requires_exact_suffix_match() {
        let p = make_provider(
            "custom",
            "https://my-api.example.com/api/v2/responses-proxy",
            None,
        );
        assert_eq!(
            p.responses_url(),
            "https://my-api.example.com/api/v2/responses-proxy/responses"
        );
    }

    #[test]
    fn responses_url_derives_from_chat_endpoint() {
        let p = make_provider(
            "custom",
            "https://my-api.example.com/api/v2/chat/completions",
            None,
        );
        assert_eq!(
            p.responses_url(),
            "https://my-api.example.com/api/v2/responses"
        );
    }

    #[test]
    fn responses_url_base_with_v1_no_duplicate() {
        let p = make_provider("test", "https://api.example.com/v1", None);
        assert_eq!(p.responses_url(), "https://api.example.com/v1/responses");
    }

    #[test]
    fn responses_url_non_v1_api_path_uses_raw_suffix() {
        let p = make_provider("test", "https://api.example.com/api/coding/v3", None);
        assert_eq!(
            p.responses_url(),
            "https://api.example.com/api/coding/v3/responses"
        );
    }

    #[test]
    fn chat_completions_url_without_v1() {
        // Provider configured without /v1 in base URL
        let p = make_provider("test", "https://api.example.com", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.example.com/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_base_with_v1() {
        // Provider configured with /v1 in base URL
        let p = make_provider("test", "https://api.example.com/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    // ----------------------------------------------------------
    // Provider-specific endpoint tests (Issue #167)
    // ----------------------------------------------------------

    #[test]
    fn chat_completions_url_zai() {
        // Z.AI uses /api/paas/v4 base path
        let p = make_provider("zai", "https://api.z.ai/api/paas/v4", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.z.ai/api/paas/v4/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_minimax() {
        // MiniMax OpenAI-compatible endpoint requires /v1 base path.
        let p = make_provider("minimax", "https://api.minimaxi.com/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://api.minimaxi.com/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_glm() {
        // GLM (BigModel) uses /api/paas/v4 base path
        let p = make_provider("glm", "https://open.bigmodel.cn/api/paas/v4", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_opencode() {
        // OpenCode Zen uses /zen/v1 base path
        let p = make_provider("opencode", "https://opencode.ai/zen/v1", None);
        assert_eq!(
            p.chat_completions_url(),
            "https://opencode.ai/zen/v1/chat/completions"
        );
    }

    #[test]
    fn parse_native_response_preserves_tool_call_id() {
        let message = ResponseMessage {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: Some("call_123".to_string()),
                kind: Some("function".to_string()),
                function: Some(Function {
                    name: Some("shell".to_string()),
                    arguments: Some(r#"{"command":"pwd"}"#.to_string()),
                }),
                name: None,
                arguments: None,
                parameters: None,
            }]),
            reasoning_content: None,
        };

        let parsed = OpenAiCompatibleProvider::parse_native_response(message);
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "call_123");
        assert_eq!(parsed.tool_calls[0].name, "shell");
    }

    #[test]
    fn convert_messages_for_native_maps_tool_result_payload() {
        let input = vec![ChatMessage::tool(
            r#"{"tool_call_id":"call_abc","content":"done"}"#,
        )];

        let converted = OpenAiCompatibleProvider::convert_messages_for_native(&input, true);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "tool");
        assert_eq!(converted[0].tool_call_id.as_deref(), Some("call_abc"));
        assert!(matches!(
            converted[0].content.as_ref(),
            Some(MessageContent::Text(value)) if value == "done"
        ));
    }

    #[test]
    fn convert_messages_for_native_keeps_user_image_markers_as_text_when_disabled() {
        let input = vec![ChatMessage::user(
            "System primer [IMAGE:data:image/png;base64,abcd] user turn",
        )];

        let converted = OpenAiCompatibleProvider::convert_messages_for_native(&input, false);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "user");
        assert!(matches!(
            converted[0].content.as_ref(),
            Some(MessageContent::Text(value))
                if value == "System primer [IMAGE:data:image/png;base64,abcd] user turn"
        ));
    }

    #[test]
    fn flatten_system_messages_merges_into_first_user() {
        let input = vec![
            ChatMessage::system("core policy"),
            ChatMessage::assistant("ack"),
            ChatMessage::system("delivery rules"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("post-user"),
        ];

        let output = OpenAiCompatibleProvider::flatten_system_messages(&input);
        assert_eq!(output.len(), 3);
        assert_eq!(output[0].role, "assistant");
        assert_eq!(output[0].content, "ack");
        assert_eq!(output[1].role, "user");
        assert_eq!(output[1].content, "core policy\n\ndelivery rules\n\nhello");
        assert_eq!(output[2].role, "assistant");
        assert_eq!(output[2].content, "post-user");
        assert!(output.iter().all(|m| m.role != "system"));
    }

    #[test]
    fn flatten_system_messages_inserts_user_when_missing() {
        let input = vec![
            ChatMessage::system("core policy"),
            ChatMessage::assistant("ack"),
        ];

        let output = OpenAiCompatibleProvider::flatten_system_messages(&input);
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].role, "user");
        assert_eq!(output[0].content, "core policy");
        assert_eq!(output[1].role, "assistant");
        assert_eq!(output[1].content, "ack");
    }

    #[test]
    fn strip_think_tags_drops_unclosed_block_suffix() {
        let input = "visible<think>hidden";
        assert_eq!(strip_think_tags(input), "visible");
    }

    #[test]
    fn native_tool_schema_unsupported_detection_is_precise() {
        assert!(OpenAiCompatibleProvider::is_native_tool_schema_unsupported(
            reqwest::StatusCode::BAD_REQUEST,
            "unknown parameter: tools"
        ));
        assert!(
            !OpenAiCompatibleProvider::is_native_tool_schema_unsupported(
                reqwest::StatusCode::UNAUTHORIZED,
                "unknown parameter: tools"
            )
        );
    }

    #[test]
    fn prompt_guided_tool_fallback_injects_system_instruction() {
        let input = vec![ChatMessage::user("check status")];
        let tools = vec![crate::tools::ToolSpec {
            name: "shell_exec".to_string(),
            description: "Execute shell command".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                },
                "required": ["command"]
            }),
        }];

        let output =
            OpenAiCompatibleProvider::with_prompt_guided_tool_instructions(&input, Some(&tools));
        assert!(!output.is_empty());
        assert_eq!(output[0].role, "system");
        assert!(output[0].content.contains("Available Tools"));
        assert!(output[0].content.contains("shell_exec"));
    }

    #[tokio::test]
    async fn warmup_without_key_is_noop() {
        let provider = make_provider("test", "https://example.com", None);
        let result = provider.warmup().await;
        assert!(result.is_ok());
    }

    // ══════════════════════════════════════════════════════════
    // Native tool calling tests
    // ══════════════════════════════════════════════════════════

    #[test]
    fn capabilities_reports_native_tool_calling() {
        let p = make_provider("test", "https://example.com", None);
        let caps = <OpenAiCompatibleProvider as Provider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(!caps.vision);
    }

    #[test]
    fn capabilities_reports_vision_for_qwen_compatible_provider() {
        let p = OpenAiCompatibleProvider::new_with_vision(
            "Qwen",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            Some("k"),
            AuthStyle::Bearer,
            true,
        );
        let caps = <OpenAiCompatibleProvider as Provider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn minimax_provider_disables_native_tool_calling() {
        let p = OpenAiCompatibleProvider::new_merge_system_into_user(
            "MiniMax",
            "https://api.minimax.chat/v1",
            Some("k"),
            AuthStyle::Bearer,
        );
        let caps = <OpenAiCompatibleProvider as Provider>::capabilities(&p);
        assert!(
            !caps.native_tool_calling,
            "MiniMax should use prompt-guided tool calling, not native"
        );
        assert!(!caps.vision);
    }

    #[test]
    fn user_agent_constructor_keeps_native_tool_calling_enabled() {
        let p = OpenAiCompatibleProvider::new_with_user_agent(
            "TestProvider",
            "https://example.com",
            Some("k"),
            AuthStyle::Bearer,
            "zeroclaw-test/1.0",
        );
        let caps = <OpenAiCompatibleProvider as Provider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(!caps.vision);
        assert_eq!(p.user_agent.as_deref(), Some("zeroclaw-test/1.0"));
    }

    #[test]
    fn user_agent_and_vision_constructor_preserves_capability_flags() {
        let p = OpenAiCompatibleProvider::new_with_user_agent_and_vision(
            "VisionProvider",
            "https://example.com",
            Some("k"),
            AuthStyle::Bearer,
            "zeroclaw-test/vision",
            true,
        );
        let caps = <OpenAiCompatibleProvider as Provider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(caps.vision);
        assert_eq!(p.user_agent.as_deref(), Some("zeroclaw-test/vision"));
    }

    #[test]
    fn no_responses_fallback_constructor_keeps_native_tool_calling_enabled() {
        let p = OpenAiCompatibleProvider::new_no_responses_fallback(
            "FallbackProvider",
            "https://example.com",
            Some("k"),
            AuthStyle::Bearer,
        );
        let caps = <OpenAiCompatibleProvider as Provider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(!caps.vision);
        assert!(p.user_agent.is_none());
    }

    #[test]
    fn to_message_content_converts_image_markers_to_openai_parts() {
        let content = "Describe this\n\n[IMAGE:data:image/png;base64,abcd]";
        let value = serde_json::to_value(OpenAiCompatibleProvider::to_message_content(
            "user", content, true,
        ))
        .unwrap();
        let parts = value
            .as_array()
            .expect("multimodal content should be an array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "Describe this");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,abcd");
    }

    #[test]
    fn to_message_content_keeps_markers_as_text_when_user_image_parts_disabled() {
        let content = "Policy [IMAGE:data:image/png;base64,abcd]";
        let value = serde_json::to_value(OpenAiCompatibleProvider::to_message_content(
            "user", content, false,
        ))
        .unwrap();
        assert_eq!(value, serde_json::json!(content));
    }

    #[test]
    fn to_message_content_keeps_plain_text_for_non_user_roles() {
        let value = serde_json::to_value(OpenAiCompatibleProvider::to_message_content(
            "system",
            "You are a helpful assistant.",
            true,
        ))
        .unwrap();
        assert_eq!(value, serde_json::json!("You are a helpful assistant."));
    }

    #[test]
    fn tool_specs_convert_to_openai_format() {
        let specs = vec![crate::tools::ToolSpec {
            name: "shell".to_string(),
            description: "Run shell command".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }),
        }];

        let tools = OpenAiCompatibleProvider::tool_specs_to_openai_format(&specs);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "shell");
        assert_eq!(tools[0]["function"]["description"], "Run shell command");
        assert_eq!(tools[0]["function"]["parameters"]["required"][0], "command");
    }

    #[test]
    fn request_serializes_with_tools() {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get weather for a location",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    }
                }
            }
        })];

        let req = ApiChatRequest {
            model: "test-model".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::Text("What is the weather?".to_string()),
            }],
            temperature: 0.7,
            max_tokens: None,
            stream: Some(false),
            tools: Some(tools),
            tool_choice: Some("auto".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"tools\""));
        assert!(json.contains("get_weather"));
        assert!(json.contains("\"tool_choice\":\"auto\""));
    }

    #[test]
    fn response_with_tool_calls_deserializes() {
        let json = r#"{
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"London\"}"
                        }
                    }]
                }
            }]
        }"#;

        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert!(msg.content.is_none());
        let tool_calls = msg.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(
            tool_calls[0].function.as_ref().unwrap().name.as_deref(),
            Some("get_weather")
        );
        assert_eq!(
            tool_calls[0]
                .function
                .as_ref()
                .unwrap()
                .arguments
                .as_deref(),
            Some("{\"location\":\"London\"}")
        );
    }

    #[test]
    fn response_with_multiple_tool_calls() {
        let json = r#"{
            "choices": [{
                "message": {
                    "content": "I'll check both.",
                    "tool_calls": [
                        {
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"location\":\"London\"}"
                            }
                        },
                        {
                            "type": "function",
                            "function": {
                                "name": "get_time",
                                "arguments": "{\"timezone\":\"UTC\"}"
                            }
                        }
                    ]
                }
            }]
        }"#;

        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.content.as_deref(), Some("I'll check both."));
        let tool_calls = msg.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(
            tool_calls[0].function.as_ref().unwrap().name.as_deref(),
            Some("get_weather")
        );
        assert_eq!(
            tool_calls[1].function.as_ref().unwrap().name.as_deref(),
            Some("get_time")
        );
    }

    #[tokio::test]
    async fn chat_with_tools_fails_without_key() {
        let p = make_provider("TestProvider", "https://example.com", None);
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "test_tool",
                "description": "A test tool",
                "parameters": {}
            }
        })];

        let result = p.chat_with_tools(&messages, &tools, "model", 0.7).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("TestProvider API key not set"));
    }

    #[test]
    fn response_with_no_tool_calls_has_empty_vec() {
        let json = r#"{"choices":[{"message":{"content":"Just text, no tools."}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.content.as_deref(), Some("Just text, no tools."));
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn flatten_system_messages_merges_into_first_user_and_removes_system_roles() {
        let messages = vec![
            ChatMessage::system("System A"),
            ChatMessage::assistant("Earlier assistant turn"),
            ChatMessage::system("System B"),
            ChatMessage::user("User turn"),
            ChatMessage::tool(r#"{"ok":true}"#),
        ];

        let flattened = OpenAiCompatibleProvider::flatten_system_messages(&messages);
        assert_eq!(flattened.len(), 3);
        assert_eq!(flattened[0].role, "assistant");
        assert_eq!(
            flattened[1].content,
            "System A\n\nSystem B\n\nUser turn".to_string()
        );
        assert_eq!(flattened[1].role, "user");
        assert_eq!(flattened[2].role, "tool");
        assert!(!flattened.iter().any(|m| m.role == "system"));
    }

    #[test]
    fn flatten_system_messages_inserts_synthetic_user_when_no_user_exists() {
        let messages = vec![
            ChatMessage::assistant("Assistant only"),
            ChatMessage::system("Synthetic system"),
        ];

        let flattened = OpenAiCompatibleProvider::flatten_system_messages(&messages);
        assert_eq!(flattened.len(), 2);
        assert_eq!(flattened[0].role, "user");
        assert_eq!(flattened[0].content, "Synthetic system");
        assert_eq!(flattened[1].role, "assistant");
    }

    #[test]
    fn strip_think_tags_removes_multiple_blocks_with_surrounding_text() {
        let input = "Answer A <think>hidden 1</think> and B <think>hidden 2</think> done";
        let output = strip_think_tags(input);
        assert_eq!(output, "Answer A  and B  done");
    }

    #[test]
    fn strip_think_tags_drops_tail_for_unclosed_block() {
        let input = "Visible<think>hidden tail";
        let output = strip_think_tags(input);
        assert_eq!(output, "Visible");
    }

    // ----------------------------------------------------------
    // Reasoning model fallback tests (reasoning_content)
    // ----------------------------------------------------------

    #[test]
    fn reasoning_content_fallback_when_content_empty() {
        // Reasoning models (Qwen3, GLM-4) return content: "" with reasoning_content populated
        let json = r#"{"choices":[{"message":{"content":"","reasoning_content":"Thinking output here"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Thinking output here");
    }

    #[test]
    fn reasoning_content_fallback_when_content_null() {
        // Some models may return content: null with reasoning_content
        let json =
            r#"{"choices":[{"message":{"content":null,"reasoning_content":"Fallback text"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Fallback text");
    }

    #[test]
    fn reasoning_content_fallback_when_content_missing() {
        // content field absent entirely, reasoning_content present
        let json = r#"{"choices":[{"message":{"reasoning_content":"Only reasoning"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Only reasoning");
    }

    #[test]
    fn reasoning_content_not_used_when_content_present() {
        // Normal model: content populated, reasoning_content should be ignored
        let json = r#"{"choices":[{"message":{"content":"Normal response","reasoning_content":"Should be ignored"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Normal response");
    }

    #[test]
    fn reasoning_content_used_when_content_only_think_tags() {
        let json = r#"{"choices":[{"message":{"content":"<think>secret</think>","reasoning_content":"Fallback text"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "Fallback text");
        assert_eq!(
            msg.effective_content_optional().as_deref(),
            Some("Fallback text")
        );
    }

    #[test]
    fn reasoning_content_both_absent_returns_empty() {
        // Neither content nor reasoning_content - returns empty string
        let json = r#"{"choices":[{"message":{}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), "");
    }

    #[test]
    fn reasoning_content_ignored_by_normal_models() {
        // Standard response without reasoning_content still works
        let json = r#"{"choices":[{"message":{"content":"Hello from Venice!"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert!(msg.reasoning_content.is_none());
        assert_eq!(msg.effective_content(), "Hello from Venice!");
    }

    // ----------------------------------------------------------
    // SSE streaming reasoning_content fallback tests
    // ----------------------------------------------------------

    #[test]
    fn parse_sse_line_with_content() {
        let line = r#"data: {"choices":[{"delta":{"content":"hello"}}]}"#;
        let result = parse_sse_line(line).unwrap();
        assert_eq!(result, Some("hello".to_string()));
    }

    #[test]
    fn parse_sse_line_with_reasoning_content() {
        let line = r#"data: {"choices":[{"delta":{"reasoning_content":"thinking..."}}]}"#;
        let result = parse_sse_line(line).unwrap();
        assert_eq!(result, Some("thinking...".to_string()));
    }

    #[test]
    fn parse_sse_line_with_both_prefers_content() {
        let line = r#"data: {"choices":[{"delta":{"content":"real answer","reasoning_content":"thinking..."}}]}"#;
        let result = parse_sse_line(line).unwrap();
        assert_eq!(result, Some("real answer".to_string()));
    }

    #[test]
    fn parse_sse_line_with_empty_content_falls_back_to_reasoning_content() {
        let line =
            r#"data: {"choices":[{"delta":{"content":"","reasoning_content":"thinking..."}}]}"#;
        let result = parse_sse_line(line).unwrap();
        assert_eq!(result, Some("thinking...".to_string()));
    }

    #[test]
    fn parse_sse_line_done_sentinel() {
        let line = "data: [DONE]";
        let result = parse_sse_line(line).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn api_response_parses_usage() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {"prompt_tokens": 150, "completion_tokens": 60}
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(150));
        assert_eq!(usage.completion_tokens, Some(60));
    }

    #[test]
    fn api_response_parses_without_usage() {
        let json = r#"{"choices": [{"message": {"content": "Hello"}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // reasoning_content pass-through tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_native_response_captures_reasoning_content() {
        let message = ResponseMessage {
            content: Some("answer".to_string()),
            reasoning_content: Some("thinking step".to_string()),
            tool_calls: Some(vec![ToolCall {
                id: Some("call_1".to_string()),
                kind: Some("function".to_string()),
                function: Some(Function {
                    name: Some("shell".to_string()),
                    arguments: Some(r#"{"cmd":"ls"}"#.to_string()),
                }),
                name: None,
                arguments: None,
                parameters: None,
            }]),
        };

        let parsed = OpenAiCompatibleProvider::parse_native_response(message);
        assert_eq!(parsed.reasoning_content.as_deref(), Some("thinking step"));
        assert_eq!(parsed.text.as_deref(), Some("answer"));
        assert_eq!(parsed.tool_calls.len(), 1);
    }

    #[test]
    fn parse_native_response_none_reasoning_content_for_normal_model() {
        let message = ResponseMessage {
            content: Some("hello".to_string()),
            reasoning_content: None,
            tool_calls: None,
        };

        let parsed = OpenAiCompatibleProvider::parse_native_response(message);
        assert!(parsed.reasoning_content.is_none());
        assert_eq!(parsed.text.as_deref(), Some("hello"));
    }

    #[test]
    fn convert_messages_for_native_round_trips_reasoning_content() {
        // Simulate stored assistant history JSON that includes reasoning_content
        let history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{\"cmd\":\"ls\"}"
            }],
            "reasoning_content": "Let me think about this..."
        });

        let messages = vec![ChatMessage::assistant(history_json.to_string())];
        let native = OpenAiCompatibleProvider::convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].role, "assistant");
        assert_eq!(
            native[0].reasoning_content.as_deref(),
            Some("Let me think about this...")
        );
        assert!(native[0].tool_calls.is_some());
    }

    #[test]
    fn convert_messages_for_native_no_reasoning_content_when_absent() {
        // Normal model history without reasoning_content key
        let history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{\"cmd\":\"ls\"}"
            }]
        });

        let messages = vec![ChatMessage::assistant(history_json.to_string())];
        let native = OpenAiCompatibleProvider::convert_messages_for_native(&messages, true);
        assert_eq!(native.len(), 1);
        assert!(native[0].reasoning_content.is_none());
    }

    #[test]
    fn convert_messages_for_native_reasoning_content_serialized_only_when_present() {
        // Verify skip_serializing_if works: reasoning_content omitted from JSON when None
        let msg_without = NativeMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("hi".to_string())),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        };
        let json = serde_json::to_string(&msg_without).unwrap();
        assert!(
            !json.contains("reasoning_content"),
            "reasoning_content should be omitted when None"
        );

        let msg_with = NativeMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("hi".to_string())),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: Some("thinking...".to_string()),
        };
        let json = serde_json::to_string(&msg_with).unwrap();
        assert!(
            json.contains("reasoning_content"),
            "reasoning_content should be present when Some"
        );
        assert!(json.contains("thinking..."));
    }
}
