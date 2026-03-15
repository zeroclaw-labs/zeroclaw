//! Notion database poller channel.
//!
//! Polls a Notion database for rows with a pending status, dispatches them as
//! [`ChannelMessage`] items, and writes the agent response back to the result
//! property. Designed for task-queue workflows where users create Notion
//! database entries and the agent processes them asynchronously.

use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::{bail, Context};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const NOTION_API_BASE: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2025-09-03";
/// Maximum result length written back to Notion (rich_text cap is 2000 chars).
const MAX_RESULT_LENGTH: usize = 1900;

/// Construction parameters for [`NotionChannel`].
pub struct NotionChannelConfig {
    pub api_key: String,
    pub database_id: String,
    /// Data source ID for queries (API 2025-09-03). Falls back to `database_id`.
    pub data_source_id: Option<String>,
    pub poll_interval_secs: u64,
    pub status_property: String,
    pub input_property: String,
    pub result_property: String,
    pub pending_value: String,
    pub running_value: String,
    pub done_value: String,
    pub error_value: String,
    pub status_type: NotionStatusType,
    pub recover_stale: bool,
}

/// A channel that polls a Notion database for pending tasks.
pub struct NotionChannel {
    api_key: String,
    database_id: String,
    /// Resolved data source ID for queries (API 2025-09-03).
    /// Starts as `data_source_id` config or `database_id` fallback, then
    /// auto-resolved from `GET /databases/{id}` on first listen.
    query_id: RwLock<String>,
    poll_interval_secs: u64,
    status_property: String,
    input_property: String,
    result_property: String,
    pending_value: String,
    running_value: String,
    done_value: String,
    error_value: String,
    status_type: NotionStatusType,
    recover_stale: bool,
    /// In-flight page IDs to prevent duplicate dispatch.
    in_flight: Mutex<HashSet<String>>,
    client: reqwest::Client,
}

/// Whether the status property is a `select` or `status` type in Notion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotionStatusType {
    Select,
    Status,
}

impl NotionChannel {
    pub fn new(cfg: NotionChannelConfig) -> Self {
        let initial_query_id = cfg
            .data_source_id
            .unwrap_or_else(|| cfg.database_id.clone());
        Self {
            api_key: cfg.api_key,
            database_id: cfg.database_id,
            query_id: RwLock::new(initial_query_id),
            poll_interval_secs: cfg.poll_interval_secs,
            status_property: cfg.status_property,
            input_property: cfg.input_property,
            result_property: cfg.result_property,
            pending_value: cfg.pending_value,
            running_value: cfg.running_value,
            done_value: cfg.done_value,
            error_value: cfg.error_value,
            status_type: cfg.status_type,
            recover_stale: cfg.recover_stale,
            in_flight: Mutex::new(HashSet::new()),
            client: reqwest::Client::new(),
        }
    }

    /// Read the current resolved query ID.
    fn get_query_id(&self) -> String {
        self.query_id.read().unwrap().clone()
    }

