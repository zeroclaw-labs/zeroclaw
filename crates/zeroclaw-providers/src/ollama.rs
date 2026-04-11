use crate::multimodal;
use crate::traits::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, TokenUsage, ToolCall,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Matches Ollama's upstream Modelfile default
/// (<https://docs.ollama.com/modelfile>): "Increasing the temperature will
/// make the model answer more creatively. (Default: 0.8)".
const TEMPERATURE_DEFAULT: f64 = 0.8;
/// Local inference is CPU/GPU-bound; give it more headroom than cloud calls.
const TIMEOUT_SECS_DEFAULT: u64 = 600;
/// Ollama's standard localhost endpoint. Overrideable via
/// `providers.models.<name>.base-url` for remote GPU boxes or non-default ports.
const BASE_URL: &str = "http://localhost:11434";

pub struct OllamaProvider {
    base_url: String,
    api_key: Option<String>,
    reasoning_enabled: Option<bool>,
}

// ─── Request Structures ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
    options: Options,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize)]
struct Message {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OutgoingToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct OutgoingToolCall {
    #[serde(rename = "type")]
    kind: String,
    function: OutgoingFunction,
}

#[derive(Debug, Clone, Serialize)]
struct OutgoingFunction {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct Options {
    temperature: f64,
}

// ─── Response Structures ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ApiChatResponse {
    message: ResponseMessage,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<OllamaToolCall>,
    /// Some models return a "thinking" field with internal reasoning
    #[serde(default)]
    thinking: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    id: Option<String>,
    function: OllamaFunction,
}

#[derive(Debug, Deserialize)]
struct OllamaFunction {
    name: String,
    #[serde(default, deserialize_with = "deserialize_args")]
    arguments: serde_json::Value,
}

// ─── serde Helpers ───────────────────────────────────────────────────────────
fn deserialize_args<'de, D>(deserializer: D) -> Result<serde_json::Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;

    if let Some(s) = value.as_str() {
        match serde_json::from_str::<serde_json::Value>(s) {
            Ok(v) => Ok(v),
            Err(_) => Ok(serde_json::json!({})),
        }
    } else {
        Ok(value)
    }
}
// ─── Implementation ───────────────────────────────────────────────────────────

impl OllamaProvider {
    fn normalize_base_url(raw_url: &str) -> String {
        let trimmed = raw_url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return String::new();
        }

