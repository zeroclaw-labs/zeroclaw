//! Notion API tool for agent-driven workspace interaction.
//!
//! Exposes Notion database queries, page reads, block reads/writes, page
//! creation/updates, and workspace search as a single LLM-callable tool
//! with an `action` parameter.

use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use anyhow::Context;
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use std::sync::Arc;

const NOTION_API_BASE: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2025-09-03";

/// Per-instance permission gates matching the Notion integration capability
/// model (<https://developers.notion.com/reference/capabilities>).
///
/// Configured via `allow_read`, `allow_insert`, `allow_update` in
/// `[channels_config.notion]`. These let the operator restrict which
/// categories of Notion API calls the agent may attempt — independent of the
/// global `SecurityPolicy` autonomy level.
#[derive(Debug, Clone)]
pub struct NotionPermissions {
    /// Notion "Read content" capability.
    pub allow_read: bool,
    /// Notion "Insert content" capability.
    pub allow_insert: bool,
    /// Notion "Update content" capability (includes trash and delete).
    pub allow_update: bool,
}

impl Default for NotionPermissions {
    fn default() -> Self {
        Self {
            allow_read: true,
            allow_insert: false,
            allow_update: false,
        }
    }
}

/// Tool that lets the agent interact with the Notion API.
pub struct NotionTool {
    api_key: String,
    security: Arc<SecurityPolicy>,
    permissions: NotionPermissions,
    client: reqwest::Client,
    base_url: String,
}

impl NotionTool {
    pub fn new(
        api_key: String,
        security: Arc<SecurityPolicy>,
        permissions: NotionPermissions,
    ) -> Self {
        Self {
            api_key,
            security,
            permissions,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            base_url: NOTION_API_BASE.to_string(),
        }
    }

    /// Create with a custom base URL (for testing against mock servers).
    #[cfg(test)]
    fn with_base_url(
        api_key: String,
        security: Arc<SecurityPolicy>,
        permissions: NotionPermissions,
        base_url: String,
    ) -> Self {
        Self {
            api_key,
            security,
            permissions,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            base_url,
        }
    }

