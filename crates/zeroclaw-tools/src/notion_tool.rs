use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

const NOTION_API_BASE: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2022-06-28";
const NOTION_REQUEST_TIMEOUT_SECS: u64 = 30;
/// Maximum number of characters to include from an error response body.
const MAX_ERROR_BODY_CHARS: usize = 500;

/// Tool for interacting with the Notion API — query databases, read/create/update pages,
/// and search the workspace. Each action is gated by the appropriate security operation
/// (Read for queries, Act for mutations).
pub struct NotionTool {
    api_key: String,
    http: reqwest::Client,
    security: Arc<SecurityPolicy>,
}

impl NotionTool {
    /// Create a new Notion tool with the given API key and security policy.
    pub fn new(api_key: String, security: Arc<SecurityPolicy>) -> Self {
        Self {
            api_key,
            http: reqwest::Client::new(),
            security,
        }
    }

    /// Build the standard Notion API headers (Authorization, version, content-type).
    fn headers(&self) -> anyhow::Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", self.api_key)
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid Notion API key header value: {e}"))?,
        );
        headers.insert("Notion-Version", NOTION_VERSION.parse().unwrap());
        headers.insert("Content-Type", "application/json".parse().unwrap());
        Ok(headers)
    }

    /// Query a Notion database with an optional filter.
    async fn query_database(
        &self,
        database_id: &str,
        filter: Option<&serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        let url = format!("{NOTION_API_BASE}/databases/{database_id}/query");
        let mut body = json!({});
        if let Some(f) = filter {
            body["filter"] = f.clone();
        }
        let resp = self
            .http
            .post(&url)
            .headers(self.headers()?)
            .json(&body)
            .timeout(std::time::Duration::from_secs(NOTION_REQUEST_TIMEOUT_SECS))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Notion query_database failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }

    /// Read a single Notion page by ID.
    async fn read_page(&self, page_id: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{NOTION_API_BASE}/pages/{page_id}");
        let resp = self
            .http
            .get(&url)
            .headers(self.headers()?)
            .timeout(std::time::Duration::from_secs(NOTION_REQUEST_TIMEOUT_SECS))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Notion read_page failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }

    /// Create a new Notion page, optionally within a database.
    async fn create_page(
        &self,
        properties: &serde_json::Value,
        database_id: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let url = format!("{NOTION_API_BASE}/pages");
        let mut body = json!({ "properties": properties });
        if let Some(db_id) = database_id {
            body["parent"] = json!({ "database_id": db_id });
        }
        let resp = self
            .http
            .post(&url)
            .headers(self.headers()?)
            .json(&body)
            .timeout(std::time::Duration::from_secs(NOTION_REQUEST_TIMEOUT_SECS))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Notion create_page failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }

    /// Update an existing Notion page's properties.
    async fn update_page(
        &self,
        page_id: &str,
        properties: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let url = format!("{NOTION_API_BASE}/pages/{page_id}");
        let body = json!({ "properties": properties });
        let resp = self
            .http
            .patch(&url)
            .headers(self.headers()?)
            .json(&body)
            .timeout(std::time::Duration::from_secs(NOTION_REQUEST_TIMEOUT_SECS))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Notion update_page failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }

    /// Search the Notion workspace by query string.
    async fn search(&self, query: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{NOTION_API_BASE}/search");
        let body = json!({ "query": query });
        let resp = self
            .http
            .post(&url)
            .headers(self.headers()?)
            .json(&body)
            .timeout(std::time::Duration::from_secs(NOTION_REQUEST_TIMEOUT_SECS))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Notion search failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }
}

#[async_trait]
impl Tool for NotionTool {
    fn name(&self) -> &str {
        "notion"
    }

    fn description(&self) -> &str {
        "Interact with Notion: query databases, read/create/update pages, and search the workspace."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["query_database", "read_page", "create_page", "update_page", "search"],
                    "description": "The Notion API action to perform"
                },
                "database_id": {
                    "type": "string",
                    "description": "Database ID (required for query_database, optional for create_page)"
                },
                "page_id": {
                    "type": "string",
                    "description": "Page ID (required for read_page and update_page)"
                },
                "filter": {
                    "type": "object",
                    "description": "Notion filter object for query_database"
                },
                "properties": {
                    "type": "object",
                    "description": "Properties object for create_page and update_page"
                },
                "query": {
                    "type": "string",
                    "description": "Search query string for the search action"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: action".into()),
                });
            }
        };

        // Enforce granular security: Read for queries, Act for mutations
        let operation = match action {
            "query_database" | "read_page" | "search" => ToolOperation::Read,
            "create_page" | "update_page" => ToolOperation::Act,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown action: {action}. Valid actions: query_database, read_page, create_page, update_page, search"
                    )),
                });
            }
        };

        if let Err(error) = self.security.enforce_tool_operation(operation, "notion") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let result = match action {
            "query_database" => {
                let database_id = match args.get("database_id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("query_database requires database_id parameter".into()),
                        });
                    }
                };
                let filter = args.get("filter");
                self.query_database(database_id, filter).await
            }
            "read_page" => {
                let page_id = match args.get("page_id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("read_page requires page_id parameter".into()),
                        });
                    }
                };
                self.read_page(page_id).await
            }
            "create_page" => {
                let properties = match args.get("properties") {
                    Some(p) => p,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("create_page requires properties parameter".into()),
                        });
                    }
                };
                let database_id = args.get("database_id").and_then(|v| v.as_str());
                self.create_page(properties, database_id).await
            }
            "update_page" => {
                let page_id = match args.get("page_id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("update_page requires page_id parameter".into()),
                        });
                    }
                };
                let properties = match args.get("properties") {
                    Some(p) => p,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("update_page requires properties parameter".into()),
                        });
                    }
                };
                self.update_page(page_id, properties).await
            }
            "search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                self.search(query).await
            }
            _ => unreachable!(), // Already handled above
        };

        match result {
            Ok(value) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_tool() -> NotionTool {
        let security = Arc::new(SecurityPolicy::default());
        NotionTool::new("test-key".into(), security)
    }

    #[test]
    fn tool_name_is_notion() {
        let tool = test_tool();
        assert_eq!(tool.name(), "notion");
    }

    #[test]
    fn parameters_schema_has_required_action() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
    }

    #[test]
    fn parameters_schema_defines_all_actions() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        let action_strs: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        assert!(action_strs.contains(&"query_database"));
        assert!(action_strs.contains(&"read_page"));
        assert!(action_strs.contains(&"create_page"));
        assert!(action_strs.contains(&"update_page"));
        assert!(action_strs.contains(&"search"));
    }

    #[tokio::test]
    async fn execute_missing_action_returns_error() {
        let tool = test_tool();
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("action"));
    }

    #[tokio::test]
    async fn execute_unknown_action_returns_error() {
        let tool = test_tool();
        let result = tool.execute(json!({"action": "invalid"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn execute_query_database_missing_id_returns_error() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "query_database"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("database_id"));
    }

    #[tokio::test]
    async fn execute_read_page_missing_id_returns_error() {
        let tool = test_tool();
        let result = tool.execute(json!({"action": "read_page"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("page_id"));
    }

    #[tokio::test]
    async fn execute_create_page_missing_properties_returns_error() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "create_page"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("properties"));
    }

    #[tokio::test]
    async fn execute_update_page_missing_page_id_returns_error() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "update_page", "properties": {}}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("page_id"));
    }

    #[tokio::test]
    async fn execute_update_page_missing_properties_returns_error() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "update_page", "page_id": "test-id"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("properties"));
    }
}
