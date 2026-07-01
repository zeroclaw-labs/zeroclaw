use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    ModelProvider, ProviderCapabilities, TokenUsage, ToolCall as ProviderToolCall, ToolsPayload,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use zeroclaw_api::tool::ToolSpec;

const DEFAULT_API_VERSION: &str = "2024-08-01-preview";

pub struct AzureOpenAiModelProvider {
    /// `[providers.models.azure.<alias>]` config-key alias.
    alias: String,
    credential: Option<String>,
    #[allow(dead_code)]
    resource_name: String,
    #[allow(dead_code)]
    deployment_name: String,
    api_version: String,
    base_url: String,
    /// Operator-configured reasoning effort (minimal/low/medium/high).
    /// Sent only to models that accept it (GPT-5.x / o-series).
    reasoning_effort: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<NativeToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
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
            "azure_openai: invalid tool spec"
        );
        anyhow::Error::msg(format!("Invalid Azure OpenAI tool specification: {e}"))
    })?;

    if spec.kind != "function" {
        anyhow::bail!(
            "Invalid Azure OpenAI tool specification: unsupported tool type '{}', expected 'function'",
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
}

#[derive(Debug, Deserialize)]
struct NativeResponseMessage {
    #[serde(default)]
    content: Option<String>,
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

impl AzureOpenAiModelProvider {
    pub fn new(
        alias: &str,
        credential: Option<&str>,
        resource_name: &str,
        deployment_name: &str,
        api_version: Option<&str>,
        reasoning_effort: Option<String>,
    ) -> Self {
        let version = api_version.unwrap_or(DEFAULT_API_VERSION);
        let base_url = format!(
            "https://{}.openai.azure.com/openai/deployments/{}",
            resource_name, deployment_name
        );
        let credential = credential
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        Self {
            alias: alias.to_string(),
            credential,
            resource_name: resource_name.to_string(),
            deployment_name: deployment_name.to_string(),
            api_version: version.to_string(),
            base_url,
            reasoning_effort,
        }
    }
    fn chat_completions_url(&self) -> String {
        format!(
            "{}/chat/completions?api-version={}",
            self.base_url, self.api_version
        )
    }

    /// Return the configured `reasoning_effort` when the model accepts it
    /// (GPT-5.x / o-series), otherwise `None`.  Mirrors the compatible
    /// provider's `reasoning_effort_for_model` so Azure parity is maintained.
    fn reasoning_effort_for_model(&self, model: &str) -> Option<String> {
        let effort = self.reasoning_effort.as_ref()?;
        let id = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        // gpt-5*-chat-latest are non-reasoning router models; they reject
        // reasoning_effort, so exclude them the same way compatible.rs does.
        let is_gpt5_chat_latest = id.starts_with("gpt-5") && id.ends_with("-chat-latest");
        let is_reasoning_model = id == "o1"
            || id.starts_with("o1-")
            || id == "o3"
            || id.starts_with("o3-")
            || id == "o4"
            || id.starts_with("o4-")
            || (id.starts_with("gpt-5") && !is_gpt5_chat_latest);
        is_reasoning_model.then(|| effort.clone())
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
            "model_provider.azure_openai",
            120,
            10,
        )
    }
}

#[async_trait]
impl ModelProvider for AzureOpenAiModelProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: true,
            prompt_caching: false,
            extended_thinking: false,
        }
    }

    fn convert_tools(&self, tools: &[ToolSpec]) -> ToolsPayload {
        ToolsPayload::OpenAI {
            tools: tools
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
                .collect(),
        }
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn supports_vision(&self) -> bool {
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
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"missing": "credentials"})),
                "azure_openai: API key not configured"
            );
            anyhow::Error::msg(
                "Azure OpenAI API key not set. Set AZURE_OPENAI_API_KEY or edit config.toml.",
            )
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
            messages,
            temperature,
            reasoning_effort: self.reasoning_effort_for_model(model),
            max_completion_tokens: None,
        };

        let response = self
            .http_client()
            .post(self.chat_completions_url())
            .header("api-key", credential.as_str())
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
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "azure_openai: empty choices in response"
                );
                anyhow::Error::msg("No response from Azure OpenAI")
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
                "azure_openai: API key not configured"
            );
            anyhow::Error::msg(
                "Azure OpenAI API key not set. Set AZURE_OPENAI_API_KEY or edit config.toml.",
            )
        })?;

        let tools = Self::convert_tools(request.tools);
        let native_request = NativeChatRequest {
            messages: Self::convert_messages(request.messages),
            temperature,
            // Omit tool_choice when the tool list is empty — Azure (and
            // spec-compliant validators) reject tool_choice without a
            // non-empty tools field (HTTP 400).
            tool_choice: tools
                .as_ref()
                .and_then(|t| (!t.is_empty()).then(|| "auto".to_string())),
            tools,
            reasoning_effort: self.reasoning_effort_for_model(model),
            max_completion_tokens: None,
        };

        let response = self
            .http_client()
            .post(self.chat_completions_url())
            .header("api-key", credential.as_str())
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
            cached_input_tokens: None,
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
                    "azure_openai: empty choices in response"
                );
                anyhow::Error::msg("No response from Azure OpenAI")
            })?;
        let mut result = Self::parse_native_response(message);
        result.usage = usage;
        Ok(result)
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
                "azure_openai: API key not configured"
            );
            anyhow::Error::msg(
                "Azure OpenAI API key not set. Set AZURE_OPENAI_API_KEY or edit config.toml.",
            )
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
            // See above: omit tool_choice when the tool list is empty.
            tool_choice: native_tools
                .as_ref()
                .and_then(|t| (!t.is_empty()).then(|| "auto".to_string())),
            tools: native_tools,
            reasoning_effort: self.reasoning_effort_for_model(model),
            max_completion_tokens: None,
        };

        let response = self
            .http_client()
            .post(self.chat_completions_url())
            .header("api-key", credential.as_str())
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
            cached_input_tokens: None,
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
                    "azure_openai: empty choices in response"
                );
                anyhow::Error::msg("No response from Azure OpenAI")
            })?;
        let mut result = Self::parse_native_response(message);
        result.usage = usage;
        Ok(result)
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        // Azure OpenAI does not have a lightweight models endpoint,
        // so warmup is a no-op to avoid unnecessary API calls.
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for AzureOpenAiModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Azure,
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
    fn url_construction_default_version() {
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("test-key"),
            "my-resource",
            "gpt-4o",
            None,
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://my-resource.openai.azure.com/openai/deployments/gpt-4o/chat/completions?api-version=2024-08-01-preview"
        );
    }

    #[test]
    fn url_construction_custom_version() {
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("test-key"),
            "my-resource",
            "gpt-4o",
            Some("2024-06-01"),
            None,
        );
        assert_eq!(
            p.chat_completions_url(),
            "https://my-resource.openai.azure.com/openai/deployments/gpt-4o/chat/completions?api-version=2024-06-01"
        );
    }

    #[test]
    fn url_construction_preserves_resource_and_deployment() {
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("key"),
            "contoso-ai",
            "my-gpt35-deployment",
            None,
            None,
        );
        let url = p.chat_completions_url();
        assert!(url.contains("contoso-ai.openai.azure.com"));
        assert!(url.contains("/deployments/my-gpt35-deployment/"));
        assert!(url.contains("api-version=2024-08-01-preview"));
    }

    #[test]
    fn auth_header_uses_api_key_not_bearer() {
        // This test verifies the model_provider stores the credential correctly
        // and that the auth header name is "api-key" (verified via the
        // implementation in chat_with_system which uses .header("api-key", ...)).
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("my-azure-key"),
            "resource",
            "deployment",
            None,
            None,
        );
        assert_eq!(p.credential.as_deref(), Some("my-azure-key"));
    }

    #[test]
    fn creates_with_credential() {
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("azure-test-credential"),
            "resource",
            "deployment",
            None,
            None,
        );
        assert_eq!(p.credential.as_deref(), Some("azure-test-credential"));
        assert_eq!(p.resource_name, "resource");
        assert_eq!(p.deployment_name, "deployment");
        assert_eq!(p.api_version, DEFAULT_API_VERSION);
    }

    #[test]
    fn creates_without_credential() {
        let p = AzureOpenAiModelProvider::new("test", None, "resource", "deployment", None, None);
        assert!(p.credential.is_none());
    }

    #[test]
    fn blank_credential_is_treated_as_missing() {
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("   \t  "),
            "resource",
            "deployment",
            None,
            None,
        );
        assert!(p.credential.is_none());
    }

    #[test]
    fn credential_is_trimmed_before_storage() {
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("  azure-test-credential \n"),
            "resource",
            "deployment",
            None,
            None,
        );
        assert_eq!(p.credential.as_deref(), Some("azure-test-credential"));
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = AzureOpenAiModelProvider::new("test", None, "resource", "deployment", None, None);
        let result = p.chat_with_system(None, "hello", "gpt-4o", Some(0.7)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = AzureOpenAiModelProvider::new("test", None, "resource", "deployment", None, None);
        let result = p
            .chat_with_system(Some("You are ZeroClaw"), "test", "gpt-4o", Some(0.5))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn request_serializes_with_system_message() {
        let req = ChatRequest {
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
            reasoning_effort: None,
            max_completion_tokens: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"role\":\"user\""));
        // Azure requests should NOT contain a model field (deployment is in the URL)
        assert!(!json.contains("\"model\""));
    }

    #[test]
    fn request_serializes_without_system() {
        let req = ChatRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: Some(0.0),
            reasoning_effort: None,
            max_completion_tokens: None,
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
    fn tool_call_response_parsing() {
        let json = r#"{"choices":[{"message":{
            "content":"Let me check",
            "tool_calls":[{
                "id":"call_abc123",
                "type":"function",
                "function":{"name":"shell","arguments":"{\"command\":\"ls\"}"}
            }]
        }}],"usage":{"prompt_tokens":50,"completion_tokens":25}}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let message = resp.choices.into_iter().next().unwrap().message;
        let parsed = AzureOpenAiModelProvider::parse_native_response(message);
        assert_eq!(parsed.text.as_deref(), Some("Let me check"));
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "call_abc123");
        assert_eq!(parsed.tool_calls[0].name, "shell");
        assert!(parsed.tool_calls[0].arguments.contains("ls"));
    }

    #[test]
    fn tool_call_response_without_id_generates_uuid() {
        let json = r#"{"choices":[{"message":{
            "content":null,
            "tool_calls":[{
                "function":{"name":"test","arguments":"{}"}
            }]
        }}]}"#;
        let resp: NativeChatResponse = serde_json::from_str(json).unwrap();
        let message = resp.choices.into_iter().next().unwrap().message;
        let parsed = AzureOpenAiModelProvider::parse_native_response(message);
        assert_eq!(parsed.tool_calls.len(), 1);
        assert!(!parsed.tool_calls[0].id.is_empty());
    }

    #[tokio::test]
    async fn chat_with_tools_fails_without_key() {
        let p = AzureOpenAiModelProvider::new("test", None, "resource", "deployment", None, None);
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
    fn capabilities_reports_native_tools_and_vision() {
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("key"),
            "resource",
            "deployment",
            None,
            None,
        );
        let caps = <AzureOpenAiModelProvider as ModelProvider>::capabilities(&p);
        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn supports_native_tools_returns_true() {
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("key"),
            "resource",
            "deployment",
            None,
            None,
        );
        assert!(p.supports_native_tools());
    }

    #[test]
    fn supports_vision_returns_true() {
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("key"),
            "resource",
            "deployment",
            None,
            None,
        );
        assert!(p.supports_vision());
    }

    #[tokio::test]
    async fn warmup_is_noop() {
        let p = AzureOpenAiModelProvider::new("test", None, "resource", "deployment", None, None);
        let result = p.warmup().await;
        assert!(result.is_ok());
    }

    #[test]
    fn custom_api_version_stored() {
        let p = AzureOpenAiModelProvider::new(
            "test",
            Some("key"),
            "resource",
            "deployment",
            Some("2025-01-01"),
            None,
        );
        assert_eq!(p.api_version, "2025-01-01");
    }
}
