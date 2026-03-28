use crate::providers::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    NormalizedStopReason, Provider, TokenUsage, ToolCall as ProviderToolCall,
};
use crate::tools::ToolSpec;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OpenAiProvider {
    base_url: String,
    credential: Option<String>,
    max_tokens_override: Option<u32>,
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
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
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
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
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
}

#[derive(Debug, Deserialize)]
struct NativeChoice {
    message: NativeResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
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
        Self::with_base_url_and_max_tokens(None, credential, None)
    }

    /// Create a provider with an optional custom base URL.
    /// Defaults to `https://api.openai.com/v1` when `base_url` is `None`.
    pub fn with_base_url(base_url: Option<&str>, credential: Option<&str>) -> Self {
        Self::with_base_url_and_max_tokens(base_url, credential, None)
    }

    pub fn with_base_url_and_max_tokens(
        base_url: Option<&str>,
        credential: Option<&str>,
        max_tokens_override: Option<u32>,
    ) -> Self {
        Self {
            base_url: base_url
                .map(|u| u.trim_end_matches('/').to_string())
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            credential: credential.map(ToString::to_string),
            max_tokens_override: max_tokens_override.filter(|value| *value > 0),
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
                            .map(ToString::to_string);
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
                    content: Some(m.content.clone()),
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning_content: None,
                }
            })
            .collect()
    }

    fn parse_native_response(choice: NativeChoice) -> ProviderChatResponse {
        let raw_stop_reason = choice.finish_reason;
        let stop_reason = raw_stop_reason
            .as_deref()
            .map(NormalizedStopReason::from_openai_finish_reason);
        let message = choice.message;
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
            quota_metadata: None,
            stop_reason,
            raw_stop_reason,
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
            temperature,
            max_tokens: self.max_tokens_override,
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

        let tools = Self::convert_tools(request.tools);
        let native_request = NativeChatRequest {
            model: model.to_string(),
            messages: Self::convert_messages(request.messages),
            temperature,
            max_tokens: self.max_tokens_override,
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
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

        // Extract quota metadata from response headers before consuming body
        let quota_extractor = super::quota_adapter::UniversalQuotaExtractor::new();
        let quota_metadata = quota_extractor.extract("openai", response.headers(), None);

        let native_response: NativeChatResponse = response.json().await?;
        let usage = native_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        });
        let choice = native_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No response from OpenAI"))?;
        let mut result = Self::parse_native_response(choice);
        result.usage = usage;
        result.quota_metadata = quota_metadata;
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
            temperature,
            max_tokens: self.max_tokens_override,
            tool_choice: native_tools.as_ref().map(|_| "auto".to_string()),
            tools: native_tools,
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

        // Extract quota metadata from response headers before consuming body
        let quota_extractor = super::quota_adapter::UniversalQuotaExtractor::new();
        let quota_metadata = quota_extractor.extract("openai", response.headers(), None);

        let native_response: NativeChatResponse = response.json().await?;
        let usage = native_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        });
        let choice = native_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No response from OpenAI"))?;
        let mut result = Self::parse_native_response(choice);
        result.usage = usage;
        result.quota_metadata = quota_metadata;
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
                    content: "You are ZeroClaw".to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                },
            ],
            temperature: 0.7,
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
            temperature: 0.0,
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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid OpenAI tool specification"));
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
        },"finish_reason":"length"}]}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let choice = resp.choices.into_iter().next().unwrap();
        let parsed = OpenAiProvider::parse_native_response(choice);
        assert_eq!(parsed.reasoning_content.as_deref(), Some("thinking step"));
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.stop_reason, Some(NormalizedStopReason::MaxTokens));
        assert_eq!(parsed.raw_stop_reason.as_deref(), Some("length"));
    }

    #[test]
    fn parse_native_response_none_reasoning_content_for_normal_model() {
        let json = r#"{"choices":[{"message":{"content":"hello"},"finish_reason":"stop"}]}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let choice = resp.choices.into_iter().next().unwrap();
        let parsed = OpenAiProvider::parse_native_response(choice);
        assert!(parsed.reasoning_content.is_none());
        assert_eq!(parsed.stop_reason, Some(NormalizedStopReason::EndTurn));
        assert_eq!(parsed.raw_stop_reason.as_deref(), Some("stop"));
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
        let native = OpenAiProvider::convert_messages(&messages);
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
        let native = OpenAiProvider::convert_messages(&messages);
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
}
