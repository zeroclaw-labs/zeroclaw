use crate::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest,
    Provider, ProviderCapabilities, StreamEvent,
};

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use futures_util::StreamExt;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;

#[derive(Clone)]
pub struct AtomicChatProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    endpoint: String,
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

        let endpoint = format!(
            "{}/v1/chat/completions",
            base_url.trim_end_matches('/')
        );

        Self {
            client,
            base_url,
            api_key,
            endpoint,
        }
    }

    #[inline]
    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    #[inline]
    fn map_messages(messages: &[ChatMessage]) -> Vec<Message<'_>> {
        messages
            .iter()
            .map(|m| Message {
                role: m.role.as_str(),
                content: &m.content,
            })
            .collect()
    }

    #[inline]
    fn extract_content(chunk: &str) -> Option<String> {
        serde_json::from_str::<StreamChunk>(chunk)
            .ok()?
            .choices
            .first()?
            .delta
            .content
            .clone()
    }
}

#[async_trait]
impl Provider for AtomicChatProvider {
    async fn stream_chat(
        &self,
        req: ProviderChatRequest,
        mut tx: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {

        let body = ChatCompletionRequest {
            model: &req.model,
            messages: Self::map_messages(&req.messages),
            temperature: req.temperature,
            stream: true,
        };

        let mut request = self.client.post(self.endpoint()).json(&body);

        if let Some(key) = &self.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            let err = response.text().await.unwrap_or_default();
            let _ = tx.send(StreamEvent::Error(err)).await;
            return Ok(());
        }

        // --- STREAM HANDLING (FIXED) ---

        let stream = response.bytes_stream();
        let mut reader = BufReader::new(tokio_util::io::StreamReader::new(
            stream.map(|res| res.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)))
        ));

        let mut line = String::new();

        let mut buffer = String::new();

        loop {
            line.clear();

            let bytes = reader.read_line(&mut line).await?;

            if bytes == 0 {
                break;
            }

            let line = line.trim();

            // ignore SSE comments / keep-alives
            if line.is_empty() || line.starts_with(':') {
                continue;
            }

            // only process data lines
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();

                if data == "[DONE]" {
                    let _ = tx.send(StreamEvent::End).await;
                    return Ok(());
                }

                // parse safely
                match Self::extract_content(data) {
                    Some(content) if !content.is_empty() => {
                        let _ = tx.send(StreamEvent::Token(content)).await;
                    }
                    _ => {
                        // ignore malformed chunks silently (or log if you want)
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
