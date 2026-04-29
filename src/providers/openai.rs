use crate::providers::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, TokenUsage, ToolCall as ProviderToolCall,
};
use crate::tools::ToolSpec;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OpenAiProvider {
    base_url: String,
    credential: Option<String>,
    max_tokens: Option<u32>,
    /// Explicit override for Anthropic-style `cache_control` markers.
    ///
    /// - `None` (default) → auto-detect per call from model name. Marks
    ///   are added when `model_supports_prompt_caching(model)` is true,
    ///   i.e. `bedrock/*anthropic*`, `anthropic/*`, `claude-*`. Other
    ///   models are sent on the legacy plain-string content shape so
    ///   non-Anthropic backends don't reject the structured form.
    /// - `Some(true)` → always mark.
    /// - `Some(false)` → never mark, even for Anthropic.
    ///
    /// Set via `with_prompt_caching(...)` from the provider factory.
    prompt_caching_override: Option<bool>,
}

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

/// `messages[*].content` accepts either a plain string (the OpenAI
/// classic shape) OR a structured array of content parts (the
/// Anthropic-style shape that LiteLLM passes through, and the only
/// shape that supports `cache_control`). `untagged` lets serde pick the
/// right wire form per-message at serialize time.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl From<&str> for MessageContent {
    fn from(s: &str) -> Self {
        MessageContent::Text(s.to_string())
    }
}

impl From<String> for MessageContent {
    fn from(s: String) -> Self {
        MessageContent::Text(s)
    }
}