    /// Build authorization headers. Returns `Err` if the API key contains
    /// characters invalid for HTTP headers.
    fn headers(&self) -> anyhow::Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        let auth_value = HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .context("Notion API key contains invalid header characters")?;
        headers.insert(AUTHORIZATION, auth_value);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("Notion-Version", HeaderValue::from_static(NOTION_VERSION));
        Ok(headers)
    }

    async fn api_get(&self, url: &str) -> anyhow::Result<Value> {
        let headers = self.headers()?;
        let resp = self.client.get(url).headers(headers).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated = crate::util::truncate_with_ellipsis(&body, 500);
            anyhow::bail!("Notion API error {status}: {truncated}");
        }
        resp.json().await.context("failed to parse Notion response")
    }

    async fn api_post(&self, url: &str, body: &Value) -> anyhow::Result<Value> {
        let headers = self.headers()?;
        let resp = self
            .client
            .post(url)
            .headers(headers)
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated = crate::util::truncate_with_ellipsis(&body, 500);
            anyhow::bail!("Notion API error {status}: {truncated}");
        }
        resp.json().await.context("failed to parse Notion response")
    }

    async fn api_patch(&self, url: &str, body: &Value) -> anyhow::Result<Value> {
        let headers = self.headers()?;
        let resp = self
            .client
            .patch(url)
            .headers(headers)
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated = crate::util::truncate_with_ellipsis(&body, 500);
            anyhow::bail!("Notion API error {status}: {truncated}");
        }
        resp.json().await.context("failed to parse Notion response")
    }

    async fn api_delete(&self, url: &str) -> anyhow::Result<Value> {
        let headers = self.headers()?;
        let resp = self.client.delete(url).headers(headers).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated = crate::util::truncate_with_ellipsis(&body, 500);
            anyhow::bail!("Notion API error {status}: {truncated}");
        }
        resp.json().await.context("failed to parse Notion response")
    }

    /// Validate that a string looks like a Notion UUID (with or without dashes).
    fn validate_id(id: &str, label: &str) -> anyhow::Result<()> {
        let cleaned: String = id.chars().filter(|c| *c != '-').collect();
        if cleaned.len() != 32 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!("{label} does not look like a valid Notion ID: {id}");
        }
        Ok(())
    }

    /// Resolve `data_source_id` from a `database_id` by calling
    /// `GET /databases/{database_id}` and reading the `data_sources` array.
    async fn resolve_data_source_id(&self, database_id: &str) -> anyhow::Result<String> {
        let url = format!("{}/databases/{database_id}", self.base_url);
        let resp = self.api_get(&url).await?;
        resp.get("data_sources")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|ds| ds.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Database {database_id} has no data_sources. \
                     Ensure the database is shared with your integration."
                )
            })
    }

    // ── Permission helpers ──────────────────────────────────────

    fn read_not_allowed() -> ToolResult {
        ToolResult {
            success: false,
            output: String::new(),
            error: Some("Notion Read content capability is disabled (allow_read = false)".into()),
        }
    }

    fn insert_not_allowed() -> ToolResult {
        ToolResult {
            success: false,
            output: String::new(),
            error: Some(
                "Notion Insert content capability is disabled (allow_insert = false)".into(),
            ),
        }
    }

    fn update_not_allowed() -> ToolResult {
        ToolResult {
            success: false,
            output: String::new(),
            error: Some(
                "Notion Update content capability is disabled (allow_update = false)".into(),
            ),
        }
    }

    // ── Read actions ──────────────────────────────────────────

    async fn query_database(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_read {
            return Ok(Self::read_not_allowed());
        }
        // Prefer explicit data_source_id; auto-resolve from database_id if only that is given
        let ds_id = if let Some(id) = args.get("data_source_id").and_then(|v| v.as_str()) {
            id.to_string()
        } else if let Some(db_id) = args.get("database_id").and_then(|v| v.as_str()) {
            Self::validate_id(db_id, "database_id")?;
            self.resolve_data_source_id(db_id).await?
        } else {
            anyhow::bail!("Missing 'data_source_id' (or 'database_id') parameter");
        };
        Self::validate_id(&ds_id, "data_source_id")?;

        let url = format!("{}/data_sources/{ds_id}/query", self.base_url);
        let mut body = json!({});
        if let Some(filter) = args.get("filter") {
            body["filter"] = filter.clone();
        }
        if let Some(sorts) = args.get("sorts") {
            body["sorts"] = sorts.clone();
        }

        let result = self.api_post(&url, &body).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn read_page(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_read {
            return Ok(Self::read_not_allowed());
        }
        let page_id = args
            .get("page_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'page_id' parameter"))?;
        Self::validate_id(page_id, "page_id")?;

        let url = format!("{}/pages/{page_id}", self.base_url);
        let result = self.api_get(&url).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn get_blocks(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_read {
            return Ok(Self::read_not_allowed());
        }
        let block_id = args
            .get("block_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("Missing 'block_id' parameter (use a page ID or block ID)")
            })?;
        Self::validate_id(block_id, "block_id")?;

        let url = format!("{}/blocks/{block_id}/children", self.base_url);
        let result = self.api_get(&url).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn search(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_read {
            return Ok(Self::read_not_allowed());
        }
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");

        let mut body = json!({ "query": query });
        if let Some(filter) = args.get("filter") {
            body["filter"] = filter.clone();
        }
        if let Some(sort) = args.get("sort") {
            body["sort"] = sort.clone();
        }
        if let Some(start_cursor) = args.get("start_cursor") {
            body["start_cursor"] = start_cursor.clone();
        }
        if let Some(page_size) = args.get("page_size") {
            body["page_size"] = page_size.clone();
        }

        let result = self
            .api_post(&format!("{}/search", self.base_url), &body)
            .await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn list_pages(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_read {
            return Ok(Self::read_not_allowed());
        }

        let mut body = json!({
            "filter": { "value": "page", "property": "object" }
        });
        if let Some(start_cursor) = args.get("start_cursor") {
            body["start_cursor"] = start_cursor.clone();
        }
        if let Some(page_size) = args.get("page_size") {
            body["page_size"] = page_size.clone();
        }

        let result = self
            .api_post(&format!("{}/search", self.base_url), &body)
            .await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn get_page_markdown(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_read {
            return Ok(Self::read_not_allowed());
        }

        let page_id = args
            .get("page_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'page_id' parameter"))?;
        Self::validate_id(page_id, "page_id")?;

        let url = format!("{}/pages/{page_id}/markdown", self.base_url);
        let result = self.api_get(&url).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    // ── Insert actions (Notion "Insert content" capability) ─────

    fn autonomy_blocked() -> ToolResult {
        ToolResult {
            success: false,
            output: String::new(),
            error: Some("Mutating operations are not allowed in read-only autonomy mode".into()),
        }
    }

    async fn create_page(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_insert {
            return Ok(Self::insert_not_allowed());
        }
        if !self.security.can_act() {
            return Ok(Self::autonomy_blocked());
        }

        let parent_id = args
            .get("database_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'database_id' parameter"))?;
        Self::validate_id(parent_id, "database_id")?;

        let properties = args
            .get("properties")
            .ok_or_else(|| anyhow::anyhow!("Missing 'properties' parameter"))?;

        let body = json!({
            "parent": { "database_id": parent_id },
            "properties": properties,
        });

        let result = self
            .api_post(&format!("{}/pages", self.base_url), &body)
            .await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn create_data_source(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_insert {
            return Ok(Self::insert_not_allowed());
        }
        if !self.security.can_act() {
            return Ok(Self::autonomy_blocked());
        }

        let parent_page_id = args
            .get("page_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'page_id' parameter (parent page)"))?;
        Self::validate_id(parent_page_id, "page_id")?;

        let title = args.get("title").ok_or_else(|| {
            anyhow::anyhow!(
                "Missing 'title' parameter (e.g. [{{\"text\": {{\"content\": \"My DB\"}}}}])"
            )
        })?;

        let properties = args.get("properties").ok_or_else(|| {
            anyhow::anyhow!(
                "Missing 'properties' parameter (schema definition, e.g. \
                 {{\"Name\": {{\"title\": {{}}}}, \"Status\": {{\"select\": {{\"options\": [...]}}}}}})"
            )
        })?;

        let mut body = json!({
            "parent": { "page_id": parent_page_id },
            "title": title,
            "properties": properties,
        });
        if let Some(is_inline) = args.get("is_inline") {
            body["is_inline"] = is_inline.clone();
        }

        let url = format!("{}/data_sources", self.base_url);
        let result = self.api_post(&url, &body).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn append_blocks(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_insert {
            return Ok(Self::insert_not_allowed());
        }
        if !self.security.can_act() {
            return Ok(Self::autonomy_blocked());
        }

        let block_id = args
            .get("block_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("Missing 'block_id' parameter (use a page ID or block ID)")
            })?;
        Self::validate_id(block_id, "block_id")?;

        let children = args.get("children").ok_or_else(|| {
            anyhow::anyhow!("Missing 'children' parameter (array of block objects)")
        })?;

        let body = json!({ "children": children });
        let url = format!("{}/blocks/{block_id}/children", self.base_url);
        let result = self.api_patch(&url, &body).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    // ── Update actions (Notion "Update content" capability) ───
    //
    // Per Notion docs, this capability also covers trash and delete_block.

    async fn update_page(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_update {
            return Ok(Self::update_not_allowed());
        }
        if !self.security.can_act() {
            return Ok(Self::autonomy_blocked());
        }

        let page_id = args
            .get("page_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'page_id' parameter"))?;
        Self::validate_id(page_id, "page_id")?;

        let properties = args
            .get("properties")
            .ok_or_else(|| anyhow::anyhow!("Missing 'properties' parameter"))?;

        let body = json!({ "properties": properties });
        let url = format!("{}/pages/{page_id}", self.base_url);
        let result = self.api_patch(&url, &body).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn update_block(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_update {
            return Ok(Self::update_not_allowed());
        }
        if !self.security.can_act() {
            return Ok(Self::autonomy_blocked());
        }

        let block_id = args
            .get("block_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'block_id' parameter"))?;
        Self::validate_id(block_id, "block_id")?;

        let block_content = args.get("block_content").ok_or_else(|| {
            anyhow::anyhow!(
                "Missing 'block_content' parameter. Provide the block type and its content, \
                     e.g. {{\"type\": \"paragraph\", \"paragraph\": {{\"rich_text\": [...]}}}}"
            )
        })?;

        let url = format!("{}/blocks/{block_id}", self.base_url);
        let result = self.api_patch(&url, block_content).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn update_page_markdown(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_update {
            return Ok(Self::update_not_allowed());
        }
        if !self.security.can_act() {
            return Ok(Self::autonomy_blocked());
        }

        let page_id = args
            .get("page_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'page_id' parameter"))?;
        Self::validate_id(page_id, "page_id")?;

        let markdown_body = args.get("markdown_body").ok_or_else(|| {
            anyhow::anyhow!(
                "Missing 'markdown_body' parameter. Provide an object with \
                     'type' (\"insert_content\" or \"replace_content_range\") and \
                     the corresponding payload."
            )
        })?;

        let url = format!("{}/pages/{page_id}/markdown", self.base_url);
        let result = self.api_patch(&url, markdown_body).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn trash_page(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_update {
            return Ok(Self::update_not_allowed());
        }
        if !self.security.can_act() {
            return Ok(Self::autonomy_blocked());
        }

        let page_id = args
            .get("page_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'page_id' parameter"))?;
        Self::validate_id(page_id, "page_id")?;

        let body = json!({ "in_trash": true });
        let url = format!("{}/pages/{page_id}", self.base_url);
        let result = self.api_patch(&url, &body).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }

    async fn delete_block(&self, args: &Value) -> anyhow::Result<ToolResult> {
        if !self.permissions.allow_update {
            return Ok(Self::update_not_allowed());
        }
        if !self.security.can_act() {
            return Ok(Self::autonomy_blocked());
        }

        let block_id = args
            .get("block_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'block_id' parameter"))?;
        Self::validate_id(block_id, "block_id")?;

        let url = format!("{}/blocks/{block_id}", self.base_url);
        let result = self.api_delete(&url).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)?,
            error: None,
        })
    }
}

