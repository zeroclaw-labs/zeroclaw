//! Generic OpenAI-compatible provider.
//! Most LLM APIs follow the same `/v1/chat/completions` format.
//! This module provides a single implementation that works for all of them.

use crate::providers::traits::{ChatMessage, ChatResponse, Provider, ToolCall};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// A provider that speaks the OpenAI-compatible chat completions API.
/// Used by: Venice, Vercel AI Gateway, Cloudflare AI Gateway, Moonshot,
/// Synthetic, `OpenCode` Zen, `Z.AI`, `GLM`, `MiniMax`, Bedrock, Qianfan, Groq, Mistral, `xAI`, etc.
pub struct OpenAiCompatibleProvider {
    pub(crate) name: String,
    pub(crate) base_url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) auth_header: AuthStyle,
    client: Client,
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

impl OpenAiCompatibleProvider {
    pub fn new(name: &str, base_url: &str, api_key: Option<&str>, auth_style: AuthStyle) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(ToString::to_string),
            auth_header: auth_style,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
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
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ApiChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ApiToolCall>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ApiToolCall {
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    function: Option<Function>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Function {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<ResponsesInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ResponsesInput {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output: Vec<ResponsesOutput>,
    #[serde(default)]
    output_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponsesOutput {
    #[serde(default)]
    content: Vec<ResponsesContent>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContent {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
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

fn extract_responses_text(response: ResponsesResponse) -> Option<String> {
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

fn map_response_message(message: ResponseMessage) -> ChatResponse {
    let text = first_nonempty(message.content.as_deref());
    let tool_calls = message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .filter_map(|(index, call)| map_api_tool_call(call, index))
        .collect();

    ChatResponse { text, tool_calls }
}

fn map_api_tool_call(call: ApiToolCall, index: usize) -> Option<ToolCall> {
    if call.kind.as_deref().is_some_and(|kind| kind != "function") {
        return None;
    }

    let function = call.function?;
    let name = function
        .name
        .and_then(|value| first_nonempty(Some(value.as_str())))?;
    let arguments = function
        .arguments
        .and_then(|value| first_nonempty(Some(value.as_str())))
        .unwrap_or_else(|| "{}".to_string());
    let id = call
        .id
        .and_then(|value| first_nonempty(Some(value.as_str())))
        .unwrap_or_else(|| format!("call_{}", index + 1));

    Some(ToolCall {
        id,
        name,
        arguments,
    })
}

impl OpenAiCompatibleProvider {
    fn apply_auth_header(
        &self,
        req: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        match &self.auth_header {
            AuthStyle::Bearer => req.header("Authorization", format!("Bearer {api_key}")),
            AuthStyle::XApiKey => req.header("x-api-key", api_key),
            AuthStyle::Custom(header) => req.header(header, api_key),
        }
    }

    async fn chat_via_responses(
        &self,
        api_key: &str,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
    ) -> anyhow::Result<ChatResponse> {
        let request = ResponsesRequest {
            model: model.to_string(),
            input: vec![ResponsesInput {
                role: "user".to_string(),
                content: message.to_string(),
            }],
            instructions: system_prompt.map(str::to_string),
            stream: Some(false),
        };

        let url = self.responses_url();

        let response = self
            .apply_auth_header(self.client.post(&url).json(&request), api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("{} Responses API error: {error}", self.name);
        }

        let responses: ResponsesResponse = response.json().await?;

        extract_responses_text(responses)
            .map(ChatResponse::with_text)
            .ok_or_else(|| anyhow::anyhow!("No response from {} Responses API", self.name))
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} API key not set. Run `zeroclaw onboard` or set the appropriate env var.",
                self.name
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
            model: model.to_string(),
            messages,
            temperature,
        };

        let url = self.chat_completions_url();

        let response = self
            .apply_auth_header(self.client.post(&url).json(&request), api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await?;
            let sanitized = super::sanitize_api_error(&error);

            if status == reqwest::StatusCode::NOT_FOUND {
                return self
                    .chat_via_responses(api_key, system_prompt, message, model)
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

        let chat_response: ApiChatResponse = response.json().await?;

        let choice = chat_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No response from {}", self.name))?;

        Ok(map_response_message(choice.message))
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} API key not set. Run `zeroclaw onboard` or set the appropriate env var.",
                self.name
            )
        })?;

        let api_messages: Vec<Message> = messages
            .iter()
            .map(|m| Message {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        let request = ChatRequest {
            model: model.to_string(),
            messages: api_messages,
            temperature,
        };

        let url = self.chat_completions_url();
        let response = self
            .apply_auth_header(self.client.post(&url).json(&request), api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();

            // Mirror chat_with_system: 404 may mean this provider uses the Responses API
            if status == reqwest::StatusCode::NOT_FOUND {
                // Extract system prompt and last user message for responses fallback
                let system = messages.iter().find(|m| m.role == "system");
                let last_user = messages.iter().rfind(|m| m.role == "user");
                if let Some(user_msg) = last_user {
                    return self
                        .chat_via_responses(
                            api_key,
                            system.map(|m| m.content.as_str()),
                            &user_msg.content,
                            model,
                        )
                        .await
                        .map_err(|responses_err| {
                            anyhow::anyhow!(
                                "{} API error (chat completions unavailable; responses fallback failed: {responses_err})",
                                self.name
                            )
                        });
                }
            }

            return Err(super::api_error(&self.name, response).await);
        }

        let chat_response: ApiChatResponse = response.json().await?;

        let choice = chat_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No response from {}", self.name))?;

        Ok(map_response_message(choice.message))
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
        let p = make_provider("venice", "https://api.venice.ai", Some("vn-key"));
        assert_eq!(p.name, "venice");
        assert_eq!(p.base_url, "https://api.venice.ai");
        assert_eq!(p.api_key.as_deref(), Some("vn-key"));
    }

    #[test]
    fn creates_without_key() {
        let p = make_provider("test", "https://example.com", None);
        assert!(p.api_key.is_none());
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
        let req = ChatRequest {
            model: "llama-3.3-70b".to_string(),
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
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("llama-3.3-70b"));
        assert!(json.contains("system"));
        assert!(json.contains("user"));
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
    fn response_with_tool_calls_maps_structured_data() {
        let json = r#"{"choices":[{"message":{"content":"Running checks","tool_calls":[{"id":"call_1","type":"function","function":{"name":"shell","arguments":"{\"command\":\"pwd\"}"}}]}}]}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        let choice = resp.choices.into_iter().next().unwrap();

        let mapped = map_response_message(choice.message);
        assert_eq!(mapped.text.as_deref(), Some("Running checks"));
        assert_eq!(mapped.tool_calls.len(), 1);
        assert_eq!(mapped.tool_calls[0].id, "call_1");
        assert_eq!(mapped.tool_calls[0].name, "shell");
        assert_eq!(mapped.tool_calls[0].arguments, r#"{"command":"pwd"}"#);
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

    #[tokio::test]
    async fn all_compatible_providers_fail_without_key() {
        let providers = vec![
            make_provider("Venice", "https://api.venice.ai", None),
            make_provider("Moonshot", "https://api.moonshot.cn", None),
            make_provider("GLM", "https://open.bigmodel.cn", None),
            make_provider("MiniMax", "https://api.minimax.chat", None),
            make_provider("Groq", "https://api.groq.com/openai", None),
            make_provider("Mistral", "https://api.mistral.ai", None),
            make_provider("xAI", "https://api.x.ai", None),
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
            extract_responses_text(response).as_deref(),
            Some("Hello from top-level")
        );
    }

    #[test]
    fn responses_extracts_nested_output_text() {
        let json =
            r#"{"output":[{"content":[{"type":"output_text","text":"Hello from nested"}]}]}"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            extract_responses_text(response).as_deref(),
            Some("Hello from nested")
        );
    }

    #[test]
    fn responses_extracts_any_text_as_fallback() {
        let json = r#"{"output":[{"content":[{"type":"message","text":"Fallback text"}]}]}"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            extract_responses_text(response).as_deref(),
            Some("Fallback text")
        );
    }

    // ══════════════════════════════════════════════════════════
    // Custom endpoint path tests (Issue #114)
    // ══════════════════════════════════════════════════════════

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

    // ══════════════════════════════════════════════════════════
    // Provider-specific endpoint tests (Issue #167)
    // ══════════════════════════════════════════════════════════

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
}
