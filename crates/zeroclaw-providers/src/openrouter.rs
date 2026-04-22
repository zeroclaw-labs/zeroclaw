use crate::compatible::sse_bytes_to_events;
use crate::multimodal;
use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, ProviderCapabilities, StreamError, StreamEvent, StreamOptions, StreamResult,
    TokenUsage, ToolCall as ProviderToolCall,
};
use async_trait::async_trait;
use futures_util::StreamExt as _;
use futures_util::stream;
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use zeroclaw_api::tool::ToolSpec;

pub struct OpenRouterProvider {
    credential: Option<String>,
    timeout_secs: u64,
    connect_timeout_secs: u64,
    max_tokens: Option<u32>,
    extra_body: Option<serde_json::Value>,
}

const DEFAULT_OPENROUTER_TIMEOUT_SECS: u64 = 120;
const OPENROUTER_CONNECT_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
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

/// RAII guard that aborts a spawned tokio task when dropped.
///
/// Used by `stream_chat` to bind the SSE-forwarding task's lifetime to the
/// returned stream. When a caller cancels the stream (timeout, user abort,
/// client disconnect), the guard is dropped together with the stream state
/// and the in-flight HTTP request is cancelled so it stops consuming
/// bandwidth and connection-pool slots. `AbortHandle::abort` is a no-op
/// after the task has finished naturally, so the happy path is unaffected.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
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
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
}

#[derive(Debug, Serialize)]
struct NativeChatRequest {
    model: String,
    messages: Vec<NativeMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize)]
struct NativeMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<NativeToolCall>>,
    /// Raw reasoning content from thinking models; pass-through for providers
    /// that require it in assistant tool-call history messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

#[derive(Debug, Serialize)]
struct NativeToolSpec {
    #[serde(rename = "type")]
    kind: String,
    function: NativeToolFunctionSpec,
}

#[derive(Debug, Serialize)]
struct NativeToolFunctionSpec {
    name: String,
    description: String,
    parameters: serde_json::Value,
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

impl OpenRouterProvider {
    pub fn new(credential: Option<&str>, timeout_secs: Option<u64>) -> Self {
        Self {
            credential: credential.map(ToString::to_string),
            timeout_secs: timeout_secs
                .filter(|secs| *secs > 0)
                .unwrap_or(DEFAULT_OPENROUTER_TIMEOUT_SECS),
            connect_timeout_secs: OPENROUTER_CONNECT_TIMEOUT_SECS,
            max_tokens: None,
            extra_body: None,
        }
    }

    /// Override the HTTP request timeout for LLM API calls.
    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Override the HTTP connect timeout for LLM API calls.
    pub fn with_connect_timeout_secs(mut self, secs: u64) -> Self {
        self.connect_timeout_secs = secs;
        self
    }

