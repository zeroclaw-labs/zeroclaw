use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, Provider, ProviderCapabilities, StreamEvent,
};

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone)]
pub struct AtomicChatProvider {
    client: Client,
    endpoint: String,
    api_key: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    delta: Delta,
}

#[derive(Debug, Deserialize)]
struct Delta {
    content: Option<String>,
}

impl AtomicChatProvider {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("failed to build reqwest client");

        let endpoint = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));

        Self {
            client,
            endpoint,
            api_key,
        }
    }

    fn extract_content(data: &str) -> Option<String> {
        serde_json::from_str::<StreamChunk>(data)
            .ok()?
            .choices
            .first()?
            .delta
            .content
            .clone()
    }

    fn build_request<'a>(&'a self, req: &'a ProviderChatRequest) -> ChatCompletionRequest<'a> {
        ChatCompletionRequest {
            model: &req.model,
            messages: req
                .messages
                .iter()
                .map(|m| Message {
                    role: m.role.as_str(),
                    content: &m.content,
                })
                .collect(),
            temperature: req.temperature,
            stream: true,
        }
    }
}

#[async_trait]
impl Provider for AtomicChatProvider {
    async fn stream_chat(
        &self,
        req: ProviderChatRequest,
        mut tx: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let body = self.build_request(&req);

        let mut request = self.client.post(&self.endpoint).json(&body);

        if let Some(key) = &self.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            let err = response.text().await.unwrap_or_default();
            let _ = tx.send(StreamEvent::Error(err)).await;
            return Ok(());
        }

        // ============================
        // STREAMING (FIXED SSE PARSER)
        // ============================

        let mut buffer = String::new();
        let mut stream = response.bytes_stream();

        use futures_util::StreamExt;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // SSE can contain multiple lines per chunk
            let mut lines = buffer.split("\n").collect::<Vec<_>>();

            // keep last partial line in buffer
            if !buffer.ends_with('\n') {
                buffer = lines.pop().unwrap_or("").to_string();
            } else {
                buffer.clear();
            }

            for line in lines {
                let line = line.trim();

                if !line.starts_with("data:") {
                    continue;
                }

                let data = line.trim_start_matches("data:").trim();

                if data == "[DONE]" {
                    let _ = tx.send(StreamEvent::End).await;
                    return Ok(());
                }

                if let Some(content) = Self::extract_content(data) {
                    if !content.is_empty() {
                        let _ = tx.send(StreamEvent::Token(content)).await;
                    }
                }
            }
        }

        let _ = tx.send(StreamEvent::End).await;
        Ok(())
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tools: false,
            vision: false,
            json_mode: false,
        }
    }
}
