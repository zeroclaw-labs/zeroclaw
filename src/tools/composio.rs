// Composio Tool Provider — optional managed tool surface with 1000+ OAuth integrations.
//
// When enabled, Aria can execute actions on Gmail, Notion, GitHub, Slack, etc.
// through Composio's API without storing raw OAuth tokens locally.
//
// This is opt-in. Users who prefer sovereign/local-only mode skip this entirely.
// The Composio API key is stored in the encrypted secret store.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

const COMPOSIO_API_BASE: &str = "https://backend.composio.dev/api/v2";

/// A tool that proxies actions to the Composio managed tool platform.
pub struct ComposioTool {
    api_key: String,
    client: Client,
}

impl ComposioTool {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// List available Composio apps/actions for the authenticated user.
    pub async fn list_actions(
        &self,
        app_name: Option<&str>,
    ) -> anyhow::Result<Vec<ComposioAction>> {
        let mut url = format!("{COMPOSIO_API_BASE}/actions");
        if let Some(app) = app_name {
            url = format!("{url}?appNames={app}");
        }

        let resp = self
            .client
            .get(&url)
            .header("x-api-key", &self.api_key)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Composio API error: {err}");
        }

        let body: ComposioActionsResponse = resp.json().await?;
        Ok(body.items)
    }

    /// Execute a Composio action by name with given parameters.
    pub async fn execute_action(
        &self,
        action_name: &str,
        params: serde_json::Value,
        entity_id: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let url = format!("{COMPOSIO_API_BASE}/actions/{action_name}/execute");

        let mut body = json!({
            "input": params,
        });

        if let Some(entity) = entity_id {
            body["entityId"] = json!(entity);
        }

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Composio action execution failed: {err}");
        }

        let result: serde_json::Value = resp.json().await?;
        Ok(result)
    }

    /// Get the OAuth connection URL for a specific app.
    pub async fn get_connection_url(
        &self,
        app_name: &str,
        entity_id: &str,
    ) -> anyhow::Result<String> {
        let url = format!("{COMPOSIO_API_BASE}/connectedAccounts");

        let body = json!({
            "integrationId": app_name,
            "entityId": entity_id,
        });

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to get connection URL: {err}");
        }

        let result: serde_json::Value = resp.json().await?;
        result
            .get("redirectUrl")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("No redirect URL in response"))
    }
}

#[async_trait]
impl Tool for ComposioTool {
    fn name(&self) -> &str {
        "composio"
    }

    fn description(&self) -> &str {
        "Execute actions on 1000+ apps via Composio (Gmail, Notion, GitHub, Slack, etc.). \
         Use action='list' to see available actions, or action='execute' with action_name and params."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The operation: 'list' (list available actions), 'execute' (run an action), or 'connect' (get OAuth URL)",
                    "enum": ["list", "execute", "connect"]
                },
                "app": {
                    "type": "string",
                    "description": "App name filter for 'list', or app name for 'connect' (e.g. 'gmail', 'notion', 'github')"
                },
                "action_name": {
                    "type": "string",
                    "description": "The Composio action name to execute (e.g. 'GMAIL_FETCH_EMAILS')"
                },
                "params": {
                    "type": "object",
                    "description": "Parameters to pass to the action"
                },
                "entity_id": {
                    "type": "string",
                    "description": "Entity ID for multi-user setups (defaults to 'default')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        let entity_id = args
            .get("entity_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default");

        match action {
            "list" => {
                let app = args.get("app").and_then(|v| v.as_str());
                match self.list_actions(app).await {
                    Ok(actions) => {
                        let summary: Vec<String> = actions
                            .iter()
                            .take(20)
                            .map(|a| {
                                format!(
                                    "- {} ({}): {}",
                                    a.name,
                                    a.app_name.as_deref().unwrap_or("?"),
                                    a.description.as_deref().unwrap_or("")
                                )
                            })
                            .collect();
                        let total = actions.len();
                        let output = format!(
                            "Found {total} available actions:\n{}{}",
                            summary.join("\n"),
                            if total > 20 {
                                format!("\n... and {} more", total - 20)
                            } else {
                                String::new()
                            }
                        );
                        Ok(ToolResult {
                            success: true,
                            output,
                            error: None,
                        })
                    }
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to list actions: {e}")),
                    }),
                }
            }

