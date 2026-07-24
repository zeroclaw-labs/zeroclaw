use crate::openai_codex::{
    ResponsesStreamApiError, ResponsesStreamState, ResponsesToolSpec, append_utf8_stream_chunk,
    build_responses_input, convert_tools, first_nonempty, process_sse_chunk,
};
use crate::stream_guard::AbortOnDrop;
use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    ModelProvider, ProviderCapabilities, StreamChunk, StreamError, StreamEvent, StreamOptions,
    StreamResult, TokenUsage, ToolCall as ProviderToolCall,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use zeroclaw_api::tool::ToolSpec;

/// OpenAI's public API endpoint.
pub(crate) const BASE_URL: &str = "https://api.openai.com/v1";

/// Default endpoint for the OpenAI Responses API.
const RESPONSES_URL: &str = "https://api.openai.com/v1/responses";

/// Max wait for the next streaming body read before the connection is treated
/// as stalled. Streaming clients omit reqwest's overall `.timeout()` (it kills
/// long-running responses mid-stream), so without a per-read bound a connection
/// that goes silent after the headers park `bytes_stream().next().await` forever
/// and the turn hangs on "working". `read_timeout` caps the gap between reads and
/// converts a silent stall into a retryable stream error.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

pub struct OpenAiModelProvider {
    /// `[providers.models.openai.<alias>]` config-key alias.
    alias: String,
    base_url: String,
    credential: Option<String>,
    max_tokens: Option<u32>,
    timeout_secs: u64,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning/thinking models may return output in `reasoning_content`.
    #[serde(default)]
    reasoning_content: Option<String>,
}

impl ResponseMessage {
    fn effective_content(&self) -> String {
        match &self.content {
            Some(c) if !c.is_empty() => c.clone(),
            _ => self.reasoning_content.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    messages: Vec<NativeMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct NativeMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<NativeToolCall>>,
    /// Raw reasoning content from thinking models; pass-through for model_providers
    /// that require it in assistant tool-call history messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeToolSpec {
    #[serde(rename = "type")]
    kind: String,
    function: NativeToolFunctionSpec,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeToolFunctionSpec {
    name: String,
    description: String,
    /// `Arc`-shared with the tool registry's stored schema — serialized
    /// transparently, never deep-cloned per request
    parameters: std::sync::Arc<serde_json::Value>,
}

fn parse_native_tool_spec(value: serde_json::Value) -> anyhow::Result<NativeToolSpec> {
    let spec: NativeToolSpec = serde_json::from_value(value).map_err(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "openai: invalid tool spec"
        );
        anyhow::Error::msg(format!("Invalid OpenAI tool specification: {e}"))
    })?;

    if spec.kind != "function" {
        anyhow::bail!(
            "Invalid OpenAI tool specification: unsupported tool type '{}', expected 'function'",
            spec.kind
        );
    }

    Ok(spec)
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    function: NativeFunctionCall,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct NativeChatResponse {
    choices: Vec<NativeChoice>,
    #[serde(default)]
    usage: Option<UsageInfo>,
}

#[derive(Debug, Deserialize)]
struct UsageInfo {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct NativeChoice {
    message: NativeResponseMessage,
}

#[derive(Debug, Deserialize)]
struct NativeResponseMessage {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning/thinking models may return output in `reasoning_content`.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<NativeToolCall>>,
}

impl NativeResponseMessage {
    fn effective_content(&self) -> Option<String> {
        match &self.content {
            Some(c) if !c.is_empty() => Some(c.clone()),
            _ => self.reasoning_content.clone(),
        }
    }
}

/// Typed builder for [`OpenAiModelProvider`].
///
/// Only `alias` is required. `base_url` defaults to the module-level
/// `BASE_URL` constant, `credential` treats whitespace-only inputs as
/// missing, and `timeout_secs` uses the 120 s workspace default.
#[must_use]
pub struct OpenAiBuilder {
    alias: String,
    credential: Option<String>,
    base_url: Option<String>,
    max_tokens: Option<u32>,
    timeout_secs: Option<u64>,
}

impl OpenAiBuilder {
    /// Explicit API credential. Whitespace-only inputs collapse to
    /// `None`.
    pub fn credential(mut self, credential: Option<&str>) -> Self {
        self.credential = credential
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToString::to_string);
        self
    }

    /// Override the API endpoint. Trailing slashes are stripped.
    pub fn base_url(mut self, base_url: &str) -> Self {
        self.base_url = Some(base_url.trim_end_matches('/').to_string());
        self
    }

    /// Set the maximum output tokens for API requests.
    pub fn max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Override the HTTP request timeout for LLM API calls. Values of 0
    /// are ignored (the default 120 s is kept) so a stray `Some(0)` from
    /// config cannot silently disable the safety timeout.
    pub fn timeout_secs(mut self, secs: u64) -> Self {
        if secs > 0 {
            self.timeout_secs = Some(secs);
        }
        self
    }

    pub fn build(self) -> OpenAiModelProvider {
        OpenAiModelProvider {
            alias: self.alias,
            base_url: self.base_url.unwrap_or_else(|| BASE_URL.to_string()),
            credential: self.credential,
            max_tokens: self.max_tokens,
            timeout_secs: self.timeout_secs.unwrap_or(120),
        }
    }
}

impl OpenAiModelProvider {
    /// Entry point. Only `alias` is required; every other field is set
    /// via a labelled chain method on the returned [`OpenAiBuilder`].
    pub fn builder(alias: &str) -> OpenAiBuilder {
        OpenAiBuilder {
            alias: alias.to_string(),
            credential: None,
            base_url: None,
            max_tokens: None,
            timeout_secs: None,
        }
    }

    /// Adjust temperature for models that have specific requirements.
    /// Some OpenAI models (like gpt-5-mini, o1, o3, etc) only accept temperature=1.0.
    fn adjust_temperature_for_model(model: &str, requested_temperature: f64) -> f64 {
        // Models that require temperature=1.0
        let requires_1_0 = matches!(
            model,
            "gpt-5"
                | "gpt-5-2025-08-07"
                | "gpt-5-mini"
                | "gpt-5-mini-2025-08-07"
                | "gpt-5-nano"
                | "gpt-5-nano-2025-08-07"
                | "gpt-5.1-chat-latest"
                | "gpt-5.2-chat-latest"
                | "gpt-5.3-chat-latest"
                | "o1"
                | "o1-2024-12-17"
                | "o1-mini"
                | "o1-mini-2024-09-12"
                | "o3"
                | "o3-2025-04-16"
                | "o3-mini"
                | "o3-mini-2025-01-31"
                | "o4-mini"
                | "o4-mini-2025-04-16"
        );

        if requires_1_0 {
            1.0
        } else {
            requested_temperature
        }
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<NativeToolSpec>> {
        tools.map(|items| {
            items
                .iter()
                .map(|tool| NativeToolSpec {
                    kind: "function".to_string(),
                    function: NativeToolFunctionSpec {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: std::sync::Arc::clone(&tool.parameters),
                    },
                })
                .collect()
        })
    }

    fn convert_messages(messages: &[ChatMessage]) -> Vec<NativeMessage> {
        messages
            .iter()
            .map(|m| {
                if m.role == "assistant"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content)
                    && let Some(tool_calls_value) = value.get("tool_calls")
                    && let Ok(parsed_calls) =
                        serde_json::from_value::<Vec<ProviderToolCall>>(tool_calls_value.clone())
                {
                    let tool_calls = parsed_calls
                        .into_iter()
                        .map(|tc| {
                            let name = tc.name;
                            NativeToolCall {
                                id: Some(tc.id),
                                kind: Some("function".to_string()),
                                function: NativeFunctionCall {
                                    arguments: crate::compatible::sanitize_tool_arguments(
                                        &name,
                                        &tc.arguments,
                                    ),
                                    name,
                                },
                            }
                        })
                        .collect::<Vec<_>>();
                    let content = crate::request_payload::non_empty_string_field(&value, "content");
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

                if m.role == "tool"
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content)
                {
                    let tool_call_id = value
                        .get("tool_call_id")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
                    return NativeMessage {
                        role: "tool".to_string(),
                        content,
                        tool_call_id,
                        tool_calls: None,
                        reasoning_content: None,
                    };
                }

                NativeMessage {
                    role: m.role.clone(),
                    content: Some(m.content.clone()),
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning_content: None,
                }
            })
            .collect()
    }

    fn parse_native_response(message: NativeResponseMessage) -> ProviderChatResponse {
        let text = message.effective_content();
        let reasoning_content = message.reasoning_content.clone();
        let tool_calls = message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ProviderToolCall {
                id: tc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: tc.function.name,
                arguments: tc.function.arguments,
                extra_content: None,
            })
            .collect::<Vec<_>>();

        ProviderChatResponse {
            text,
            tool_calls,
            usage: None,
            reasoning_content,
        }
    }

    fn http_client(&self) -> Client {
        zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            "model_provider.openai",
            self.timeout_secs,
            10,
        )
    }
}

