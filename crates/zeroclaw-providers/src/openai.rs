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
use serde::{Deserialize, Serialize};
use zeroclaw_api::tool::ToolSpec;

/// OpenAI's public API endpoint.
pub(crate) const BASE_URL: &str = "https://api.openai.com/v1";

/// Default endpoint for the OpenAI Responses API.
const RESPONSES_URL: &str = "https://api.openai.com/v1/responses";

pub struct OpenAiModelProvider {
    /// `[model_providers.openai.<alias>]` config-key alias.
    alias: String,
    base_url: String,
    credential: Option<String>,
    max_tokens: Option<u32>,
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
    parameters: serde_json::Value,
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

impl OpenAiModelProvider {
    pub fn new(alias: &str, credential: Option<&str>) -> Self {
        Self::with_base_url(alias, None, credential)
    }

    /// Create a model_provider with an optional custom base URL.
    /// Falls back to `https://api.openai.com/v1` when `base_url` is `None`.
    pub fn with_base_url(alias: &str, base_url: Option<&str>, credential: Option<&str>) -> Self {
        Self {
            alias: alias.to_string(),
            base_url: base_url
                .map(|u| u.trim_end_matches('/').to_string())
                .unwrap_or_else(|| BASE_URL.to_string()),
            credential: credential.map(ToString::to_string),
            max_tokens: None,
        }
    }

    /// Set the maximum output tokens for API requests.
    pub fn with_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
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
                        parameters: tool.parameters.clone(),
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
                        .map(|tc| NativeToolCall {
                            id: Some(tc.id),
                            kind: Some("function".to_string()),
                            function: NativeFunctionCall {
                                name: tc.name,
                                arguments: tc.arguments,
                            },
                        })
                        .collect::<Vec<_>>();
                    let content = value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string);
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
            120,
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
        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(request.messages),
            temperature: adjusted_temperature,
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
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
            tool_choice: native_tools.as_ref().map(|_| "auto".to_string()),
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

// ── OpenAI Responses API provider (wire_api = "responses") ────────────────
//
// Uses the OpenAI Responses API (`/v1/responses`) with a standard API key.
// Supports full streaming tool calls, unlike the chat-completions `OpenAiModelProvider`.
// Constructed by the factory when `wire_api = "responses"` without Codex OAuth.

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
///
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

    let _ = tx.send(Ok(StreamEvent::Final)).await;
}

pub struct OpenAiResponsesModelProvider {
    alias: String,
    responses_url: String,
    credential: Option<String>,
    max_tokens: Option<u32>,
    reasoning_effort: Option<String>,
}

impl OpenAiResponsesModelProvider {
    pub fn new(alias: &str, api_url: Option<&str>, credential: Option<&str>) -> Self {
        let responses_url = api_url
            .map(|url| {
                let trimmed = url.trim_end_matches('/');
                if trimmed.ends_with("/responses") {
                    trimmed.to_string()
                } else {
                    format!("{trimmed}/responses")
                }
            })
            .unwrap_or_else(|| RESPONSES_URL.to_string());
        Self {
            alias: alias.to_string(),
            responses_url,
            credential: credential.map(ToString::to_string),
            max_tokens: None,
            reasoning_effort: None,
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
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
        let has_tools = tools.is_some();
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

    fn streaming_client(&self) -> Client {
        Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new())
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

    fn default_base_url(&self) -> Option<&str> {
        Some(RESPONSES_URL)
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
        let response = Client::new()
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
        let req = self.build_request(instructions, input, tools, model, temperature, false);
        let response = Client::new()
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

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);
        let handle = ::zeroclaw_spawn::spawn!(async move {
            let (instructions, input) = build_responses_input(&messages_owned);
            let instructions = if instructions.is_empty() {
                None
            } else {
                Some(instructions)
            };
            let tools = convert_tools(tools_owned.as_deref());
            let has_tools = tools.is_some();
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
        let p = OpenAiModelProvider::new("test", Some("openai-test-credential"));
        assert_eq!(p.credential.as_deref(), Some("openai-test-credential"));
    }

    #[test]
    fn creates_without_key() {
        let p = OpenAiModelProvider::new("test", None);
        assert!(p.credential.is_none());
    }

    #[test]
    fn creates_with_empty_key() {
        let p = OpenAiModelProvider::new("test", Some(""));
        assert_eq!(p.credential.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = OpenAiModelProvider::new("test", None);
        let result = p.chat_with_system(None, "hello", "gpt-4o", Some(0.7)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = OpenAiModelProvider::new("test", None);
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
        let model_provider = OpenAiModelProvider::new("test", None);
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
        let p = OpenAiModelProvider::new("test", None);
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
        let p = OpenAiModelProvider::new("test", Some("openai-test-credential"));
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
}
