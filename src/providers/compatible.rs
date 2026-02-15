//! Generic OpenAI-compatible provider.
//! Most LLM APIs follow the same `/v1/chat/completions` format.
//! For BYOP/custom endpoints we can also prefer `/v1/responses`
//! with compatibility retries, then fall back to chat completions.
//! This module provides a single implementation that works for all of them.

use crate::providers::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A provider that speaks the OpenAI-compatible chat completions API.
/// Used by: Venice, Vercel AI Gateway, Cloudflare AI Gateway, Moonshot,
/// Synthetic, `OpenCode` Zen, `Z.AI`, `GLM`, `MiniMax`, Bedrock, Qianfan, Groq, Mistral, `xAI`, etc.
pub struct OpenAiCompatibleProvider {
    pub(crate) name: String,
    pub(crate) base_url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) auth_header: AuthStyle,
    pub(crate) prefer_responses_api: bool,
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
            prefer_responses_api: false,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    #[must_use]
    pub fn with_responses_api_preference(mut self, prefer: bool) -> Self {
        self.prefer_responses_api = prefer;
        self
    }

    fn with_auth_headers(
        &self,
        req: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        match &self.auth_header {
            AuthStyle::Bearer => req.header("Authorization", format!("Bearer {api_key}")),
            AuthStyle::XApiKey => req.header("x-api-key", api_key),
            AuthStyle::Custom(header) => req.header(header.as_str(), api_key),
        }
    }

    async fn chat_via_chat_completions(
        &self,
        api_key: &str,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
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

        let url = format!("{}/v1/chat/completions", self.base_url);
        let req = self.with_auth_headers(self.client.post(&url).json(&request), api_key);
        let response = req.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await?;
            anyhow::bail!(
                "{} API error from /v1/chat/completions ({}): {error}",
                self.name,
                status
            );
        }

        let chat_response: ChatResponse = response.json().await?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("No response from {}", self.name))
    }

    async fn chat_via_responses(
        &self,
        api_key: &str,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let mut input = Vec::with_capacity(2);
        if let Some(sys) = system_prompt {
            input.push(ResponsesInputItem {
                role: "system".to_string(),
                content: ResponsesContent::Text(sys.to_string()),
            });
        }
        input.push(ResponsesInputItem {
            role: "user".to_string(),
            content: ResponsesContent::Text(message.to_string()),
        });

        let request = ResponsesRequest {
            model: model.to_string(),
            input,
            instructions: None,
            temperature,
        };

        let url = format!("{}/v1/responses", self.base_url);
        let req = self.with_auth_headers(self.client.post(&url).json(&request), api_key);
        let response = req.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await?;
            anyhow::bail!(
                "{} API error from /v1/responses ({}): {error}",
                self.name,
                status
            );
        }

        let body: Value = response.json().await?;
        extract_text_from_responses_json(&body)
            .ok_or_else(|| anyhow::anyhow!("No text output from {} Responses API", self.name))
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

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<ResponsesInputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ResponsesInputItem {
    role: String,
    content: ResponsesContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ResponsesContent {
    Text(String),
    Rich(Vec<ResponsesContentPart>),
}

#[derive(Debug, Serialize)]
struct ResponsesContentPart {
    #[serde(rename = "type")]
    kind: String,
    text: String,
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
    content: String,
}

fn extract_text_from_responses_json(body: &Value) -> Option<String> {
    if let Some(text) = body.get("output_text").and_then(Value::as_str) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let mut chunks: Vec<String> = Vec::new();

    if let Some(output_items) = body.get("output").and_then(Value::as_array) {
        for item in output_items {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    chunks.push(trimmed.to_string());
                }
            }

            if let Some(content_parts) = item.get("content").and_then(Value::as_array) {
                for part in content_parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            chunks.push(trimmed.to_string());
                        }
                    }

                    if let Some(text) = part.get("output_text").and_then(Value::as_str) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            chunks.push(trimmed.to_string());
                        }
                    }
                }
            }
        }
    }

    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join("\n"))
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
    ) -> anyhow::Result<String> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} API key not set. Run `zeroclaw onboard` or set the appropriate env var.",
                self.name
            )
        })?;

        if self.prefer_responses_api {
            let first_responses_error = match self
                .chat_via_responses(api_key, system_prompt, message, model, Some(temperature))
                .await
            {
                Ok(text) => return Ok(text),
                Err(err) => err,
            };

            let second_responses_error = match self
                .chat_via_responses(api_key, system_prompt, message, model, None)
                .await
            {
                Ok(text) => return Ok(text),
                Err(err) => err,
            };

            return match self
                .chat_via_chat_completions(api_key, system_prompt, message, model, temperature)
                .await
            {
                Ok(text) => Ok(text),
                Err(chat_error) => Err(anyhow::anyhow!(
                    "{} API error via /v1/responses with temperature ({first_responses_error}), /v1/responses without temperature ({second_responses_error}), and /v1/chat/completions ({chat_error})",
                    self.name
                )),
            };
        }

        self.chat_via_chat_completions(api_key, system_prompt, message, model, temperature)
            .await
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
        assert!(!p.prefer_responses_api);
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
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].message.content, "Hello from Venice!");
    }

    #[test]
    fn response_empty_choices() {
        let json = r#"{"choices":[]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.choices.is_empty());
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

    #[test]
    fn responses_preference_can_be_enabled() {
        let p = OpenAiCompatibleProvider::new(
            "custom",
            "https://api.example.com",
            Some("key"),
            AuthStyle::Bearer,
        )
        .with_responses_api_preference(true);
        assert!(p.prefer_responses_api);
    }

    #[test]
    fn responses_request_serializes_instructions() {
        let req = ResponsesRequest {
            model: "gpt-4.1-mini".to_string(),
            input: vec![ResponsesInputItem {
                role: "user".to_string(),
                content: ResponsesContent::Text("hello".to_string()),
            }],
            instructions: None,
            temperature: Some(0.2),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"model\":\"gpt-4.1-mini\""));
        assert!(json.contains("\"input\":[{\"role\":\"user\",\"content\":\"hello\"}]"));
        assert!(json.contains("\"temperature\":0.2"));
    }

    #[test]
    fn responses_request_omits_temperature_when_none() {
        let req = ResponsesRequest {
            model: "gpt-4.1-mini".to_string(),
            input: vec![ResponsesInputItem {
                role: "user".to_string(),
                content: ResponsesContent::Text("hello".to_string()),
            }],
            instructions: None,
            temperature: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("temperature"));
    }

    #[test]
    fn responses_content_rich_serializes() {
        let req = ResponsesRequest {
            model: "gpt-4.1-mini".to_string(),
            input: vec![ResponsesInputItem {
                role: "user".to_string(),
                content: ResponsesContent::Rich(vec![ResponsesContentPart {
                    kind: "input_text".to_string(),
                    text: "hello".to_string(),
                }]),
            }],
            instructions: None,
            temperature: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"input_text\""));
    }

    #[test]
    fn responses_text_extractor_prefers_output_text() {
        let body = serde_json::json!({
            "output_text": "Direct text",
            "output": [{
                "content": [{"type": "output_text", "text": "Nested text"}]
            }]
        });
        let extracted = extract_text_from_responses_json(&body);
        assert_eq!(extracted.as_deref(), Some("Direct text"));
    }

    #[test]
    fn responses_text_extractor_reads_nested_content() {
        let body = serde_json::json!({
            "output": [{
                "type": "message",
                "content": [
                    {"type": "output_text", "text": "First line"},
                    {"type": "output_text", "text": "Second line"}
                ]
            }]
        });
        let extracted = extract_text_from_responses_json(&body);
        assert_eq!(extracted.as_deref(), Some("First line\nSecond line"));
    }

    #[test]
    fn responses_text_extractor_returns_none_when_missing() {
        let body = serde_json::json!({
            "output": [{"type": "message", "content": [{"type": "tool_call"}]}]
        });
        assert!(extract_text_from_responses_json(&body).is_none());
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
}
