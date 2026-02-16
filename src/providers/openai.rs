use crate::providers::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OpenAiProvider {
    api_key: Option<String>,
    client: Client,
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output_text: Option<String>,
    #[serde(default)]
    output: Vec<ResponsesOutputItem>,
}

#[derive(Debug, Deserialize)]
struct ResponsesOutputItem {
    #[serde(default)]
    content: Vec<ResponsesContentItem>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContentItem {
    text: Option<String>,
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

    fn compose_input(system_prompt: Option<&str>, message: &str) -> serde_json::Value {
        let mut messages = vec![];

        if let Some(system) = system_prompt {
            if !system.trim().is_empty() {
                messages.push(serde_json::json!({
                    "role": "system",
                    "content": [
                        { "type": "text", "text": system }
                    ]
                }));
            }
        }

        messages.push(serde_json::json!({
            "role": "user",
            "content": [
                { "type": "text", "text": message }
            ]
        }));

        serde_json::json!(messages)
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

        let request = ResponsesRequest {
            model: model.to_string(),
            input: Self::compose_input(system_prompt, message),
            temperature: Self::request_temperature(model, temperature),
        };

        let response = self
            .client
            .post("https://api.openai.com/v1/responses")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(super::api_error("OpenAI", response).await);
        }

        let responses_response: ResponsesResponse = response.json().await?;

        if let Some(output_text) = responses_response.output_text {
            if !output_text.trim().is_empty() {
                return Ok(output_text);
            }
        }

        for output_item in responses_response.output {
            for content_item in output_item.content {
                if let Some(text) = content_item.text {
                    if !text.trim().is_empty() {
                        return Ok(text);
                    }
                }
            }
        }

        Err(anyhow::anyhow!("No response from OpenAI"))
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
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: OpenAiProvider::compose_input(Some("You are ZeroClaw"), "hello"),
            temperature: Some(0.7),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("You are ZeroClaw"));
        assert!(json.contains("\"text\":\"hello\""));
        assert!(json.contains("gpt-4o"));
    }

    #[test]
    fn request_serializes_without_system() {
        let req = ResponsesRequest {
            model: "gpt-4o".to_string(),
            input: OpenAiProvider::compose_input(None, "hello"),
            temperature: Some(0.0),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"role\":\"system\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"text\":\"hello\""));
        assert!(json.contains("\"temperature\":0.0"));
    }

    #[test]
    fn response_deserializes_output_text() {
        let json = r#"{"output_text":"Hi!"}"#;
        let resp: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.output_text.as_deref(), Some("Hi!"));
    }

    #[test]
    fn response_deserializes_empty_output() {
        let json = r#"{"output":[]}"#;
        let resp: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert!(resp.output.is_empty());
    }

    #[test]
    fn response_deserializes_output_content_text() {
        let json = r#"{"output":[{"content":[{"text":"A"},{"text":"B"}]}]}"#;
        let resp: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.output.len(), 1);
        assert_eq!(resp.output[0].content[0].text.as_deref(), Some("A"));
    }

    #[test]
    fn response_with_unicode() {
        let json = r#"{"output_text":"こんにちは 🦀"}"#;
        let resp: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.output_text.as_deref(), Some("こんにちは 🦀"));
    }

    #[test]
    fn response_with_long_content() {
        let long = "x".repeat(100_000);
        let json = format!(r#"{{"output_text":"{long}"}}"#);
        let resp: ResponsesResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.output_text.unwrap().len(), 100_000);
    }

    #[test]
    fn compose_input_includes_system_when_present() {
        let input = OpenAiProvider::compose_input(Some("Be concise"), "hello");
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"role\":\"system\""));
        assert!(json.contains("\"text\":\"Be concise\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"text\":\"hello\""));
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
