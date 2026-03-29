//! Example: Implementing a custom Provider for ZeroClaw
//!
//! This shows how to add a new LLM backend in ~30 lines of code.
//! Copy this file, modify the API call, and register in `src/providers/mod.rs`.

use anyhow::Result;
use async_trait::async_trait;

// In a real implementation, you'd import from the crate:
// use zeroclaw::providers::traits::Provider;

/// Minimal Provider trait (mirrors src/providers/traits.rs)
#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, message: &str, model: &str, temperature: f64) -> Result<String>;
}

/// Example: Ollama local provider
pub struct OllamaProvider {
    base_url: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            base_url: base_url.unwrap_or("http://localhost:11434").to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    async fn chat(&self, message: &str, model: &str, temperature: f64) -> Result<String> {
        let url = format!("{}/api/generate", self.base_url);

        let body = serde_json::json!({
            "model": model,
            "prompt": message,
            "temperature": temperature,
            "stream": false,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        resp["response"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("No response field in Ollama reply"))
    }
}

fn main() {
    println!("This is an example — see CONTRIBUTING.md for integration steps.");
    println!("Register your provider in src/providers/mod.rs:");
    println!("  \"ollama\" => Ok(Box::new(ollama::OllamaProvider::new(None))),");
}
