use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;

pub struct NvidiaCosmosWorldModelTool {
    api_key: String,
    base_url: String,
    default_model: String,
}

impl NvidiaCosmosWorldModelTool {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://integrate.api.nvidia.com/v1".to_string(),
            default_model: "nvidia/cosmos-nemotron-34b".to_string(),
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
impl Tool for NvidiaCosmosWorldModelTool {
    fn name(&self) -> &str {
        "nvidia_cosmos"
    }

    fn description(&self) -> &str {
        "Simulate world scenarios using NVIDIA Cosmos world foundation models via NIM API. \
        Accepts a scenario description and returns predictions about physical world outcomes, \
        useful for robotics planning, environment simulation, and autonomous agent decision-making."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "scenario": {
                    "type": "string",
                    "description": "Description of the physical scenario to simulate"
                },
                "context": {
                    "type": "string",
                    "description": "Additional context about the environment or constraints"
                },
                "model": {
                    "type": "string",
                    "description": "Cosmos model to use (default: nvidia/cosmos-nemotron-34b)"
                },
                "max_tokens": {
                    "type": "integer",
                    "description": "Maximum tokens in response (default: 2048)",
                    "default": 2048
                }
            },
            "required": ["scenario"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let scenario = args
            .get("scenario")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'scenario' parameter"))?;

        let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");

        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.default_model);

        let max_tokens = args
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(2048);

        let system_prompt = "You are a world simulation engine. Given a physical scenario, \
            predict outcomes based on physics, causality, and environmental constraints. \
            Be specific about spatial relationships, forces, and temporal sequences.";

        let user_message = if context.is_empty() {
            format!("Simulate this scenario:\n{scenario}")
        } else {
            format!("Context: {context}\n\nSimulate this scenario:\n{scenario}")
        };

        let payload = json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_message}
            ],
            "max_tokens": max_tokens,
            "temperature": 0.3
        });

        let url = format!("{}/chat/completions", self.base_url);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
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
                        .unwrap_or("No simulation output")
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
                        error: Some(format!("Cosmos API error {}: {}", status, body)),
                    })
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Cosmos request failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool() -> NvidiaCosmosWorldModelTool {
        NvidiaCosmosWorldModelTool::new("test-key".to_string())
    }

    #[test]
    fn spec_returns_correct_metadata() {
        let tool = test_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "nvidia_cosmos");
        assert!(spec.description.contains("Cosmos"));
        assert!(spec.description.contains("world"));
    }

    #[test]
    fn parameters_schema_requires_scenario() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("scenario")));
        assert!(!required.contains(&json!("context")));
    }

    #[test]
    fn parameters_schema_has_all_properties() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("scenario"));
        assert!(props.contains_key("context"));
        assert!(props.contains_key("model"));
        assert!(props.contains_key("max_tokens"));
    }

    #[test]
    fn with_model_overrides_default() {
        let tool = test_tool().with_model("nvidia/cosmos-2.0");
        assert_eq!(tool.default_model, "nvidia/cosmos-2.0");
    }

    #[tokio::test]
    async fn execute_rejects_missing_scenario() {
        let tool = test_tool();
        let result = tool.execute(json!({"context": "test"})).await;
        assert!(result.is_err());
    }
}
