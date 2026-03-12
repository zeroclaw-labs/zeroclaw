use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;

pub struct NvidiaVisionTool {
    api_key: String,
    base_url: String,
    default_model: String,
}

impl NvidiaVisionTool {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://integrate.api.nvidia.com/v1".to_string(),
            default_model: "nvidia/neva-22b".to_string(),
        }
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.default_model = model.to_string();
        self
    }

    pub fn with_base_url(mut self, url: &str) -> Self {
        self.base_url = url.to_string();
        self
    }
}

#[async_trait]
impl Tool for NvidiaVisionTool {
    fn name(&self) -> &str {
        "nvidia_vision"
    }

    fn description(&self) -> &str {
        "Analyze images using NVIDIA NIM vision models. Accepts an image URL and a text prompt, \
        returns the model's visual analysis. Powered by NVIDIA NIM API."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "image_url": {
                    "type": "string",
                    "description": "URL of the image to analyze"
                },
                "prompt": {
                    "type": "string",
                    "description": "Text prompt describing what to analyze in the image"
                },
                "model": {
                    "type": "string",
                    "description": "Vision model to use (default: nvidia/neva-22b)"
                }
            },
            "required": ["image_url", "prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let image_url = args
            .get("image_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'image_url' parameter"))?;

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt' parameter"))?;

        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.default_model);

        let payload = json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": prompt
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": image_url
                        }
                    }
                ]
            }],
            "max_tokens": 1024,
            "temperature": 0.2
        });

        let url = format!("{}/chat/completions", self.base_url);

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
                let body = resp.text().await.unwrap_or_default();

                if status.is_success() {
                    let parsed: serde_json::Value =
                        serde_json::from_str(&body).unwrap_or(json!({}));
                    let text = parsed["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or("No response content")
                        .to_string();

                    Ok(ToolResult {
                        success: true,
                        output: text,
                        error: None,
                    })
                } else {
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("NVIDIA NIM API error {}: {}", status, body)),
                    })
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("NVIDIA Vision request failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool() -> NvidiaVisionTool {
        NvidiaVisionTool::new("test-key".to_string())
    }

    #[test]
    fn spec_returns_correct_metadata() {
        let tool = test_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "nvidia_vision");
        assert!(spec.description.contains("NVIDIA"));
        assert!(spec.description.contains("vision"));
    }

    #[test]
    fn parameters_schema_has_required_fields() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("image_url")));
        assert!(required.contains(&json!("prompt")));
    }

    #[test]
    fn parameters_schema_has_model_field() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["model"]["type"].as_str() == Some("string"));
    }

    #[test]
    fn with_model_overrides_default() {
        let tool = test_tool().with_model("custom/model");
        assert_eq!(tool.default_model, "custom/model");
    }

    #[test]
    fn with_base_url_overrides_default() {
        let tool = test_tool().with_base_url("http://localhost:8000/v1");
        assert_eq!(tool.base_url, "http://localhost:8000/v1");
    }

    #[tokio::test]
    async fn execute_rejects_missing_image_url() {
        let tool = test_tool();
        let result = tool.execute(json!({"prompt": "describe"})).await;
        assert!(result.is_err() || !result.unwrap().success);
    }

    #[tokio::test]
    async fn execute_rejects_missing_prompt() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"image_url": "https://example.com/img.png"}))
            .await;
        assert!(result.is_err() || !result.unwrap().success);
    }
}
