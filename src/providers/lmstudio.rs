//! LM Studio provider — uses the native `/api/v1/chat` endpoint.
//!
//! Exposes LM Studio-specific parameters not available through the generic
//! OpenAI-compatible shim:
//!
//! - `context_length` — override the loaded model's context window per request
//! - `reasoning` — control reasoning budget (`off` / `low` / `medium` / `high` / `on`)
//!
//! Endpoint strategy:
//! - [`chat_with_system`] → `POST /api/v1/chat` (native; gets context_length + reasoning + vision)
//! - [`chat_with_history`] → `POST /v1/chat/completions` (compat; supports full message history)
//! - [`chat_with_tools`]   → `POST /v1/chat/completions` (compat; only path that supports custom tools)
//! - [`stream_chat_with_system`] → `POST /api/v1/chat` with `stream: true` (native SSE)
//!
//! Authentication is optional and disabled by default in LM Studio.
//! Enable it via Developers Page → Server Settings → Require authentication.

use crate::multimodal;
use crate::providers::traits::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, StreamChunk, StreamError,
    StreamOptions, StreamResult, TokenUsage, ToolCall,
};
use async_trait::async_trait;
use futures_util::{stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};

// ─── Public types ─────────────────────────────────────────────────────────────

pub struct LmStudioProvider {
    pub(crate) base_url: String,
    api_key: Option<String>,
    pub(crate) context_length: Option<u32>,
    pub(crate) reasoning: Option<LmStudioReasoning>,
}

/// Reasoning budget levels supported by LM Studio's native API.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LmStudioReasoning {
    Off,
    Low,
    Medium,
    High,
    On,
}

// ─── Request types (native /api/v1/chat) ─────────────────────────────────────

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    input: NativeChatInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<LmStudioReasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    // Disable server-side storage; we manage history locally.
    store: bool,
}

