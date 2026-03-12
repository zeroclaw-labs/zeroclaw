use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;

pub struct NvidiaRivaSpeechTool {
    api_key: String,
    base_url: String,
}

impl NvidiaRivaSpeechTool {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://integrate.api.nvidia.com/v1".to_string(),
        }
    }

    pub fn with_base_url(mut self, url: &str) -> Self {
        self.base_url = url.to_string();
        self
    }
}

#[async_trait]
impl Tool for NvidiaRivaSpeechTool {
    fn name(&self) -> &str {
        "nvidia_speech"
    }

    fn description(&self) -> &str {
        "Text-to-speech synthesis using NVIDIA Riva via NIM API. \
        Converts text to audio descriptions with configurable voice and language. \
        Returns audio metadata and synthesis status."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to synthesize into speech"
                },
                "voice": {
                    "type": "string",
                    "description": "Voice ID to use for synthesis (default: English-US)",
                    "default": "English-US"
                },
                "language": {
                    "type": "string",
                    "description": "Language code (default: en-US)",
                    "default": "en-US"
                },
                "output_format": {
                    "type": "string",
                    "description": "Audio format: wav, mp3, opus (default: wav)",
                    "default": "wav"
                }
            },
            "required": ["text"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'text' parameter"))?;

        if text.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Text cannot be empty".to_string()),
            });
        }

        let voice = args
            .get("voice")
            .and_then(|v| v.as_str())
            .unwrap_or("English-US");

        let language = args
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("en-US");

        let output_format = args
            .get("output_format")
            .and_then(|v| v.as_str())
            .unwrap_or("wav");

        let payload = json!({
            "input": text,
            "voice": {
                "name": voice,
                "language_code": language
            },
            "audio_config": {
                "audio_encoding": output_format.to_uppercase()
            }
        });

        let url = format!("{}/audio/speech", self.base_url);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;

        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await;

        match response {
            Ok(resp) => {
                let status = resp.status();

                if status.is_success() {
                    let content_length = resp
                        .headers()
                        .get("content-length")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("unknown");

                    let output = format!(
                        "Speech synthesized successfully.\n\
                        Text length: {} chars\n\
                        Voice: {}\n\
                        Language: {}\n\
                        Format: {}\n\
                        Audio size: {} bytes",
                        text.len(),
                        voice,
                        language,
                        output_format,
                        content_length
                    );

                    Ok(ToolResult {
                        success: true,
                        output,
                        error: None,
                    })
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Riva API error {}: {}", status, body)),
                    })
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Speech synthesis request failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool() -> NvidiaRivaSpeechTool {
        NvidiaRivaSpeechTool::new("test-key".to_string())
    }

    #[test]
    fn spec_returns_correct_metadata() {
        let tool = test_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "nvidia_speech");
        assert!(spec.description.contains("speech"));
        assert!(spec.description.contains("Riva"));
    }

    #[test]
    fn parameters_schema_requires_text() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("text")));
    }

    #[test]
    fn parameters_schema_has_voice_and_language() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("voice"));
        assert!(props.contains_key("language"));
        assert!(props.contains_key("output_format"));
    }

    #[test]
    fn with_base_url_overrides() {
        let tool = test_tool().with_base_url("http://localhost:50051");
        assert_eq!(tool.base_url, "http://localhost:50051");
    }

    #[tokio::test]
    async fn execute_rejects_missing_text() {
        let tool = test_tool();
        let result = tool.execute(json!({"voice": "en-US"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_empty_text() {
        let tool = test_tool();
        let result = tool.execute(json!({"text": ""})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("empty"));
    }
}
