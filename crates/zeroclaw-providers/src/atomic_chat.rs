use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest,
    Provider, ProviderCapabilities, StreamEvent,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use futures_util::StreamExt;

#[derive(Clone)]
pub struct AtomicChatProvider {
    client: Client,
    base_url: String,
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
            .timeout(Duration::from_secs(120))
            .build()
            .expect("failed to build reqwest client");

        Self {
            client,
            base_url,
            api_key,
        }
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }

    fn map_messages(messages: &[ChatMessage]) -> Vec<Message<'_>> {
        messages
            .iter()
            .map(|m| Message {
                role: m.role.as_str(),
                content: &m.content,
            })
            .collect()
    }
}

#[async_trait]
impl Provider for AtomicChatProvider {
    async fn stream_chat(
        &self,
        req: ProviderChatRequest,
        mut tx: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {

        let url = self.endpoint();

        let body = ChatCompletionRequest {
            model: &req.model,
            messages: Self::map_messages(&req.messages),
            temperature: req.temperature,
            stream: true,
        };

        let mut request = self.client.post(url).json(&body);

        if let Some(key) = &self.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            let err = response.text().await.unwrap_or_default();
            let _ = tx.send(StreamEvent::Error(err)).await;
            return Ok(());
        }

        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                    break;
                }
            };

            let text = String::from_utf8_lossy(&chunk);

            for line in text.lines() {
                let line = line.trim();

                if !line.starts_with("data:") {
                    continue;
                }

                let data = line.trim_start_matches("data:").trim();

                if data == "[DONE]" {
                    let _ = tx.send(StreamEvent::End).await;
                    return Ok(());
                }

                if let Ok(parsed) = serde_json::from_str::<StreamChunk>(data) {
                    if let Some(choice) = parsed.choices.first() {
                        if let Some(content) = &choice.delta.content {
                            let _ = tx.send(StreamEvent::Token(content.clone())).await;
                        }
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