    /// Set the maximum output tokens for API requests.
    pub fn with_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set extra JSON parameters to merge into every API request body.
    /// Keys in `extra` are inserted at the top level of the serialized request,
    /// overriding any existing keys with the same name.
    pub fn with_extra_body(mut self, extra: serde_json::Value) -> Self {
        self.extra_body = Some(extra);
        self
    }

    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<NativeToolSpec>> {
        let items = tools?;
        if items.is_empty() {
            return None;
        }
        let valid: Vec<NativeToolSpec> = items
            .iter()
            .filter(|tool| is_valid_openai_tool_name(&tool.name))
            .map(|tool| NativeToolSpec {
                kind: "function".to_string(),
                function: NativeToolFunctionSpec {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                },
            })
            .collect();
        if valid.is_empty() { None } else { Some(valid) }
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
                        .map(|value| MessageContent::Text(value.to_string()))
                        .or_else(|| Some(MessageContent::Text(m.content.clone())));
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
                    content: Some(Self::to_message_content(&m.role, &m.content)),
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning_content: None,
                }
            })
            .collect()
    }

    fn to_message_content(role: &str, content: &str) -> MessageContent {
        if role != "user" {
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

    fn parse_native_response(message: NativeResponseMessage) -> ProviderChatResponse {
        let reasoning_content = message.reasoning_content.clone();
        let tool_calls = message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ProviderToolCall {
                id: tc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect::<Vec<_>>();

        ProviderChatResponse {
            text: message.content,
            tool_calls,
            usage: None,
            reasoning_content,
        }
    }

    fn compact_sanitized_body_snippet(body: &str) -> String {
        super::sanitize_api_error(body)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    async fn read_response_body(
        provider_name: &str,
        response: reqwest::Response,
    ) -> anyhow::Result<String> {
        response.text().await.map_err(|error| {
            let sanitized = super::sanitize_api_error(&error.to_string());
            anyhow::anyhow!(
                "{provider_name} transport error while reading response body: {sanitized}"
            )
        })
    }

    fn parse_response_body<T: DeserializeOwned>(
        provider_name: &str,
        body: &str,
        kind: &str,
    ) -> anyhow::Result<T> {
        serde_json::from_str::<T>(body).map_err(|error| {
            let snippet = Self::compact_sanitized_body_snippet(body);
            anyhow::anyhow!(
                "{provider_name} API returned an unexpected {kind} payload: {error}; body={snippet}"
            )
        })
    }

    /// Serialize `request` to JSON, merge `self.extra_body` keys at the top
    /// level (extra_body wins on conflicts), and return the merged Value.
    fn merge_extra_body<T: Serialize>(&self, request: &T) -> anyhow::Result<serde_json::Value> {
        let Some(extra) = &self.extra_body else {
            return Ok(serde_json::to_value(request)?);
        };
        let overrides = extra
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("provider_extra must be a JSON object, got: {extra}"))?;
        let mut value = serde_json::to_value(request)?;
        if let Some(base) = value.as_object_mut() {
            for (k, v) in overrides {
                base.insert(k.clone(), v.clone());
            }
        }
        Ok(value)
    }

    fn http_client(&self) -> Client {
        zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            "provider.openrouter",
            self.timeout_secs,
            self.connect_timeout_secs,
        )
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: true,
            prompt_caching: false,
        }
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        // Hit a lightweight endpoint to establish TLS + HTTP/2 connection pool.
        // This prevents the first real chat request from timing out on cold start.
        if let Some(credential) = self.credential.as_ref() {
            self.http_client()
                .get("https://openrouter.ai/api/v1/auth/key")
                .header("Authorization", format!("Bearer {credential}"))
                .send()
                .await?
                .error_for_status()?;
        }
        Ok(())
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let credential = self.credential.as_ref()
            .ok_or_else(|| anyhow::anyhow!("OpenRouter API key not set. Run `zeroclaw onboard` or set OPENROUTER_API_KEY env var."))?;

        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: MessageContent::Text(sys.to_string()),
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: Self::to_message_content("user", message),
        });

        let request = ChatRequest {
            model: model.to_string(),
            messages,
            temperature,
            max_tokens: self.max_tokens,
        };

        let body = self.merge_extra_body(&request)?;
        let response = self
            .http_client()
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {credential}"))
            .header("HTTP-Referer", "https://github.com/zeroclaw-labs/zeroclaw")
            .header("X-Title", "ZeroClaw")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenRouter", response).await);
        }

        let resp_body = Self::read_response_body("OpenRouter", response).await?;
        let chat_response = Self::parse_response_body::<ApiChatResponse>(
            "OpenRouter",
            &resp_body,
            "chat-completions",
        )?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("No response from OpenRouter"))
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let credential = self.credential.as_ref()
            .ok_or_else(|| anyhow::anyhow!("OpenRouter API key not set. Run `zeroclaw onboard` or set OPENROUTER_API_KEY env var."))?;

        let api_messages: Vec<Message> = messages
            .iter()
            .map(|m| Message {
                role: m.role.clone(),
                content: Self::to_message_content(&m.role, &m.content),
            })
            .collect();

        let request = ChatRequest {
            model: model.to_string(),
            messages: api_messages,
            temperature,
            max_tokens: self.max_tokens,
        };

        let body = self.merge_extra_body(&request)?;
        let response = self
            .http_client()
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {credential}"))
            .header("HTTP-Referer", "https://github.com/zeroclaw-labs/zeroclaw")
            .header("X-Title", "ZeroClaw")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenRouter", response).await);
        }

        let resp_body = Self::read_response_body("OpenRouter", response).await?;
        let chat_response = Self::parse_response_body::<ApiChatResponse>(
            "OpenRouter",
            &resp_body,
            "chat-completions",
        )?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("No response from OpenRouter"))
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
            "OpenRouter API key not set. Run `zeroclaw onboard` or set OPENROUTER_API_KEY env var."
        )
        })?;

        let tools = Self::convert_tools(request.tools);
        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(request.messages),
            temperature,
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
            max_tokens: self.max_tokens,
            stream: None,
        };

        let body = self.merge_extra_body(&native_request)?;
        let response = self
            .http_client()
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {credential}"))
            .header("HTTP-Referer", "https://github.com/zeroclaw-labs/zeroclaw")
            .header("X-Title", "ZeroClaw")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenRouter", response).await);
        }

        let resp_body = Self::read_response_body("OpenRouter", response).await?;
        let native_response = Self::parse_response_body::<NativeChatResponse>(
            "OpenRouter",
            &resp_body,
            "native chat",
        )?;
        let usage = native_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_input_tokens: None,
        });
        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| anyhow::anyhow!("No response from OpenRouter"))?;
        let mut result = Self::parse_native_response(message);
        result.usage = usage;
        Ok(result)
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

    fn stream_chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        if !options.enabled {
            return stream::once(async { Ok(StreamEvent::Final) }).boxed();
        }

        let credential = match self.credential.as_ref() {
            Some(c) => c.clone(),
            None => {
                return stream::once(async {
                    Err(StreamError::Provider(
                        "OpenRouter API key not set. Run `zeroclaw onboard` or set OPENROUTER_API_KEY env var.".to_string(),
                    ))
                })
                .boxed();
            }
        };

        let tools = Self::convert_tools(request.tools);
        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(request.messages),
            temperature,
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
            max_tokens: self.max_tokens,
            stream: Some(true),
        };

        let payload = match serde_json::to_value(&native_request) {
            Ok(v) => v,
            Err(e) => {
                return stream::once(async move { Err(StreamError::Json(e)) }).boxed();
            }
        };

        let client = self.http_client();
        let count_tokens = options.count_tokens;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);

        let handle = tokio::spawn(async move {
            let response = match client
                .post("https://openrouter.ai/api/v1/chat/completions")
                .header("Authorization", format!("Bearer {credential}"))
                .header("HTTP-Referer", "https://github.com/zeroclaw-labs/zeroclaw")
                .header("X-Title", "ZeroClaw")
                .header("Accept", "text/event-stream")
                .json(&payload)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(StreamError::Http(e.to_string()))).await;
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let error = response
                    .text()
                    .await
                    .unwrap_or_else(|_| format!("HTTP error: {status}"));
                let _ = tx
                    .send(Err(StreamError::Provider(format!("{status}: {error}"))))
                    .await;
                return;
            }

            let mut event_stream = sse_bytes_to_events(response, count_tokens);
            while let Some(event) = event_stream.next().await {
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });

        // Bind the task's lifetime to the returned stream so dropping the
        // stream cancels the in-flight HTTP request. Without this guard the
        // spawned task keeps reading the response body to completion after
        // the consumer is gone, holding a connection-pool slot and
        // consuming OpenRouter quota for a request the caller no longer
        // wants. `AbortHandle::abort` is a no-op if the task has already
        // finished, so the happy path is unaffected. See #5822.
        let guard = AbortOnDrop(handle.abort_handle());

        stream::unfold((rx, guard), |(mut rx, guard)| async move {
            rx.recv().await.map(|event| (event, (rx, guard)))
        })
        .boxed()
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
                "OpenRouter API key not set. Run `zeroclaw onboard` or set OPENROUTER_API_KEY env var."
            )
        })?;

        // Convert tool JSON values to NativeToolSpec
        let native_tools: Option<Vec<NativeToolSpec>> = if tools.is_empty() {
            None
        } else {
            let specs: Vec<NativeToolSpec> = tools
                .iter()
                .filter_map(|t| {
                    let func = t.get("function")?;
                    Some(NativeToolSpec {
                        kind: "function".to_string(),
                        function: NativeToolFunctionSpec {
                            name: func.get("name")?.as_str()?.to_string(),
                            description: func
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_string(),
                            parameters: func
                                .get("parameters")
                                .cloned()
                                .unwrap_or(serde_json::json!({})),
                        },
                    })
                })
                .collect();
            if specs.is_empty() { None } else { Some(specs) }
        };

        // Convert ChatMessage to NativeMessage, preserving structured assistant/tool entries
        // when history contains native tool-call metadata.
        let native_messages = Self::convert_messages(messages);

        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: native_messages,
            temperature,
            tool_choice: native_tools.as_ref().map(|_| "auto".to_string()),
            tools: native_tools,
            max_tokens: self.max_tokens,
            stream: None,
        };

        let body = self.merge_extra_body(&native_request)?;
        let response = self
            .http_client()
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {credential}"))
            .header("HTTP-Referer", "https://github.com/zeroclaw-labs/zeroclaw")
            .header("X-Title", "ZeroClaw")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenRouter", response).await);
        }

        let resp_body = Self::read_response_body("OpenRouter", response).await?;
        let native_response = Self::parse_response_body::<NativeChatResponse>(
            "OpenRouter",
            &resp_body,
            "native chat",
        )?;
        let usage = native_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_input_tokens: None,
        });
        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| anyhow::anyhow!("No response from OpenRouter"))?;
        let mut result = Self::parse_native_response(message);
        result.usage = usage;
        Ok(result)
    }
}

