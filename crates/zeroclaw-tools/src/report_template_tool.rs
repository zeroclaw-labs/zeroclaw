//! Report template tool — standalone access to template engine.
//!
//! Exposes the report template engine directly so agents can render
//! templates with custom variable maps without going through ProjectIntelTool.

use super::report_templates;
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Standalone report template tool.
///
/// Provides direct access to the template engine for rendering
/// weekly_status, sprint_review, risk_register, and milestone_report
/// templates in en/de/fr/it.
pub struct ReportTemplateTool;

impl ReportTemplateTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReportTemplateTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReportTemplateTool {
    fn name(&self) -> &str {
        "report_template"
    }

    fn description(&self) -> &str {
        "Render a report template with custom variables. Supports weekly_status, sprint_review, risk_register, milestone_report in en/de/fr/it."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "template": {
                    "type": "string",
                    "enum": ["weekly_status", "sprint_review", "risk_register", "milestone_report"],
                    "description": "Template name"
                },
                "language": {
                    "type": "string",
                    "enum": ["en", "de", "fr", "it"],
                    "default": "en",
                    "description": "Language code"
                },
                "variables": {
                    "type": "object",
                    "description": "Map of placeholder names to values (e.g., {\"project_name\": \"Acme\"})"
                }
            },
            "required": ["template", "variables"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<ToolResult> {
        let template = params
            .get("template")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing template"))?;

        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("en");

        let variables = params
            .get("variables")
            .and_then(|v| v.as_object())
            .ok_or_else(|| anyhow::anyhow!("variables must be object"))?;

        // Convert JSON object to HashMap<String, String>
        // Non-string values are coerced to strings
        let var_map: HashMap<String, String> = variables
            .iter()
            .map(|(k, v)| {
                let value_str = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Null
                    | serde_json::Value::Array(_)
                    | serde_json::Value::Object(_) => String::new(),
                };
                (k.clone(), value_str)
            })
            .collect();

        let rendered = report_templates::render_template(template, language, &var_map)?;

        Ok(ToolResult {
            success: true,
            output: rendered,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tool_name_is_report_template() {
        let tool = ReportTemplateTool::new();
        assert_eq!(tool.name(), "report_template");
    }

    #[tokio::test]
    async fn tool_has_description() {
        let tool = ReportTemplateTool::new();
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn tool_has_parameters_schema() {
        let tool = ReportTemplateTool::new();
        let schema = tool.parameters_schema();
        assert!(schema.is_object());
        assert!(schema["properties"].is_object());
        assert!(schema["required"].is_array());
    }

    #[tokio::test]
    async fn execute_renders_weekly_status() {
        let tool = ReportTemplateTool::new();
        let params = json!({
            "template": "weekly_status",
            "language": "en",
            "variables": {
                "project_name": "Test",
                "period": "W1",
                "completed": "Done",
                "in_progress": "WIP",
                "blocked": "None",
                "next_steps": "Next"
            }
        });

        let result = tool.execute(params).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Project: Test"));
    }

    #[tokio::test]
    async fn execute_defaults_to_english() {
        let tool = ReportTemplateTool::new();
        let params = json!({
            "template": "weekly_status",
            "variables": {
                "project_name": "Test"
            }
        });

        let result = tool.execute(params).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("## Summary"));
    }

    #[tokio::test]
    async fn execute_fails_on_missing_template() {
        let tool = ReportTemplateTool::new();
        let params = json!({
            "variables": {
                "project_name": "Test"
            }
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_fails_on_missing_variables() {
        let tool = ReportTemplateTool::new();
        let params = json!({
            "template": "weekly_status"
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_fails_on_invalid_template() {
        let tool = ReportTemplateTool::new();
        let params = json!({
            "template": "unknown",
            "variables": {}
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
    }
}
