use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

pub struct NvidiaTritonInferenceTool {
    server_url: String,
}

impl NvidiaTritonInferenceTool {
    pub fn new(server_url: &str) -> Self {
        Self {
            server_url: server_url.trim_end_matches('/').to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
struct TritonInferRequest {
    inputs: Vec<TritonTensorInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outputs: Option<Vec<TritonTensorOutput>>,
}

#[derive(Debug, Serialize)]
struct TritonTensorInput {
    name: String,
    shape: Vec<i64>,
    datatype: String,
    data: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct TritonTensorOutput {
    name: String,
}

#[derive(Debug, Deserialize)]
struct TritonInferResponse {
    model_name: String,
    #[serde(default)]
    model_version: String,
    outputs: Vec<TritonOutputTensor>,
}

#[derive(Debug, Deserialize)]
struct TritonOutputTensor {
    name: String,
    shape: Vec<i64>,
    datatype: String,
    data: serde_json::Value,
}

#[async_trait]
impl Tool for NvidiaTritonInferenceTool {
    fn name(&self) -> &str {
        "nvidia_triton"
    }

    fn description(&self) -> &str {
        "Run inference on NVIDIA Triton Inference Server using the KServe v2 REST protocol. \
        Supports any model hosted on Triton with tensor-based input/output. \
        Use for custom ML models, embeddings, classification, and other tensor operations."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "model_name": {
                    "type": "string",
                    "description": "Name of the model deployed on Triton"
                },
                "model_version": {
                    "type": "string",
                    "description": "Model version (default: latest)"
                },
                "inputs": {
                    "type": "array",
                    "description": "Input tensors as [{name, shape, datatype, data}]",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"},
                            "shape": {"type": "array", "items": {"type": "integer"}},
                            "datatype": {"type": "string"},
                            "data": {}
                        },
                        "required": ["name", "shape", "datatype", "data"]
                    }
                },
                "outputs": {
                    "type": "array",
                    "description": "Requested output tensor names (optional, returns all if omitted)",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"}
                        }
                    }
                },
                "action": {
                    "type": "string",
                    "description": "Action: infer (default), health, model_info",
                    "default": "infer"
                }
            },
            "required": ["model_name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let model_name = args
            .get("model_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'model_name' parameter"))?;

        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("infer");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;

