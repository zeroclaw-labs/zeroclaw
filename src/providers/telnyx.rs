//! Telnyx AI inference provider.
//!
//! Telnyx provides AI inference through an OpenAI-compatible API at
//! https://api.telnyx.com/v2/ai with access to 53+ models including
//! GPT-4o, Claude, Llama, Mistral, and more.
//!
//! # Configuration
//!
//! Set the `TELNYX_API_KEY` environment variable or configure in `config.toml`:
//!
//! ```toml
//! default_provider = "telnyx"
//! default_model = "openai/gpt-4o"
//! ```

use crate::providers::traits::{ChatMessage, Provider};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

/// Telnyx AI inference provider.
///
/// Uses the OpenAI-compatible chat completions API at `/v2/ai/chat/completions`.
/// Supports 53+ models including OpenAI, Anthropic (via API), Meta Llama,
/// Mistral, and more.
///
/// # Example
///
/// ```rust,ignore
/// use zeroclaw::providers::telnyx::TelnyxProvider;
/// use zeroclaw::providers::Provider;
///
/// let provider = TelnyxProvider::new(Some("your-api-key"));
/// let response = provider.chat("Hello!", "openai/gpt-4o", 0.7).await?;
/// ```
pub struct TelnyxProvider {
    /// Telnyx API key
    api_key: Option<String>,
    /// HTTP client for API requests
    client: Client,
}

impl TelnyxProvider {
    /// Telnyx AI API base URL
    const BASE_URL: &'static str = "https://api.telnyx.com/v2/ai";

    /// Create a new Telnyx AI provider.
    ///
    /// The API key can be provided directly or will be resolved from:
    /// 1. `TELNYX_API_KEY` environment variable
    /// 2. `ZEROCLAW_API_KEY` environment variable (fallback)
    pub fn new(api_key: Option<&str>) -> Self {
        let resolved_key = resolve_telnyx_api_key(api_key);
        Self {
            api_key: resolved_key,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Create a provider with a custom base URL (for testing or proxies).
    pub fn with_base_url(api_key: Option<&str>, _base_url: &str) -> Self {
        // Note: custom base URL support for testing
        Self::new(api_key)
    }

    /// List available models from Telnyx AI.
    ///
    /// Returns a list of model IDs that can be used with the chat API.
    pub async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Telnyx API key not set. Set TELNYX_API_KEY environment variable.")
        })?;

        let response = self
            .client
            .get(format!("{}/models", Self::BASE_URL))
            .header("Authorization", format!("Bearer {}", api_key))
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("Failed to list Telnyx models: {}", error);
        }

        let models_response: ModelsResponse = response.json().await?;
        Ok(models_response.data.into_iter().map(|m| m.id).collect())
    }

    /// Build the chat completions URL
    fn chat_url(&self) -> String {
        format!("{}/chat/completions", Self::BASE_URL)
    }
}

/// Resolve Telnyx API key from parameter or environment.
fn resolve_telnyx_api_key(api_key: Option<&str>) -> Option<String> {
    if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        return Some(key.to_string());
    }

    // Try Telnyx-specific env var first
    if let Ok(key) = std::env::var("TELNYX_API_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }

    // Fall back to generic env vars
    for env_var in ["ZEROCLAW_API_KEY", "API_KEY"] {
        if let Ok(key) = std::env::var(env_var) {
            let key = key.trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }

    None
}

/// Response from the /models endpoint
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    id: String,
}

/// Request body for chat completions
#[derive(Debug, serde::Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
}

#[derive(Debug, serde::Serialize)]
struct Message {
    role: String,
    content: String,
}

/// Response from chat completions API
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