#[derive(Debug, Serialize)]
struct ContentPart {
    #[serde(rename = "type")]
    kind: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CacheControl {
    #[serde(rename = "type")]
    kind: String,
}

impl CacheControl {
    fn ephemeral() -> Self {
        Self {
            kind: "ephemeral".to_string(),
        }
    }
}

impl ContentPart {
    fn text(text: impl Into<String>) -> Self {
        Self {
            kind: "text".to_string(),
            text: text.into(),
            cache_control: None,
        }
    }
    fn cached_text(text: impl Into<String>) -> Self {
        Self {
            kind: "text".to_string(),
            text: text.into(),
            cache_control: Some(CacheControl::ephemeral()),
        }
    }
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
    temperature: f64,
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

#[derive(Debug, Serialize, Deserialize)]
struct NativeToolSpec {
    #[serde(rename = "type")]
    kind: String,
    function: NativeToolFunctionSpec,
    /// Anthropic-style cache breakpoint at the tool level. Set on the LAST
    /// tool spec to make Anthropic cache the entire tools block. LiteLLM
    /// passes this through to Bedrock-Anthropic Converse.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    cache_control: Option<CacheControl>,
}

#[derive(Debug, Serialize, Deserialize)]
struct NativeToolFunctionSpec {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

fn parse_native_tool_spec(value: serde_json::Value) -> anyhow::Result<NativeToolSpec> {
    let spec: NativeToolSpec = serde_json::from_value(value)
        .map_err(|e| anyhow::anyhow!("Invalid OpenAI tool specification: {e}"))?;

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

impl OpenAiProvider {
    pub fn new(credential: Option<&str>) -> Self {
        Self::with_base_url(None, credential)
    }

    /// Create a provider with an optional custom base URL.
    /// Defaults to `https://api.openai.com/v1` when `base_url` is `None`.
    pub fn with_base_url(base_url: Option<&str>, credential: Option<&str>) -> Self {
        Self {
            base_url: base_url
                .map(|u| u.trim_end_matches('/').to_string())
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            credential: credential.map(ToString::to_string),
            max_tokens: None,
            prompt_caching_override: None,
        }
    }

    /// Set the maximum output tokens for API requests.
    pub fn with_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Pin Anthropic-style prompt caching to an explicit value, overriding
    /// the auto-detect-from-model default. Use sparingly: most callers want
    /// the auto path so the same provider instance can serve mixed
    /// Anthropic / non-Anthropic models without double-config.
    pub fn with_prompt_caching(mut self, enabled: bool) -> Self {
        self.prompt_caching_override = Some(enabled);
        self
    }

    /// Heuristic detector for "this model resolves to an Anthropic Claude
    /// backend that supports prompt caching". Used at chat-call time so
    /// the same provider instance can correctly handle Anthropic routes
    /// (mark cache_control) and non-Anthropic routes (legacy shape) on
    /// the same LiteLLM proxy.
    pub fn model_supports_prompt_caching(model: &str) -> bool {
        let m = model.to_ascii_lowercase();
        m.starts_with("anthropic/")
            || m.starts_with("bedrock/") && (m.contains("anthropic") || m.contains("claude"))
            || m.contains("claude-")
    }

    /// Decide per-call whether to emit Anthropic cache_control markers.
    /// Explicit override wins; otherwise falls back to model name detection.
    fn caching_enabled_for(&self, model: &str) -> bool {
        self.prompt_caching_override
            .unwrap_or_else(|| Self::model_supports_prompt_caching(model))
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

    fn convert_tools(
        tools: Option<&[ToolSpec]>,
        enable_caching: bool,
    ) -> Option<Vec<NativeToolSpec>> {
        tools.map(|items| {
            let mut converted: Vec<NativeToolSpec> = items
                .iter()
                .map(|tool| NativeToolSpec {
                    kind: "function".to_string(),
                    function: NativeToolFunctionSpec {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                    },
                    cache_control: None,
                })
                .collect();
            // Anthropic prompt caching rule: marking cache_control on the
            // LAST tool spec causes the entire tools block to be cached.
            // This is the cheapest way to cache the (typically very large
            // and very stable) tool catalog. We only do this when caching
            // is on AND there's at least one tool.
            if enable_caching {
                if let Some(last) = converted.last_mut() {
                    last.cache_control = Some(CacheControl::ephemeral());
                }
            }
            converted
        })
    }

    fn convert_messages(messages: &[ChatMessage], enable_caching: bool) -> Vec<NativeMessage> {
        let mut converted: Vec<NativeMessage> =
            messages
                .iter()
                .map(|m| {
                    if m.role == "assistant" {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content) {
                            if let Some(tool_calls_value) = value.get("tool_calls") {
                                if let Ok(parsed_calls) =
                                    serde_json::from_value::<Vec<ProviderToolCall>>(
                                        tool_calls_value.clone(),
                                    )
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
                                        .map(|s| MessageContent::Text(s.to_string()));
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

                    if m.role == "tool" {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content) {
                            let tool_call_id = value
                                .get("tool_call_id")
                                .and_then(serde_json::Value::as_str)
                                .map(ToString::to_string);
                            let content = value
                                .get("content")
                                .and_then(serde_json::Value::as_str)
                                .map(|s| MessageContent::Text(s.to_string()));
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
                        role: m.role.clone(),
                        content: Some(MessageContent::Text(m.content.clone())),
                        tool_call_id: None,
                        tool_calls: None,
                        reasoning_content: None,
                    }
                })
                .collect();

        if enable_caching {
            // Mark the system prompt as a cache breakpoint. System prompts in
            // an agentic loop are large (~5–15k tokens incl. persona, tools
            // hint, format rules) and stable across the whole conversation.
            // Caching it is the single biggest first-byte win on Anthropic.
            if let Some(system) = converted.iter_mut().find(|m| m.role == "system") {
                if let Some(MessageContent::Text(text)) = system.content.take() {
                    system.content =
                        Some(MessageContent::Parts(vec![ContentPart::cached_text(text)]));
                }
            }
        }

        converted
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
        crate::config::build_runtime_proxy_client_with_timeouts("provider.openai", 120, 10)
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;

        let adjusted_temperature = Self::adjust_temperature_for_model(model, temperature);

        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            // System prompt is the canonical caching target — large, stable,
            // identical across every turn in a session. Mark with
            // cache_control when caching is enabled; otherwise stay on the
            // plain-string shape that legacy providers expect.
            let content = if self.caching_enabled_for(model) {
                MessageContent::Parts(vec![ContentPart::cached_text(sys)])
            } else {
                MessageContent::Text(sys.to_string())
            };
            messages.push(Message {
                role: "system".to_string(),
                content,
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: MessageContent::Text(message.to_string()),
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
            .ok_or_else(|| anyhow::anyhow!("No response from OpenAI"))
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;

        let adjusted_temperature = Self::adjust_temperature_for_model(model, temperature);

        let enable_caching = self.caching_enabled_for(model);
        let tools = Self::convert_tools(request.tools, enable_caching);
        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(request.messages, enable_caching),
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
            .ok_or_else(|| anyhow::anyhow!("No response from OpenAI"))?;
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
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let credential = self.credential.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;

        let adjusted_temperature = Self::adjust_temperature_for_model(model, temperature);

        let enable_caching = self.caching_enabled_for(model);
        let native_tools: Option<Vec<NativeToolSpec>> = if tools.is_empty() {
            None
        } else {
            let mut converted: Vec<NativeToolSpec> = tools
                .iter()
                .cloned()
                .map(parse_native_tool_spec)
                .collect::<Result<Vec<_>, _>>()?;
            // Tail-of-tools cache breakpoint, same trick as `convert_tools`.
            if enable_caching {
                if let Some(last) = converted.last_mut() {
                    last.cache_control = Some(CacheControl::ephemeral());
                }
            }
            Some(converted)
        };

        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(messages, enable_caching),
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
            .ok_or_else(|| anyhow::anyhow!("No response from OpenAI"))?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_key() {
        let p = OpenAiProvider::new(Some("openai-test-credential"));
        assert_eq!(p.credential.as_deref(), Some("openai-test-credential"));
    }

    #[test]
    fn creates_without_key() {
        let p = OpenAiProvider::new(None);
        assert!(p.credential.is_none());
    }

    #[test]
    fn creates_with_empty_key() {
        let p = OpenAiProvider::new(Some(""));
        assert_eq!(p.credential.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = OpenAiProvider::new(None);
        let result = p.chat_with_system(None, "hello", "gpt-4o", 0.7).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = OpenAiProvider::new(None);
        let result = p
            .chat_with_system(Some("You are ZeroClaw"), "test", "gpt-4o", 0.5)
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
                    content: MessageContent::Text("You are ZeroClaw".to_string()),
                },
                Message {
                    role: "user".to_string(),
                    content: MessageContent::Text("hello".to_string()),
                },
            ],
            temperature: 0.7,
            max_tokens: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("gpt-4o"));
        // Plain string shape is preserved when caching is off — important
        // for non-Anthropic backends that may reject the structured shape.
        assert!(json.contains("\"content\":\"You are ZeroClaw\""));
    }

    #[test]
    fn request_serializes_without_system() {
        let req = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::Text("hello".to_string()),
            }],
            temperature: 0.0,
            max_tokens: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("system"));
        assert!(json.contains("\"temperature\":0.0"));
    }

    // ----------------------------------------------------------
    // Anthropic prompt caching (cache_control) wire format
    // ----------------------------------------------------------

    #[test]
    fn cache_control_serializes_as_ephemeral_part() {
        let part = ContentPart::cached_text("system prompt body");
        let json = serde_json::to_string(&part).unwrap();
        // LiteLLM expects exactly this shape (Anthropic-style content part).
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"system prompt body\""));
        assert!(json.contains("\"cache_control\":{\"type\":\"ephemeral\"}"));
    }

    #[test]
    fn parts_message_serializes_as_array() {
        let msg = Message {
            role: "system".to_string(),
            content: MessageContent::Parts(vec![ContentPart::cached_text("be helpful")]),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"system\""));
        // Array shape is the only one Anthropic honors cache_control on.
        assert!(json.contains("\"content\":[{"));
        assert!(json.contains("\"cache_control\""));
    }