/// Check if a tool name is valid for OpenAI-compatible APIs.
/// Must match `^[a-zA-Z0-9_-]{1,64}$`.
fn is_valid_openai_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{ChatMessage, Provider};

    #[test]
    fn capabilities_report_vision_support() {
        let provider = OpenRouterProvider::new(Some("openrouter-test-credential"), None);
        let caps = <OpenRouterProvider as Provider>::capabilities(&provider);
        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn supports_streaming_returns_true() {
        let provider = OpenRouterProvider::new(Some("openrouter-test-credential"), None);
        assert!(provider.supports_streaming());
    }

    #[test]
    fn supports_streaming_tool_events_returns_true() {
        let provider = OpenRouterProvider::new(Some("openrouter-test-credential"), None);
        assert!(provider.supports_streaming_tool_events());
    }

    #[tokio::test]
    async fn stream_chat_without_key_returns_error_event() {
        use crate::traits::{ChatMessage, ChatRequest};
        use futures_util::StreamExt as _;

        let provider = OpenRouterProvider::new(None, None);
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "hello".into(),
        }];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };

        let mut stream = provider.stream_chat(
            request,
            "anthropic/claude-haiku-4-5",
            0.0,
            crate::traits::StreamOptions {
                enabled: true,
                count_tokens: false,
            },
        );

        let first = stream
            .next()
            .await
            .expect("stream should yield at least one event");
        assert!(first.is_err(), "expected error without API key");
        let err = first.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("API key not set"),
            "error should mention API key: {msg}"
        );
    }

    #[tokio::test]
    async fn stream_chat_disabled_options_returns_final() {
        use crate::traits::{ChatMessage, ChatRequest, StreamEvent};
        use futures_util::StreamExt as _;

        let provider = OpenRouterProvider::new(Some("key"), None);
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "hello".into(),
        }];
        let request = ChatRequest {
            messages: &messages,
            tools: None,
        };

        let mut stream = provider.stream_chat(
            request,
            "anthropic/claude-haiku-4-5",
            0.0,
            crate::traits::StreamOptions {
                enabled: false,
                count_tokens: false,
            },
        );

        let first = stream
            .next()
            .await
            .expect("stream should yield Final immediately");
        assert!(matches!(first, Ok(StreamEvent::Final)));
    }

    #[test]
    fn native_chat_request_serializes_stream_true() {
        let req = NativeChatRequest {
            model: "anthropic/claude-haiku-4-5".into(),
            messages: vec![],
            temperature: 0.0,
            tools: None,
            tool_choice: None,
            max_tokens: None,
            stream: Some(true),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"stream\":true"));
    }

    #[test]
    fn native_chat_request_omits_stream_when_none() {
        let req = NativeChatRequest {
            model: "anthropic/claude-haiku-4-5".into(),
            messages: vec![],
            temperature: 0.0,
            tools: None,
            tool_choice: None,
            max_tokens: None,
            stream: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("stream"));
    }

    #[test]
    fn creates_with_key() {
        let provider = OpenRouterProvider::new(Some("openrouter-test-credential"), None);
        assert_eq!(
            provider.credential.as_deref(),
            Some("openrouter-test-credential")
        );
    }

    #[test]
    fn creates_without_key() {
        let provider = OpenRouterProvider::new(None, None);
        assert!(provider.credential.is_none());
    }

    #[test]
    fn uses_configured_timeout_when_provided() {
        let provider = OpenRouterProvider::new(Some("openrouter-test-credential"), Some(1200));
        assert_eq!(provider.timeout_secs, 1200);
    }

    #[test]
    fn falls_back_to_default_timeout_for_zero() {
        let provider = OpenRouterProvider::new(Some("openrouter-test-credential"), Some(0));
        assert_eq!(provider.timeout_secs, DEFAULT_OPENROUTER_TIMEOUT_SECS);
    }

    #[tokio::test]
    async fn warmup_without_key_is_noop() {
        let provider = OpenRouterProvider::new(None, None);
        let result = provider.warmup().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let provider = OpenRouterProvider::new(None, None);
        let result = provider
            .chat_with_system(Some("system"), "hello", "openai/gpt-4o", 0.2)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[tokio::test]
    async fn chat_with_history_fails_without_key() {
        let provider = OpenRouterProvider::new(None, None);
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: "be concise".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "hello".into(),
            },
        ];

        let result = provider
            .chat_with_history(&messages, "anthropic/claude-sonnet-4", 0.7)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[test]
    fn chat_request_serializes_with_system_and_user() {
        let request = ChatRequest {
            model: "anthropic/claude-sonnet-4".into(),
            messages: vec![
                Message {
                    role: "system".into(),
                    content: MessageContent::Text("You are helpful".into()),
                },
                Message {
                    role: "user".into(),
                    content: MessageContent::Text("Summarize this".into()),
                },
            ],
            temperature: 0.5,
            max_tokens: None,
        };

        let json = serde_json::to_string(&request).unwrap();

        assert!(json.contains("anthropic/claude-sonnet-4"));
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"temperature\":0.5"));
    }

    #[test]
    fn chat_request_serializes_history_messages() {
        let messages = [
            ChatMessage {
                role: "assistant".into(),
                content: "Previous answer".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Follow-up".into(),
            },
        ];

        let request = ChatRequest {
            model: "google/gemini-2.5-pro".into(),
            messages: messages
                .iter()
                .map(|msg| Message {
                    role: msg.role.clone(),
                    content: MessageContent::Text(msg.content.clone()),
                })
                .collect(),
            temperature: 0.0,
            max_tokens: None,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"role\":\"assistant\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("google/gemini-2.5-pro"));
    }

    #[test]
    fn response_deserializes_single_choice() {
        let json = r#"{"choices":[{"message":{"content":"Hi from OpenRouter"}}]}"#;

        let response: ApiChatResponse = serde_json::from_str(json).unwrap();

        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].message.content, "Hi from OpenRouter");
    }

    #[test]
    fn response_deserializes_empty_choices() {
        let json = r#"{"choices":[]}"#;

        let response: ApiChatResponse = serde_json::from_str(json).unwrap();

        assert!(response.choices.is_empty());
    }

    #[test]
    fn parse_chat_response_body_reports_sanitized_snippet() {
        let body = r#"{"choices":"invalid","api_key":"sk-test-secret-value"}"#;
        let err = OpenRouterProvider::parse_response_body::<ApiChatResponse>(
            "OpenRouter",
            body,
            "chat-completions",
        )
        .expect_err("payload should fail");
        let msg = err.to_string();

        assert!(msg.contains("OpenRouter API returned an unexpected chat-completions payload"));
        assert!(msg.contains("body="));
        assert!(msg.contains("[REDACTED]"));
        assert!(!msg.contains("sk-test-secret-value"));
    }

    #[test]
    fn parse_native_response_body_reports_sanitized_snippet() {
        let body = r#"{"choices":123,"api_key":"sk-another-secret"}"#;
        let err = OpenRouterProvider::parse_response_body::<NativeChatResponse>(
            "OpenRouter",
            body,
            "native chat",
        )
        .expect_err("payload should fail");
        let msg = err.to_string();

        assert!(msg.contains("OpenRouter API returned an unexpected native chat payload"));
        assert!(msg.contains("body="));
        assert!(msg.contains("[REDACTED]"));
        assert!(!msg.contains("sk-another-secret"));
    }

    #[tokio::test]
    async fn chat_with_tools_fails_without_key() {
        let provider = OpenRouterProvider::new(None, None);
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "What is the date?".into(),
        }];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": "Run a shell command",
                "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}
            }
        })];

        let result = provider
            .chat_with_tools(&messages, &tools, "deepseek/deepseek-chat", 0.5)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[test]
    fn native_response_deserializes_with_tool_calls() {
        let json = r#"{
            "choices":[{
                "message":{
                    "content":null,
                    "tool_calls":[
                        {"id":"call_123","type":"function","function":{"name":"get_price","arguments":"{\"symbol\":\"BTC\"}"}}
                    ]
                }
            }]
        }"#;

        let response: NativeChatResponse = serde_json::from_str(json).unwrap();

        assert_eq!(response.choices.len(), 1);
        let message = &response.choices[0].message;
        assert!(message.content.is_none());
        let tool_calls = message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id.as_deref(), Some("call_123"));
        assert_eq!(tool_calls[0].function.name, "get_price");
        assert_eq!(tool_calls[0].function.arguments, "{\"symbol\":\"BTC\"}");
    }

    #[test]
    fn native_response_deserializes_with_text_and_tool_calls() {
        let json = r#"{
            "choices":[{
                "message":{
                    "content":"I'll get that for you.",
                    "tool_calls":[
                        {"id":"call_456","type":"function","function":{"name":"shell","arguments":"{\"command\":\"date\"}"}}
                    ]
                }
            }]
        }"#;

        let response: NativeChatResponse = serde_json::from_str(json).unwrap();

        assert_eq!(response.choices.len(), 1);
        let message = &response.choices[0].message;
        assert_eq!(message.content.as_deref(), Some("I'll get that for you."));
        let tool_calls = message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "shell");
    }

    #[test]
    fn parse_native_response_converts_to_chat_response() {
        let message = NativeResponseMessage {
            content: Some("Here you go.".into()),
            reasoning_content: None,
            tool_calls: Some(vec![NativeToolCall {
                id: Some("call_789".into()),
                kind: Some("function".into()),
                function: NativeFunctionCall {
                    name: "file_read".into(),
                    arguments: r#"{"path":"test.txt"}"#.into(),
                },
            }]),
        };

        let response = OpenRouterProvider::parse_native_response(message);

        assert_eq!(response.text.as_deref(), Some("Here you go."));
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "call_789");
        assert_eq!(response.tool_calls[0].name, "file_read");
    }

    #[test]
    fn convert_messages_parses_assistant_tool_call_payload() {
        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: r#"{"content":"Using tool","tool_calls":[{"id":"call_abc","name":"shell","arguments":"{\"command\":\"pwd\"}"}]}"#
                .into(),
        }];

        let converted = OpenRouterProvider::convert_messages(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "assistant");
        assert_eq!(
            converted[0]
                .content
                .as_ref()
                .and_then(|content| match content {
                    MessageContent::Text(value) => Some(value.as_str()),
                    MessageContent::Parts(_) => None,
                }),
            Some("Using tool")
        );

        let tool_calls = converted[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id.as_deref(), Some("call_abc"));
        assert_eq!(tool_calls[0].function.name, "shell");
        assert_eq!(tool_calls[0].function.arguments, r#"{"command":"pwd"}"#);
    }

    #[test]
    fn convert_messages_parses_tool_result_payload() {
        let messages = vec![ChatMessage {
            role: "tool".into(),
            content: r#"{"tool_call_id":"call_xyz","content":"done"}"#.into(),
        }];

        let converted = OpenRouterProvider::convert_messages(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "tool");
        assert_eq!(converted[0].tool_call_id.as_deref(), Some("call_xyz"));
        assert_eq!(
            converted[0]
                .content
                .as_ref()
                .and_then(|content| match content {
                    MessageContent::Text(value) => Some(value.as_str()),
                    MessageContent::Parts(_) => None,
                }),
            Some("done")
        );
        assert!(converted[0].tool_calls.is_none());
    }

    #[test]
    fn to_message_content_converts_image_markers_to_openai_parts() {
        let content = "Describe this\n\n[IMAGE:data:image/png;base64,abcd]";
        let value =
            serde_json::to_value(OpenRouterProvider::to_message_content("user", content)).unwrap();
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
    fn native_response_parses_usage() {
        let json = r#"{
            "choices": [{"message": {"content": "Hello"}}],
            "usage": {"prompt_tokens": 42, "completion_tokens": 15}
        }"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(42));
        assert_eq!(usage.completion_tokens, Some(15));
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
        let message = NativeResponseMessage {
            content: Some("answer".into()),
            reasoning_content: Some("thinking step".into()),
            tool_calls: Some(vec![NativeToolCall {
                id: Some("call_1".into()),
                kind: Some("function".into()),
                function: NativeFunctionCall {
                    name: "shell".into(),
                    arguments: "{}".into(),
                },
            }]),
        };
        let parsed = OpenRouterProvider::parse_native_response(message);
        assert_eq!(parsed.reasoning_content.as_deref(), Some("thinking step"));
        assert_eq!(parsed.tool_calls.len(), 1);
    }

    #[test]
    fn parse_native_response_none_reasoning_content_for_normal_model() {
        let message = NativeResponseMessage {
            content: Some("hello".into()),
            reasoning_content: None,
            tool_calls: None,
        };
        let parsed = OpenRouterProvider::parse_native_response(message);
        assert!(parsed.reasoning_content.is_none());
    }

    #[test]
    fn native_response_deserializes_reasoning_content() {
        let json = r#"{
            "choices":[{
                "message":{
                    "content":"answer",
                    "reasoning_content":"deep thought",
                    "tool_calls":[
                        {"id":"call_r1","type":"function","function":{"name":"shell","arguments":"{}"}}
                    ]
                }
            }]
        }"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let message = &resp.choices[0].message;
        assert_eq!(message.reasoning_content.as_deref(), Some("deep thought"));
    }

    #[test]
    fn convert_messages_round_trips_reasoning_content() {
        let history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{}"
            }],
            "reasoning_content": "Let me think..."
        });

        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: history_json.to_string(),
        }];
        let native = OpenRouterProvider::convert_messages(&messages);
        assert_eq!(native.len(), 1);
        assert_eq!(
            native[0].reasoning_content.as_deref(),
            Some("Let me think...")
        );
    }

    #[test]
    fn convert_messages_no_reasoning_content_when_absent() {
        let history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{}"
            }]
        });

        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: history_json.to_string(),
        }];
        let native = OpenRouterProvider::convert_messages(&messages);
        assert_eq!(native.len(), 1);
        assert!(native[0].reasoning_content.is_none());
    }

    #[test]
    fn native_message_omits_reasoning_content_when_none() {
        let msg = NativeMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("hi".into())),
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
            content: Some(MessageContent::Text("hi".into())),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: Some("thinking...".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("reasoning_content"));
        assert!(json.contains("thinking..."));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // timeout_secs configuration tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn default_timeout_is_120() {
        let provider = OpenRouterProvider::new(Some("key"), None);
        assert_eq!(provider.timeout_secs, 120);
    }

    #[test]
    fn with_timeout_secs_overrides_default() {
        let provider = OpenRouterProvider::new(Some("key"), None).with_timeout_secs(300);
        assert_eq!(provider.timeout_secs, 300);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // tool name validation tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn valid_openai_tool_names() {
        assert!(is_valid_openai_tool_name("shell"));
        assert!(is_valid_openai_tool_name("file_read"));
        assert!(is_valid_openai_tool_name("web-search"));
        assert!(is_valid_openai_tool_name("Tool123"));
        assert!(is_valid_openai_tool_name("a"));
    }

    #[test]
    fn invalid_openai_tool_names() {
        assert!(!is_valid_openai_tool_name(""));
        assert!(!is_valid_openai_tool_name("mcp:server.tool"));
        assert!(!is_valid_openai_tool_name("node.js"));
        assert!(!is_valid_openai_tool_name("tool name"));
        assert!(!is_valid_openai_tool_name(
            "this_tool_name_is_way_too_long_and_exceeds_the_sixty_four_character_limit_xxxxx"
        ));
    }

    #[test]
    fn convert_tools_skips_invalid_names() {
        use zeroclaw_api::tool::ToolSpec;

        let tools = vec![
            ToolSpec {
                name: "valid_tool".into(),
                description: "A valid tool".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolSpec {
                name: "mcp:server.bad".into(),
                description: "Invalid name".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolSpec {
                name: "another-valid".into(),
                description: "Also valid".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        ];

        let result = OpenRouterProvider::convert_tools(Some(&tools)).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].function.name, "valid_tool");
        assert_eq!(result[1].function.name, "another-valid");
    }

    #[test]
    fn convert_tools_returns_none_when_all_invalid() {
        use zeroclaw_api::tool::ToolSpec;

        let tools = vec![ToolSpec {
            name: "mcp:bad.name".into(),
            description: "Invalid".into(),
            parameters: serde_json::json!({"type": "object"}),
        }];

        assert!(OpenRouterProvider::convert_tools(Some(&tools)).is_none());
    }

    #[test]
    fn with_extra_body_sets_value() {
        let extra = serde_json::json!({"provider": {"only": ["Anthropic"]}});
        let provider = OpenRouterProvider::new(Some("key"), None).with_extra_body(extra.clone());
        assert_eq!(provider.extra_body, Some(extra));
    }

    #[test]
    fn extra_body_none_produces_unchanged_request() {
        let provider = OpenRouterProvider::new(Some("key"), None);
        let request = ChatRequest {
            model: "test-model".into(),
            messages: vec![],
            temperature: 0.5,
            max_tokens: None,
        };

        let base = serde_json::to_value(&request).unwrap();
        let merged = provider.merge_extra_body(&request).unwrap();
        assert_eq!(base, merged);
    }

    #[test]
    fn extra_body_empty_object_produces_unchanged_request() {
        let provider =
            OpenRouterProvider::new(Some("key"), None).with_extra_body(serde_json::json!({}));
        let request = ChatRequest {
            model: "test-model".into(),
            messages: vec![],
            temperature: 0.5,
            max_tokens: None,
        };

        let base = serde_json::to_value(&request).unwrap();
        let merged = provider.merge_extra_body(&request).unwrap();
        assert_eq!(base, merged);
    }

    #[test]
    fn extra_body_adds_new_top_level_keys() {
        let provider = OpenRouterProvider::new(Some("key"), None)
            .with_extra_body(serde_json::json!({"provider": {"only": ["Anthropic"]}}));
        let request = ChatRequest {
            model: "test-model".into(),
            messages: vec![],
            temperature: 0.5,
            max_tokens: None,
        };

        let merged = provider.merge_extra_body(&request).unwrap();
        let obj = merged.as_object().unwrap();
        assert_eq!(
            obj.get("provider").unwrap(),
            &serde_json::json!({"only": ["Anthropic"]})
        );
        assert_eq!(obj.get("model").unwrap(), "test-model");
        assert_eq!(obj.get("temperature").unwrap(), 0.5);
    }

    #[test]
    fn extra_body_overrides_existing_keys() {
        let provider = OpenRouterProvider::new(Some("key"), None)
            .with_extra_body(serde_json::json!({"temperature": 0.9}));
        let request = ChatRequest {
            model: "test-model".into(),
            messages: vec![],
            temperature: 0.5,
            max_tokens: None,
        };

        let merged = provider.merge_extra_body(&request).unwrap();
        let obj = merged.as_object().unwrap();
        assert_eq!(obj.get("temperature").unwrap(), 0.9);
    }

    #[test]
    fn extra_body_merges_at_top_level_not_nested() {
        let provider = OpenRouterProvider::new(Some("key"), None)
            .with_extra_body(serde_json::json!({"transforms": ["middle-out"]}));
        let request = ChatRequest {
            model: "test-model".into(),
            messages: vec![],
            temperature: 0.5,
            max_tokens: None,
        };

        let merged = provider.merge_extra_body(&request).unwrap();
        let obj = merged.as_object().unwrap();
        assert_eq!(
            obj.get("transforms").unwrap(),
            &serde_json::json!(["middle-out"])
        );
        assert!(obj.get("extra_body").is_none());
    }

    #[test]
    fn extra_body_with_nested_provider_routing() {
        let provider = OpenRouterProvider::new(Some("key"), None).with_extra_body(
            serde_json::json!({"provider": {"only": ["Anthropic"], "allow_fallbacks": false}}),
        );
        let request = NativeChatRequest {
            model: "anthropic/claude-sonnet-4".into(),
            messages: vec![],
            temperature: 0.7,
            tools: None,
            tool_choice: None,
            max_tokens: None,
            stream: None,
        };

        let merged = provider.merge_extra_body(&request).unwrap();
        let obj = merged.as_object().unwrap();
        let prov = obj.get("provider").unwrap();
        assert_eq!(prov["only"], serde_json::json!(["Anthropic"]));
        assert_eq!(prov["allow_fallbacks"], false);
    }

    /// Regression for #5822.
    ///
    /// `AbortOnDrop` must cancel the bound tokio task when it is dropped.
    /// This guards the `stream_chat` invariant that a dropped stream stops
    /// the in-flight SSE-forwarding task instead of letting it run to
    /// completion.
    #[tokio::test]
    async fn abort_on_drop_cancels_long_running_task() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::time::{Duration, timeout};

        let finished = Arc::new(AtomicBool::new(false));
        let finished_clone = Arc::clone(&finished);

        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            finished_clone.store(true, Ordering::SeqCst);
        });
        let raw_handle = handle.abort_handle();
        let guard = AbortOnDrop(handle.abort_handle());

        assert!(!raw_handle.is_finished());

        drop(guard);

        let cancelled = timeout(Duration::from_secs(2), async {
            loop {
                if raw_handle.is_finished() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;

        assert!(
            cancelled.is_ok(),
            "task should be aborted within 2 s of AbortOnDrop being dropped"
        );
        assert!(
            !finished.load(Ordering::SeqCst),
            "cancelled task must not have run its completion side effect"
        );
    }
}