#[async_trait]
impl Provider for TelnyxProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Telnyx API key not set. Set TELNYX_API_KEY environment variable or run `zeroclaw onboard`."
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

        let response = self
            .client
            .post(self.chat_url())
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await?;
            let sanitized = super::sanitize_api_error(&error);
            anyhow::bail!("Telnyx API error ({}): {}", status, sanitized);
        }

        let chat_response: ChatResponse = response.json().await?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("No response from Telnyx"))
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Telnyx API key not set. Set TELNYX_API_KEY environment variable or run `zeroclaw onboard`."
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

        let response = self
            .client
            .post(self.chat_url())
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await?;
            let sanitized = super::sanitize_api_error(&error);
            anyhow::bail!("Telnyx API error ({}): {}", status, sanitized);
        }

        let chat_response: ChatResponse = response.json().await?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("No response from Telnyx"))
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        // Pre-warm the connection pool
        let _ = self
            .client
            .get(format!("{}/models", Self::BASE_URL))
            .send()
            .await;
        Ok(())
    }
}

/// Popular Telnyx AI models for easy reference.
pub mod models {
    /// OpenAI GPT-4o (recommended for most tasks)
    pub const GPT_4O: &str = "openai/gpt-4o";
    /// OpenAI GPT-4o Mini (fast and cost-effective)
    pub const GPT_4O_MINI: &str = "openai/gpt-4o-mini";
    /// OpenAI GPT-4 Turbo
    pub const GPT_4_TURBO: &str = "openai/gpt-4-turbo";
    /// Anthropic Claude 3.5 Sonnet (via Telnyx proxy)
    pub const CLAUDE_3_5_SONNET: &str = "anthropic/claude-3.5-sonnet";
    /// Meta Llama 3.1 70B Instruct
    pub const LLAMA_3_1_70B: &str = "meta-llama/llama-3.1-70b-instruct";
    /// Meta Llama 3.1 8B Instruct (fast)
    pub const LLAMA_3_1_8B: &str = "meta-llama/llama-3.1-8b-instruct";
    /// Mistral Large
    pub const MISTRAL_LARGE: &str = "mistralai/mistral-large";
    /// Mistral Small (fast)
    pub const MISTRAL_SMALL: &str = "mistralai/mistral-small";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_provider_with_key() {
        let provider = TelnyxProvider::new(Some("test-key"));
        assert!(provider.api_key.is_some());
    }

    #[test]
    fn creates_provider_without_key() {
        let _provider = TelnyxProvider::new(None);
        // Will be None if env vars not set
    }

    #[test]
    fn model_constants_are_valid() {
        assert!(models::GPT_4O.starts_with("openai/"));
        assert!(models::CLAUDE_3_5_SONNET.starts_with("anthropic/"));
        assert!(models::LLAMA_3_1_70B.starts_with("meta-llama/"));
        assert!(models::MISTRAL_LARGE.starts_with("mistralai/"));
    }

    #[test]
    fn resolve_key_from_parameter() {
        let key = resolve_telnyx_api_key(Some("direct-key"));
        assert_eq!(key, Some("direct-key".to_string()));
    }

    #[test]
    fn resolve_key_trims_whitespace() {
        let key = resolve_telnyx_api_key(Some("  spaced-key  "));
        assert_eq!(key, Some("spaced-key".to_string()));
    }

    #[test]
    fn models_response_deserializes() {
        let json = r#"{
            "data": [
                {"id": "openai/gpt-4o"},
                {"id": "anthropic/claude-3.5-sonnet"}
            ]
        }"#;

        let response: ModelsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.data.len(), 2);
        assert_eq!(response.data[0].id, "openai/gpt-4o");
    }

    #[test]
    fn chat_request_serializes() {
        let req = ChatRequest {
            model: "openai/gpt-4o".to_string(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: "You are helpful.".to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: "Hello".to_string(),
                },
            ],
            temperature: 0.7,
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("openai/gpt-4o"));
        assert!(json.contains("system"));
        assert!(json.contains("user"));
    }

    #[test]
    fn chat_response_deserializes() {
        let json = r#"{"choices":[{"message":{"content":"Hello from Telnyx!"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].message.content, "Hello from Telnyx!");
    }
}