    #[test]
    fn convert_messages_marks_system_when_caching_enabled() {
        let messages = vec![
            ChatMessage::system("You are an agent.".to_string()),
            ChatMessage::user("hi".to_string()),
        ];
        let native = OpenAiProvider::convert_messages(&messages, true);
        let json = serde_json::to_string(&native).unwrap();
        // System role's content must be the structured array form so
        // cache_control rides along.
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"cache_control\":{\"type\":\"ephemeral\"}"));
        // User role keeps the plain string shape — no need to cache the
        // ephemeral query, and Anthropic only allows up to 4 breakpoints.
        assert!(json.contains("\"role\":\"user\",\"content\":\"hi\""));
    }

    #[test]
    fn convert_messages_does_not_mark_when_caching_disabled() {
        let messages = vec![
            ChatMessage::system("You are an agent.".to_string()),
            ChatMessage::user("hi".to_string()),
        ];
        let native = OpenAiProvider::convert_messages(&messages, false);
        let json = serde_json::to_string(&native).unwrap();
        // With caching off the wire stays exactly as before — preserves
        // back-compat for non-Anthropic backends that may reject the
        // structured shape or the cache_control field.
        assert!(!json.contains("cache_control"));
        assert!(json.contains("\"content\":\"You are an agent.\""));
    }