            "execute" => {
                let action_name = args
                    .get("action_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'action_name' for execute"))?;

                let params = args.get("params").cloned().unwrap_or(json!({}));

                match self
                    .execute_action(action_name, params, Some(entity_id))
                    .await
                {
                    Ok(result) => {
                        let output = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| format!("{result:?}"));
                        Ok(ToolResult {
                            success: true,
                            output,
                            error: None,
                        })
                    }
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Action execution failed: {e}")),
                    }),
                }
            }

            "connect" => {
                let app = args
                    .get("app")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'app' for connect"))?;

                match self.get_connection_url(app, entity_id).await {
                    Ok(url) => Ok(ToolResult {
                        success: true,
                        output: format!("Open this URL to connect {app}:\n{url}"),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to get connection URL: {e}")),
                    }),
                }
            }

            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{action}'. Use 'list', 'execute', or 'connect'."
                )),
            }),
        }
    }
}

// ── API response types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ComposioActionsResponse {
    #[serde(default)]
    items: Vec<ComposioAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposioAction {
    pub name: String,
    #[serde(rename = "appName")]
    pub app_name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Constructor ───────────────────────────────────────────

    #[test]
    fn composio_tool_has_correct_name() {
        let tool = ComposioTool::new("test-key");
        assert_eq!(tool.name(), "composio");
    }

    #[test]
    fn composio_tool_has_description() {
        let tool = ComposioTool::new("test-key");
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("1000+"));
    }

    #[test]
    fn composio_tool_schema_has_required_fields() {
        let tool = ComposioTool::new("test-key");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["action_name"].is_object());
        assert!(schema["properties"]["params"].is_object());
        assert!(schema["properties"]["app"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("action")));
    }

    #[test]
    fn composio_tool_spec_roundtrip() {
        let tool = ComposioTool::new("test-key");
        let spec = tool.spec();
        assert_eq!(spec.name, "composio");
        assert!(spec.parameters.is_object());
    }

    // ── Execute validation ────────────────────────────────────

    #[tokio::test]
    async fn execute_missing_action_returns_error() {
        let tool = ComposioTool::new("test-key");
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_unknown_action_returns_error() {
        let tool = ComposioTool::new("test-key");
        let result = tool.execute(json!({"action": "unknown"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn execute_without_action_name_returns_error() {
        let tool = ComposioTool::new("test-key");
        let result = tool.execute(json!({"action": "execute"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn connect_without_app_returns_error() {
        let tool = ComposioTool::new("test-key");
        let result = tool.execute(json!({"action": "connect"})).await;
        assert!(result.is_err());
    }

    // ── API response parsing ──────────────────────────────────

    #[test]
    fn composio_action_deserializes() {
        let json_str = r#"{"name": "GMAIL_FETCH_EMAILS", "appName": "gmail", "description": "Fetch emails", "enabled": true}"#;
        let action: ComposioAction = serde_json::from_str(json_str).unwrap();
        assert_eq!(action.name, "GMAIL_FETCH_EMAILS");
        assert_eq!(action.app_name.as_deref(), Some("gmail"));
        assert!(action.enabled);
    }

    #[test]
    fn composio_actions_response_deserializes() {
        let json_str = r#"{"items": [{"name": "TEST_ACTION", "appName": "test", "description": "A test", "enabled": true}]}"#;
        let resp: ComposioActionsResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "TEST_ACTION");
    }

    #[test]
    fn composio_actions_response_empty() {
        let json_str = r#"{"items": []}"#;
        let resp: ComposioActionsResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.items.is_empty());
    }

    #[test]
    fn composio_actions_response_missing_items_defaults() {
        let json_str = r"{}";
        let resp: ComposioActionsResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.items.is_empty());
    }
}