#[async_trait]
impl ModelProvider for OpenAiModelProvider {
    // ── ModelProvider-family defaults ──
    fn default_base_url(&self) -> Option<&str> {
        Some(BASE_URL)
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"missing": "credentials"})),
                "openai: API key not configured"
            );
            anyhow::Error::msg("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;

        let adjusted_temperature =
            temperature.map(|t| Self::adjust_temperature_for_model(model, t));

        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: sys.to_string(),
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: message.to_string(),
        });

        let request = ChatRequest {
            model: model.to_string(),
            messages,
            temperature: adjusted_temperature,
            max_tokens: self.max_tokens,
        };

        let response = self
            .http_client()
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {credential}"))
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenAI", response).await);
        }

        let chat_response: ChatResponse = response.json().await?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.effective_content())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "openai: empty choices in response"
                );
                anyhow::Error::msg("No response from OpenAI")
            })
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"missing": "credentials"})),
                "openai: API key not configured"
            );
            anyhow::Error::msg("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;

        let adjusted_temperature =
            temperature.map(|t| Self::adjust_temperature_for_model(model, t));

        let tools = Self::convert_tools(request.tools);
        let tools_count = tools.as_ref().map_or(0, Vec::len);
        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(request.messages),
            temperature: adjusted_temperature,
            tool_choice: tools
                .as_ref()
                .and_then(|t| (!t.is_empty()).then(|| "auto".to_string())),
            tools,
            max_tokens: self.max_tokens,
        };
        if ::zeroclaw_log::debug_enabled() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_attrs(::serde_json::json!({
                        "provider": "openai",
                        "alias": &self.alias,
                        "request_api": "chat_completions",
                        "model": model,
                        "stream": false,
                        "tools_count": tools_count,
                        "tool_choice": native_request.tool_choice.as_deref(),
                    })),
                "openai provider request prepared"
            );
        }

        let response = self
            .http_client()
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {credential}"))
            .json(&native_request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenAI", response).await);
        }

        let native_response: NativeChatResponse = response.json().await?;
        let usage = native_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_input_tokens: u.prompt_tokens_details.and_then(|d| d.cached_tokens),
        });
        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "openai: empty choices in response"
                );
                anyhow::Error::msg("No response from OpenAI")
            })?;
        let mut result = Self::parse_native_response(message);
        result.usage = usage;
        Ok(result)
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"missing": "credentials"})),
                "openai: API key not configured"
            );
            anyhow::Error::msg("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;

        let adjusted_temperature =
            temperature.map(|t| Self::adjust_temperature_for_model(model, t));

        let native_tools: Option<Vec<NativeToolSpec>> = if tools.is_empty() {
            None
        } else {
            Some(
                tools
                    .iter()
                    .cloned()
                    .map(parse_native_tool_spec)
                    .collect::<Result<Vec<_>, _>>()?,
            )
        };

        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(messages),
            temperature: adjusted_temperature,
            // See above: omit tool_choice when the tool list is empty.
            tool_choice: native_tools
                .as_ref()
                .and_then(|t| (!t.is_empty()).then(|| "auto".to_string())),
            tools: native_tools,
            max_tokens: self.max_tokens,
        };

        let response = self
            .http_client()
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {credential}"))
            .json(&native_request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenAI", response).await);
        }

        let native_response: NativeChatResponse = response.json().await?;
        let usage = native_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_input_tokens: u.prompt_tokens_details.and_then(|d| d.cached_tokens),
        });
        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "openai: empty choices in response"
                );
                anyhow::Error::msg("No response from OpenAI")
            })?;
        let mut result = Self::parse_native_response(message);
        result.usage = usage;
        Ok(result)
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        if let Some(credential) = self.credential.as_ref() {
            self.http_client()
                .get(format!("{}/models", self.base_url))
                .header("Authorization", format!("Bearer {credential}"))
                .send()
                .await?
                .error_for_status()?;
        }
        Ok(())
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        // OpenAI's /v1/models requires a credential. models.dev is the no-auth
        // path onboard uses before the user has entered a key.
        crate::models_dev::list_models_for("openai").await
    }
}

impl ::zeroclaw_api::attribution::Attributable for OpenAiModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::OpenAi,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

/// Request body for the standard OpenAI Responses API.
#[derive(Debug, Serialize)]
struct ResponsesApiRequest {
    model: String,
    input: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ResponsesToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ResponsesApiReasoning>,
}

#[derive(Debug, Serialize)]
struct ResponsesApiReasoning {
    effort: String,
}

fn has_responses_tools(tools: Option<&[ResponsesToolSpec]>) -> bool {
    tools.is_some_and(|tools| !tools.is_empty())
}

/// Non-streaming response body from `/v1/responses`.
#[derive(Debug, Deserialize)]
struct ResponsesApiBody {
    #[serde(default)]
    output: Vec<serde_json::Value>,
    #[serde(default)]
    output_text: Option<String>,
}

fn extract_responses_api_text(body: &ResponsesApiBody) -> Option<String> {
    if let Some(text) = first_nonempty(body.output_text.as_deref()) {
        return Some(text);
    }
    for item in &body.output {
        if item.get("type").and_then(serde_json::Value::as_str) != Some("message") {
            continue;
        }
        if let Some(parts) = item.get("content").and_then(serde_json::Value::as_array) {
            for part in parts {
                if part.get("type").and_then(serde_json::Value::as_str) == Some("output_text")
                    && let Some(text) =
                        first_nonempty(part.get("text").and_then(serde_json::Value::as_str))
                {
                    return Some(text);
                }
            }
        }
    }
    None
}