    #[test]
    fn convert_tools_marks_last_when_caching_enabled() {
        let tools = vec![
            crate::tools::ToolSpec {
                name: "shell".into(),
                description: "Run a shell command".into(),
                parameters: serde_json::json!({}),
            },
            crate::tools::ToolSpec {
                name: "browser".into(),
                description: "Browse the web".into(),
                parameters: serde_json::json!({}),
            },
        ];
        let converted = OpenAiProvider::convert_tools(Some(&tools), true).unwrap();
        assert_eq!(converted.len(), 2);
        // Only the LAST tool gets cache_control — that's how Anthropic
        // caches the entire prefix tools block in one breakpoint.
        assert!(converted[0].cache_control.is_none());
        assert!(converted[1].cache_control.is_some());
    }

    #[test]
    fn convert_tools_marks_none_when_caching_disabled() {
        let tools = vec![crate::tools::ToolSpec {
            name: "shell".into(),
            description: "Run a shell command".into(),
            parameters: serde_json::json!({}),
        }];
        let converted = OpenAiProvider::convert_tools(Some(&tools), false).unwrap();
        assert!(converted[0].cache_control.is_none());
    }

    #[test]
    fn model_supports_prompt_caching_classification() {
        // Bedrock-Anthropic is the path used in dev/prod via LiteLLM.
        assert!(OpenAiProvider::model_supports_prompt_caching(
            "bedrock/global.anthropic.claude-sonnet-4-6"
        ));
        assert!(OpenAiProvider::model_supports_prompt_caching(
            "anthropic/claude-3-5-sonnet-20240620"
        ));
        assert!(OpenAiProvider::model_supports_prompt_caching(
            "claude-3-5-sonnet-20240620"
        ));
        // Non-Anthropic models must NOT auto-enable caching, since
        // they may not understand `cache_control` and could reject it.
        assert!(!OpenAiProvider::model_supports_prompt_caching("gpt-4o"));
        assert!(!OpenAiProvider::model_supports_prompt_caching(
            "gemini-2.5-pro"
        ));
        assert!(!OpenAiProvider::model_supports_prompt_caching(
            "deepseek-chat"
        ));
    }

    #[test]
    fn caching_enabled_for_uses_model_detection_by_default() {
        let p = OpenAiProvider::new(Some("k"));
        // Auto-on for Anthropic-via-Bedrock-via-LiteLLM (the dev setup).
        assert!(p.caching_enabled_for("bedrock/global.anthropic.claude-sonnet-4-6"));
        // Auto-off for OpenAI's own models — they use server-side automatic
        // caching, which doesn't need (or accept) cache_control markers.
        assert!(!p.caching_enabled_for("gpt-4o"));
    }