/// Serialises as either a plain string or a typed item array.
///
/// ```json
/// "input": "Hello"
/// "input": [{"type":"text","content":"Hello"},{"type":"image","data_url":"data:..."}]
/// ```
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum NativeChatInput {
    Text(String),
    Multimodal(Vec<NativeInputItem>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum NativeInputItem {
    Text { content: String },
    Image { data_url: String },
}

// ─── Response types (native /api/v1/chat) ─────────────────────────────────────

#[derive(Debug, Deserialize)]
struct NativeChatResponse {
    #[serde(default)]
    output: Vec<NativeOutputItem>,
    #[serde(default)]
    stats: Option<NativeStats>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum NativeOutputItem {
    Message {
        #[serde(default)]
        content: String,
    },
    Reasoning {
        #[serde(default)]
        content: String,
    },
    ToolCall {
        #[serde(default)]
        tool: String,
        #[serde(default)]
        arguments: serde_json::Value,
        #[serde(default)]
        output: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct NativeStats {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    total_output_tokens: Option<u64>,
}

// ─── Request/response types (compat /v1/chat/completions) ────────────────────

#[derive(Debug, Serialize)]
struct CompatChatRequest {
    model: String,
    messages: Vec<CompatMessage>,
    temperature: f64,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
struct CompatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct CompatChatResponse {
    choices: Vec<CompatChoice>,
    #[serde(default)]
    usage: Option<CompatUsage>,
}

#[derive(Debug, Deserialize)]
struct CompatChoice {
    message: CompatResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompatResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<CompatToolCall>,
}

#[derive(Debug, Deserialize)]
struct CompatToolCall {
    id: Option<String>,
    function: CompatFunction,
}

#[derive(Debug, Deserialize)]
struct CompatFunction {
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct CompatUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

// ─── SSE streaming types ──────────────────────────────────────────────────────

/// Parsed SSE event from LM Studio's native streaming API.
#[derive(Debug, Deserialize)]
struct NativeSseEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    content: Option<String>,
}

// ─── Implementation ───────────────────────────────────────────────────────────

impl LmStudioProvider {
    /// Create a new provider instance.
    ///
    /// `base_url` defaults to `http://localhost:1234` when `None` or empty.
    /// `api_key` is optional; LM Studio authentication is disabled by default.
    pub fn new(base_url: Option<&str>, api_key: Option<&str>) -> Self {
        let base_url = Self::normalize_base_url(base_url.unwrap_or("http://localhost:1234"));
        let api_key = api_key
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);

        Self {
            base_url,
            api_key,
            context_length: None,
            reasoning: None,
        }
    }

    /// Set a default context length override for all requests.
    pub fn with_context_length(mut self, ctx: u32) -> Self {
        self.context_length = Some(ctx);
        self
    }

    /// Set a default reasoning budget for all requests.
    pub fn with_reasoning(mut self, reasoning: LmStudioReasoning) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    fn normalize_base_url(raw: &str) -> String {
        raw.trim().trim_end_matches('/').to_string()
    }

    pub(crate) fn native_chat_url(&self) -> String {
        format!("{}/api/v1/chat", self.base_url)
    }

    pub(crate) fn compat_chat_url(&self) -> String {
        format!("{}/v1/chat/completions", self.base_url)
    }

    pub(crate) fn models_url(&self) -> String {
        format!("{}/v1/models", self.base_url)
    }

    fn http_client(&self) -> Client {
        crate::config::build_runtime_proxy_client_with_timeouts("provider.lmstudio", 300, 10)
    }

    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => builder.bearer_auth(key),
            None => builder,
        }
    }

    /// Build multimodal input items from a user message that may contain image markers.
    fn build_native_input(&self, message: &str) -> NativeChatInput {
        let (cleaned, image_refs) = multimodal::parse_image_markers(message);
        if image_refs.is_empty() {
            return NativeChatInput::Text(message.to_string());
        }

        let mut items = Vec::new();
        let cleaned = cleaned.trim().to_string();
        if !cleaned.is_empty() {
            items.push(NativeInputItem::Text { content: cleaned });
        }

        for image_ref in &image_refs {
            // Only data URIs are supported by the native LM Studio API.
            if image_ref.starts_with("data:") {
                items.push(NativeInputItem::Image {
                    data_url: image_ref.clone(),
                });
            } else {
                tracing::warn!(
                    "LM Studio native API only supports data URI images, skipping: {}",
                    super::sanitize_api_error(image_ref)
                );
            }
        }

        if items.is_empty() {
            NativeChatInput::Text(message.to_string())
        } else {
            NativeChatInput::Multimodal(items)
        }
    }

    /// Send a request to the native `/api/v1/chat` endpoint.
    async fn send_native_request(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<NativeChatResponse> {
        let input = self.build_native_input(message);

        let request = NativeChatRequest {
            model: model.to_string(),
            input,
            system_prompt: system_prompt.map(ToString::to_string),
            temperature: Some(temperature),
            stream: false,
            context_length: self.context_length,
            reasoning: self.reasoning.clone(),
            max_output_tokens: None,
            store: false,
        };

        let url = self.native_chat_url();

        tracing::debug!(
            "LM Studio native request: url={} model={} temperature={}",
            url,
            model,
            temperature,
        );

        let response = self
            .apply_auth(self.http_client().post(&url).json(&request))
            .send()
            .await?;

        let status = response.status();
        let body = response.bytes().await?;

        if !status.is_success() {
            let raw = String::from_utf8_lossy(&body);
            let sanitized = super::sanitize_api_error(&raw);
            tracing::error!(
                "LM Studio native error: status={} body_excerpt={}",
                status,
                sanitized
            );
            anyhow::bail!(
                "LM Studio API error ({}): {}. Is LM Studio running with a model loaded?",
                status,
                sanitized
            );
        }

        serde_json::from_slice(&body).map_err(|e| {
            let raw = String::from_utf8_lossy(&body);
            let sanitized = super::sanitize_api_error(&raw);
            tracing::error!(
                "LM Studio response deserialization failed: {e}. body_excerpt={sanitized}"
            );
            anyhow::anyhow!("Failed to parse LM Studio response: {e}")
        })
    }

    /// Extract text and reasoning content from native response output items.
    fn extract_native_text(response: &NativeChatResponse) -> (Option<String>, Option<String>) {
        let mut text: Option<String> = None;
        let mut reasoning: Option<String> = None;

        for item in &response.output {
            match item {
                NativeOutputItem::Message { content } if !content.trim().is_empty() => {
                    text = Some(content.clone());
                }
                NativeOutputItem::Reasoning { content } if !content.trim().is_empty() => {
                    reasoning = Some(content.clone());
                }
                _ => {}
            }
        }

        (text, reasoning)
    }

    /// Build a `ChatResponse` from a native API response.
    fn native_to_chat_response(response: NativeChatResponse) -> ChatResponse {
        let usage = response.stats.as_ref().map(|s| TokenUsage {
            input_tokens: s.input_tokens,
            output_tokens: s.total_output_tokens,
        });

        let (text, reasoning_content) = Self::extract_native_text(&response);

        ChatResponse {
            text,
            tool_calls: Vec::new(),
            usage,
            reasoning_content,
            quota_metadata: None,
            stop_reason: None,
            raw_stop_reason: None,
        }
    }

    /// Send a request to the compat `/v1/chat/completions` endpoint.
    async fn send_compat_request(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
        tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<ChatResponse> {
        let compat_messages: Vec<CompatMessage> = messages
            .iter()
            .map(|m| CompatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        let request = CompatChatRequest {
            model: model.to_string(),
            messages: compat_messages,
            temperature,
            stream: false,
            tools: tools.map(|t| t.to_vec()),
        };

        let url = self.compat_chat_url();

        tracing::debug!(
            "LM Studio compat request: url={} model={} temperature={} tool_count={}",
            url,
            model,
            temperature,
            tools.map_or(0, |t| t.len()),
        );

        let response = self
            .apply_auth(self.http_client().post(&url).json(&request))
            .send()
            .await?;

        let status = response.status();
        let body = response.bytes().await?;

        if !status.is_success() {
            let raw = String::from_utf8_lossy(&body);
            let sanitized = super::sanitize_api_error(&raw);
            tracing::error!(
                "LM Studio compat error: status={} body_excerpt={}",
                status,
                sanitized
            );
            anyhow::bail!(
                "LM Studio API error ({}): {}. Is LM Studio running with a model loaded?",
                status,
                sanitized
            );
        }

        let compat: CompatChatResponse = serde_json::from_slice(&body).map_err(|e| {
            let raw = String::from_utf8_lossy(&body);
            let sanitized = super::sanitize_api_error(&raw);
            tracing::error!(
                "LM Studio compat response deserialization failed: {e}. body_excerpt={sanitized}"
            );
            anyhow::anyhow!("Failed to parse LM Studio compat response: {e}")
        })?;

        let usage = compat.usage.as_ref().map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        });

        if compat.choices.is_empty() {
            anyhow::bail!("LM Studio compat response missing choices (empty choices array)");
        }

        let choice = compat.choices.into_iter().next().unwrap();

        let tool_calls: Vec<ToolCall> = choice
            .message
            .tool_calls
            .iter()
            .map(|tc| ToolCall {
                id: tc.id.clone().unwrap_or_default(),
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            })
            .collect();

        let text = choice
            .message
            .content
            .and_then(|t| if t.trim().is_empty() { None } else { Some(t) });

        Ok(ChatResponse {
            text,
            tool_calls,
            usage,
            reasoning_content: None,
            quota_metadata: None,
            stop_reason: None,
            raw_stop_reason: None,
        })
    }
}

// ─── Provider trait ───────────────────────────────────────────────────────────

#[async_trait]
impl Provider for LmStudioProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            // Native tool calling via /v1/chat/completions fallback.
            native_tool_calling: true,
            // Vision via native /api/v1/chat typed input array.
            vision: true,
        }
    }

    /// Single-turn chat using the native `/api/v1/chat` endpoint.
    ///
    /// This path applies `context_length` and `reasoning` overrides.
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let response = self
            .send_native_request(system_prompt, message, model, temperature)
            .await?;

        let (text, reasoning) = Self::extract_native_text(&response);

        text.or(reasoning).ok_or_else(|| {
            anyhow::anyhow!("LM Studio returned no content. Is a model loaded and responding?")
        })
    }

    /// Multi-turn chat using the compat `/v1/chat/completions` endpoint.
    ///
    /// The native `/api/v1/chat` endpoint does not accept full message histories,
    /// so the OpenAI-compatible path is used here to preserve conversation context.
    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let response = self
            .send_compat_request(messages, model, temperature, None)
            .await?;

        response.text.ok_or_else(|| {
            anyhow::anyhow!("LM Studio returned no content. Is a model loaded and responding?")
        })
    }

    /// Tool-augmented chat using the compat `/v1/chat/completions` endpoint.
    ///
    /// Custom tools are only supported on the OpenAI-compatible endpoint.
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        self.send_compat_request(messages, model, temperature, Some(tools))
            .await
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    /// Streaming single-turn chat using the native `/api/v1/chat` endpoint.
    ///
    /// Parses `message.delta` SSE events. The full response is available at `chat.end`
    /// but we yield deltas progressively for low-latency output.
    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamChunk>> {
        let input = self.build_native_input(message);

        let request = NativeChatRequest {
            model: model.to_string(),
            input,
            system_prompt: system_prompt.map(ToString::to_string),
            temperature: Some(temperature),
            stream: true,
            context_length: self.context_length,
            reasoning: self.reasoning.clone(),
            max_output_tokens: None,
            store: false,
        };

        let url = self.native_chat_url();
        let client = self.http_client();
        let api_key = self.api_key.clone();
        let count_tokens = options.count_tokens;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamChunk>>(100);

        tokio::spawn(async move {
            let mut req = client.post(&url).json(&request);
            if let Some(key) = &api_key {
                req = req.bearer_auth(key);
            }

            let response = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e))).await;
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body = response.bytes().await.unwrap_or_default();
                let msg = String::from_utf8_lossy(&body).to_string();
                let _ = tx
                    .send(Err(StreamError::Provider(format!(
                        "LM Studio streaming error ({}): {}",
                        status,
                        &msg[..msg.len().min(200)]
                    ))))
                    .await;
                return;
            }

            let mut bytes_stream = response.bytes_stream();
            let mut buffer = String::new();

            while let Some(item) = bytes_stream.next().await {
                match item {
                    Ok(bytes) => {
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

                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..=pos].trim().to_string();
                            buffer = buffer[pos + 1..].to_string();

                            if let Some(delta) = parse_native_sse_line(&line) {
                                let mut chunk = StreamChunk::delta(delta);
                                if count_tokens {
                                    chunk = chunk.with_token_estimate();
                                }
                                if tx.send(Ok(chunk)).await.is_err() {
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

            let _ = tx.send(Ok(StreamChunk::final_chunk())).await;
        });

        stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|chunk| (chunk, rx))
        })
        .boxed()
    }
}

