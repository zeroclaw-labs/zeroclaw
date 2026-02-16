use crate::providers::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OllamaProvider {
    base_url: String,
    client: Client,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
    options: Options,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct Options {
    temperature: f64,
}

#[derive(Debug, Deserialize)]
struct ApiChatResponse {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
}

impl OllamaProvider {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            base_url: base_url
                .unwrap_or("http://localhost:11434")
                .trim_end_matches('/')
                .to_string(),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(300)) // Ollama runs locally, may be slow
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    async fn chat_with_system(
        &self,
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
            stream: false,
            options: Options { temperature },
        };

        let url = format!("{}/api/chat", self.base_url);

        let response = self.client.post(&url).json(&request).send().await?;

        if !response.status().is_success() {
            let err = super::api_error("Ollama", response).await;
            anyhow::bail!("{err}. Is Ollama running? (brew install ollama && ollama serve)");
        }

        let chat_response: ApiChatResponse = response.json().await?;
        Ok(chat_response.message.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_url() {
        let p = OllamaProvider::new(None);
        assert_eq!(p.base_url, "http://localhost:11434");
    }

    #[test]
    fn custom_url_trailing_slash() {
        let p = OllamaProvider::new(Some("http://192.168.1.100:11434/"));
        assert_eq!(p.base_url, "http://192.168.1.100:11434");
    }

    #[test]
    fn custom_url_no_trailing_slash() {
        let p = OllamaProvider::new(Some("http://myserver:11434"));
        assert_eq!(p.base_url, "http://myserver:11434");
    }

    #[test]
    fn empty_url_uses_empty() {
        let p = OllamaProvider::new(Some(""));
        assert_eq!(p.base_url, "");
    }

    #[test]
    fn request_serializes_with_system() {
        let req = ChatRequest {
            model: "llama3".to_string(),
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
            stream: false,
            options: Options { temperature: 0.7 },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"stream\":false"));
        assert!(json.contains("llama3"));
        assert!(json.contains("system"));
        assert!(json.contains("\"temperature\":0.7"));
    }

    #[test]
    fn request_serializes_without_system() {
        let req = ChatRequest {
            model: "mistral".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "test".to_string(),
            }],
            stream: false,
            options: Options { temperature: 0.0 },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"role\":\"system\""));
        assert!(json.contains("mistral"));
    }

    #[test]
    fn response_deserializes() {
        let json = r#"{"message":{"role":"assistant","content":"Hello from Ollama!"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message.content, "Hello from Ollama!");
    }

    #[test]
    fn response_with_empty_content() {
        let json = r#"{"message":{"role":"assistant","content":""}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.is_empty());
    }

    #[test]
    fn response_with_multiline() {
        let json = r#"{"message":{"role":"assistant","content":"line1\nline2\nline3"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.contains("line1"));
    }
}