        match action {
            "health" => {
                let url = format!("{}/v2/health/ready", self.server_url);
                match client.get(&url).send().await {
                    Ok(resp) if resp.status().is_success() => Ok(ToolResult {
                        success: true,
                        output: "Triton server is healthy and ready".to_string(),
                        error: None,
                    }),
                    Ok(resp) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Triton not ready: HTTP {}", resp.status())),
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Cannot reach Triton server: {e}")),
                    }),
                }
            }

            "model_info" => {
                let version = args
                    .get("model_version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let url = if version.is_empty() {
                    format!("{}/v2/models/{}", self.server_url, model_name)
                } else {
                    format!(
                        "{}/v2/models/{}/versions/{}",
                        self.server_url, model_name, version
                    )
                };

                match client.get(&url).send().await {
                    Ok(resp) => {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        Ok(ToolResult {
                            success: status.is_success(),
                            output: body,
                            error: if status.is_success() {
                                None
                            } else {
                                Some(format!("HTTP {status}"))
                            },
                        })
                    }
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Model info request failed: {e}")),
                    }),
                }
            }

            "infer" => {
                let inputs_val = args.get("inputs").cloned().unwrap_or(json!([]));
                let inputs_arr = inputs_val
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("'inputs' must be an array for inference"))?;

                if inputs_arr.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("At least one input tensor is required for inference".into()),
                    });
                }

                let inputs: Vec<TritonTensorInput> = inputs_arr
                    .iter()
                    .filter_map(|input| {
                        Some(TritonTensorInput {
                            name: input.get("name")?.as_str()?.to_string(),
                            shape: input
                                .get("shape")?
                                .as_array()?
                                .iter()
                                .filter_map(|v| v.as_i64())
                                .collect(),
                            datatype: input.get("datatype")?.as_str()?.to_string(),
                            data: input.get("data")?.clone(),
                        })
                    })
                    .collect();

                let outputs = args.get("outputs").and_then(|v| {
                    v.as_array().map(|arr| {
                        arr.iter()
                            .filter_map(|o| {
                                Some(TritonTensorOutput {
                                    name: o.get("name")?.as_str()?.to_string(),
                                })
                            })
                            .collect()
                    })
                });

                let request = TritonInferRequest { inputs, outputs };

                let version = args
                    .get("model_version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let url = if version.is_empty() {
                    format!("{}/v2/models/{}/infer", self.server_url, model_name)
                } else {
                    format!(
                        "{}/v2/models/{}/versions/{}/infer",
                        self.server_url, model_name, version
                    )
                };

                match client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .json(&request)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();

                        if status.is_success() {
                            let parsed: Result<TritonInferResponse, _> =
                                serde_json::from_str(&body);
                            match parsed {
                                Ok(infer_resp) => {
                                    let output_summary: Vec<String> = infer_resp
                                        .outputs
                                        .iter()
                                        .map(|o| {
                                            format!(
                                                "{}: shape={:?} dtype={} data={}",
                                                o.name, o.shape, o.datatype, o.data
                                            )
                                        })
                                        .collect();

                                    Ok(ToolResult {
                                        success: true,
                                        output: format!(
                                            "Model: {} v{}\nOutputs:\n{}",
                                            infer_resp.model_name,
                                            infer_resp.model_version,
                                            output_summary.join("\n")
                                        ),
                                        error: None,
                                    })
                                }
                                Err(_) => Ok(ToolResult {
                                    success: true,
                                    output: body,
                                    error: None,
                                }),
                            }
                        } else {
                            Ok(ToolResult {
                                success: false,
                                output: String::new(),
                                error: Some(format!("Triton infer error {}: {}", status, body)),
                            })
                        }
                    }
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Triton inference request failed: {e}")),
                    }),
                }
            }

            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{action}'. Valid: infer, health, model_info"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool() -> NvidiaTritonInferenceTool {
        NvidiaTritonInferenceTool::new("http://localhost:8000")
    }

    #[test]
    fn spec_returns_correct_metadata() {
        let tool = test_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "nvidia_triton");
        assert!(spec.description.contains("Triton"));
        assert!(spec.description.contains("KServe"));
    }

    #[test]
    fn parameters_schema_requires_model_name() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("model_name")));
    }

    #[test]
    fn parameters_schema_has_inputs_and_outputs() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("inputs"));
        assert!(props.contains_key("outputs"));
        assert!(props.contains_key("action"));
    }

    #[test]
    fn triton_infer_request_serializes() {
        let request = TritonInferRequest {
            inputs: vec![TritonTensorInput {
                name: "input".to_string(),
                shape: vec![1, 3],
                datatype: "FP32".to_string(),
                data: json!([[1.0, 2.0, 3.0]]),
            }],
            outputs: Some(vec![TritonTensorOutput {
                name: "output".to_string(),
            }]),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("input"));
        assert!(json.contains("FP32"));
        assert!(json.contains("[1,3]"));
    }

    #[test]
    fn triton_infer_response_deserializes() {
        let json = r#"{
            "model_name": "resnet50",
            "model_version": "1",
            "outputs": [{
                "name": "output",
                "shape": [1, 1000],
                "datatype": "FP32",
                "data": [0.1, 0.2, 0.7]
            }]
        }"#;

        let resp: TritonInferResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.model_name, "resnet50");
        assert_eq!(resp.outputs.len(), 1);
        assert_eq!(resp.outputs[0].name, "output");
    }

    #[test]
    fn server_url_trailing_slash_stripped() {
        let tool = NvidiaTritonInferenceTool::new("http://localhost:8000/");
        assert_eq!(tool.server_url, "http://localhost:8000");
    }

    #[tokio::test]
    async fn execute_rejects_missing_model_name() {
        let tool = test_tool();
        let result = tool.execute(json!({"inputs": []})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_empty_inputs_for_infer() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"model_name": "test", "inputs": []}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("At least one input"));
    }

    #[tokio::test]
    async fn execute_rejects_unknown_action() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"model_name": "test", "action": "bad"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }
}