fn extract_responses_api_tool_calls(body: &ResponsesApiBody) -> Vec<ProviderToolCall> {
    body.output
        .iter()
        .filter(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
        })
        .filter_map(|item| {
            let name = item
                .get("name")
                .and_then(serde_json::Value::as_str)?
                .to_string();
            let arguments = item
                .get("arguments")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("{}")
                .to_string();
            let id = item
                .get("call_id")
                .and_then(serde_json::Value::as_str)
                .or_else(|| item.get("id").and_then(serde_json::Value::as_str))
                .map(ToString::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            Some(ProviderToolCall {
                id,
                name,
                arguments,
                extra_content: None,
            })
        })
        .collect()
}

/// Drive a Responses API SSE connection to completion, emitting events on `tx`.
/// `request_builder` must already have URL, auth headers, `Accept: text/event-stream`,
/// and the JSON body attached. Sends `StreamEvent::Final` on clean stream end.
pub(crate) async fn run_responses_sse(
    request_builder: reqwest::RequestBuilder,
    tx: &tokio::sync::mpsc::Sender<StreamResult<StreamEvent>>,
    count_tokens: bool,
) {
    let http_response = match request_builder.send().await {
        Ok(r) => r,
        Err(err) => {
            let _ = tx
                .send(Err(StreamError::ModelProvider(err.to_string())))
                .await;
            return;
        }
    };

    if !http_response.status().is_success() {
        let status = http_response.status();
        let body = http_response.text().await.unwrap_or_default();
        let sanitized = super::sanitize_api_error(&body);
        let _ = tx
            .send(Err(StreamError::ModelProvider(format!(
                "OpenAI API error ({status}): {sanitized}"
            ))))
            .await;
        return;
    }

    let mut state = ResponsesStreamState::default();
    let mut byte_stream = http_response.bytes_stream();
    let mut pending_utf8: Vec<u8> = Vec::new();
    let mut chunk_buf = String::new();

    loop {
        match byte_stream.next().await {
            Some(Ok(bytes)) => {
                if let Err(err) =
                    append_utf8_stream_chunk(&mut chunk_buf, &mut pending_utf8, &bytes)
                {
                    let _ = tx
                        .send(Err(StreamError::ModelProvider(err.to_string())))
                        .await;
                    return;
                }
            }
            Some(Err(err)) => {
                let _ = tx
                    .send(Err(StreamError::ModelProvider(err.to_string())))
                    .await;
                return;
            }
            None => break,
        }

        while let Some(idx) = chunk_buf.find("\n\n") {
            let chunk_str = chunk_buf[..idx].to_string();
            chunk_buf = chunk_buf[idx + 2..].to_string();

            match process_sse_chunk(&chunk_str, &mut state) {
                Ok(events) => {
                    for event in events {
                        if let StreamEvent::TextDelta(ref chunk) = event {
                            let event = if count_tokens {
                                StreamEvent::TextDelta(
                                    StreamChunk::delta(chunk.delta.clone()).with_token_estimate(),
                                )
                            } else {
                                event
                            };
                            if tx.send(Ok(event)).await.is_err() {
                                return;
                            }
                        } else if tx.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(err) => {
                    if err.downcast_ref::<ResponsesStreamApiError>().is_some() {
                        let _ = tx
                            .send(Err(StreamError::ModelProvider(err.to_string())))
                            .await;
                        return;
                    }
                }
            }
        }
    }

    if !chunk_buf.trim().is_empty()
        && let Ok(events) = process_sse_chunk(&chunk_buf, &mut state)
    {
        for event in events {
            let _ = tx.send(Ok(event)).await;
        }
    }

    if !state.saw_text_delta
        && let Some(text) = state.fallback_text.filter(|t| !t.is_empty())
    {
        let chunk = if count_tokens {
            StreamChunk::delta(text).with_token_estimate()
        } else {
            StreamChunk::delta(text)
        };
        let _ = tx.send(Ok(StreamEvent::TextDelta(chunk))).await;
    }

    crate::stream_guard::finish_sse_stream(
        tx,
        state.saw_completion,
        "response.completed or [DONE]",
    )
    .await;
}

pub struct OpenAiResponsesModelProvider {
    alias: String,
    responses_url: String,
    credential: Option<String>,
    max_tokens: Option<u32>,
    reasoning_effort: Option<String>,
    /// HTTP request timeout in seconds for non-streaming LLM API calls.
    /// Streaming SSE calls use `streaming_client` which sets only a
    /// connect timeout so long-running responses aren't killed mid-stream.
    /// Default: 120 (matches `OpenAiCompatibleModelProvider`).
    timeout_secs: u64,
    extra_headers: std::collections::HashMap<String, String>,
}

/// Typed builder for [`OpenAiResponsesModelProvider`].
///
/// Only `alias` is required. `api_url` defaults to the OpenAI Responses
/// endpoint; if a custom URL is supplied, `/responses` is appended when
/// not already present so callers can pass either shape. Every runtime
/// override (`timeout_secs` / `max_tokens` / `reasoning_effort` /
/// `extra_headers`) is set via a chain method on this builder before
/// [`Self::build`] — the built provider itself has no post-construction
/// mutators.
#[must_use]
pub struct OpenAiResponsesBuilder {
    alias: String,
    api_url: Option<String>,
    credential: Option<String>,
    max_tokens: Option<u32>,
    reasoning_effort: Option<String>,
    timeout_secs: Option<u64>,
    extra_headers: std::collections::HashMap<String, String>,
}

impl OpenAiResponsesBuilder {
    /// Override the API endpoint. The `/responses` suffix is appended
    /// automatically if the input does not already end in it.
    pub fn api_url(mut self, api_url: &str) -> Self {
        self.api_url = Some(api_url.to_string());
        self
    }

    /// Explicit API credential. Whitespace-only inputs collapse to
    /// `None`.
    pub fn credential(mut self, credential: Option<&str>) -> Self {
        self.credential = credential
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToString::to_string);
        self
    }

    pub fn max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn reasoning_effort(mut self, reasoning_effort: Option<String>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    /// Override the non-streaming HTTP request timeout. Values of 0 are
    /// ignored (the default 120 s is kept) so a stray `Some(0)` from
    /// config cannot silently disable the safety timeout — same guard
    /// applied by [`OpenAiBuilder::timeout_secs`],
    /// [`crate::compatible::OpenAiCompatibleBuilder::timeout_secs`], and
    /// [`crate::openrouter::OpenRouterBuilder::timeout_secs`].
    pub fn timeout_secs(mut self, secs: u64) -> Self {
        if secs > 0 {
            self.timeout_secs = Some(secs);
        }
        self
    }

    /// Set extra HTTP headers to include on every request. Reserved
    /// keys (e.g. `Authorization`) are dropped at request-build time —
    /// see [`OpenAiResponsesModelProvider`] for details.
    pub fn extra_headers(mut self, headers: std::collections::HashMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    pub fn build(self) -> OpenAiResponsesModelProvider {
        let responses_url = self
            .api_url
            .as_deref()
            .map(|url| {
                let trimmed = url.trim_end_matches('/');
                if trimmed.ends_with("/responses") {
                    trimmed.to_string()
                } else {
                    format!("{trimmed}/responses")
                }
            })
            .unwrap_or_else(|| RESPONSES_URL.to_string());
        OpenAiResponsesModelProvider {
            alias: self.alias,
            responses_url,
            credential: self.credential,
            max_tokens: self.max_tokens,
            reasoning_effort: self.reasoning_effort,
            timeout_secs: self.timeout_secs.unwrap_or(120),
            extra_headers: self.extra_headers,
        }
    }
}

impl OpenAiResponsesModelProvider {
    /// Entry point. Only `alias` is required; every other field is set
    /// via a labelled chain method on the returned [`OpenAiResponsesBuilder`].
    pub fn builder(alias: &str) -> OpenAiResponsesBuilder {
        OpenAiResponsesBuilder {
            alias: alias.to_string(),
            api_url: None,
            credential: None,
            max_tokens: None,
            reasoning_effort: None,
            timeout_secs: None,
            extra_headers: std::collections::HashMap::new(),
        }
    }

    fn build_request(
        &self,
        instructions: Option<String>,
        input: Vec<serde_json::Value>,
        tools: Option<Vec<ResponsesToolSpec>>,
        model: &str,
        temperature: Option<f64>,
        stream: bool,
    ) -> ResponsesApiRequest {
        let has_tools = has_responses_tools(tools.as_deref());
        let reasoning = self
            .reasoning_effort
            .as_deref()
            .map(|effort| ResponsesApiReasoning {
                effort: effort.to_string(),
            });
        ResponsesApiRequest {
            model: model.to_string(),
            input,
            instructions,
            stream,
            tools,
            tool_choice: has_tools.then(|| "auto".to_string()),
            parallel_tool_calls: has_tools.then_some(true),
            temperature,
            max_output_tokens: self.max_tokens,
            reasoning,
        }
    }

    fn build_default_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (key, value) in &self.extra_headers {
            if key.eq_ignore_ascii_case("authorization") {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "header": key,
                            "reason": "reserved_authorization_overridden_by_provider_credential",
                        })),
                    "Dropping reserved 'Authorization' entry from extra_headers; built-in provider credential is authoritative. Rotate the credential via the 'credential' constructor argument instead."
                );
                continue;
            }
            match (
                HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                (Ok(name), Ok(val)) => {
                    headers.insert(name, val);
                }
                _ => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"header": key})),
                        "Skipping invalid extra header name or value"
                    );
                }
            }
        }
        headers
    }

    fn http_client(&self) -> Client {
        let default_headers = self.build_default_headers();
        let mut builder = Client::builder()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .connect_timeout(std::time::Duration::from_secs(10));
        if !default_headers.is_empty() {
            builder = builder.default_headers(default_headers);
        }
        builder.build().unwrap_or_else(|_| Client::new())
    }

    fn streaming_client(&self) -> Client {
        let default_headers = self.build_default_headers();
        let mut builder = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(STREAM_IDLE_TIMEOUT);
        if !default_headers.is_empty() {
            builder = builder.default_headers(default_headers);
        }
        builder.build().unwrap_or_else(|_| Client::new())
    }
}

