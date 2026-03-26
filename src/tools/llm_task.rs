//! Lightweight LLM task tool for structured JSON-only sub-calls.
//!
//! Runs a single prompt through an LLM provider with no tool access and
//! optionally validates the response against a caller-supplied JSON Schema.
//! Ideal for structured data extraction in workflows.

use super::traits::{Tool, ToolResult};
use crate::providers::{self, Provider};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Tool that runs a single prompt through an LLM and optionally validates
/// the response against a JSON Schema. No tools are provided to the LLM —
/// this is a pure text-in, text-out (or JSON-out) call.
pub struct LlmTaskTool {
    security: Arc<SecurityPolicy>,
    /// Default provider name from root config (e.g. "openrouter").
    default_provider: String,
    /// Default model from root config.
    default_model: String,
    /// Default temperature from root config.
    default_temperature: f64,
    /// API key for provider authentication.
    api_key: Option<String>,
    /// Provider runtime options inherited from root config.
    provider_runtime_options: providers::ProviderRuntimeOptions,
}

impl LlmTaskTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        default_provider: String,
        default_model: String,
        default_temperature: f64,
        api_key: Option<String>,
        provider_runtime_options: providers::ProviderRuntimeOptions,
    ) -> Self {
        Self {
            security,
            default_provider,
            default_model,
            default_temperature,
            api_key,
            provider_runtime_options,
        }
    }
}

#[async_trait]
impl Tool for LlmTaskTool {
    fn name(&self) -> &str {
        "llm_task"
    }

    fn description(&self) -> &str {
        "Run a prompt through an LLM with no tool access and return the response. \
         Optionally validates the output against a JSON Schema. Ideal for structured \
         data extraction, classification, summarization, and transformation tasks."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The prompt to send to the LLM."
                },
                "schema": {
                    "type": "object",
                    "description": "Optional JSON Schema to validate the LLM response against. \
                                    When provided, the LLM is instructed to return valid JSON \
                                    matching this schema."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override (e.g. 'anthropic/claude-sonnet-4-6'). \
                                    Defaults to the configured default model."
                },
                "temperature": {
                    "type": "number",
                    "description": "Optional temperature override (0.0-2.0). \
                                    Defaults to the configured default temperature."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security gate
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "llm_task")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        // Extract required prompt
        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing or empty required parameter: prompt".to_string()),
                });
            }
        };

        // Extract optional overrides
        let schema = args.get("schema").and_then(|v| v.as_object());
        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.default_model);
        let temperature = args
            .get("temperature")
            .and_then(|v| v.as_f64())
            .unwrap_or(self.default_temperature);

        // Build the effective prompt, adding JSON schema instructions when needed
        let effective_prompt = if let Some(schema_obj) = schema {
            let schema_json =
                serde_json::to_string_pretty(&serde_json::Value::Object(schema_obj.clone()))
                    .unwrap_or_else(|_| "{}".to_string());
            format!(
                "{prompt}\n\n\
                 IMPORTANT: You MUST respond with valid JSON that conforms to this schema:\n\
                 ```json\n{schema_json}\n```\n\
                 Respond ONLY with the JSON object, no explanation or markdown."
            )
        } else {
            prompt.to_string()
        };

        // Create provider
        let api_key_ref = self.api_key.as_deref();
        let provider: Box<dyn Provider> = match providers::create_provider_with_options(
            &self.default_provider,
            api_key_ref,
            &self.provider_runtime_options,
        ) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to create provider: {e}")),
                });
            }
        };

        // Make the LLM call (no tools, no agent loop)
        let response = match provider
            .simple_chat(&effective_prompt, model, temperature)
            .await
        {
            Ok(text) => text,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("LLM call failed: {e}")),
                });
            }
        };

        // If schema was provided, validate the response
        if let Some(schema_obj) = schema {
            let schema_value = serde_json::Value::Object(schema_obj.clone());
            match validate_json_response(&response, &schema_value) {
                Ok(validated_json) => Ok(ToolResult {
                    success: true,
                    output: validated_json,
                    error: None,
                }),
                Err(validation_error) => Ok(ToolResult {
                    success: false,
                    output: response,
                    error: Some(format!("Schema validation failed: {validation_error}")),
                }),
            }
        } else {
            Ok(ToolResult {
                success: true,
                output: response,
                error: None,
            })
        }
    }
}

/// Validate a JSON response string against a JSON Schema value.
///
/// Performs lightweight validation: parses the response as JSON, checks that
/// required fields exist, and verifies basic type constraints (string, number,
/// integer, boolean, array, object) for each declared property.
fn validate_json_response(response: &str, schema: &serde_json::Value) -> Result<String, String> {
    // Strip markdown code fences if the LLM wrapped the response
    let trimmed = response.trim();
    let json_str = if trimmed.starts_with("```") {
        let inner = trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        inner
    } else {
        trimmed
    };

    // Parse as JSON
    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("Invalid JSON: {e}"))?;

    // Check required fields
    if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
        for req in required {
            if let Some(field_name) = req.as_str() {
                if parsed.get(field_name).is_none() {
                    return Err(format!("Missing required field: {field_name}"));
                }
            }
        }
    }

    // Check property types
    if let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) {
        for (prop_name, prop_schema) in properties {
            if let Some(value) = parsed.get(prop_name) {
                if let Some(expected_type) = prop_schema.get("type").and_then(|t| t.as_str()) {
                    if !type_matches(value, expected_type) {
                        return Err(format!(
                            "Field '{prop_name}' has wrong type: expected {expected_type}, \
                             got {}",
                            json_type_name(value)
                        ));
                    }
                }
            }
        }
    }

    // Return the cleaned, re-serialized JSON
    serde_json::to_string(&parsed).map_err(|e| format!("JSON serialization error: {e}"))
}

