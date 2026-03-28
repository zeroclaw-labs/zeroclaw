// src/providers/litellm.rs

use crate::providers::traits::{ChatRequest, ChatResponse, Provider, TokenUsage};
use crate::tools::ToolSpec;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

#[derive(Debug, Serialize, Deserialize)]
struct LiteLLMChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f64,
    max_tokens: u32,
    tools: Option<Vec<Tool>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Tool {
    type_: String,
    function: Function,
}

#[derive(Debug, Serialize, Deserialize)]
struct Function {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct LiteLLMChatResponse {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Choice {
    index: u32,
    message: Message,
    finish_reason: Option<String>,
    delta: Option<Delta>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Delta {
    content: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

pub struct LiteLLMProvider {
    client: Arc<Client>,
    base_url: String,
    api_key: Option<String>,
    model: String,
    temperature: f64,
}

impl LiteLLMProvider {
    pub fn new(
        base_url: String,
        api_key: Option<String>,
        model: String,
        temperature: f64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let client = Client::new();
        
        Ok(Self {
            client: Arc::new(client),
            base_url,
            api_key,
            model,
            temperature,
        })
    }
}

#[async_trait]
impl Provider for LiteLLMProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let request = LiteLLMChatRequest {
            model: model.to_string(),
            messages: vec![
                Message { 
                    role: "system".to_string(), 
                    content: system_prompt.unwrap_or_default().to_string() 
                },
                Message { role: "user".to_string(), content: message.to_string() },
            ],
            temperature,
            max_tokens: 4096,
            tools: None,
        };

        let url = format!("{}v1/chat/completions", self.base_url);
        
        debug!("Sending request to LiteLLM: {}", url);
        
        let response = self.client
            .post(&url)
            .json(&request)
            .send()
            .await?;

        if response.status().is_success() {
            let response_data: LiteLLMChatResponse = response.json().await?;
            
            let text = response_data.choices.get(0)
                .and_then(|choice| choice.message.content.clone())
                .unwrap_or_default();

            info!("Successfully received response from LiteLLM");
            
            Ok(text)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            
            error!("LiteLLM request failed with status {}: {}", status, text);
            
            Err(format!("LiteLLM request failed with status {}: {}", status, text).into())
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: false,
            vision: false,
        }
    }

    fn supports_native_tools(&self) -> bool {
        false
    }

    fn supports_vision(&self) -> bool {
        false
    }

    async fn health_check(&self) -> Result<(), Box<dyn std::error::Error>> {
        let url = format!("{}v1/models", self.base_url);
        
        let response = self.client
            .get(&url)
            .send()
            .await?;

        if response.status().is_success() {
            info!("LiteLLM health check successful");
            Ok(())
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            
            error!("LiteLLM health check failed with status {}: {}", status, text);
            
            Err(format!("LiteLLM health check failed with status {}: {}", status, text).into())
        }
    }

    async fn get_token_usage(&self) -> Result<TokenUsage, Box<dyn std::error::Error>> {
        // LiteLLM doesn't expose token usage directly, return placeholder
        Ok(TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            estimated_cost: 0.0,
        })
    }

    fn name(&self) -> &str {
        "litellm"
    }
}

impl Drop for LiteLLMProvider {
    fn drop(&mut self) {
        info!("LiteLLM provider dropped");
    }
}