#[async_trait]
impl ModelProvider for OpenAiResponsesModelProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: false,
            prompt_caching: false,
            extended_thinking: false,
        }
    }

    /// Reports the instance's resolved endpoint so callers can verify which
    /// host a responses provider will actually hit (e.g. a compat family's
    /// default base vs. OpenAI's).
    fn default_base_url(&self) -> Option<&str> {
        Some(&self.responses_url)
    }

    fn default_wire_api(&self) -> &str {
        "responses"
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_streaming_tool_events(&self) -> bool {
        true
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::Error::msg("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;
        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(ChatMessage::system(sys));
        }
        messages.push(ChatMessage::user(message));
        let (instructions, input) = build_responses_input(&messages);
        let instructions = if instructions.is_empty() {
            None
        } else {
            Some(instructions)
        };
        let req = self.build_request(instructions, input, None, model, temperature, false);
        let response = self
            .http_client()
            .post(&self.responses_url)
            .header("Authorization", format!("Bearer {credential}"))
            .json(&req)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(super::api_error("OpenAI", response).await);
        }
        let body: ResponsesApiBody = response.json().await?;
        extract_responses_api_text(&body)
            .ok_or_else(|| anyhow::Error::msg("No response from OpenAI"))
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::Error::msg("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;
        let (instructions, input) = build_responses_input(request.messages);
        let instructions = if instructions.is_empty() {
            None
        } else {
            Some(instructions)
        };
        let tools = convert_tools(request.tools);
        let tools_count = tools.as_ref().map_or(0, Vec::len);
        let req = self.build_request(instructions, input, tools, model, temperature, false);
        if ::zeroclaw_log::debug_enabled() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_attrs(::serde_json::json!({
                        "provider": "openai",
                        "alias": &self.alias,
                        "request_api": "responses",
                        "model": model,
                        "stream": false,
                        "tools_count": tools_count,
                        "tool_choice": req.tool_choice.as_deref(),
                        "parallel_tool_calls": req.parallel_tool_calls,
                    })),
                "openai responses provider request prepared"
            );
        }
        let response = self
            .http_client()
            .post(&self.responses_url)
            .header("Authorization", format!("Bearer {credential}"))
            .json(&req)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(super::api_error("OpenAI", response).await);
        }
        let body: ResponsesApiBody = response.json().await?;
        Ok(ProviderChatResponse {
            text: extract_responses_api_text(&body),
            tool_calls: extract_responses_api_tool_calls(&body),
            usage: None,
            reasoning_content: None,
        })
    }

    fn stream_chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        if !options.enabled {
            return stream::once(async { Ok(StreamEvent::Final) }).boxed();
        }

        let credential = match self.credential.clone() {
            Some(c) => c,
            None => {
                let err = StreamError::ModelProvider("OpenAI API key not set".to_string());
                return stream::once(async move { Err(err) }).boxed();
            }
        };

        let messages_owned = request.messages.to_vec();
        let tools_owned = request.tools.map(<[ToolSpec]>::to_vec);
        let model = model.to_string();
        let responses_url = self.responses_url.clone();
        let count_tokens = options.count_tokens;
        let reasoning_effort = self.reasoning_effort.clone();
        let max_tokens = self.max_tokens;
        let client = self.streaming_client();
        let alias = ::zeroclaw_log::debug_enabled().then(|| self.alias.clone());

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);
        let handle = ::zeroclaw_spawn::spawn!(async move {
            let (instructions, input) = build_responses_input(&messages_owned);
            let instructions = if instructions.is_empty() {
                None
            } else {
                Some(instructions)
            };
            let tools = convert_tools(tools_owned.as_deref());
            let tools_count = tools.as_ref().map_or(0, Vec::len);
            let has_tools = has_responses_tools(tools.as_deref());
            let reasoning = reasoning_effort
                .as_deref()
                .map(|effort| ResponsesApiReasoning {
                    effort: effort.to_string(),
                });
            let req = ResponsesApiRequest {
                model,
                input,
                instructions,
                stream: true,
                tools,
                tool_choice: has_tools.then(|| "auto".to_string()),
                parallel_tool_calls: has_tools.then_some(true),
                temperature,
                max_output_tokens: max_tokens,
                reasoning,
            };
            if let Some(alias) = alias.as_deref() {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                        .with_attrs(::serde_json::json!({
                            "provider": "openai",
                            "alias": alias,
                            "request_api": "responses",
                            "model": &req.model,
                            "stream": true,
                            "tools_count": tools_count,
                            "tool_choice": req.tool_choice.as_deref(),
                            "parallel_tool_calls": req.parallel_tool_calls,
                        })),
                    "openai responses streaming provider request prepared"
                );
            }

            let request_builder = client
                .post(&responses_url)
                .header("Authorization", format!("Bearer {credential}"))
                .header("Accept", "text/event-stream")
                .json(&req);

            run_responses_sse(request_builder, &tx, count_tokens).await;
        });

        let guard = AbortOnDrop::new(handle.abort_handle());
        stream::unfold((rx, guard), |(mut rx, guard)| async move {
            rx.recv().await.map(|event| (event, (rx, guard)))
        })
        .boxed()
    }
}