// ─── SSE parser ──────────────────────────────────────────────────────────────

/// Parse a single SSE line from LM Studio's native streaming API.
///
/// LM Studio emits typed events:
/// ```text
/// event: message.delta
/// data: {"type":"message.delta","content":"Hello"}
///
/// event: chat.end
/// data: {"type":"chat.end","result":{...}}
/// ```
///
/// We only yield content from `message.delta` events; all others are skipped.
fn parse_native_sse_line(line: &str) -> Option<String> {
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }

    let event: NativeSseEvent = serde_json::from_str(data).ok()?;
    if event.event_type == "message.delta" {
        event.content.filter(|c| !c.is_empty())
    } else {
        None
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_url_uses_localhost_1234() {
        let p = LmStudioProvider::new(None, None);
        assert_eq!(p.base_url, "http://localhost:1234");
    }

    #[test]
    fn custom_url_accepted() {
        let p = LmStudioProvider::new(Some("http://192.168.1.5:1234"), None);
        assert_eq!(p.base_url, "http://192.168.1.5:1234");
    }

    #[test]
    fn trailing_slash_stripped() {
        let p = LmStudioProvider::new(Some("http://localhost:1234/"), None);
        assert_eq!(p.base_url, "http://localhost:1234");
    }

    #[test]
    fn blank_key_treated_as_none() {
        let p = LmStudioProvider::new(None, Some("   "));
        assert!(p.api_key.is_none());
    }

    #[test]
    fn empty_key_treated_as_none() {
        let p = LmStudioProvider::new(None, Some(""));
        assert!(p.api_key.is_none());
    }

    #[test]
    fn native_chat_url_appends_api_path() {
        let p = LmStudioProvider::new(None, None);
        assert_eq!(p.native_chat_url(), "http://localhost:1234/api/v1/chat");
    }

    #[test]
    fn compat_chat_url_appends_v1_path() {
        let p = LmStudioProvider::new(None, None);
        assert_eq!(
            p.compat_chat_url(),
            "http://localhost:1234/v1/chat/completions"
        );
    }

    #[test]
    fn models_url_appends_v1_models() {
        let p = LmStudioProvider::new(None, None);
        assert_eq!(p.models_url(), "http://localhost:1234/v1/models");
    }

    #[test]
    fn capabilities_includes_vision() {
        let p = LmStudioProvider::new(None, None);
        assert!(p.capabilities().vision);
    }

    #[test]
    fn capabilities_includes_native_tools() {
        let p = LmStudioProvider::new(None, None);
        assert!(p.capabilities().native_tool_calling);
    }

    #[test]
    fn reasoning_high_serializes_correctly() {
        let json = serde_json::to_string(&LmStudioReasoning::High).unwrap();
        assert_eq!(json, r#""high""#);
    }

    #[test]
    fn reasoning_off_serializes_correctly() {
        let json = serde_json::to_string(&LmStudioReasoning::Off).unwrap();
        assert_eq!(json, r#""off""#);
    }

    #[test]
    fn with_context_length_stores_value() {
        let p = LmStudioProvider::new(None, None).with_context_length(8192);
        assert_eq!(p.context_length, Some(8192));
    }

    #[test]
    fn with_context_length_default_is_none() {
        let p = LmStudioProvider::new(None, None);
        assert!(p.context_length.is_none());
    }

    #[test]
    fn custom_url_on_different_port() {
        let p = LmStudioProvider::new(Some("http://localhost:5678"), None);
        assert_eq!(p.native_chat_url(), "http://localhost:5678/api/v1/chat");
        assert_eq!(p.models_url(), "http://localhost:5678/v1/models");
    }

    #[test]
    fn parse_native_sse_message_delta_yields_content() {
        let line = r#"data: {"type":"message.delta","content":"Hello"}"#;
        assert_eq!(parse_native_sse_line(line), Some("Hello".to_string()));
    }

    #[test]
    fn parse_native_sse_chat_end_yields_nothing() {
        let line = r#"data: {"type":"chat.end","result":{}}"#;
        assert_eq!(parse_native_sse_line(line), None);
    }

    #[test]
    fn parse_native_sse_empty_content_yields_nothing() {
        let line = r#"data: {"type":"message.delta","content":""}"#;
        assert_eq!(parse_native_sse_line(line), None);
    }

    #[test]
    fn parse_native_sse_non_data_line_yields_nothing() {
        assert_eq!(parse_native_sse_line("event: message.delta"), None);
        assert_eq!(parse_native_sse_line(""), None);
        assert_eq!(parse_native_sse_line(": keep-alive"), None);
    }

    #[test]
    fn extract_native_text_from_message_output() {
        let response = NativeChatResponse {
            output: vec![NativeOutputItem::Message {
                content: "The answer is 42.".to_string(),
            }],
            stats: None,
        };
        let (text, reasoning) = LmStudioProvider::extract_native_text(&response);
        assert_eq!(text.as_deref(), Some("The answer is 42."));
        assert!(reasoning.is_none());
    }

    #[test]
    fn extract_native_text_returns_reasoning_when_no_message() {
        let response = NativeChatResponse {
            output: vec![NativeOutputItem::Reasoning {
                content: "Let me think...".to_string(),
            }],
            stats: None,
        };
        let (text, reasoning) = LmStudioProvider::extract_native_text(&response);
        assert!(text.is_none());
        assert_eq!(reasoning.as_deref(), Some("Let me think..."));
    }

    #[test]
    fn extract_native_text_empty_output_returns_none() {
        let response = NativeChatResponse {
            output: vec![],
            stats: None,
        };
        let (text, reasoning) = LmStudioProvider::extract_native_text(&response);
        assert!(text.is_none());
        assert!(reasoning.is_none());
    }
}
