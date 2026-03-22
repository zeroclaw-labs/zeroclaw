//! Proxy provider — routes LLM calls through a remote `/api/llm/proxy` endpoint.
//!
//! **Security**: The actual LLM API key never leaves the proxy server.
//! This provider sends a short-lived proxy token with each request, and the
//! server injects the operator's API key server-side before forwarding to
//! the actual LLM provider.
//!
//! Used in hybrid relay mode: local device executes tools locally (with its
//! own tool API keys) while LLM calls go through Railway's proxy endpoint.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::traits::{ChatMessage, ChatResponse, Provider, TokenUsage, ToolCall};

/// Provider that routes LLM requests through a remote proxy endpoint.
///
/// The proxy server holds the actual LLM API keys; this provider only
/// holds a short-lived token that authorizes proxy access.
pub struct ProxyProvider {
    /// Full URL to the LLM proxy endpoint (e.g., "https://moa.up.railway.app/api/llm/proxy")
    proxy_url: String,
    /// Short-lived authorization token for the proxy
    proxy_token: String,
    /// The underlying LLM provider name (e.g., "anthropic", "gemini")
    provider_name: String,
    /// HTTP client
    client: reqwest::Client,
}

/// Request body sent to the proxy endpoint.
#[derive(Debug, Serialize)]
struct ProxyRequest<'a> {
    provider: &'a str,
    model: &'a str,
    messages: &'a [ProxyMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_prompt: Option<&'a str>,
    /// Serialized tool schemas for tool calling support
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [serde_json::Value]>,
}

/// Message format for the proxy endpoint.
#[derive(Debug, Serialize, Deserialize, Clone)]
struct ProxyMessage {
    role: String,
    content: String,
}

/// Response from the proxy endpoint.
#[derive(Debug, Deserialize)]
struct ProxyResponse {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ProxyToolCall>,
    #[serde(default)]
    usage: Option<ProxyUsage>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProxyToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ProxyUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

impl ProxyProvider {
    /// Create a new proxy provider.
    ///
    /// # Arguments
    /// - `proxy_url`: Full URL to the LLM proxy endpoint
    /// - `proxy_token`: Short-lived authorization token
    /// - `provider_name`: The underlying LLM provider (e.g., "anthropic")
    pub fn new(proxy_url: String, proxy_token: String, provider_name: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();

        Self {
            proxy_url,
            proxy_token,
            provider_name,
            client,
        }
    }

    /// Send a request to the proxy endpoint.
    async fn proxy_chat(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
        system_prompt: Option<&str>,
        tools: Option<&[serde_json::Value]>,
    ) -> Result<ChatResponse> {
        let proxy_messages: Vec<ProxyMessage> = messages
            .iter()
            .map(|m| ProxyMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        let request_body = ProxyRequest {
            provider: &self.provider_name,
            model,
            messages: &proxy_messages,
            temperature: Some(temperature),
            max_tokens: None,
            system_prompt,
            tools,
        };

        let response = self
            .client
            .post(&self.proxy_url)
            .header("Authorization", format!("Bearer {}", self.proxy_token))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Proxy request failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            match status.as_u16() {
                401 => bail!("Proxy token expired or invalid. Please reconnect."),
                402 => bail!("Insufficient credits. Please add credits to continue."),
                503 => bail!(
                    "No operator key configured for provider '{}'. Please contact the operator.",
                    self.provider_name
                ),
                _ => bail!("Proxy error ({}): {}", status, error_text),
            }
        }

        let proxy_resp: ProxyResponse = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse proxy response: {e}"))?;

        if let Some(error) = proxy_resp.error {
            bail!("Proxy LLM error: {error}");
        }

        let tool_calls: Vec<ToolCall> = proxy_resp
            .tool_calls
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                name: tc.name,
                arguments: tc.arguments,
            })
            .collect();

        let usage = proxy_resp.usage.map(|u| TokenUsage {
            input_tokens: u.input_tokens.map(|v| v as u64),
            output_tokens: u.output_tokens.map(|v| v as u64),
        });

        Ok(ChatResponse {
            text: proxy_resp.content,
            tool_calls,
            usage,
            reasoning_content: None,
            quota_metadata: None,
        })
    }
}

#[async_trait::async_trait]
impl Provider for ProxyProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> Result<String> {
        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(ChatMessage::system(sys));
        }
        messages.push(ChatMessage::user(message));

        let resp = self
            .proxy_chat(&messages, model, temperature, None, None)
            .await?;
        Ok(resp.text.unwrap_or_default())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> Result<String> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        let resp = self
            .proxy_chat(messages, model, temperature, system, None)
            .await?;
        Ok(resp.text.unwrap_or_default())
    }

    async fn chat(
        &self,
        request: super::traits::ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        // Convert ToolSpec to serde_json::Value for the proxy
        let tools_json: Option<Vec<serde_json::Value>> = request.tools.map(|specs| {
            specs
                .iter()
                .map(|spec| serde_json::to_value(spec).unwrap_or_default())
                .collect()
        });

        let system = request
            .messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        self.proxy_chat(
            request.messages,
            model,
            temperature,
            system,
            tools_json.as_deref(),
        )
        .await
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        self.proxy_chat(messages, model, temperature, system, Some(tools))
            .await
    }

    fn supports_native_tools(&self) -> bool {
        // The proxy server uses the underlying provider which may support native tools.
        // We report true so the agent loop sends tool schemas to the proxy.
        true
    }

    async fn warmup(&self) -> Result<()> {
        // No warmup needed — the proxy server handles connection pooling.
        Ok(())
    }
}