impl ::zeroclaw_api::attribution::Attributable for OpenAiResponsesModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::OpenAi,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_key() {
        let p = OpenAiModelProvider::builder("test")
            .credential(Some("openai-test-credential"))
            .build();
        assert_eq!(p.credential.as_deref(), Some("openai-test-credential"));
    }

    #[test]
    fn creates_without_key() {
        let p = OpenAiModelProvider::builder("test")
            .credential(None)
            .build();
        assert!(p.credential.is_none());
    }

    #[test]
    fn responses_url_appends_responses_to_custom_base() {
        let p = OpenAiResponsesModelProvider::builder("opencode")
            .api_url("https://opencode.ai/zen/v1")
            .credential(None)
            .build();
        assert_eq!(p.responses_url, "https://opencode.ai/zen/v1/responses");
    }

    #[test]
    fn responses_url_defaults_to_openai_when_base_absent() {
        let p = OpenAiResponsesModelProvider::builder("test")
            .credential(None)
            .build();
        assert_eq!(p.responses_url, RESPONSES_URL);
    }

    #[test]
    fn responses_provider_defaults_timeout_to_120() {
        let p = OpenAiResponsesModelProvider::builder("test").build();
        assert_eq!(
            p.timeout_secs, 120,
            "fresh provider must default timeout_secs to 120 (matches OpenAiCompatibleModelProvider)"
        );
    }

    #[test]
    fn responses_provider_defaults_extra_headers_to_empty() {
        let p = OpenAiResponsesModelProvider::builder("test").build();
        assert!(
            p.extra_headers.is_empty(),
            "fresh provider must default extra_headers to an empty HashMap"
        );
    }

    #[test]
    fn timeout_secs_overrides_default() {
        let p = OpenAiResponsesModelProvider::builder("test")
            .timeout_secs(45)
            .build();
        assert_eq!(
            p.timeout_secs, 45,
            "builder .timeout_secs(...) must override the 120 default"
        );
    }

    #[test]
    fn extra_headers_propagates() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("X-Title".to_string(), "zeroclaw".to_string());
        headers.insert(
            "HTTP-Referer".to_string(),
            "https://example.com".to_string(),
        );
        let p = OpenAiResponsesModelProvider::builder("test")
            .extra_headers(headers)
            .build();
        assert_eq!(
            p.extra_headers.len(),
            2,
            "builder .extra_headers(...) must store the configured headers"
        );
        assert_eq!(
            p.extra_headers.get("X-Title").map(String::as_str),
            Some("zeroclaw"),
            "configured extra_headers entry must be retrievable by name"
        );
        assert_eq!(
            p.extra_headers.get("HTTP-Referer").map(String::as_str),
            Some("https://example.com"),
            "configured extra_headers entry must be retrievable by name"
        );
    }

    #[test]
    fn build_default_headers_is_empty_when_no_extra_headers() {
        let p = OpenAiResponsesModelProvider::builder("test").build();
        let headers = p.build_default_headers();
        assert!(
            headers.is_empty(),
            "build_default_headers must return an empty HeaderMap when extra_headers is empty"
        );
    }

    #[test]
    fn build_default_headers_includes_every_configured_entry() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("X-Title".to_string(), "zeroclaw".to_string());
        headers.insert(
            "HTTP-Referer".to_string(),
            "https://example.com".to_string(),
        );
        let p = OpenAiResponsesModelProvider::builder("test")
            .extra_headers(headers)
            .build();
        let default_headers = p.build_default_headers();
        assert_eq!(
            default_headers.len(),
            2,
            "every configured extra_headers entry must appear in build_default_headers output"
        );
        assert_eq!(
            default_headers.get("X-Title").and_then(|v| v.to_str().ok()),
            Some("zeroclaw"),
            "X-Title must round-trip into the HeaderMap"
        );
        assert_eq!(
            default_headers
                .get("HTTP-Referer")
                .and_then(|v| v.to_str().ok()),
            Some("https://example.com"),
            "HTTP-Referer must round-trip into the HeaderMap"
        );
    }

    #[test]
    fn build_default_headers_skips_invalid_header_name_without_panicking() {
        // A name with a space is invalid per RFC 7230; `HeaderName::from_bytes`
        // returns `Err`. The builder must log WARN and skip the entry rather
        // than panicking, matching `OpenAiCompatibleModelProvider::http_client`.
        let mut headers = std::collections::HashMap::new();
        headers.insert("X Valid".to_string(), "ok".to_string()); // space → invalid
        headers.insert("X-Also-Valid".to_string(), "ok".to_string());
        let p = OpenAiResponsesModelProvider::builder("test")
            .extra_headers(headers)
            .build();
        let default_headers = p.build_default_headers();
        assert_eq!(
            default_headers.len(),
            1,
            "only the valid header name should land in the HeaderMap"
        );
        assert!(
            default_headers.get("X-Also-Valid").is_some(),
            "X-Also-Valid must be present in the HeaderMap"
        );
    }

    #[test]
    fn build_default_headers_skips_invalid_header_value_without_panicking() {
        // A value containing a NUL byte is invalid per RFC 7230;
        // `HeaderValue::from_str` returns `Err`. The builder must skip
        // the entry rather than panicking.
        let mut headers = std::collections::HashMap::new();
        headers.insert("X-Bad-Value".to_string(), "has\0nul".to_string()); // NUL → invalid
        headers.insert("X-Good-Value".to_string(), "ok".to_string());
        let p = OpenAiResponsesModelProvider::builder("test")
            .extra_headers(headers)
            .build();
        let default_headers = p.build_default_headers();
        assert_eq!(
            default_headers.len(),
            1,
            "only the valid header value should land in the HeaderMap"
        );
        assert!(
            default_headers.get("X-Good-Value").is_some(),
            "X-Good-Value must be present in the HeaderMap"
        );
    }

    #[test]
    fn build_default_headers_drops_authorization_in_favor_of_builtin() {
        let mut headers = std::collections::HashMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer operator-override".to_string(),
        );
        let p = OpenAiResponsesModelProvider::builder("test")
            .api_url("sk-builtin")
            .extra_headers(headers)
            .build();
        let default_headers = p.build_default_headers();
        assert!(
            default_headers.get("Authorization").is_none(),
            "operator-set Authorization must be dropped from default_headers so the built-in provider credential stays authoritative on the wire"
        );
        assert_eq!(
            default_headers.len(),
            0,
            "the only configured extra_headers entry was the reserved Authorization and must not appear in the resulting HeaderMap"
        );
    }

    #[test]
    fn build_default_headers_drops_authorization_case_insensitively() {
        // `authorization`, `AUTHORIZATION`, `Authorization` must all be
        // dropped — reqwest stores header names lowercased internally,
        // so the public contract is "any case". Verify all three.
        for variant in [
            "Authorization",
            "authorization",
            "AUTHORIZATION",
            "AuThOrIzAtIoN",
        ] {
            let mut headers = std::collections::HashMap::new();
            headers.insert(variant.to_string(), "Bearer x".to_string());
            let p = OpenAiResponsesModelProvider::builder("test")
                .api_url("sk-builtin")
                .extra_headers(headers)
                .build();
            let default_headers = p.build_default_headers();
            assert!(
                default_headers.get("Authorization").is_none(),
                "case variant {variant:?} must be dropped from default_headers (case-insensitive)"
            );
            assert_eq!(
                default_headers.len(),
                0,
                "case variant {variant:?} must produce an empty HeaderMap (only reserved Authorization configured)"
            );
        }
    }

    #[test]
    fn build_default_headers_preserves_non_authorization_extra_headers() {
        let mut headers = std::collections::HashMap::new();
        headers.insert(
            "Authorization".to_string(),
            "Bearer operator-override".to_string(),
        );
        headers.insert("X-Title".to_string(), "zeroclaw".to_string());
        headers.insert(
            "HTTP-Referer".to_string(),
            "https://example.com".to_string(),
        );
        headers.insert("X-Trace-Id".to_string(), "trace-123".to_string());
        let p = OpenAiResponsesModelProvider::builder("test")
            .api_url("sk-builtin")
            .extra_headers(headers)
            .build();
        let default_headers = p.build_default_headers();
        assert_eq!(
            default_headers.len(),
            3,
            "only the reserved Authorization is dropped; the three other custom headers must flow through"
        );
        assert!(
            default_headers.get("Authorization").is_none(),
            "Authorization must not appear in default_headers regardless of other entries"
        );
        assert_eq!(
            default_headers.get("X-Title").and_then(|v| v.to_str().ok()),
            Some("zeroclaw"),
            "X-Title must round-trip"
        );
        assert_eq!(
            default_headers
                .get("HTTP-Referer")
                .and_then(|v| v.to_str().ok()),
            Some("https://example.com"),
            "HTTP-Referer must round-trip"
        );
        assert_eq!(
            default_headers
                .get("X-Trace-Id")
                .and_then(|v| v.to_str().ok()),
            Some("trace-123"),
            "X-Trace-Id must round-trip"
        );
    }

    #[test]
    fn empty_key_is_treated_as_missing() {
        // Whitespace-only / empty credentials collapse to None so a stray
        // "" from config cannot produce a bogus `Bearer ` header. Matches
        // the trim-then-filter contract shared with anthropic/openrouter/
        // azure/compat.
        let p = OpenAiModelProvider::builder("test")
            .credential(Some(""))
            .build();
        assert!(p.credential.is_none());

        let p = OpenAiModelProvider::builder("test")
            .credential(Some("  \t  "))
            .build();
        assert!(p.credential.is_none());
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = OpenAiModelProvider::builder("test")
            .credential(None)
            .build();
        let result = p.chat_with_system(None, "hello", "gpt-4o", Some(0.7)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = OpenAiModelProvider::builder("test")
            .credential(None)
            .build();
        let result = p
            .chat_with_system(Some("You are ZeroClaw"), "test", "gpt-4o", Some(0.5))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn request_serializes_with_system_message() {
        let req = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: "You are ZeroClaw".to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                },
            ],
            temperature: Some(0.7),
            max_tokens: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("gpt-4o"));
    }

    #[test]
    fn request_serializes_without_system() {
        let req = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: Some(0.0),
            max_tokens: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("system"));
        assert!(json.contains("\"temperature\":0.0"));
    }

    #[test]
    fn response_deserializes_single_choice() {
        let json = r#"{"choices":[{"message":{"content":"Hi!"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.effective_content(), "Hi!");
    }

    #[test]
    fn response_deserializes_empty_choices() {
        let json = r#"{"choices":[]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.choices.is_empty());
    }

    #[test]
    fn response_deserializes_multiple_choices() {
        let json = r#"{"choices":[{"message":{"content":"A"}},{"message":{"content":"B"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 2);
        assert_eq!(resp.choices[0].message.effective_content(), "A");
    }

    #[test]
    fn response_with_unicode() {
        let json = r#"{"choices":[{"message":{"content":"Hello \u03A9"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0].message.effective_content(),
            "Hello \u{03A9}"
        );
    }

    #[test]
    fn response_with_long_content() {
        let long = "x".repeat(100_000);
        let json = format!(r#"{{"choices":[{{"message":{{"content":"{long}"}}}}]}}"#);
        let resp: ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(
            resp.choices[0].message.content.as_ref().unwrap().len(),
            100_000
        );
    }

    #[tokio::test]
    async fn warmup_without_key_is_noop() {
        let model_provider = OpenAiModelProvider::builder("test")
            .credential(None)
            .build();
        let result = model_provider.warmup().await;
        assert!(result.is_ok());
    }

    // ----------------------------------------------------------
    // Reasoning model fallback tests (reasoning_content)
    // ----------------------------------------------------------

    #[test]
    fn reasoning_content_fallback_empty_content() {
        let json = r#"{"choices":[{"message":{"content":"","reasoning_content":"Thinking..."}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].message.effective_content(), "Thinking...");
    }

    #[test]
    fn reasoning_content_fallback_null_content() {
        let json =
            r#"{"choices":[{"message":{"content":null,"reasoning_content":"Thinking..."}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].message.effective_content(), "Thinking...");
    }

    #[test]
    fn reasoning_content_not_used_when_content_present() {
        let json = r#"{"choices":[{"message":{"content":"Hello","reasoning_content":"Ignored"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].message.effective_content(), "Hello");
    }

    #[test]
    fn native_response_reasoning_content_fallback() {
        let json =
            r#"{"choices":[{"message":{"content":"","reasoning_content":"Native thinking"}}]}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), Some("Native thinking".to_string()));
    }

    #[test]
    fn native_response_reasoning_content_ignored_when_content_present() {
        let json =
            r#"{"choices":[{"message":{"content":"Real answer","reasoning_content":"Ignored"}}]}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.effective_content(), Some("Real answer".to_string()));
    }

    #[tokio::test]
    async fn chat_with_tools_fails_without_key() {
        let p = OpenAiModelProvider::builder("test")
            .credential(None)
            .build();
        let messages = vec![ChatMessage::user("hello".to_string())];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": "Run a shell command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }
            }
        })];
        let result = p
            .chat_with_tools(&messages, &tools, "gpt-4o", Some(0.7))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[tokio::test]
    async fn chat_with_tools_rejects_invalid_tool_shape() {
        let p = OpenAiModelProvider::builder("test")
            .credential(Some("openai-test-credential"))
            .build();
        let messages = vec![ChatMessage::user("hello".to_string())];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "shell",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }
            }
        })];

        let result = p
            .chat_with_tools(&messages, &tools, "gpt-4o", Some(0.7))
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid OpenAI tool specification")
        );
    }

    #[test]
    fn native_tool_spec_deserializes_from_openai_format() {
        let json = serde_json::json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": "Run a shell command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }
            }
        });
        let spec = parse_native_tool_spec(json).unwrap();
        assert_eq!(spec.kind, "function");
        assert_eq!(spec.function.name, "shell");
    }

    #[test]
    fn native_response_parses_usage() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 50}
        }"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.completion_tokens, Some(50));
    }

    #[test]
    fn native_response_parses_without_usage() {
        let json = r#"{"choices": [{"message": {"content": "Hello"}}]}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // reasoning_content pass-through tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_native_response_captures_reasoning_content() {
        let json = r#"{"choices":[{"message":{
            "content":"answer",
            "reasoning_content":"thinking step",
            "tool_calls":[{"id":"call_1","type":"function","function":{"name":"shell","arguments":"{}"}}]
        }}]}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let message = resp.choices.into_iter().next().unwrap().message;
        let parsed = OpenAiModelProvider::parse_native_response(message);
        assert_eq!(parsed.reasoning_content.as_deref(), Some("thinking step"));
        assert_eq!(parsed.tool_calls.len(), 1);
    }

    #[test]
    fn parse_native_response_none_reasoning_content_for_normal_model() {
        let json = r#"{"choices":[{"message":{"content":"hello"}}]}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let message = resp.choices.into_iter().next().unwrap().message;
        let parsed = OpenAiModelProvider::parse_native_response(message);
        assert!(parsed.reasoning_content.is_none());
    }

    #[test]
    fn convert_messages_round_trips_reasoning_content() {
        use zeroclaw_api::model_provider::ChatMessage;

        let history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{}"
            }],
            "reasoning_content": "Let me think..."
        });

        let messages = vec![ChatMessage::assistant(history_json.to_string())];
        let native = OpenAiModelProvider::convert_messages(&messages);
        assert_eq!(native.len(), 1);
        assert_eq!(
            native[0].reasoning_content.as_deref(),
            Some("Let me think...")
        );
    }

    #[test]
    fn convert_messages_no_reasoning_content_when_absent() {
        use zeroclaw_api::model_provider::ChatMessage;

        let history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{}"
            }]
        });

        let messages = vec![ChatMessage::assistant(history_json.to_string())];
        let native = OpenAiModelProvider::convert_messages(&messages);
        assert_eq!(native.len(), 1);
        assert!(native[0].reasoning_content.is_none());
    }

    #[test]
    fn convert_messages_sanitizes_invalid_tool_arguments_to_empty_object() {
        // Pins that the openai `convert_messages` call site of
        // `sanitize_tool_arguments` is wired in. The helper contract itself is
        // covered in `compatible::tests::sanitize_tool_arguments_*`.
        use zeroclaw_api::model_provider::ChatMessage;

        let messages = vec![ChatMessage::assistant(
            r#"{"content":"trying","tool_calls":[{"id":"call_bad","name":"shell","arguments":"{\"command\":\"rm -rf"}]}"#.to_string(),
        )];

        let native = OpenAiModelProvider::convert_messages(&messages);
        let tool_calls = native[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id.as_deref(), Some("call_bad"));
        assert_eq!(tool_calls[0].function.name, "shell");
        assert_eq!(tool_calls[0].function.arguments, "{}");
    }

    #[test]
    fn convert_messages_passes_through_valid_tool_arguments() {
        // Companion regression: valid JSON must round-trip byte-for-byte.
        use zeroclaw_api::model_provider::ChatMessage;

        let messages = vec![ChatMessage::assistant(
            r#"{"content":"using","tool_calls":[{"id":"call_ok","name":"shell","arguments":"{\"command\":\"pwd\"}"}]}"#.to_string(),
        )];

        let native = OpenAiModelProvider::convert_messages(&messages);
        let tool_calls = native[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls[0].function.arguments, r#"{"command":"pwd"}"#);
    }

    #[test]
    fn native_message_omits_reasoning_content_when_none() {
        let msg = NativeMessage {
            role: "assistant".to_string(),
            content: Some("hi".to_string()),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("reasoning_content"));
    }

    #[test]
    fn native_message_includes_reasoning_content_when_some() {
        let msg = NativeMessage {
            role: "assistant".to_string(),
            content: Some("hi".to_string()),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: Some("thinking...".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("reasoning_content"));
        assert!(json.contains("thinking..."));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Temperature adjustment tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn adjust_temperature_for_o1_models() {
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o1", 0.7),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o1-2024-12-17", 0.5),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o1-mini", 0.5),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o1-mini-2024-09-12", 0.7),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_for_o3_models() {
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o3", 0.7),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o3-2025-04-16", 0.5),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o3-mini", 0.3),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o3-mini-2025-01-31", 0.8),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_for_o4_models() {
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o4-mini", 0.7),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o4-mini-2025-04-16", 0.5),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_for_gpt5_models() {
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-5", 0.7),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-5-2025-08-07", 0.5),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-5-mini", 0.3),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-5-mini-2025-08-07", 0.8),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-5-nano", 0.6),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-5-nano-2025-08-07", 0.4),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_for_gpt5_chat_latest_models() {
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-5.1-chat-latest", 0.7),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-5.2-chat-latest", 0.5),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-5.3-chat-latest", 0.3),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_preserves_for_standard_models() {
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-4o", 0.7),
            0.7
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-4-turbo", 0.5),
            0.5
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-3.5-turbo", 0.3),
            0.3
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-4", 1.0),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_handles_edge_cases() {
        // Temperature 0.0 should be preserved for standard models
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-4o", 0.0),
            0.0
        );
        // Temperature 1.0 should be preserved for all models
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("o1", 1.0),
            1.0
        );
        assert_eq!(
            OpenAiModelProvider::adjust_temperature_for_model("gpt-4o", 1.0),
            1.0
        );
    }

    #[test]
    fn responses_request_propagates_max_tokens_when_set() {
        let provider = OpenAiResponsesModelProvider::builder("openai")
            .credential(None)
            .max_tokens(Some(2048))
            .build();
        let req = provider.build_request(
            None,
            vec![serde_json::json!({"role": "user", "content": "hi"})],
            None,
            "gpt-5",
            None,
            false,
        );
        let json = serde_json::to_value(&req).expect("ResponsesApiRequest must serialize");
        assert_eq!(
            json.get("max_output_tokens")
                .and_then(serde_json::Value::as_u64),
            Some(2048),
            "max_tokens configured on the provider must survive into max_output_tokens on the wire body"
        );
    }

    #[test]
    fn responses_request_omits_max_tokens_when_unset() {
        let provider = OpenAiResponsesModelProvider::builder("openai")
            .credential(None)
            .build();
        assert!(
            provider.max_tokens.is_none(),
            "fresh provider must default max_tokens to None"
        );
        let req = provider.build_request(
            None,
            vec![serde_json::json!({"role": "user", "content": "hi"})],
            None,
            "gpt-5",
            None,
            false,
        );
        let json = serde_json::to_value(&req).expect("ResponsesApiRequest must serialize");
        assert!(
            json.get("max_output_tokens").is_none()
                || json
                    .get("max_output_tokens")
                    .and_then(serde_json::Value::as_null)
                    .is_some(),
            "unset max_tokens must not surface as a wire-bound integer (skipped via skip_serializing_if)"
        );
    }

    #[test]
    fn responses_request_propagates_reasoning_effort_when_set() {
        let provider = OpenAiResponsesModelProvider::builder("openai")
            .credential(None)
            .reasoning_effort(Some("high".to_string()))
            .build();
        let req = provider.build_request(
            None,
            vec![serde_json::json!({"role": "user", "content": "hi"})],
            None,
            "o3",
            None,
            false,
        );
        let json = serde_json::to_value(&req).expect("ResponsesApiRequest must serialize");
        let reasoning = json
            .get("reasoning")
            .expect("reasoning_effort must populate the `reasoning` object");
        assert_eq!(
            reasoning.get("effort").and_then(serde_json::Value::as_str),
            Some("high"),
            ".reasoning_effort(Some(\"high\")) must surface as reasoning.effort = \"high\" on the wire body"
        );
    }

    #[test]
    fn responses_request_omits_reasoning_when_unset() {
        let provider = OpenAiResponsesModelProvider::builder("openai")
            .credential(None)
            .build();
        assert!(
            provider.reasoning_effort.is_none(),
            "fresh provider must default reasoning_effort to None"
        );
        let req = provider.build_request(
            None,
            vec![serde_json::json!({"role": "user", "content": "hi"})],
            None,
            "gpt-5",
            None,
            false,
        );
        let json = serde_json::to_value(&req).expect("ResponsesApiRequest must serialize");
        assert!(
            json.get("reasoning").is_none()
                || json
                    .get("reasoning")
                    .and_then(serde_json::Value::as_null)
                    .is_some(),
            "unset reasoning_effort must not surface as a wire-bound object (skipped via skip_serializing_if)"
        );
    }

    #[test]
    fn responses_request_propagates_instructions_and_temperature_and_model() {
        let provider = OpenAiResponsesModelProvider::builder("openai")
            .api_url("https://api.example.test/v1")
            .credential(None)
            .build();
        let req = provider.build_request(
            Some("You are a careful assistant.".to_string()),
            vec![serde_json::json!({"role": "user", "content": "summarize"})],
            None,
            "gpt-5-mini",
            Some(0.3),
            true,
        );
        let json = serde_json::to_value(&req).expect("ResponsesApiRequest must serialize");
        assert_eq!(
            json.get("model").and_then(serde_json::Value::as_str),
            Some("gpt-5-mini"),
            "model argument must reach the wire body verbatim"
        );
        assert_eq!(
            json.get("instructions").and_then(serde_json::Value::as_str),
            Some("You are a careful assistant."),
            "non-None instructions argument must reach the wire body"
        );
        assert_eq!(
            json.get("temperature").and_then(serde_json::Value::as_f64),
            Some(0.3),
            "temperature argument must reach the wire body as f64"
        );
        assert_eq!(
            json.get("stream").and_then(serde_json::Value::as_bool),
            Some(true),
            "stream argument must reach the wire body as bool"
        );
    }

    #[test]
    fn responses_request_propagates_tool_choice_and_parallel_when_tools_present() {
        let provider = OpenAiResponsesModelProvider::builder("openai")
            .credential(None)
            .max_tokens(Some(1024))
            .build();
        let tools = Some(vec![ResponsesToolSpec {
            kind: "function".to_string(),
            name: "lookup_weather".to_string(),
            description: "Look up the weather for a city.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
            })
            .into(),
            strict: true,
        }]);
        let req = provider.build_request(
            None,
            vec![serde_json::json!({"role": "user", "content": "weather?"})],
            tools,
            "gpt-5",
            None,
            false,
        );
        let json = serde_json::to_value(&req).expect("ResponsesApiRequest must serialize");
        assert_eq!(
            json.get("tool_choice").and_then(serde_json::Value::as_str),
            Some("auto"),
            "with tools present, wire body must carry tool_choice = \"auto\""
        );
        assert_eq!(
            json.get("parallel_tool_calls")
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "with tools present, wire body must carry parallel_tool_calls = true"
        );
        let wire_tools = json
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .expect("tools must be a JSON array on the wire body");
        assert_eq!(
            wire_tools.len(),
            1,
            "exactly one tool spec must reach the wire body"
        );
    }

    #[test]
    fn responses_request_omits_tool_choice_and_parallel_when_tools_absent() {
        let provider = OpenAiResponsesModelProvider::builder("openai")
            .credential(None)
            .build();
        let req = provider.build_request(
            None,
            vec![serde_json::json!({"role": "user", "content": "hi"})],
            None,
            "gpt-5",
            None,
            false,
        );
        let json = serde_json::to_value(&req).expect("ResponsesApiRequest must serialize");
        assert!(
            json.get("tool_choice").is_none()
                || json
                    .get("tool_choice")
                    .and_then(serde_json::Value::as_null)
                    .is_some(),
            "no tools → tool_choice must be omitted (skipped via skip_serializing_if)"
        );
        assert!(
            json.get("parallel_tool_calls").is_none()
                || json
                    .get("parallel_tool_calls")
                    .and_then(serde_json::Value::as_null)
                    .is_some(),
            "no tools → parallel_tool_calls must be omitted (skipped via skip_serializing_if)"
        );
    }

    #[test]
    fn responses_request_omits_tool_choice_and_parallel_when_tools_empty() {
        let provider = OpenAiResponsesModelProvider::builder("openai")
            .credential(None)
            .build();
        let req = provider.build_request(
            None,
            vec![serde_json::json!({"role": "user", "content": "hi"})],
            Some(Vec::new()),
            "gpt-5",
            None,
            false,
        );
        let json = serde_json::to_value(&req).expect("ResponsesApiRequest must serialize");
        assert!(
            json.get("tool_choice").is_none()
                || json
                    .get("tool_choice")
                    .and_then(serde_json::Value::as_null)
                    .is_some(),
            "empty tools list → tool_choice must be omitted (vLLM rejects tool_choice without non-empty tools)"
        );
        assert!(
            json.get("parallel_tool_calls").is_none()
                || json
                    .get("parallel_tool_calls")
                    .and_then(serde_json::Value::as_null)
                    .is_some(),
            "empty tools list → parallel_tool_calls must be omitted with tool_choice"
        );
        let wire_tools = json
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .expect("tools must be a JSON array on the wire body");
        assert!(
            wire_tools.is_empty(),
            "empty tools should remain an empty tools array; only tool_choice and parallel_tool_calls are suppressed"
        );
    }
}