#[async_trait]
impl Tool for NotionTool {
    fn name(&self) -> &str {
        "notion"
    }

    fn description(&self) -> &str {
        "Interact with the Notion API (version 2025-09-03). \
         Read: search, list_pages, read_page, get_blocks, get_page_markdown, query_database. \
         Insert: create_page, create_data_source, append_blocks. \
         Update: update_page, update_block, update_page_markdown, trash_page, delete_block."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "search", "list_pages", "read_page", "get_blocks",
                        "get_page_markdown", "query_database",
                        "create_page", "create_data_source", "append_blocks",
                        "update_page", "update_block", "update_page_markdown",
                        "trash_page", "delete_block"
                    ],
                    "description": "The Notion API action to perform"
                },
                "data_source_id": {
                    "type": "string",
                    "description": "Notion data source ID (for query_database). In API 2025-09-03, databases have a separate data_source_id used for queries."
                },
                "database_id": {
                    "type": "string",
                    "description": "Notion database ID (for create_page parent). Also accepted as fallback for query_database if data_source_id is not provided."
                },
                "page_id": {
                    "type": "string",
                    "description": "Notion page ID (for read_page, get_page_markdown, update_page, update_page_markdown, trash_page, create_data_source parent)"
                },
                "block_id": {
                    "type": "string",
                    "description": "Block or page ID (for get_blocks, update_block, delete_block, append_blocks). Use a page ID to get/add top-level content."
                },
                "block_content": {
                    "type": "object",
                    "description": "Block update payload (for update_block). Must include the block type and its content. Example: {\"type\": \"paragraph\", \"paragraph\": {\"rich_text\": [{\"text\": {\"content\": \"text\"}, \"annotations\": {\"strikethrough\": true}}]}}"
                },
                "markdown_body": {
                    "type": "object",
                    "description": "Markdown update payload (for update_page_markdown). Either {\"type\": \"insert_content\", \"insert_content\": {\"content\": \"# heading\\ntext\", \"after\": \"start...end\"}} or {\"type\": \"replace_content_range\", \"replace_content_range\": {\"content\": \"new\", \"content_range\": \"old start...old end\", \"allow_deleting_content\": true}}."
                },
                "properties": {
                    "type": "object",
                    "description": "Page properties (for create_page, update_page) or schema (for create_data_source)."
                },
                "title": {
                    "type": "array",
                    "description": "Data source title (for create_data_source). Example: [{\"text\": {\"content\": \"My Database\"}}]"
                },
                "is_inline": {
                    "type": "boolean",
                    "description": "Set true to embed the data source inline in the parent page (for create_data_source)."
                },
                "children": {
                    "type": "array",
                    "description": "Array of block objects to append (for append_blocks)."
                },
                "filter": {
                    "type": "object",
                    "description": "Filter object. For search: {\"property\": \"object\", \"value\": \"page\"} or {\"property\": \"object\", \"value\": \"data_source\"} (API 2025-09-03 uses 'data_source', not 'database'). For query_database: Notion database filter."
                },
                "sorts": {
                    "type": "array",
                    "description": "Sort criteria (for query_database). Example: [{\"property\": \"Date\", \"direction\": \"descending\"}]"
                },
                "sort": {
                    "type": "object",
                    "description": "Sort for search results. Example: {\"direction\": \"descending\", \"timestamp\": \"last_edited_time\"}"
                },
                "query": {
                    "type": "string",
                    "description": "Search query string (for search). Omit or leave empty to list all accessible objects."
                },
                "start_cursor": {
                    "type": "string",
                    "description": "Pagination cursor (for search, list_pages). Use next_cursor from a previous response to get the next page."
                },
                "page_size": {
                    "type": "integer",
                    "description": "Number of results per page (for search, list_pages). Max 100, default 100."
                }
            },
            "additionalProperties": false,
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        if self.security.is_rate_limited() {
            tracing::warn!("Notion tool: rate limit exceeded, rejecting call");
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded".into()),
            });
        }

        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        self.security.record_action();

        match action {
            // Read content
            "search" => self.search(&args).await,
            "list_pages" => self.list_pages(&args).await,
            "read_page" => self.read_page(&args).await,
            "get_blocks" => self.get_blocks(&args).await,
            "get_page_markdown" => self.get_page_markdown(&args).await,
            "query_database" => self.query_database(&args).await,
            // Insert content
            "create_page" => self.create_page(&args).await,
            "create_data_source" => self.create_data_source(&args).await,
            "append_blocks" => self.append_blocks(&args).await,
            // Update content
            "update_page" => self.update_page(&args).await,
            "update_block" => self.update_block(&args).await,
            "update_page_markdown" => self.update_page_markdown(&args).await,
            "trash_page" => self.trash_page(&args).await,
            "delete_block" => self.delete_block(&args).await,
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action: {action}. Valid: search, list_pages, read_page, \
                     get_blocks, get_page_markdown, query_database, create_page, \
                     create_data_source, append_blocks, update_page, update_block, \
                     update_page_markdown, trash_page, delete_block"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TEST_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn dummy_tool() -> NotionTool {
        NotionTool::new(
            "test-key".into(),
            Arc::new(SecurityPolicy::default()),
            NotionPermissions {
                allow_read: true,
                allow_insert: true,
                allow_update: true,
            },
        )
    }

    fn mock_tool(base_url: String) -> NotionTool {
        NotionTool::with_base_url(
            "test-key".into(),
            Arc::new(SecurityPolicy::default()),
            NotionPermissions {
                allow_read: true,
                allow_insert: true,
                allow_update: true,
            },
            base_url,
        )
    }

    // ── Spec / schema ──────────────────────────────────────────

    #[test]
    fn tool_spec_matches() {
        let tool = dummy_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "notion");
        assert!(spec.description.contains("Notion API"));
        assert_eq!(spec.parameters["required"][0], "action");
    }

    #[test]
    fn parameters_schema_has_all_actions() {
        let tool = dummy_tool();
        let schema = tool.parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert_eq!(actions.len(), 14);
        let names: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        for expected in [
            "search",
            "list_pages",
            "read_page",
            "get_blocks",
            "get_page_markdown",
            "query_database",
            "create_page",
            "create_data_source",
            "append_blocks",
            "update_page",
            "update_block",
            "update_page_markdown",
            "trash_page",
            "delete_block",
        ] {
            assert!(names.contains(&expected), "missing action: {expected}");
        }
    }

    #[test]
    fn schema_has_block_params() {
        let tool = dummy_tool();
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["block_id"].is_object());
        assert!(schema["properties"]["children"].is_object());
    }

    // ── ID validation ──────────────────────────────────────────

    #[test]
    fn validate_id_accepts_valid_uuid() {
        assert!(NotionTool::validate_id(TEST_UUID, "test").is_ok());
    }

    #[test]
    fn validate_id_accepts_no_dashes() {
        assert!(NotionTool::validate_id("550e8400e29b41d4a716446655440000", "test").is_ok());
    }

    #[test]
    fn validate_id_rejects_short() {
        assert!(NotionTool::validate_id("abc123", "test").is_err());
    }

    #[test]
    fn validate_id_rejects_non_hex() {
        assert!(NotionTool::validate_id("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz", "test").is_err());
    }

    // ── Dispatch / param validation ────────────────────────────

    #[tokio::test]
    async fn execute_missing_action() {
        let tool = dummy_tool();
        let result = tool.execute(json!({})).await;
        assert!(result.is_err() || !result.unwrap().success);
    }

    #[tokio::test]
    async fn execute_unknown_action() {
        let tool = dummy_tool();
        let result = tool
            .execute(json!({"action": "delete_everything"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn read_page_missing_page_id_fails() {
        let tool = dummy_tool();
        let result = tool.execute(json!({"action": "read_page"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn query_database_missing_id_fails() {
        let tool = dummy_tool();
        let result = tool.execute(json!({"action": "query_database"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_page_invalid_id_fails() {
        let tool = dummy_tool();
        let result = tool
            .execute(json!({"action": "read_page", "page_id": "not-a-uuid"}))
            .await;
        assert!(result.is_err());
    }

    // ── HTTP-level tests (wiremock) ────────────────────────────

    #[tokio::test]
    async fn search_sends_correct_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(header("authorization", "Bearer test-key"))
            .and(header("notion-version", NOTION_VERSION))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"results": [], "has_more": false})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({"action": "search", "query": "meeting notes"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("results"));
    }

    #[tokio::test]
    async fn search_works_without_query_param() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"results": [], "has_more": false})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool.execute(json!({"action": "search"})).await.unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn search_passes_filter_sort_pagination() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"results": [{"object": "page"}], "has_more": false})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({
                "action": "search",
                "query": "",
                "filter": {"value": "page", "property": "object"},
                "sort": {"direction": "descending", "timestamp": "last_edited_time"},
                "page_size": 10
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("page"));
    }

    #[tokio::test]
    async fn list_pages_sends_page_filter() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"results": [{"object": "page", "id": "p1"}], "has_more": false}),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool.execute(json!({"action": "list_pages"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("p1"));
    }

    #[tokio::test]
    async fn list_pages_blocked_when_allow_read_false() {
        let perms = NotionPermissions {
            allow_read: false,
            ..NotionPermissions::default()
        };
        let tool = NotionTool::with_base_url(
            "test-key".into(),
            Arc::new(SecurityPolicy::default()),
            perms,
            "http://unused".into(),
        );

        let result = tool.execute(json!({"action": "list_pages"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("allow_read"));
    }

    #[tokio::test]
    async fn read_page_sends_get_to_correct_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/pages/{TEST_UUID}")))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"object": "page", "id": TEST_UUID})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({"action": "read_page", "page_id": TEST_UUID}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains(TEST_UUID));
    }

    #[tokio::test]
    async fn get_blocks_sends_get_to_children_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/blocks/{TEST_UUID}/children")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"results": [], "has_more": false})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({"action": "get_blocks", "block_id": TEST_UUID}))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn query_database_sends_post_with_filter() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/data_sources/{TEST_UUID}/query")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"results": [{"id": "row-1"}], "has_more": false})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({
                "action": "query_database",
                "data_source_id": TEST_UUID,
                "filter": {"property": "Status", "select": {"equals": "Done"}}
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("row-1"));
    }

    #[tokio::test]
    async fn query_database_falls_back_to_database_id() {
        let resolved_ds_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let server = MockServer::start().await;

        // Mock the database lookup that returns the data_source_id
        Mock::given(method("GET"))
            .and(path(format!("/databases/{TEST_UUID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "database",
                "id": TEST_UUID,
                "data_sources": [{"id": resolved_ds_id, "name": "Default"}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        // Mock the query using the resolved data_source_id
        Mock::given(method("POST"))
            .and(path(format!("/data_sources/{resolved_ds_id}/query")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"results": [], "has_more": false})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({
                "action": "query_database",
                "database_id": TEST_UUID
            }))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn create_page_sends_post_to_pages() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/pages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"object": "page", "id": "new-page-id"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({
                "action": "create_page",
                "database_id": TEST_UUID,
                "properties": {"Name": {"title": [{"text": {"content": "Test"}}]}}
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("new-page-id"));
    }

    #[tokio::test]
    async fn update_page_sends_patch() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path(format!("/pages/{TEST_UUID}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"object": "page", "id": TEST_UUID})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({
                "action": "update_page",
                "page_id": TEST_UUID,
                "properties": {"Status": {"select": {"name": "Done"}}}
            }))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn append_blocks_sends_patch_to_children() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path(format!("/blocks/{TEST_UUID}/children")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"results": [{"object": "block"}]})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({
                "action": "append_blocks",
                "block_id": TEST_UUID,
                "children": [{"object": "block", "type": "paragraph", "paragraph": {"rich_text": [{"text": {"content": "Hello"}}]}}]
            }))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn create_data_source_sends_post() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/data_sources"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "data_source",
                "id": "new-ds-id"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({
                "action": "create_data_source",
                "page_id": TEST_UUID,
                "title": [{"text": {"content": "My Database"}}],
                "properties": {
                    "Name": {"title": {}},
                    "Status": {"select": {"options": [{"name": "Todo"}, {"name": "Done"}]}}
                }
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("new-ds-id"));
    }

    #[tokio::test]
    async fn update_block_sends_patch_to_block_path() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path(format!("/blocks/{TEST_UUID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "block",
                "id": TEST_UUID,
                "type": "bulleted_list_item"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({
                "action": "update_block",
                "block_id": TEST_UUID,
                "block_content": {
                    "type": "bulleted_list_item",
                    "bulleted_list_item": {
                        "rich_text": [{
                            "text": {"content": "Harshal Valley"},
                            "annotations": {"strikethrough": true}
                        }]
                    }
                }
            }))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn update_block_missing_content_fails() {
        let tool = dummy_tool();
        let result = tool
            .execute(json!({"action": "update_block", "block_id": TEST_UUID}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_block_sends_delete_request() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path(format!("/blocks/{TEST_UUID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "object": "block",
                "id": TEST_UUID,
                "in_trash": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({"action": "delete_block", "block_id": TEST_UUID}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("in_trash"));
    }

    // ── Error handling ─────────────────────────────────────────

    #[tokio::test]
    async fn api_401_returns_err_with_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/pages/{TEST_UUID}")))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(json!({"message": "API token is invalid"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({"action": "read_page", "page_id": TEST_UUID}))
            .await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("401"), "expected 401 in error: {err}");
    }

    #[tokio::test]
    async fn api_404_returns_err() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/data_sources/{TEST_UUID}/query")))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({"object": "error", "code": "object_not_found"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({"action": "query_database", "data_source_id": TEST_UUID}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn api_500_returns_err() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .expect(1)
            .mount(&server)
            .await;

        let tool = mock_tool(server.uri());
        let result = tool
            .execute(json!({"action": "search", "query": "test"}))
            .await;
        assert!(result.is_err());
    }

    // ── Security policy ────────────────────────────────────────

    #[tokio::test]
    async fn mutating_actions_blocked_in_read_only_autonomy() {
        let policy = SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        };
        let tool = NotionTool::with_base_url(
            "test-key".into(),
            Arc::new(policy),
            NotionPermissions {
                allow_read: true,
                allow_insert: true,
                allow_update: true,
            },
            "http://unused".into(),
        );

        for action in [
            "create_page",
            "create_data_source",
            "append_blocks",
            "update_page",
            "update_block",
            "update_page_markdown",
            "trash_page",
            "delete_block",
        ] {
            let result = tool
                .execute(json!({
                    "action": action,
                    "database_id": TEST_UUID,
                    "page_id": TEST_UUID,
                    "block_id": TEST_UUID,
                    "properties": {},
                    "title": [{"text": {"content": "test"}}],
                    "children": [],
                    "block_content": {"type": "paragraph", "paragraph": {"rich_text": []}},
                    "markdown_body": {"type": "insert_content", "insert_content": {"content": "t"}}
                }))
                .await
                .unwrap();
            assert!(
                !result.success,
                "{action} should be blocked in read-only autonomy"
            );
            assert!(
                result.error.as_deref().unwrap().contains("read-only"),
                "{action} error should mention read-only"
            );
        }
    }

    #[tokio::test]
    async fn rate_limiter_does_not_false_positive_on_fresh_policy() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"results": [], "has_more": false})),
            )
            .expect(1)
            .mount(&server)
            .await;

        // Fresh default policy should NOT rate-limit on first call
        let tool = mock_tool(server.uri());
        let result = tool.execute(json!({"action": "search"})).await.unwrap();
        assert!(
            result.success,
            "first call should not be rate-limited; error: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn rate_limiter_blocks_when_budget_exhausted() {
        let policy = SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        };
        let tool = NotionTool::with_base_url(
            "test-key".into(),
            Arc::new(policy),
            NotionPermissions::default(),
            "http://unused".into(),
        );

        let result = tool.execute(json!({"action": "search"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Rate limit"));
    }

    // ── Notion capability permission gates ─────────────────────

    #[tokio::test]
    async fn read_actions_blocked_when_allow_read_false() {
        let perms = NotionPermissions {
            allow_read: false,
            ..NotionPermissions::default()
        };
        let tool = NotionTool::with_base_url(
            "test-key".into(),
            Arc::new(SecurityPolicy::default()),
            perms,
            "http://unused".into(),
        );

        for action in [
            "search",
            "list_pages",
            "read_page",
            "get_blocks",
            "get_page_markdown",
            "query_database",
        ] {
            let result = tool
                .execute(json!({
                    "action": action,
                    "page_id": TEST_UUID,
                    "block_id": TEST_UUID,
                    "data_source_id": TEST_UUID,
                    "query": "test"
                }))
                .await
                .unwrap();
            assert!(
                !result.success,
                "{action} should be blocked when allow_read=false"
            );
            assert!(
                result.error.as_deref().unwrap().contains("allow_read"),
                "{action} error should mention allow_read"
            );
        }
    }

    #[tokio::test]
    async fn insert_actions_blocked_when_allow_insert_false() {
        let perms = NotionPermissions {
            allow_insert: false,
            ..NotionPermissions::default()
        };
        let tool = NotionTool::with_base_url(
            "test-key".into(),
            Arc::new(SecurityPolicy::default()),
            perms,
            "http://unused".into(),
        );

        for action in ["create_page", "create_data_source", "append_blocks"] {
            let result = tool
                .execute(json!({
                    "action": action,
                    "database_id": TEST_UUID,
                    "page_id": TEST_UUID,
                    "block_id": TEST_UUID,
                    "properties": {},
                    "title": [{"text": {"content": "test"}}],
                    "children": []
                }))
                .await
                .unwrap();
            assert!(
                !result.success,
                "{action} should be blocked when allow_insert=false"
            );
            assert!(
                result.error.as_deref().unwrap().contains("allow_insert"),
                "{action} error should mention allow_insert"
            );
        }
    }

    #[tokio::test]
    async fn update_actions_blocked_when_allow_update_false() {
        let perms = NotionPermissions {
            allow_update: false,
            ..NotionPermissions::default()
        };
        let tool = NotionTool::with_base_url(
            "test-key".into(),
            Arc::new(SecurityPolicy::default()),
            perms,
            "http://unused".into(),
        );

        for action in [
            "update_page",
            "update_block",
            "update_page_markdown",
            "trash_page",
            "delete_block",
        ] {
            let result = tool
                .execute(json!({
                    "action": action,
                    "page_id": TEST_UUID,
                    "block_id": TEST_UUID,
                    "properties": {},
                    "block_content": {"type": "paragraph", "paragraph": {"rich_text": []}},
                    "markdown_body": {"type": "insert_content", "insert_content": {"content": "test"}}
                }))
                .await
                .unwrap();
            assert!(
                !result.success,
                "{action} should be blocked when allow_update=false"
            );
            assert!(
                result.error.as_deref().unwrap().contains("allow_update"),
                "{action} error should mention allow_update"
            );
        }
    }

    #[tokio::test]
    async fn read_only_config_still_allows_reads() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"results": [], "has_more": false})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let perms = NotionPermissions {
            allow_read: true,
            allow_insert: false,
            allow_update: false,
        };
        let tool = NotionTool::with_base_url(
            "test-key".into(),
            Arc::new(SecurityPolicy::default()),
            perms,
            server.uri(),
        );

        let result = tool
            .execute(json!({"action": "search", "query": "test"}))
            .await
            .unwrap();
        assert!(result.success, "search should succeed with allow_read=true");
    }
}