/// Check whether a JSON value matches an expected JSON Schema type string.
fn type_matches(value: &serde_json::Value, expected: &str) -> bool {
    match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true, // Unknown type — accept
    }
}

/// Return a human-readable type name for a JSON value.
fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Schema validation tests ──────────────────────────────────────

    #[test]
    fn validate_valid_json_against_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            },
            "required": ["name", "age"]
        });

        let response = r#"{"name": "Alice", "age": 30}"#;
        let result = validate_json_response(response, &schema);
        assert!(result.is_ok());

        let parsed: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["name"], "Alice");
        assert_eq!(parsed["age"], 30);
    }

    #[test]
    fn validate_missing_required_field() {
        let schema = json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "score": { "type": "number" }
            },
            "required": ["title", "score"]
        });

        let response = r#"{"title": "Test"}"#;
        let result = validate_json_response(response, &schema);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Missing required field: score"));
    }

    #[test]
    fn validate_wrong_type() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer" }
            },
            "required": ["count"]
        });

        let response = r#"{"count": "not_a_number"}"#;
        let result = validate_json_response(response, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("wrong type"));
    }

    #[test]
    fn validate_strips_markdown_code_fences() {
        let schema = json!({
            "type": "object",
            "properties": {
                "result": { "type": "string" }
            },
            "required": ["result"]
        });

        let response = "```json\n{\"result\": \"ok\"}\n```";
        let result = validate_json_response(response, &schema);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_invalid_json() {
        let schema = json!({ "type": "object" });
        let response = "this is not json at all";
        let result = validate_json_response(response, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid JSON"));
    }

    #[test]
    fn validate_optional_fields_accepted() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "bio": { "type": "string" }
            },
            "required": ["name"]
        });

        // bio is optional, so this should pass
        let response = r#"{"name": "Bob"}"#;
        let result = validate_json_response(response, &schema);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_all_type_checks() {
        assert!(type_matches(&json!("hello"), "string"));
        assert!(!type_matches(&json!(42), "string"));

        assert!(type_matches(&json!(2.72), "number"));
        assert!(type_matches(&json!(42), "number"));
        assert!(!type_matches(&json!("42"), "number"));

        assert!(type_matches(&json!(42), "integer"));
        assert!(!type_matches(&json!(2.72), "integer"));

        assert!(type_matches(&json!(true), "boolean"));
        assert!(!type_matches(&json!(1), "boolean"));

        assert!(type_matches(&json!([1, 2]), "array"));
        assert!(!type_matches(&json!({}), "array"));

        assert!(type_matches(&json!({}), "object"));
        assert!(!type_matches(&json!([]), "object"));

        assert!(type_matches(&json!(null), "null"));

        // Unknown types are accepted
        assert!(type_matches(&json!("anything"), "custom_type"));
    }

    // ── Tool trait tests ─────────────────────────────────────────────

    #[test]
    fn tool_metadata() {
        let tool = LlmTaskTool::new(
            Arc::new(SecurityPolicy::default()),
            "openrouter".to_string(),
            "test-model".to_string(),
            0.7,
            None,
            providers::ProviderRuntimeOptions::default(),
        );

        assert_eq!(tool.name(), "llm_task");
        assert!(tool.description().contains("LLM"));

        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["prompt"].is_object());
        assert!(schema["properties"]["schema"].is_object());
        assert!(schema["properties"]["model"].is_object());
        assert!(schema["properties"]["temperature"].is_object());

        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "prompt");
    }

    #[tokio::test]
    async fn execute_missing_prompt_returns_error() {
        let tool = LlmTaskTool::new(
            Arc::new(SecurityPolicy::default()),
            "openrouter".to_string(),
            "test-model".to_string(),
            0.7,
            None,
            providers::ProviderRuntimeOptions::default(),
        );

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("prompt"));
    }

    #[tokio::test]
    async fn execute_empty_prompt_returns_error() {
        let tool = LlmTaskTool::new(
            Arc::new(SecurityPolicy::default()),
            "openrouter".to_string(),
            "test-model".to_string(),
            0.7,
            None,
            providers::ProviderRuntimeOptions::default(),
        );

        let result = tool.execute(json!({"prompt": "  "})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("prompt"));
    }

    #[tokio::test]
    async fn execute_with_invalid_provider_returns_error() {
        let tool = LlmTaskTool::new(
            Arc::new(SecurityPolicy::default()),
            "nonexistent_provider_xyz".to_string(),
            "test-model".to_string(),
            0.7,
            None,
            providers::ProviderRuntimeOptions::default(),
        );

        let result = tool
            .execute(json!({"prompt": "Hello world"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("provider"));
    }
}
