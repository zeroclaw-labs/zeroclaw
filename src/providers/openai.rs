use crate::providers::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OpenAiProvider {
    api_key: Option<String>,
    client: Client,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Debug, Serialize)]
struct CompletionRequest {
    model: String,
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    choices: Vec<CompletionChoice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct CompletionChoice {
    text: String,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
}

impl OpenAiProvider {
    pub fn new(api_key: Option<&str>) -> Self {
        Self {
            api_key: api_key.map(ToString::to_string),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn uses_text_completion_endpoint(model: &str) -> bool {
        model.contains("search-api")
    }

    fn supports_custom_temperature(model: &str) -> bool {
        // GPT-5 family currently only supports default temperature behavior.
        !model.starts_with("gpt-5")
    }

    fn request_temperature(model: &str, temperature: f64) -> Option<f64> {
        if Self::supports_custom_temperature(model) {
            Some(temperature)
        } else {
            None
        }
    }

    fn compose_prompt(system_prompt: Option<&str>, message: &str) -> String {
        match system_prompt {
            Some(system) if !system.trim().is_empty() => {
                format!("System:\n{system}\n\nUser:\n{message}")
            }
            _ => message.to_string(),
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("OpenAI API key not set. Set OPENAI_API_KEY or edit config.toml.")
        })?;

        if Self::uses_text_completion_endpoint(model) {
            let request = CompletionRequest {
                model: model.to_string(),
                prompt: Self::compose_prompt(system_prompt, message),
                temperature: Self::request_temperature(model, temperature),
            };

            let response = self
                .client
                .post("https://api.openai.com/v1/completions")
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&request)
                .send()
                .await?;

            if !response.status().is_success() {
                return Err(super::api_error("OpenAI", response).await);
            }

            let completion_response: CompletionResponse = response.json().await?;
            return completion_response
                .choices
                .into_iter()
                .next()
                .map(|c| c.text)
                .ok_or_else(|| anyhow::anyhow!("No response from OpenAI"));
        }

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
            temperature: Self::request_temperature(model, temperature),
        };

        let response = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenAI", response).await);
        }

        let chat_response: ChatResponse = response.json().await?;

        chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("No response from OpenAI"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_key() {
        let p = OpenAiProvider::new(Some("sk-proj-abc123"));
        assert_eq!(p.api_key.as_deref(), Some("sk-proj-abc123"));
    }

    #[test]
    fn creates_without_key() {
        let p = OpenAiProvider::new(None);
        assert!(p.api_key.is_none());
    }

    #[test]
    fn creates_with_empty_key() {
        let p = OpenAiProvider::new(Some(""));
        assert_eq!(p.api_key.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = OpenAiProvider::new(None);
        let result = p.chat_with_system(None, "hello", "gpt-4o", 0.7).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key not set"));
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = OpenAiProvider::new(None);
        let result = p
            .chat_with_system(Some("You are ZeroClaw"), "test", "gpt-4o", 0.5)
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn request_serializes_with_system_message() {
        let req = ChatRequest {
            model: "gpt-4o".to_string(),
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
            temperature: Some(0.7),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("gpt-4o"));
    }

    #[test]
    fn request_serializes_without_system() {
        let req = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: Some(0.0),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("system"));
        assert!(json.contains("\"temperature\":0.0"));
    }

    #[test]
    fn response_deserializes_single_choice() {
        let json = r#"{"choices":[{"message":{"content":"Hi!"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.content, "Hi!");
    }

    #[test]
    fn response_deserializes_empty_choices() {
        let json = r#"{"choices":[]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.choices.is_empty());
    }

    #[test]
    fn response_deserializes_multiple_choices() {
        let json = r#"{"choices":[{"message":{"content":"A"}},{"message":{"content":"B"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 2);
        assert_eq!(resp.choices[0].message.content, "A");
    }

    #[test]
    fn response_with_unicode() {
        let json = r#"{"choices":[{"message":{"content":"こんにちは 🦀"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].message.content, "こんにちは 🦀");
    }

    #[test]
    fn response_with_long_content() {
        let long = "x".repeat(100_000);
        let json = format!(r#"{{"choices":[{{"message":{{"content":"{long}"}}}}]}}"#);
        let resp: ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.choices[0].message.content.len(), 100_000);
    }

    #[test]
    fn search_api_models_use_completions_endpoint() {
        assert!(OpenAiProvider::uses_text_completion_endpoint(
            "gpt-5-search-api"
        ));
        assert!(OpenAiProvider::uses_text_completion_endpoint(
            "gpt-5-search-api-2025-10-14"
        ));
        assert!(!OpenAiProvider::uses_text_completion_endpoint("gpt-5"));
    }

    #[test]
    fn compose_prompt_includes_system_when_present() {
        let prompt = OpenAiProvider::compose_prompt(Some("Be concise"), "hello");
        assert!(prompt.contains("System:\nBe concise"));
        assert!(prompt.contains("User:\nhello"));
    }

    #[test]
    fn gpt5_family_omits_temperature() {
        assert_eq!(OpenAiProvider::request_temperature("gpt-5", 0.7), None);
        assert_eq!(
            OpenAiProvider::request_temperature("gpt-5.2-pro", 0.2),
            None
        );
        assert_eq!(
            OpenAiProvider::request_temperature("gpt-4o", 0.7),
            Some(0.7)
        );
    }
}