    #[test]
    fn caching_enabled_for_respects_explicit_override() {
        // Override-true forces caching even on a model that wouldn't auto-on.
        let force_on = OpenAiProvider::new(Some("k")).with_prompt_caching(true);
        assert!(force_on.caching_enabled_for("gpt-4o"));
        // Override-false disables caching even on a model that would auto-on.
        let force_off = OpenAiProvider::new(Some("k")).with_prompt_caching(false);
        assert!(!force_off.caching_enabled_for("bedrock/global.anthropic.claude-sonnet-4-6"));
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
        let provider = OpenAiProvider::new(None);
        let result = provider.warmup().await;
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
        let p = OpenAiProvider::new(None);
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
        let result = p.chat_with_tools(&messages, &tools, "gpt-4o", 0.7).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[tokio::test]
    async fn chat_with_tools_rejects_invalid_tool_shape() {
        let p = OpenAiProvider::new(Some("openai-test-credential"));
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

        let result = p.chat_with_tools(&messages, &tools, "gpt-4o", 0.7).await;
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
        let parsed = OpenAiProvider::parse_native_response(message);
        assert_eq!(parsed.reasoning_content.as_deref(), Some("thinking step"));
        assert_eq!(parsed.tool_calls.len(), 1);
    }

    #[test]
    fn parse_native_response_none_reasoning_content_for_normal_model() {
        let json = r#"{"choices":[{"message":{"content":"hello"}}]}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let message = resp.choices.into_iter().next().unwrap().message;
        let parsed = OpenAiProvider::parse_native_response(message);
        assert!(parsed.reasoning_content.is_none());
    }

    #[test]
    fn convert_messages_round_trips_reasoning_content() {
        use crate::providers::ChatMessage;

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
        let native = OpenAiProvider::convert_messages(&messages, false);
        assert_eq!(native.len(), 1);
        assert_eq!(
            native[0].reasoning_content.as_deref(),
            Some("Let me think...")
        );
    }

    #[test]
    fn convert_messages_no_reasoning_content_when_absent() {
        use crate::providers::ChatMessage;

        let history_json = serde_json::json!({
            "content": "I will check",
            "tool_calls": [{
                "id": "tc_1",
                "name": "shell",
                "arguments": "{}"
            }]
        });

        let messages = vec![ChatMessage::assistant(history_json.to_string())];
        let native = OpenAiProvider::convert_messages(&messages, false);
        assert_eq!(native.len(), 1);
        assert!(native[0].reasoning_content.is_none());
    }

    #[test]
    fn native_message_omits_reasoning_content_when_none() {
        let msg = NativeMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("hi".to_string())),
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
            content: Some(MessageContent::Text("hi".to_string())),
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
        assert_eq!(OpenAiProvider::adjust_temperature_for_model("o1", 0.7), 1.0);
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("o1-2024-12-17", 0.5),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_for_o3_models() {
        assert_eq!(OpenAiProvider::adjust_temperature_for_model("o3", 0.7), 1.0);
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("o3-2025-04-16", 0.5),
            1.0
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("o3-mini", 0.3),
            1.0
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("o3-mini-2025-01-31", 0.8),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_for_o4_models() {
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("o4-mini", 0.7),
            1.0
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("o4-mini-2025-04-16", 0.5),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_for_gpt5_models() {
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-5", 0.7),
            1.0
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-5-2025-08-07", 0.5),
            1.0
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-5-mini", 0.3),
            1.0
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-5-mini-2025-08-07", 0.8),
            1.0
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-5-nano", 0.6),
            1.0
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-5-nano-2025-08-07", 0.4),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_for_gpt5_chat_latest_models() {
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-5.1-chat-latest", 0.7),
            1.0
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-5.2-chat-latest", 0.5),
            1.0
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-5.3-chat-latest", 0.3),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_preserves_for_standard_models() {
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-4o", 0.7),
            0.7
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-4-turbo", 0.5),
            0.5
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-3.5-turbo", 0.3),
            0.3
        );
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-4", 1.0),
            1.0
        );
    }

    #[test]
    fn adjust_temperature_handles_edge_cases() {
        // Temperature 0.0 should be preserved for standard models
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-4o", 0.0),
            0.0
        );
        // Temperature 1.0 should be preserved for all models
        assert_eq!(OpenAiProvider::adjust_temperature_for_model("o1", 1.0), 1.0);
        assert_eq!(
            OpenAiProvider::adjust_temperature_for_model("gpt-4o", 1.0),
            1.0
        );
    }
}