        trimmed
            .strip_suffix("/api/chat")
            .or_else(|| trimmed.strip_suffix("/api"))
            .unwrap_or(trimmed)
            .trim_end_matches('/')
            .to_string()
    }

    pub fn new(base_url: Option<&str>, api_key: Option<&str>) -> Self {
        Self::new_with_reasoning(base_url, api_key, None)
    }

    pub fn new_with_reasoning(
        base_url: Option<&str>,
        api_key: Option<&str>,
        reasoning_enabled: Option<bool>,
    ) -> Self {
        let api_key = api_key.and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });

        Self {
            base_url: Self::normalize_base_url(base_url.unwrap_or(BASE_URL)),
            api_key,
            reasoning_enabled,
        }
    }

    fn is_local_endpoint(&self) -> bool {
        reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|url| url.host_str().map(|host| host.to_string()))
            .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    }

    fn http_client(&self) -> Client {
        zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            "provider.ollama",
            300,
            10,
        )
    }

    fn resolve_request_details(&self, model: &str) -> anyhow::Result<(String, bool)> {
        let requests_cloud = model.ends_with(":cloud");
        let normalized_model = model.strip_suffix(":cloud").unwrap_or(model).to_string();

        if requests_cloud && self.is_local_endpoint() {
            anyhow::bail!(
                "Model '{}' requested cloud routing, but Ollama endpoint is local. Configure api_url with a remote Ollama endpoint.",
                model
            );
        }

        if requests_cloud && self.api_key.is_none() {
            anyhow::bail!(
                "Model '{}' requested cloud routing, but no API key is configured. Set OLLAMA_API_KEY or config api_key.",
                model
            );
        }

        let should_auth = self.api_key.is_some() && !self.is_local_endpoint();

        Ok((normalized_model, should_auth))
    }

    fn parse_tool_arguments(arguments: &str) -> serde_json::Value {
        serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}))
    }

    fn normalize_response_text(content: String) -> Option<String> {
        let stripped = Self::strip_think_tags(&content);
        if stripped.trim().is_empty() {
            None
        } else {
            Some(stripped)
        }
    }

    /// Remove `<think>...</think>` blocks from model output.
    /// Qwen and other reasoning models may embed chain-of-thought inline
    /// in the `content` field using `<think>` tags.  These must be stripped
    /// before returning text to the user or parsing for tool calls.
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

    /// Derive the effective text content from a response, stripping `<think>` tags
    /// and falling back to the `thinking` field when `content` is empty after
    /// stripping.  This ensures that tool-call XML tags embedded alongside (or
    /// after) thinking blocks are preserved for downstream parsing.
    fn effective_content(content: &str, thinking: Option<&str>) -> Option<String> {
        // First try the content field with think tags stripped.
        let stripped = Self::strip_think_tags(content);
        if !stripped.trim().is_empty() {
            return Some(stripped);
        }

        // Content was empty or only thinking — check the thinking field.
        // Some models (Qwen) put the full output including tool-call XML in
        // the thinking field when `think: true` is set.
        if let Some(thinking) = thinking.map(str::trim).filter(|t| !t.is_empty()) {
            let stripped_thinking = Self::strip_think_tags(thinking);
            if !stripped_thinking.trim().is_empty() {
                tracing::debug!(
                    "Ollama: using thinking field as effective content ({} chars)",
                    stripped_thinking.len()
                );
                return Some(stripped_thinking);
            }
        }

        None
    }

    fn fallback_text_for_empty_content(model: &str, thinking: Option<&str>) -> String {
        if let Some(thinking) = thinking.map(str::trim).filter(|value| !value.is_empty()) {
            let thinking_log_excerpt: String = thinking.chars().take(100).collect();
            let thinking_reply_excerpt: String = thinking.chars().take(200).collect();
            tracing::warn!(
                "Ollama returned empty content with only thinking for model '{}': '{}'. Model may have stopped prematurely.",
                model,
                thinking_log_excerpt
            );
            return format!(
                "I was thinking about this: {}... but I didn't complete my response. Could you try asking again?",
                thinking_reply_excerpt
            );
        }

        tracing::warn!(
            "Ollama returned empty or whitespace content with no tool calls for model '{}'",
            model
        );
        "I couldn't get a complete response from Ollama. Please try again or switch to a different model."
            .to_string()
    }

    #[allow(dead_code)]
    fn build_chat_request(
        &self,
        messages: Vec<Message>,
        model: &str,
        temperature: f64,
        tools: Option<&[serde_json::Value]>,
    ) -> ChatRequest {
        self.build_chat_request_with_think(
            messages,
            model,
            temperature,
            tools,
            self.reasoning_enabled,
        )
    }

    /// Build a chat request with an explicit `think` value.
    fn build_chat_request_with_think(
        &self,
        messages: Vec<Message>,
        model: &str,
        temperature: f64,
        tools: Option<&[serde_json::Value]>,
        think: Option<bool>,
    ) -> ChatRequest {
        ChatRequest {
            model: model.to_string(),
            messages,
            stream: false,
            options: Options { temperature },
            think,
            tools: tools.map(|t| t.to_vec()),
        }
    }

    fn convert_user_message_content(&self, content: &str) -> (Option<String>, Option<Vec<String>>) {
        let (cleaned, image_refs) = multimodal::parse_image_markers(content);
        if image_refs.is_empty() {
            return (Some(content.to_string()), None);
        }

        let images: Vec<String> = image_refs
            .iter()
            .filter_map(|reference| multimodal::extract_ollama_image_payload(reference))
            .collect();

        if images.is_empty() {
            return (Some(content.to_string()), None);
        }

        let cleaned = cleaned.trim();
        let content = if cleaned.is_empty() {
            None
        } else {
            Some(cleaned.to_string())
        };

        (content, Some(images))
    }

    /// Convert internal chat history format to Ollama's native tool-call message schema.
    ///
    /// `run_tool_call_loop` stores native assistant/tool entries as JSON strings in
    /// `ChatMessage.content`. We decode those payloads here so follow-up requests send
    /// structured `assistant.tool_calls` and `tool.tool_name`, as expected by Ollama.
    fn convert_messages(&self, messages: &[ChatMessage]) -> Vec<Message> {
        let mut tool_name_by_id: HashMap<String, String> = HashMap::new();

        messages
            .iter()
            .map(|message| {
                if message.role == "assistant"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                    && let Some(tool_calls_value) = value.get("tool_calls")
                    && let Ok(parsed_calls) =
                        serde_json::from_value::<Vec<ToolCall>>(tool_calls_value.clone())
                {
                    let outgoing_calls: Vec<OutgoingToolCall> = parsed_calls
                        .into_iter()
                        .map(|call| {
                            tool_name_by_id.insert(call.id.clone(), call.name.clone());
                            OutgoingToolCall {
                                kind: "function".to_string(),
                                function: OutgoingFunction {
                                    name: call.name,
                                    arguments: Self::parse_tool_arguments(&call.arguments),
                                },
                            }
                        })
                        .collect();
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    return Message {
                        role: "assistant".to_string(),
                        content,
                        images: None,
                        tool_calls: Some(outgoing_calls),
                        tool_name: None,
                    };
                }

                if message.role == "tool"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
                {
                    let tool_name = value
                        .get("tool_name")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string)
                        .or_else(|| {
                            value
                                .get("tool_call_id")
                                .and_then(serde_json::Value::as_str)
                                .and_then(|id| tool_name_by_id.get(id))
                                .cloned()
                        });
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string)
                        .or_else(|| {
                            (!message.content.trim().is_empty()).then_some(message.content.clone())
                        });

                    return Message {
                        role: "tool".to_string(),
                        content,
                        images: None,
                        tool_calls: None,
                        tool_name,
                    };
                }

                if message.role == "user" {
                    let (content, images) = self.convert_user_message_content(&message.content);
                    return Message {
                        role: "user".to_string(),
                        content,
                        images,
                        tool_calls: None,
                        tool_name: None,
                    };
                }

                Message {
                    role: message.role.clone(),
                    content: Some(message.content.clone()),
                    images: None,
                    tool_calls: None,
                    tool_name: None,
                }
            })
            .collect()
    }

    /// Send a single HTTP request to Ollama and parse the response.
    async fn send_request_inner(
        &self,
        messages: &[Message],
        model: &str,
        temperature: f64,
        should_auth: bool,
        tools: Option<&[serde_json::Value]>,
        think: Option<bool>,
    ) -> anyhow::Result<ApiChatResponse> {
        let request =
            self.build_chat_request_with_think(messages.to_vec(), model, temperature, tools, think);

        let url = format!("{}/api/chat", self.base_url);

        tracing::debug!(
            "Ollama request: url={} model={} message_count={} temperature={} think={:?} tool_count={}",
            url,
            model,
            request.messages.len(),
            temperature,
            request.think,
            request.tools.as_ref().map_or(0, |t| t.len()),
        );

        let mut request_builder = self.http_client().post(&url).json(&request);

        if should_auth && let Some(key) = self.api_key.as_ref() {
            request_builder = request_builder.bearer_auth(key);
        }

        let response = request_builder.send().await?;
        let status = response.status();
        tracing::debug!("Ollama response status: {}", status);

        let body = response.bytes().await?;
        tracing::debug!("Ollama response body length: {} bytes", body.len());

        if !status.is_success() {
            let raw = String::from_utf8_lossy(&body);
            let sanitized = super::sanitize_api_error(&raw);
            tracing::error!(
                "Ollama error response: status={} body_excerpt={}",
                status,
                sanitized
            );
            anyhow::bail!(
                "Ollama API error ({}): {}. Is Ollama running? (brew install ollama && ollama serve)",
                status,
                sanitized
            );
        }

        let chat_response: ApiChatResponse = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                let raw = String::from_utf8_lossy(&body);
                let sanitized = super::sanitize_api_error(&raw);
                tracing::error!(
                    "Ollama response deserialization failed: {e}. body_excerpt={}",
                    sanitized
                );
                anyhow::bail!("Failed to parse Ollama response: {e}");
            }
        };

        Ok(chat_response)
    }

    /// Send a request to Ollama and get the parsed response.
    /// Pass `tools` to enable native function-calling for models that support it.
    ///
    /// When `reasoning_enabled` (`think`) is set to `true`, the first request
    /// includes `think: true`.  If that request fails (the model may not support
    /// the `think` parameter), we automatically retry once with `think` omitted
    /// so the call succeeds instead of entering an infinite retry loop.
    async fn send_request(
        &self,
        messages: Vec<Message>,
        model: &str,
        temperature: f64,
        should_auth: bool,
        tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<ApiChatResponse> {
        let result = self
            .send_request_inner(
                &messages,
                model,
                temperature,
                should_auth,
                tools,
                self.reasoning_enabled,
            )
            .await;

        match result {
            Ok(resp) => Ok(resp),
            Err(first_err) if self.reasoning_enabled == Some(true) => {
                tracing::warn!(
                    model = model,
                    error = %first_err,
                    "Ollama request failed with think=true; retrying without reasoning \
                     (model may not support it)"
                );
                // Retry with think omitted from the request entirely.
                self.send_request_inner(&messages, model, temperature, should_auth, tools, None)
                    .await
                    .map_err(|retry_err| {
                        // Both attempts failed — return the original error for clarity.
                        tracing::error!(
                            model = model,
                            original_error = %first_err,
                            retry_error = %retry_err,
                            "Ollama request also failed without think; returning original error"
                        );
                        first_err
                    })
            }
            Err(e) => Err(e),
        }
    }

    /// Convert Ollama tool calls to the JSON format expected by parse_tool_calls in loop_.rs
    ///
    /// Handles quirky model behavior where tool calls are wrapped:
    /// - `{"name": "tool_call", "arguments": {"name": "shell", "arguments": {...}}}`
    /// - `{"name": "tool.shell", "arguments": {...}}`
    fn format_tool_calls_for_loop(&self, tool_calls: &[OllamaToolCall]) -> String {
        let formatted_calls: Vec<serde_json::Value> = tool_calls
            .iter()
            .map(|tc| {
                let (tool_name, tool_args) = self.extract_tool_name_and_args(tc);

                // Arguments must be a JSON string for parse_tool_calls compatibility
                let args_str =
                    serde_json::to_string(&tool_args).unwrap_or_else(|_| "{}".to_string());

                serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": args_str
                    }
                })
            })
            .collect();

        serde_json::json!({
            "content": "",
            "tool_calls": formatted_calls
        })
        .to_string()
    }

    /// Extract the actual tool name and arguments from potentially nested structures
    fn extract_tool_name_and_args(&self, tc: &OllamaToolCall) -> (String, serde_json::Value) {
        let name = &tc.function.name;
        let args = &tc.function.arguments;

        // Pattern 1: Nested tool_call wrapper (various malformed versions)
        // {"name": "tool_call", "arguments": {"name": "shell", "arguments": {"command": "date"}}}
        // {"name": "tool_call><json", "arguments": {"name": "shell", ...}}
        // {"name": "tool.call", "arguments": {"name": "shell", ...}}
        if (name == "tool_call"
            || name == "tool.call"
            || name.starts_with("tool_call>")
            || name.starts_with("tool_call<"))
            && let Some(nested_name) = args.get("name").and_then(|v| v.as_str())
        {
            let nested_args = args
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            tracing::debug!(
                "Unwrapped nested tool call: {} -> {} with args {:?}",
                name,
                nested_name,
                nested_args
            );
            return (nested_name.to_string(), nested_args);
        }

        // Pattern 2: Prefixed tool name (tool.shell, tool.file_read, etc.)
        if let Some(stripped) = name.strip_prefix("tool.") {
            return (stripped.to_string(), args.clone());
        }

        // Pattern 3: Normal tool call
        (name.clone(), args.clone())
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    // ── Provider-family defaults ──
    fn default_temperature(&self) -> f64 {
        TEMPERATURE_DEFAULT
    }

    fn default_timeout_secs(&self) -> u64 {
        TIMEOUT_SECS_DEFAULT
    }

    fn default_base_url(&self) -> Option<&str> {
        Some(BASE_URL)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: false,
            vision: true,
            prompt_caching: false,
            extended_thinking: false,
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let temperature = temperature.unwrap_or(self.default_temperature());
        let (normalized_model, should_auth) = self.resolve_request_details(model)?;

        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: Some(sys.to_string()),
                images: None,
                tool_calls: None,
                tool_name: None,
            });
        }

        let (user_content, user_images) = self.convert_user_message_content(message);
        messages.push(Message {
            role: "user".to_string(),
            content: user_content,
            images: user_images,
            tool_calls: None,
            tool_name: None,
        });

        let response = self
            .send_request(messages, &normalized_model, temperature, should_auth, None)
            .await?;

        // If model returned tool calls, format them for loop_.rs's parse_tool_calls
        if !response.message.tool_calls.is_empty() {
            tracing::debug!(
                "Ollama returned {} tool call(s), formatting for loop parser",
                response.message.tool_calls.len()
            );
            return Ok(self.format_tool_calls_for_loop(&response.message.tool_calls));
        }

        // Plain text response — strip <think> tags and fall back to thinking field.
        if let Some(content) = Self::effective_content(
            &response.message.content,
            response.message.thinking.as_deref(),
        ) {
            return Ok(content);
        }

        Ok(Self::fallback_text_for_empty_content(
            &normalized_model,
            response.message.thinking.as_deref(),
        ))
    }

    async fn chat_with_history(
        &self,
        messages: &[crate::traits::ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let temperature = temperature.unwrap_or(self.default_temperature());
        let (normalized_model, should_auth) = self.resolve_request_details(model)?;

        let api_messages = self.convert_messages(messages);

        let response = self
            .send_request(
                api_messages,
                &normalized_model,
                temperature,
                should_auth,
                None,
            )
            .await?;

        // If model returned tool calls, format them for loop_.rs's parse_tool_calls
        if !response.message.tool_calls.is_empty() {
            tracing::debug!(
                "Ollama returned {} tool call(s), formatting for loop parser",
                response.message.tool_calls.len()
            );
            return Ok(self.format_tool_calls_for_loop(&response.message.tool_calls));
        }

        // Plain text response — strip <think> tags and fall back to thinking field.
        if let Some(content) = Self::effective_content(
            &response.message.content,
            response.message.thinking.as_deref(),
        ) {
            return Ok(content);
        }

        Ok(Self::fallback_text_for_empty_content(
            &normalized_model,
            response.message.thinking.as_deref(),
        ))
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let temperature = temperature.unwrap_or(self.default_temperature());
        let (normalized_model, should_auth) = self.resolve_request_details(model)?;

        let api_messages = self.convert_messages(messages);

        // Tools arrive pre-formatted in OpenAI/Ollama-compatible JSON from
        // tools_to_openai_format() in loop_.rs — pass them through directly.
        let tools_opt = if tools.is_empty() { None } else { Some(tools) };

        let response = self
            .send_request(
                api_messages,
                &normalized_model,
                temperature,
                should_auth,
                tools_opt,
            )
            .await?;

        let usage = if response.prompt_eval_count.is_some() || response.eval_count.is_some() {
            Some(TokenUsage {
                input_tokens: response.prompt_eval_count,
                output_tokens: response.eval_count,
                cached_input_tokens: None,
            })
        } else {
            None
        };

        // Native tool calls returned by the model.
        if !response.message.tool_calls.is_empty() {
            let tool_calls: Vec<ToolCall> = response
                .message
                .tool_calls
                .iter()
                .map(|tc| {
                    let (name, args) = self.extract_tool_name_and_args(tc);
                    ToolCall {
                        id: tc
                            .id
                            .clone()
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        name,
                        arguments: serde_json::to_string(&args)
                            .unwrap_or_else(|_| "{}".to_string()),
                    }
                })
                .collect();
            let text = Self::normalize_response_text(response.message.content);
            return Ok(ChatResponse {
                text,
                tool_calls,
                usage,
                reasoning_content: None,
            });
        }

        // No native tool calls — use the effective content (content with
        // `<think>` tags stripped, falling back to thinking field).
        // The loop_.rs `parse_tool_calls` will extract any XML-style tool
        // calls from the text, so preserve `<tool_call>` tags here.
        let effective = Self::effective_content(
            &response.message.content,
            response.message.thinking.as_deref(),
        );
        let text = if let Some(content) = effective {
            content
        } else {
            Self::fallback_text_for_empty_content(
                &normalized_model,
                response.message.thinking.as_deref(),
            )
        };
        Ok(ChatResponse {
            text: Some(text),
            tool_calls: vec![],
            usage,
            reasoning_content: None,
        })
    }

    fn supports_native_tools(&self) -> bool {
        // Default to prompt-guided tool calling (XML instructions in system prompt)
        // because many Ollama-served models do not support Ollama's native
        // /api/chat tool-calling parameter. Models that lack support silently
        // ignore the tools array and emit tool-call JSON as plain text, which the
        // agent loop cannot parse without the XML protocol instructions.
        // See: https://github.com/zeroclaw-labs/zeroclaw/issues/3999
        false
    }

    async fn chat(
        &self,
        request: zeroclaw_api::provider::ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        // Convert ToolSpec to OpenAI-compatible JSON and delegate to chat_with_tools.
        if let Some(specs) = request.tools
            && !specs.is_empty()
        {
            let tools: Vec<serde_json::Value> = specs
                .iter()
                .map(|s| {
                    let params =
                        zeroclaw_api::schema::SchemaCleanr::clean_for_openai(s.parameters.clone());
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": s.name,
                            "description": s.description,
                            "parameters": params
                        }
                    })
                })
                .collect();
            return self
                .chat_with_tools(request.messages, &tools, model, temperature)
                .await;
        }

        // No tools — fall back to plain text chat.
        let text = self
            .chat_with_history(request.messages, model, temperature)
            .await?;
        Ok(ChatResponse {
            text: Some(text),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        })
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        // Local Ollama's /api/tags lists installed models and requires no auth.
        // Remote Ollama endpoints attach the Bearer key; local ones don't.
        let url = format!("{}/api/tags", self.base_url.trim_end_matches('/'));
        let mut request = self.http_client().get(&url);
        if !self.is_local_endpoint()
            && let Some(key) = self.api_key.as_deref()
        {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        let response = request.send().await?.error_for_status()?;

        #[derive(Deserialize)]
        struct Resp {
            models: Vec<Entry>,
        }
        #[derive(Deserialize)]
        struct Entry {
            name: String,
        }

        let body: Resp = response.json().await?;
        Ok(body.models.into_iter().map(|e| e.name).collect())
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_url() {
        let p = OllamaProvider::new(None, None);
        assert_eq!(p.base_url, "http://localhost:11434");
    }

    #[test]
    fn custom_url_trailing_slash() {
        let p = OllamaProvider::new(Some("http://192.168.1.100:11434/"), None);
        assert_eq!(p.base_url, "http://192.168.1.100:11434");
    }

    #[test]
    fn custom_url_no_trailing_slash() {
        let p = OllamaProvider::new(Some("http://myserver:11434"), None);
        assert_eq!(p.base_url, "http://myserver:11434");
    }

    #[test]
    fn custom_url_strips_api_suffix() {
        let p = OllamaProvider::new(Some("https://ollama.com/api/"), None);
        assert_eq!(p.base_url, "https://ollama.com");
    }

    #[test]
    fn custom_url_strips_api_chat_suffix() {
        let p = OllamaProvider::new(Some("http://172.30.30.50:11434/api/chat"), None);
        assert_eq!(p.base_url, "http://172.30.30.50:11434");
    }

    #[test]
    fn empty_url_uses_empty() {
        let p = OllamaProvider::new(Some(""), None);
        assert_eq!(p.base_url, "");
    }

    #[test]
    fn cloud_suffix_strips_model_name() {
        let p = OllamaProvider::new(Some("https://ollama.com"), Some("ollama-key"));
        let (model, should_auth) = p.resolve_request_details("qwen3:cloud").unwrap();
        assert_eq!(model, "qwen3");
        assert!(should_auth);
    }

    #[test]
    fn cloud_suffix_with_local_endpoint_errors() {
        let p = OllamaProvider::new(None, Some("ollama-key"));
        let error = p
            .resolve_request_details("qwen3:cloud")
            .expect_err("cloud suffix should fail on local endpoint");
        assert!(
            error
                .to_string()
                .contains("requested cloud routing, but Ollama endpoint is local")
        );
    }

    #[test]
    fn cloud_suffix_without_api_key_errors() {
        let p = OllamaProvider::new(Some("https://ollama.com"), None);
        let error = p
            .resolve_request_details("qwen3:cloud")
            .expect_err("cloud suffix should require API key");
        assert!(
            error
                .to_string()
                .contains("requested cloud routing, but no API key is configured")
        );
    }

    #[test]
    fn remote_endpoint_auth_enabled_when_key_present() {
        let p = OllamaProvider::new(Some("https://ollama.com"), Some("ollama-key"));
        let (_model, should_auth) = p.resolve_request_details("qwen3").unwrap();
        assert!(should_auth);
    }

    #[test]
    fn remote_endpoint_with_api_suffix_still_allows_cloud_models() {
        let p = OllamaProvider::new(Some("https://ollama.com/api"), Some("ollama-key"));
        let (model, should_auth) = p.resolve_request_details("qwen3:cloud").unwrap();
        assert_eq!(model, "qwen3");
        assert!(should_auth);
    }

    #[test]
    fn local_endpoint_auth_disabled_even_with_key() {
        let p = OllamaProvider::new(None, Some("ollama-key"));
        let (_model, should_auth) = p.resolve_request_details("llama3").unwrap();
        assert!(!should_auth);
    }

    #[test]
    fn request_omits_think_when_reasoning_not_configured() {
        let provider = OllamaProvider::new(None, None);
        let request = provider.build_chat_request(
            vec![Message {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                images: None,
                tool_calls: None,
                tool_name: None,
            }],
            "llama3",
            0.7,
            None,
        );

        let json = serde_json::to_value(request).unwrap();
        assert!(json.get("think").is_none());
    }

    #[test]
    fn request_includes_think_when_reasoning_configured() {
        let provider = OllamaProvider::new_with_reasoning(None, None, Some(false));
        let request = provider.build_chat_request(
            vec![Message {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                images: None,
                tool_calls: None,
                tool_name: None,
            }],
            "llama3",
            0.7,
            None,
        );

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json.get("think"), Some(&serde_json::json!(false)));
    }

    #[test]
    fn response_deserializes() {
        let json = r#"{"message":{"role":"assistant","content":"Hello from Ollama!"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message.content, "Hello from Ollama!");
    }

    #[test]
    fn response_with_empty_content() {
        let json = r#"{"message":{"role":"assistant","content":""}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.is_empty());
    }

    #[test]
    fn normalize_response_text_rejects_whitespace_only_content() {
        assert_eq!(
            OllamaProvider::normalize_response_text("\n \t".to_string()),
            None
        );
        assert_eq!(
            OllamaProvider::normalize_response_text(" hello ".to_string()),
            Some("hello".to_string())
        );
    }

    #[test]
    fn normalize_response_text_strips_think_tags() {
        assert_eq!(
            OllamaProvider::normalize_response_text("<think>reasoning</think> hello".to_string()),
            Some("hello".to_string())
        );
    }

    #[test]
    fn normalize_response_text_rejects_think_only_content() {
        assert_eq!(
            OllamaProvider::normalize_response_text(
                "<think>only thinking here</think>".to_string()
            ),
            None
        );
    }

    #[test]
    fn fallback_text_for_empty_content_without_thinking_is_generic() {
        let text = OllamaProvider::fallback_text_for_empty_content("qwen3-coder", None);
        assert!(text.contains("couldn't get a complete response from Ollama"));
    }

    #[test]
    fn response_with_missing_content_defaults_to_empty() {
        let json = r#"{"message":{"role":"assistant"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.is_empty());
    }

    #[test]
    fn response_with_thinking_field_extracts_content() {
        let json =
            r#"{"message":{"role":"assistant","content":"hello","thinking":"internal reasoning"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message.content, "hello");
    }

    #[test]
    fn response_with_tool_calls_parses_correctly() {
        let json = r#"{"message":{"role":"assistant","content":"","tool_calls":[{"id":"call_123","function":{"name":"shell","arguments":{"command":"date"}}}]}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.is_empty());
        assert_eq!(resp.message.tool_calls.len(), 1);
        assert_eq!(resp.message.tool_calls[0].function.name, "shell");
    }

    #[test]
    fn extract_tool_name_handles_nested_tool_call() {
        let provider = OllamaProvider::new(None, None);
        let tc = OllamaToolCall {
            id: Some("call_123".into()),
            function: OllamaFunction {
                name: "tool_call".into(),
                arguments: serde_json::json!({
                    "name": "shell",
                    "arguments": {"command": "date"}
                }),
            },
        };
        let (name, args) = provider.extract_tool_name_and_args(&tc);
        assert_eq!(name, "shell");
        assert_eq!(args.get("command").unwrap(), "date");
    }

    #[test]
    fn extract_tool_name_handles_prefixed_name() {
        let provider = OllamaProvider::new(None, None);
        let tc = OllamaToolCall {
            id: Some("call_123".into()),
            function: OllamaFunction {
                name: "tool.shell".into(),
                arguments: serde_json::json!({"command": "ls"}),
            },
        };
        let (name, args) = provider.extract_tool_name_and_args(&tc);
        assert_eq!(name, "shell");
        assert_eq!(args.get("command").unwrap(), "ls");
    }

    #[test]
    fn extract_tool_name_handles_normal_call() {
        let provider = OllamaProvider::new(None, None);
        let tc = OllamaToolCall {
            id: Some("call_123".into()),
            function: OllamaFunction {
                name: "file_read".into(),
                arguments: serde_json::json!({"path": "/tmp/test"}),
            },
        };
        let (name, args) = provider.extract_tool_name_and_args(&tc);
        assert_eq!(name, "file_read");
        assert_eq!(args.get("path").unwrap(), "/tmp/test");
    }

    #[test]
    fn format_tool_calls_produces_valid_json() {
        let provider = OllamaProvider::new(None, None);
        let tool_calls = vec![OllamaToolCall {
            id: Some("call_abc".into()),
            function: OllamaFunction {
                name: "shell".into(),
                arguments: serde_json::json!({"command": "date"}),
            },
        }];

        let formatted = provider.format_tool_calls_for_loop(&tool_calls);
        let parsed: serde_json::Value = serde_json::from_str(&formatted).unwrap();

        assert!(parsed.get("tool_calls").is_some());
        let calls = parsed.get("tool_calls").unwrap().as_array().unwrap();
        assert_eq!(calls.len(), 1);

        let func = calls[0].get("function").unwrap();
        assert_eq!(func.get("name").unwrap(), "shell");
        // arguments should be a string (JSON-encoded)
        assert!(func.get("arguments").unwrap().is_string());
    }

    #[test]
    fn convert_messages_parses_native_assistant_tool_calls() {
        let provider = OllamaProvider::new(None, None);
        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: r#"{"content":null,"tool_calls":[{"id":"call_1","name":"shell","arguments":"{\"command\":\"ls\"}"}]}"#.into(),
        }];

        let converted = provider.convert_messages(&messages);

        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "assistant");
        assert!(converted[0].content.is_none());
        let calls = converted[0]
            .tool_calls
            .as_ref()
            .expect("tool calls expected");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.name, "shell");
        assert_eq!(calls[0].function.arguments.get("command").unwrap(), "ls");
    }

    #[test]
    fn convert_messages_maps_tool_result_call_id_to_tool_name() {
        let provider = OllamaProvider::new(None, None);
        let messages = vec![
            ChatMessage {
                role: "assistant".into(),
                content: r#"{"content":null,"tool_calls":[{"id":"call_7","name":"file_read","arguments":"{\"path\":\"README.md\"}"}]}"#.into(),
            },
            ChatMessage {
                role: "tool".into(),
                content: r#"{"tool_call_id":"call_7","content":"ok"}"#.into(),
            },
        ];

        let converted = provider.convert_messages(&messages);

        assert_eq!(converted.len(), 2);
        assert_eq!(converted[1].role, "tool");
        assert_eq!(converted[1].tool_name.as_deref(), Some("file_read"));
        assert_eq!(converted[1].content.as_deref(), Some("ok"));
        assert!(converted[1].tool_calls.is_none());
    }

    #[test]
    fn convert_messages_extracts_images_from_user_marker() {
        let provider = OllamaProvider::new(None, None);
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "Inspect this screenshot [IMAGE:data:image/png;base64,abcd==]".into(),
        }];

        let converted = provider.convert_messages(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "user");
        assert_eq!(
            converted[0].content.as_deref(),
            Some("Inspect this screenshot")
        );
        let images = converted[0]
            .images
            .as_ref()
            .expect("images should be present");
        assert_eq!(images, &vec!["abcd==".to_string()]);
    }

    #[test]
    fn capabilities_disable_native_tools_and_enable_vision() {
        let provider = OllamaProvider::new(None, None);
        let caps = <OllamaProvider as Provider>::capabilities(&provider);
        assert!(
            !caps.native_tool_calling,
            "Ollama should default to prompt-guided tool calling"
        );
        assert!(caps.vision);
    }

    #[test]
    fn api_response_parses_eval_counts() {
        let json = r#"{
            "message": {"content": "Hello", "tool_calls": []},
            "prompt_eval_count": 50,
            "eval_count": 25
        }"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.prompt_eval_count, Some(50));
        assert_eq!(resp.eval_count, Some(25));
    }

    #[test]
    fn api_response_parses_without_eval_counts() {
        let json = r#"{"message": {"content": "Hello", "tool_calls": []}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.prompt_eval_count.is_none());
        assert!(resp.eval_count.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // <think> tag stripping tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn strip_think_tags_removes_single_block() {
        let input = "<think>internal reasoning</think>Hello world";
        assert_eq!(OllamaProvider::strip_think_tags(input), "Hello world");
    }

    #[test]
    fn strip_think_tags_removes_multiple_blocks() {
        let input = "<think>first</think>A<think>second</think>B";
        assert_eq!(OllamaProvider::strip_think_tags(input), "AB");
    }

    #[test]
    fn strip_think_tags_handles_unclosed_block() {
        let input = "visible<think>hidden tail";
        assert_eq!(OllamaProvider::strip_think_tags(input), "visible");
    }

    #[test]
    fn strip_think_tags_preserves_text_without_tags() {
        let input = "plain text response";
        assert_eq!(
            OllamaProvider::strip_think_tags(input),
            "plain text response"
        );
    }

    #[test]
    fn strip_think_tags_returns_empty_for_think_only() {
        let input = "<think>only thinking</think>";
        assert_eq!(OllamaProvider::strip_think_tags(input), "");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // effective_content tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn effective_content_strips_think_and_returns_rest() {
        let result = OllamaProvider::effective_content(
            "<think>reasoning</think>\n<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}</tool_call>",
            None,
        );
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(text.contains("<tool_call>"));
        assert!(!text.contains("<think>"));
    }

    #[test]
    fn effective_content_falls_back_to_thinking_field() {
        let result = OllamaProvider::effective_content(
            "",
            Some(
                "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"date\"}}</tool_call>",
            ),
        );
        assert!(result.is_some());
        assert!(result.unwrap().contains("<tool_call>"));
    }

    #[test]
    fn effective_content_returns_none_when_both_empty() {
        assert!(OllamaProvider::effective_content("", None).is_none());
        assert!(OllamaProvider::effective_content("", Some("")).is_none());
        assert!(
            OllamaProvider::effective_content(
                "<think>only thinking</think>",
                Some("<think>also only thinking</think>")
            )
            .is_none()
        );
    }

    #[test]
    fn effective_content_prefers_content_over_thinking() {
        let result = OllamaProvider::effective_content("content text", Some("thinking text"));
        assert_eq!(result, Some("content text".to_string()));
    }

    #[test]
    fn effective_content_uses_thinking_when_content_is_think_only() {
        let result = OllamaProvider::effective_content(
            "<think>just reasoning</think>",
            Some("actual useful text from thinking field"),
        );
        assert_eq!(
            result,
            Some("actual useful text from thinking field".to_string())
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Qwen tool-call regression scenario tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn qwen_think_with_tool_call_in_content_preserved() {
        // Qwen produces <think> tags followed by <tool_call> in content,
        // with no structured tool_calls. The <tool_call> tags must survive
        // for downstream parse_tool_calls to extract them.
        let content = "<think>I should list files</think>\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}\n</tool_call>";
        let result = OllamaProvider::effective_content(content, None);
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(text.contains("<tool_call>"));
        assert!(text.contains("shell"));
        assert!(!text.contains("<think>"));
    }

    #[test]
    fn qwen_thinking_field_with_tool_call_xml_extracted() {
        // When think=true, Ollama separates thinking, but Qwen may put tool
        // call XML in the thinking field with empty content.
        let content = "";
        let thinking = "I need to check the date\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"date\"}}\n</tool_call>";
        let result = OllamaProvider::effective_content(content, Some(thinking));
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(text.contains("<tool_call>"));
        assert!(text.contains("date"));
    }
}