    /// Resolve `data_source_id` from `GET /databases/{database_id}`.
    /// The response contains a `data_sources` array; we take the first entry's `id`.
    /// On failure, falls back to using `database_id` directly.
    async fn resolve_data_source_id(&self) {
        let url = format!("{NOTION_API_BASE}/databases/{}", self.database_id);
        match self.api_call("GET", &url, None).await {
            Ok(resp) => {
                if let Some(ds_arr) = resp.get("data_sources").and_then(|v| v.as_array()) {
                    if let Some(first_id) = ds_arr
                        .first()
                        .and_then(|ds| ds.get("id"))
                        .and_then(|v| v.as_str())
                    {
                        tracing::info!(
                            "Notion: resolved data_source_id={first_id} from database_id={}",
                            self.database_id
                        );
                        if let Ok(mut qid) = self.query_id.write() {
                            *qid = first_id.to_string();
                        }
                        return;
                    }
                }
                tracing::warn!(
                    "Notion: database response had no data_sources array; using database_id as fallback"
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Notion: failed to resolve data_source_id from database_id={}: {e}; using database_id as fallback",
                    self.database_id
                );
            }
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

    /// Make an API call to Notion with retry on 429/5xx.
    async fn api_call(
        &self,
        method: &str,
        url: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<Value> {
        let headers = self.headers()?;
        let mut backoff = 1u64;

        for attempt in 0..4u32 {
            let builder = match method {
                "POST" => self.client.post(url),
                "PATCH" => self.client.patch(url),
                "GET" => self.client.get(url),
                _ => bail!("unsupported HTTP method: {method}"),
            };

            let mut req = builder.headers(headers.clone());
            if let Some(b) = body {
                req = req.json(b);
            }

            let resp = req.send().await?;
            let status = resp.status();

            if status.is_success() {
                return resp
                    .json()
                    .await
                    .context("failed to parse Notion response JSON");
            }

            let body_text = resp.text().await.unwrap_or_default();
            let truncated = crate::util::truncate_with_ellipsis(&body_text, 500);

            if (status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
                && attempt < 3
            {
                tracing::warn!(
                    "Notion API {status} on attempt {attempt}, retrying in {backoff}s: {truncated}"
                );
                tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(16);
                continue;
            }

            bail!("Notion API error {status}: {truncated}");
        }

        bail!("Notion API: max retries exceeded")
    }

    /// Query the data source for rows matching the pending status.
    async fn query_pending(&self) -> anyhow::Result<Vec<Value>> {
        let qid = self.get_query_id();
        let url = format!("{NOTION_API_BASE}/data_sources/{qid}/query");

        let filter = match self.status_type {
            NotionStatusType::Select => serde_json::json!({
                "filter": {
                    "property": self.status_property,
                    "select": { "equals": self.pending_value }
                }
            }),
            NotionStatusType::Status => serde_json::json!({
                "filter": {
                    "property": self.status_property,
                    "status": { "equals": self.pending_value }
                }
            }),
        };

        let result = self.api_call("POST", &url, Some(&filter)).await?;
        let results = result
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(results)
    }

    /// Update a page's status property.
    async fn set_status(&self, page_id: &str, status_value: &str) -> anyhow::Result<()> {
        let url = format!("{NOTION_API_BASE}/pages/{page_id}");

        let payload = match self.status_type {
            NotionStatusType::Select => serde_json::json!({
                "properties": {
                    self.status_property.clone(): {
                        "select": { "name": status_value }
                    }
                }
            }),
            NotionStatusType::Status => serde_json::json!({
                "properties": {
                    self.status_property.clone(): {
                        "status": { "name": status_value }
                    }
                }
            }),
        };

        self.api_call("PATCH", &url, Some(&payload)).await?;
        Ok(())
    }

    /// Write the agent result back to the result property.
    async fn write_result(&self, page_id: &str, text: &str) -> anyhow::Result<()> {
        let url = format!("{NOTION_API_BASE}/pages/{page_id}");
        let truncated = truncate_result(text, MAX_RESULT_LENGTH);

        let payload = serde_json::json!({
            "properties": {
                self.result_property.clone(): {
                    "rich_text": [{
                        "type": "text",
                        "text": { "content": truncated }
                    }]
                }
            }
        });

        self.api_call("PATCH", &url, Some(&payload)).await?;
        Ok(())
    }

    /// Extract text from a page's input property.
    fn extract_input(page: &Value, property_name: &str) -> String {
        let props = page.get("properties").and_then(|p| p.get(property_name));

        if let Some(prop) = props {
            // Try title type
            if let Some(title_arr) = prop.get("title").and_then(|t| t.as_array()) {
                return title_arr
                    .iter()
                    .filter_map(|t| t.get("plain_text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("");
            }
            // Try rich_text type
            if let Some(rt_arr) = prop.get("rich_text").and_then(|t| t.as_array()) {
                return rt_arr
                    .iter()
                    .filter_map(|t| t.get("plain_text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("");
            }
        }

        String::new()
    }

    /// Attempt to claim a page for processing (prevent duplicates).
    fn try_claim(&self, page_id: &str) -> bool {
        if let Ok(mut set) = self.in_flight.lock() {
            set.insert(page_id.to_string())
        } else {
            false
        }
    }

    /// Release a claimed page after processing.
    fn release(&self, page_id: &str) {
        if let Ok(mut set) = self.in_flight.lock() {
            set.remove(page_id);
        }
    }

    /// On startup, reset any pages stuck in "running" status back to "pending".
    async fn recover_stale_tasks(&self) {
        let qid = self.get_query_id();
        let url = format!("{NOTION_API_BASE}/data_sources/{qid}/query");

        let filter = match self.status_type {
            NotionStatusType::Select => serde_json::json!({
                "filter": {
                    "property": self.status_property,
                    "select": { "equals": self.running_value }
                }
            }),
            NotionStatusType::Status => serde_json::json!({
                "filter": {
                    "property": self.status_property,
                    "status": { "equals": self.running_value }
                }
            }),
        };

        match self.api_call("POST", &url, Some(&filter)).await {
            Ok(result) => {
                let pages = result
                    .get("results")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                for page in pages {
                    if let Some(page_id) = page.get("id").and_then(|v| v.as_str()) {
                        tracing::info!("Notion: recovering stale task {}", page_id);
                        if let Err(e) = self.set_status(page_id, &self.pending_value).await {
                            tracing::warn!("Notion: failed to recover task {page_id}: {e}");
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Notion: stale task recovery query failed: {e}");
            }
        }
    }
}

#[async_trait]
impl Channel for NotionChannel {
    fn name(&self) -> &str {
        "notion"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // The recipient is the page ID. Write result and mark done.
        let page_id = &message.recipient;
        let res = async {
            self.write_result(page_id, &message.content).await?;
            self.set_status(page_id, &self.done_value).await?;
            Ok(())
        }
        .await;

        self.release(page_id);
        res
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Auto-resolve data_source_id from database_id (API 2025-09-03)
        self.resolve_data_source_id().await;

        // Recover stale tasks on startup
        if self.recover_stale {
            self.recover_stale_tasks().await;
        }

        let qid = self.get_query_id();
        tracing::info!(
            "Notion channel: polling data_source {qid} (database {}) every {}s",
            self.database_id,
            self.poll_interval_secs
        );

        let interval = std::time::Duration::from_secs(self.poll_interval_secs);

        loop {
            match self.query_pending().await {
                Ok(pages) => {
                    for page in pages {
                        let page_id = match page.get("id").and_then(|v| v.as_str()) {
                            Some(id) => id.to_string(),
                            None => continue,
                        };

                        if !self.try_claim(&page_id) {
                            continue; // already in flight
                        }

                        // Mark as running
                        if let Err(e) = self.set_status(&page_id, &self.running_value).await {
                            tracing::warn!(
                                "Notion: failed to set running status for {page_id}: {e}"
                            );
                            self.release(&page_id);
                            continue;
                        }

                        let input_text = Self::extract_input(&page, &self.input_property);
                        if input_text.is_empty() {
                            tracing::debug!("Notion: skipping page {page_id} with empty input");
                            // Mark as error since there's nothing to process
                            let _ = self.set_status(&page_id, &self.error_value).await;
                            let _ = self.write_result(&page_id, "Empty input").await;
                            self.release(&page_id);
                            continue;
                        }

                        let timestamp = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);

                        let msg = ChannelMessage {
                            id: page_id.clone(),
                            sender: format!("notion:{}", page_id),
                            reply_target: page_id,
                            content: input_text,
                            channel: "notion".into(),
                            timestamp,
                            thread_ts: None,
                        };

                        if tx.send(msg).await.is_err() {
                            tracing::warn!("Notion: message channel closed, stopping listener");
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Notion: poll failed: {e}");
                }
            }

            tokio::time::sleep(interval).await;
        }
    }

    async fn health_check(&self) -> bool {
        self.resolve_data_source_id().await;
        let qid = self.get_query_id();
        let url = format!("{NOTION_API_BASE}/data_sources/{qid}");
        self.api_call("GET", &url, None).await.is_ok()
    }
}

/// Truncate result text to fit Notion's rich_text limit, respecting UTF-8 boundaries.
fn truncate_result(text: &str, max_len: usize) -> &str {
    if text.len() <= max_len {
        text
    } else {
        let boundary = crate::util::floor_utf8_char_boundary(text, max_len);
        &text[..boundary]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> NotionChannelConfig {
        NotionChannelConfig {
            api_key: "test-key".into(),
            database_id: "db-id".into(),
            data_source_id: None,
            poll_interval_secs: 5,
            status_property: "Status".into(),
            input_property: "Input".into(),
            result_property: "Result".into(),
            pending_value: "Pending".into(),
            running_value: "Running".into(),
            done_value: "Done".into(),
            error_value: "Error".into(),
            status_type: NotionStatusType::Select,
            recover_stale: false,
        }
    }

    fn test_channel() -> NotionChannel {
        NotionChannel::new(NotionChannelConfig {
            api_key: "test-key".into(),
            database_id: "db-id".into(),
            data_source_id: None,
            poll_interval_secs: 5,
            status_property: "Status".into(),
            input_property: "Input".into(),
            result_property: "Result".into(),
            pending_value: "Pending".into(),
            running_value: "Running".into(),
            done_value: "Done".into(),
            error_value: "Error".into(),
            status_type: NotionStatusType::Select,
            recover_stale: false,
        })
    }

    #[test]
    fn try_claim_prevents_duplicate() {
        let channel = test_channel();

        assert!(channel.try_claim("page-1"));
        assert!(!channel.try_claim("page-1")); // duplicate
        assert!(channel.try_claim("page-2")); // different page
    }

    #[test]
    fn release_allows_reclaim() {
        let channel = test_channel();

        assert!(channel.try_claim("page-1"));
        channel.release("page-1");
        assert!(channel.try_claim("page-1")); // can reclaim after release
    }

    #[test]
    fn truncate_result_within_limit() {
        assert_eq!(truncate_result("hello", 10), "hello");
    }

    #[test]
    fn truncate_result_over_limit() {
        let text = "abcdefghij";
        assert_eq!(truncate_result(text, 5), "abcde");
    }

    #[test]
    fn truncate_result_multibyte_safe() {
        let text = "hello 測試 world";
        // '測' is 3 bytes at offset 6; cutting at 7 would split it
        let result = truncate_result(text, 7);
        assert!(result.len() <= 7);
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn extract_input_from_title() {
        let page = serde_json::json!({
            "properties": {
                "Input": {
                    "title": [
                        { "plain_text": "Hello " },
                        { "plain_text": "World" }
                    ]
                }
            }
        });
        assert_eq!(NotionChannel::extract_input(&page, "Input"), "Hello World");
    }

    #[test]
    fn extract_input_from_rich_text() {
        let page = serde_json::json!({
            "properties": {
                "Input": {
                    "rich_text": [
                        { "plain_text": "query content" }
                    ]
                }
            }
        });
        assert_eq!(
            NotionChannel::extract_input(&page, "Input"),
            "query content"
        );
    }

    #[test]
    fn extract_input_missing_property() {
        let page = serde_json::json!({ "properties": {} });
        assert_eq!(NotionChannel::extract_input(&page, "Input"), "");
    }

    #[test]
    fn status_payload_select_type() {
        let channel = test_channel();
        assert_eq!(channel.status_type, NotionStatusType::Select);
    }

    #[test]
    fn status_payload_status_type() {
        let channel = NotionChannel::new(NotionChannelConfig {
            status_type: NotionStatusType::Status,
            ..test_config()
        });
        assert_eq!(channel.status_type, NotionStatusType::Status);
    }
}
