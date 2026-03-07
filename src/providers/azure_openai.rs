use crate::providers::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, ProviderCapabilities, TokenUsage, ToolCall as ProviderToolCall, ToolsPayload,
};
use crate::tools::ToolSpec;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};use std::time::Duration;
pub struct AzureOpenAiProvider {
    base_url: String,
    api_key: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    messages: Vec<Message>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
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
    messages: Vec<NativeMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
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
        .map_err(|e| anyhow::anyhow!("Invalid Azure OpenAI tool specification: {e}"))?;

    if spec.kind != "function" {
        anyhow::bail!(
            "Invalid Azure OpenAI tool specification: unsupported tool type '{}', expected 'function'",
            spec.kind
        );
    }

    Ok(spec)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NativeToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    function: NativeFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl NativeResponseMessage {
    fn effective_content(&self) -> Option<String> {
        match &self.content {
            Some(c) if !c.is_empty() => Some(c.clone()),
            _ => self.reasoning_content.clone(),
        }
    }
}

impl AzureOpenAiProvider {
    pub fn new(base_url: &str, api_key: Option<&str>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(ToString::to_string),
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
                // Handle structured assistant messages (with tool calls)
                if m.role == "assistant" && m.content.starts_with('{') {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&m.content) {
                        if let Some(content_val) = parsed.get("content") {
                            // Extract tool calls if present
                            let tool_calls = parsed
                                .get("tool_calls")
                                .and_then(|tc| tc.as_array())
                                .map(|calls| {
                                    calls
                                        .iter()
                                        .filter_map(|call| {
                                            Some(NativeToolCall {
                                                id: call.get("id")?.as_str()?.to_string().into(),
                                                kind: Some("function".to_string()),
                                                function: NativeFunctionCall {
                                                    name: call.get("name")?.as_str()?.to_string(),
                                                    arguments: call
                                                        .get("arguments")?
                                                        .as_str()?
                                                        .to_string(),
                                                },
                                            })
                                        })
                                        .collect()
                                });

                            // Extract reasoning content if present
                            let reasoning_content = parsed
                                .get("reasoning_content")
                                .and_then(|rc| rc.as_str())
                                .map(ToString::to_string);

                            return NativeMessage {
                                role: m.role.clone(),
                                content: content_val.as_str().map(ToString::to_string),
                                tool_call_id: None,
                                tool_calls,
                                reasoning_content,
                            };
                        }
                    }
                }

                // Handle tool result messages
                if m.role == "tool" {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&m.content) {
                        if let Some(tool_call_id) = parsed.get("tool_call_id") {
                            return NativeMessage {
                                role: m.role.clone(),
                                content: parsed.get("content").and_then(|c| c.as_str()).map(ToString::to_string),
                                tool_call_id: tool_call_id.as_str().map(ToString::to_string),
                                tool_calls: None,
                                reasoning_content: None,
                            };
                        }
                    }
                }

                // Default message conversion
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
        // Extract all values to avoid partial move issues
        let content = message.content.clone();
        let reasoning_content = message.reasoning_content.clone();
        let tool_calls_data = message.tool_calls.clone();
        
        // Generate text using the same logic as effective_content
        let text = match &content {
            Some(c) if !c.is_empty() => c.clone(),
            _ => reasoning_content.clone().unwrap_or_default(),
        };
        
        let tool_calls = tool_calls_data
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tc| {
                tc.id.map(|id| ProviderToolCall {
                    id,
                    name: tc.function.name,
                    arguments: tc.function.arguments,
                })
            })
            .collect();

        ProviderChatResponse {
            text: Some(text),
            tool_calls,
            usage: None, // Will be set by caller
            reasoning_content,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| Client::new())
    }

    fn chat_completions_url(&self, model: &str) -> String {
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version=2024-10-21",
            self.base_url, model
        )
    }
}

