use crate::providers::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Default Anthropic API base URL
const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com";

pub struct AnthropicProvider {
    api_key: Option<String>,
    client: Client,
    base_url: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<Message>,
    temperature: f64,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    text: String,
}

impl AnthropicProvider {
    /// Build a configured HTTP client
    fn build_client() -> Client {
        Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new())
    }

    /// Create provider for official Anthropic API
    pub fn new(api_key: Option<&str>) -> Self {
        Self::new_with_url(api_key, ANTHROPIC_API_BASE)
    }

    /// Create provider with custom base URL (for Anthropic-compatible APIs like Kimi Coding)
    pub fn new_with_url(api_key: Option<&str>, base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(ToString::to_string),
            client: Self::build_client(),
        }
    }

    /// Get provider display name based on base URL
    fn provider_name(&self) -> &'static str {
        if self.base_url == ANTHROPIC_API_BASE {
            "Anthropic"
        } else {
            "Kimi Coding"
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let provider_name = self.provider_name();

        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{provider_name} API key not set. Set {} or edit config.toml.",
                if provider_name == "Anthropic" {
                    "ANTHROPIC_API_KEY"
                } else {
                    "KIMI_API_KEY"
                }
            )
        })?;

        let request = ChatRequest {
            model: model.to_string(),
            max_tokens: 4096,
            system: system_prompt.map(ToString::to_string),
            messages: vec![Message {
                role: "user".to_string(),
                content: message.to_string(),
            }],
            temperature,
        };

        let endpoint = format!("{}/v1/messages", self.base_url);
        let response = self
            .client
            .post(&endpoint)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("{provider_name} API error: {error}");
        }

        let chat_response: ChatResponse = response.json().await?;

        chat_response
            .content
            .into_iter()
            .next()
            .map(|c| c.text)
            .ok_or_else(|| anyhow::anyhow!("No response from {provider_name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_key() {
        let p = AnthropicProvider::new(Some("sk-ant-test123"));
        assert!(p.api_key.is_some());
        assert_eq!(p.api_key.as_deref(), Some("sk-ant-test123"));
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn creates_with_custom_url() {
        let p = AnthropicProvider::new_with_url(Some("kimi-key"), "https://api.kimi.com/coding");
        assert_eq!(p.api_key.as_deref(), Some("kimi-key"));
        assert_eq!(p.base_url, "https://api.kimi.com/coding");
    }

    #[test]
    fn creates_without_key() {
        let p = AnthropicProvider::new(None);
        assert!(p.api_key.is_none());
    }

    #[test]
    fn creates_with_empty_key() {
        let p = AnthropicProvider::new(Some(""));
        assert!(p.api_key.is_some());
        assert_eq!(p.api_key.as_deref(), Some(""));
    }

    #[test]
    fn provider_name_is_anthropic_for_default() {
        let p = AnthropicProvider::new(Some("key"));
        assert_eq!(p.provider_name(), "Anthropic");
    }

    #[test]
    fn provider_name_is_kimi_for_custom_url() {
        let p = AnthropicProvider::new_with_url(Some("key"), "https://api.kimi.com/coding/v1");
        assert_eq!(p.provider_name(), "Kimi Coding");
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let result = p
            .chat_with_system(None, "hello", "claude-3-opus", 0.7)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("API key not set"),
            "Expected key error, got: {err}"
        );
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let result = p
            .chat_with_system(Some("You are ZeroClaw"), "hello", "claude-3-opus", 0.7)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn kimi_coding_fails_without_key() {
        let p = AnthropicProvider::new_with_url(None, "https://api.kimi.com/coding/v1");
        let result = p.chat_with_system(None, "hello", "k2p5", 0.7).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Kimi Coding API key not set"),
            "Expected Kimi key error, got: {err}"
        );
    }

    #[test]
    fn chat_request_serializes_without_system() {
        let req = ChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: None,
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("system"),
            "system field should be skipped when None"
        );
        assert!(json.contains("claude-3-opus"));
        assert!(json.contains("hello"));
    }

    #[test]
    fn chat_request_serializes_with_system() {
        let req = ChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: Some("You are ZeroClaw".to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"system\":\"You are ZeroClaw\""));
    }

    #[test]
    fn chat_response_deserializes() {
        let json = r#"{"content":[{"type":"text","text":"Hello there!"}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 1);
        assert_eq!(resp.content[0].text, "Hello there!");
    }

    #[test]
    fn chat_response_empty_content() {
        let json = r#"{"content":[]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.content.is_empty());
    }

    #[test]
    fn chat_response_multiple_blocks() {
        let json =
            r#"{"content":[{"type":"text","text":"First"},{"type":"text","text":"Second"}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert_eq!(resp.content[0].text, "First");
        assert_eq!(resp.content[1].text, "Second");
    }

    #[test]
    fn temperature_range_serializes() {
        for temp in [0.0, 0.5, 1.0, 2.0] {
            let req = ChatRequest {
                model: "claude-3-opus".to_string(),
                max_tokens: 4096,
                system: None,
                messages: vec![],
                temperature: temp,
            };
            let json = serde_json::to_string(&req).unwrap();
            assert!(json.contains(&format!("{temp}")));
        }
    }
}