#[async_trait]
impl Provider for AzureOpenAiProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: false,
        }
    }

    fn convert_tools(&self, tools: &[ToolSpec]) -> ToolsPayload {
        let openai_tools: Vec<serde_json::Value> = tools
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
            .collect();

        ToolsPayload::OpenAI { tools: openai_tools }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Azure OpenAI API key not set. Set AZURE_OPENAI_API_KEY or edit config.toml.")
        })?;

        let mut messages = Vec::new();

        if let Some(system) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: system.to_string(),
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: message.to_string(),
        });

        let request = ChatRequest {
            messages,
            temperature,
            max_completion_tokens: Some(4096),
        };

        let response = self
            .http_client()
            .post(self.chat_completions_url(model))
            .header("api-key", api_key)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("Azure OpenAI", response).await);
        }

        let chat_response: ChatResponse = response.json().await?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.effective_content())
            .ok_or_else(|| anyhow::anyhow!("No response from Azure OpenAI"))
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Azure OpenAI API key not set. Set AZURE_OPENAI_API_KEY or edit config.toml.")
        })?;

        let tools = Self::convert_tools(request.tools);
        let native_request = NativeChatRequest {
            messages: Self::convert_messages(request.messages),
            temperature,
            max_completion_tokens: Some(4096),
            tool_choice: tools.as_ref().map(|_| "auto".to_string()),
            tools,
        };

        let response = self
            .http_client()
            .post(self.chat_completions_url(model))
            .header("api-key", api_key)
            .header("Content-Type", "application/json")
            .json(&native_request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("Azure OpenAI", response).await);
        }

        let native_response: NativeChatResponse = response.json().await?;
        let usage = native_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        });
        
        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| anyhow::anyhow!("No response from Azure OpenAI"))?;
        
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
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Azure OpenAI API key not set. Set AZURE_OPENAI_API_KEY or edit config.toml.")
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
            messages: Self::convert_messages(messages),
            temperature,
            max_completion_tokens: Some(4096),
            tool_choice: native_tools.as_ref().map(|_| "auto".to_string()),
            tools: native_tools,
        };

        let response = self
            .http_client()
            .post(self.chat_completions_url(model))
            .header("api-key", api_key)
            .header("Content-Type", "application/json")
            .json(&native_request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("Azure OpenAI", response).await);
        }

        let native_response: NativeChatResponse = response.json().await?;
        let usage = native_response.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        });
        
        let message = native_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| anyhow::anyhow!("No response from Azure OpenAI"))?;
        
        let mut result = Self::parse_native_response(message);
        result.usage = usage;
        Ok(result)
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        if let Some(api_key) = self.api_key.as_ref() {
            let deployments_url = format!(
                "{}/openai/deployments?api-version=2024-10-21",
                self.base_url
            );
            self.http_client()
                .get(deployments_url)
                .header("api-key", api_key)
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
    fn creates_with_all_params() {
        let provider = AzureOpenAiProvider::new(
            "https://my-resource.openai.azure.com",
            Some("test-key")
        );
        assert_eq!(provider.base_url, "https://my-resource.openai.azure.com");
        assert_eq!(provider.api_key.as_deref(), Some("test-key"));
    }

    #[test]
    fn creates_without_key() {
        let provider = AzureOpenAiProvider::new(
            "https://my-resource.openai.azure.com",
            None
        );
        assert!(provider.api_key.is_none());
    }

    #[test]
    fn chat_completions_url_is_correct() {
        let provider = AzureOpenAiProvider::new(
            "https://my-resource.openai.azure.com",
            Some("test-key")
        );
        let url = provider.chat_completions_url("gpt-5.2-chat");
        assert_eq!(
            url,
            "https://my-resource.openai.azure.com/openai/deployments/gpt-5.2-chat/chat/completions?api-version=2024-10-21"
        );
    }

    #[test]
    fn strips_trailing_slash_from_base_url() {
        let provider = AzureOpenAiProvider::new(
            "https://my-resource.openai.azure.com/",
            Some("test-key")
        );
        assert_eq!(provider.base_url, "https://my-resource.openai.azure.com");
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let provider = AzureOpenAiProvider::new(
            "https://my-resource.openai.azure.com",
            None
        );
        let result = provider.chat_with_system(None, "hello", "gpt-5.2-chat", 0.7).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[test]
    fn request_serializes_with_max_completion_tokens() {
        let request = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.7,
            max_completion_tokens: Some(4096),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("max_completion_tokens"));
        assert!(json.contains("4096"));
        assert!(json.contains("temperature"));
        assert!(json.contains("0.7"));
    }

    #[test]
    fn request_omits_max_completion_tokens_when_none() {
        let request = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.0,
            max_completion_tokens: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("max_completion_tokens"));
    }

    #[test]
    fn native_tool_spec_deserializes_from_azure_openai_format() {
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
    fn response_deserializes_single_choice() {
        let json = r#"{"choices":[{"message":{"content":"Hi!"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.effective_content(), "Hi!");
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

    #[tokio::test]
    async fn chat_with_tools_fails_without_key() {
        let provider = AzureOpenAiProvider::new(
            "https://my-resource.openai.azure.com",
            None
        );
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
        let result = provider.chat_with_tools(&messages, &tools, "gpt-5.2-chat", 0.7).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[test]
    fn url_generation_with_different_models() {
        let provider = AzureOpenAiProvider::new(
            "https://my-resource.openai.azure.com",
            Some("test-key")
        );
        
        // Test different model/deployment names
        let url1 = provider.chat_completions_url("gpt-5.2-chat");
        assert!(url1.contains("/deployments/gpt-5.2-chat/"));
        
        let url2 = provider.chat_completions_url("gpt-4o");
        assert!(url2.contains("/deployments/gpt-4o/"));
        
        let url3 = provider.chat_completions_url("custom-deployment");
        assert!(url3.contains("/deployments/custom-deployment/"));
        
        // All URLs should have the correct API version
        assert!(url1.contains("api-version=2024-10-21"));
        assert!(url2.contains("api-version=2024-10-21"));
        assert!(url3.contains("api-version=2024-10-21"));
    }

    #[test]
    fn supports_native_tools() {
        let provider = AzureOpenAiProvider::new(
            "https://my-resource.openai.azure.com",
            Some("test-key")
        );
        assert!(provider.supports_native_tools());
    }

    #[test]
    fn capabilities_include_native_tool_calling() {
        let provider = AzureOpenAiProvider::new(
            "https://my-resource.openai.azure.com",
            Some("test-key")
        );
        let capabilities = provider.capabilities();
        assert!(capabilities.native_tool_calling);
        assert!(!capabilities.vision); // Azure OpenAI doesn't support vision in this implementation
    }

    #[test]
    fn native_chat_request_serializes_correctly() {
        let native_request = NativeChatRequest {
            messages: vec![NativeMessage {
                role: "user".to_string(),
                content: Some("Hello".to_string()),
                tool_call_id: None,
                tool_calls: None,
                reasoning_content: None,
            }],
            temperature: 0.5,
            max_completion_tokens: Some(2048),
            tools: None,
            tool_choice: None,
        };
        
        let json = serde_json::to_string(&native_request).unwrap();
        assert!(json.contains("max_completion_tokens"));
        assert!(json.contains("2048"));
        assert!(json.contains("temperature"));
        assert!(json.contains("0.5"));
        assert!(!json.contains("tools"));
        assert!(!json.contains("tool_choice"));
    }

    #[test]
    fn native_chat_request_with_tools_serializes_correctly() {
        let tools = vec![NativeToolSpec {
            kind: "function".to_string(),
            function: NativeToolFunctionSpec {
                name: "test_tool".to_string(),
                description: "A test tool".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }];
        
        let native_request = NativeChatRequest {
            messages: vec![NativeMessage {
                role: "user".to_string(),
                content: Some("Hello".to_string()),
                tool_call_id: None,
                tool_calls: None,
                reasoning_content: None,
            }],
            temperature: 0.7,
            max_completion_tokens: Some(1024),
            tools: Some(tools),
            tool_choice: Some("auto".to_string()),
        };
        
        let json = serde_json::to_string(&native_request).unwrap();
        assert!(json.contains("tools"));
        assert!(json.contains("tool_choice"));
        assert!(json.contains("auto"));
        assert!(json.contains("test_tool"));
    }

    #[test]
    fn convert_tools_creates_correct_format() {
        use crate::tools::ToolSpec;
        
        let tools = vec![ToolSpec {
            name: "shell".to_string(),
            description: "Run shell command".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                }
            }),
        }];
        
        let converted = AzureOpenAiProvider::convert_tools(Some(&tools));
        assert!(converted.is_some());
        
        let converted_tools = converted.unwrap();
        assert_eq!(converted_tools.len(), 1);
        assert_eq!(converted_tools[0].kind, "function");
        assert_eq!(converted_tools[0].function.name, "shell");
        assert_eq!(converted_tools[0].function.description, "Run shell command");
    }

    #[test]
    fn parse_native_tool_spec_rejects_invalid_type() {
        let invalid_json = serde_json::json!({
            "type": "invalid_type",
            "function": {
                "name": "test",
                "description": "test",
                "parameters": {}
            }
        });
        
        let result = parse_native_tool_spec(invalid_json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported tool type"));
    }

    #[test]
    fn response_message_effective_content_prefers_content() {
        let msg = ResponseMessage {
            content: Some("main content".to_string()),
            reasoning_content: Some("reasoning".to_string()),
        };
        assert_eq!(msg.effective_content(), "main content");
    }

    #[test]
    fn response_message_effective_content_falls_back_to_reasoning() {
        let msg = ResponseMessage {
            content: Some("".to_string()),
            reasoning_content: Some("reasoning content".to_string()),
        };
        assert_eq!(msg.effective_content(), "reasoning content");
    }

    #[test]
    fn response_message_effective_content_handles_none() {
        let msg = ResponseMessage {
            content: None,
            reasoning_content: None,
        };
        assert_eq!(msg.effective_content(), "");
    }
}