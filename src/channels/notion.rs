use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::{bail, Result};
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

const NOTION_API_BASE: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2022-06-28";
const MAX_RESULT_LENGTH: usize = 2000;
const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY_MS: u64 = 2000;
/// Maximum number of characters to include from an error response body.
const MAX_ERROR_BODY_CHARS: usize = 500;

/// Find the largest byte index <= `max_bytes` that falls on a UTF-8 char boundary.
fn floor_utf8_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Notion channel — polls a Notion database for pending tasks and writes results back.
///
/// The channel connects to the Notion API, queries a database for rows with a "pending"
/// status, dispatches them as channel messages, and writes results back when processing
/// completes. It supports crash recovery by resetting stale "running" tasks on startup.
pub struct NotionChannel {
    api_key: String,
    database_id: String,
    poll_interval_secs: u64,
    status_property: String,
    input_property: String,
    result_property: String,
    max_concurrent: usize,
    status_type: Arc<RwLock<String>>,
    inflight: Arc<RwLock<HashSet<String>>>,
    http: reqwest::Client,
    recover_stale: bool,
}

impl NotionChannel {
    /// Create a new Notion channel with the given configuration.
    pub fn new(
        api_key: String,
        database_id: String,
        poll_interval_secs: u64,
        status_property: String,
        input_property: String,
        result_property: String,
        max_concurrent: usize,
        recover_stale: bool,
    ) -> Self {
        Self {
            api_key,
            database_id,
            poll_interval_secs,
            status_property,
            input_property,
            result_property,
            max_concurrent,
            status_type: Arc::new(RwLock::new("select".to_string())),
            inflight: Arc::new(RwLock::new(HashSet::new())),
            http: reqwest::Client::new(),
            recover_stale,
        }
    }

    /// Build the standard Notion API headers (Authorization, version, content-type).
    fn headers(&self) -> Result<reqwest::header::HeaderMap> {
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

    /// Make a Notion API call with automatic retry on rate-limit (429) and server errors (5xx).
    async fn api_call(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let mut last_err = None;
        for attempt in 0..MAX_RETRIES {
            let mut req = self
                .http
                .request(method.clone(), url)
                .headers(self.headers()?);
            if let Some(ref b) = body {
                req = req.json(b);
            }
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return resp
                            .json()
                            .await
                            .map_err(|e| anyhow::anyhow!("Failed to parse response: {e}"));
                    }
                    let status_code = status.as_u16();
                    // Only retry on 429 (rate limit) or 5xx (server errors)
                    if status_code != 429 && (400..500).contains(&status_code) {
                        let body_text = resp.text().await.unwrap_or_default();
                        let truncated =
                            crate::util::truncate_with_ellipsis(&body_text, MAX_ERROR_BODY_CHARS);
                        bail!("Notion API error {status_code}: {truncated}");
                    }
                    last_err = Some(anyhow::anyhow!("Notion API error: {status_code}"));
                }
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("HTTP request failed: {e}"));
                }
            }
            let delay = RETRY_BASE_DELAY_MS * 2u64.pow(attempt);
            tracing::warn!(
                "Notion API call failed (attempt {}/{}), retrying in {}ms",
                attempt + 1,
                MAX_RETRIES,
                delay
            );
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Notion API call failed after retries")))
    }

    /// Query the database schema and detect whether Status uses "select" or "status" type.
    async fn detect_status_type(&self) -> Result<String> {
        let url = format!("{NOTION_API_BASE}/databases/{}", self.database_id);
        let resp = self.api_call(reqwest::Method::GET, &url, None).await?;
        let status_type = resp
            .get("properties")
            .and_then(|p| p.get(&self.status_property))
            .and_then(|s| s.get("type"))
            .and_then(|t| t.as_str())
            .unwrap_or("select")
            .to_string();
        Ok(status_type)
    }

    /// Query for rows where Status = "pending".
    async fn query_pending(&self) -> Result<Vec<serde_json::Value>> {
        let url = format!("{NOTION_API_BASE}/databases/{}/query", self.database_id);
        let status_type = self.status_type.read().await.clone();
        let filter = build_status_filter(&self.status_property, &status_type, "pending");
        let resp = self
            .api_call(
                reqwest::Method::POST,
                &url,
                Some(serde_json::json!({ "filter": filter })),
            )
            .await?;
        Ok(resp
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Atomically claim a task. Returns true if this caller got it.
    async fn claim_task(&self, page_id: &str) -> bool {
        let mut inflight = self.inflight.write().await;
        if inflight.contains(page_id) {
            return false;
        }
        if inflight.len() >= self.max_concurrent {
            return false;
        }
        inflight.insert(page_id.to_string());
        true
    }

    /// Release a task from the inflight set.
    async fn release_task(&self, page_id: &str) {
        let mut inflight = self.inflight.write().await;
        inflight.remove(page_id);
    }

    /// Update a row's status.
    async fn set_status(&self, page_id: &str, status_value: &str) -> Result<()> {
        let url = format!("{NOTION_API_BASE}/pages/{page_id}");
        let status_type = self.status_type.read().await.clone();
        let payload = serde_json::json!({
            "properties": {
                &self.status_property: build_status_payload(&status_type, status_value),
            }
        });
        self.api_call(reqwest::Method::PATCH, &url, Some(payload))
            .await?;
        Ok(())
    }

    /// Write result text to the Result column.
    async fn set_result(&self, page_id: &str, result_text: &str) -> Result<()> {
        let url = format!("{NOTION_API_BASE}/pages/{page_id}");
        let payload = serde_json::json!({
            "properties": {
                &self.result_property: build_rich_text_payload(result_text),
            }
        });
        self.api_call(reqwest::Method::PATCH, &url, Some(payload))
            .await?;
        Ok(())
    }

    /// On startup, reset "running" tasks back to "pending" for crash recovery.
    async fn recover_stale(&self) -> Result<()> {
        let url = format!("{NOTION_API_BASE}/databases/{}/query", self.database_id);
        let status_type = self.status_type.read().await.clone();
        let filter = build_status_filter(&self.status_property, &status_type, "running");
        let resp = self
            .api_call(
                reqwest::Method::POST,
                &url,
                Some(serde_json::json!({ "filter": filter })),
            )
            .await?;
        let stale = resp
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        if stale.is_empty() {
            return Ok(());
        }
        tracing::warn!(
            "Found {} stale task(s) in 'running' state, resetting to 'pending'",
            stale.len()
        );
        for task in &stale {
            if let Some(page_id) = task.get("id").and_then(|v| v.as_str()) {
                let page_url = format!("{NOTION_API_BASE}/pages/{page_id}");
                let payload = serde_json::json!({
                    "properties": {
                        &self.status_property: build_status_payload(&status_type, "pending"),
                        &self.result_property: build_rich_text_payload(
                            "Reset: poller restarted while task was running"
                        ),
                    }
                });
                let short_id_end = floor_utf8_char_boundary(page_id, 8);
                let short_id = &page_id[..short_id_end];
                if let Err(e) = self
                    .api_call(reqwest::Method::PATCH, &page_url, Some(payload))
                    .await
                {
                    tracing::error!("Could not reset stale task {short_id}: {e}");
                } else {
                    tracing::info!("Reset stale task {short_id} to pending");
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for NotionChannel {
    fn name(&self) -> &str {
        "notion"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // recipient is the page_id for Notion
        let page_id = &message.recipient;
        let status_type = self.status_type.read().await.clone();
        let url = format!("{NOTION_API_BASE}/pages/{page_id}");
        let payload = serde_json::json!({
            "properties": {
                &self.status_property: build_status_payload(&status_type, "done"),
                &self.result_property: build_rich_text_payload(&message.content),
            }
        });
        self.api_call(reqwest::Method::PATCH, &url, Some(payload))
            .await?;
        self.release_task(page_id).await;
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Detect status property type
        match self.detect_status_type().await {
            Ok(st) => {
                tracing::info!("Notion status property type: {st}");
                *self.status_type.write().await = st;
            }
            Err(e) => {
                bail!("Failed to detect Notion database schema: {e}");
            }
        }

        // Crash recovery
        if self.recover_stale {
            if let Err(e) = self.recover_stale().await {
                tracing::error!("Notion stale task recovery failed: {e}");
            }
        }

        // Polling loop
        loop {
            match self.query_pending().await {
                Ok(tasks) => {
                    if !tasks.is_empty() {
                        tracing::info!("Notion: found {} pending task(s)", tasks.len());
                    }
                    for task in tasks {
                        let page_id = match task.get("id").and_then(|v| v.as_str()) {
                            Some(id) => id.to_string(),
                            None => continue,
                        };

                        let input_text = extract_text_from_property(
                            task.get("properties")
                                .and_then(|p| p.get(&self.input_property)),
                        );

                        if input_text.trim().is_empty() {
                            let short_end = floor_utf8_char_boundary(&page_id, 8);
                            tracing::warn!(
                                "Notion: empty input for task {}, skipping",
                                &page_id[..short_end]
                            );
                            continue;
                        }

                        if !self.claim_task(&page_id).await {
                            continue;
                        }

                        // Set status to running
                        if let Err(e) = self.set_status(&page_id, "running").await {
                            tracing::error!("Notion: failed to set running status: {e}");
                            self.release_task(&page_id).await;
                            continue;
                        }

                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        if tx
                            .send(ChannelMessage {
                                id: page_id.clone(),
                                sender: "notion".into(),
                                reply_target: page_id,
                                content: input_text,
                                channel: "notion".into(),
                                timestamp,
                                thread_ts: None,
                                interruption_scope_id: None,
                                attachments: vec![],
                                observe_group: false,
                            })
                            .await
                            .is_err()
                        {
                            tracing::info!("Notion channel shutting down");
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Notion poll error: {e}");
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(self.poll_interval_secs)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{NOTION_API_BASE}/databases/{}", self.database_id);
        self.api_call(reqwest::Method::GET, &url, None)
            .await
            .is_ok()
    }
}

// ── Helper functions ──────────────────────────────────────────────

/// Build a Notion API filter object for the given status property.
fn build_status_filter(property: &str, status_type: &str, value: &str) -> serde_json::Value {
    if status_type == "status" {
        serde_json::json!({
            "property": property,
            "status": { "equals": value }
        })
    } else {
        serde_json::json!({
            "property": property,
            "select": { "equals": value }
        })
    }
}

/// Build a Notion API property-update payload for a status field.
fn build_status_payload(status_type: &str, value: &str) -> serde_json::Value {
    if status_type == "status" {
        serde_json::json!({ "status": { "name": value } })
    } else {
        serde_json::json!({ "select": { "name": value } })
    }
}

/// Build a Notion API rich-text property payload, truncating if necessary.
fn build_rich_text_payload(value: &str) -> serde_json::Value {
    let truncated = truncate_result(value);
    serde_json::json!({
        "rich_text": [{
            "text": { "content": truncated }
        }]
    })
}

/// Truncate result text to fit within the Notion rich-text content limit.
fn truncate_result(value: &str) -> String {
    if value.len() <= MAX_RESULT_LENGTH {
        return value.to_string();
    }
    let cut = MAX_RESULT_LENGTH.saturating_sub(30);
    // Ensure we cut on a char boundary
    let end = floor_utf8_char_boundary(value, cut);
    format!("{}\n\n... [output truncated]", &value[..end])
}

/// Extract plain text from a Notion property (title or rich_text type).
fn extract_text_from_property(prop: Option<&serde_json::Value>) -> String {
    let Some(prop) = prop else {
        return String::new();
    };
    let ptype = prop.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let array_key = match ptype {
        "title" => "title",
        "rich_text" => "rich_text",
        _ => return String::new(),
    };
    prop.get(array_key)
        .and_then(|arr| arr.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("plain_text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn claim_task_deduplication() {
        let channel = NotionChannel::new(
            "test-key".into(),
            "test-db".into(),
            5,
            "Status".into(),
            "Input".into(),
            "Result".into(),
            4,
            false,
        );

        assert!(channel.claim_task("page-1").await);
        // Second claim for same page should fail
        assert!(!channel.claim_task("page-1").await);
        // Different page should succeed
        assert!(channel.claim_task("page-2").await);

        // After release, can claim again
        channel.release_task("page-1").await;
        assert!(channel.claim_task("page-1").await);
    }

    #[test]
    fn result_truncation_within_limit() {
        let short = "hello world";
        assert_eq!(truncate_result(short), short);
    }

    #[test]
    fn result_truncation_over_limit() {
        let long = "a".repeat(MAX_RESULT_LENGTH + 100);
        let truncated = truncate_result(&long);
        assert!(truncated.len() <= MAX_RESULT_LENGTH);
        assert!(truncated.ends_with("... [output truncated]"));
    }

    #[test]
    fn result_truncation_multibyte_safe() {
        // Build a string that would cut in the middle of a multibyte char
        let mut s = String::new();
        for _ in 0..700 {
            s.push('\u{6E2C}'); // 3-byte UTF-8 char
        }
        let truncated = truncate_result(&s);
        // Should not panic and should be valid UTF-8
        assert!(truncated.len() <= MAX_RESULT_LENGTH);
        assert!(truncated.ends_with("... [output truncated]"));
    }

    #[test]
    fn status_payload_select_type() {
        let payload = build_status_payload("select", "pending");
        assert_eq!(
            payload,
            serde_json::json!({ "select": { "name": "pending" } })
        );
    }

    #[test]
    fn status_payload_status_type() {
        let payload = build_status_payload("status", "done");
        assert_eq!(payload, serde_json::json!({ "status": { "name": "done" } }));
    }

    #[test]
    fn rich_text_payload_construction() {
        let payload = build_rich_text_payload("test output");
        let text = payload["rich_text"][0]["text"]["content"].as_str().unwrap();
        assert_eq!(text, "test output");
    }

    #[test]
    fn status_filter_select_type() {
        let filter = build_status_filter("Status", "select", "pending");
        assert_eq!(
            filter,
            serde_json::json!({
                "property": "Status",
                "select": { "equals": "pending" }
            })
        );
    }

    #[test]
    fn status_filter_status_type() {
        let filter = build_status_filter("Status", "status", "running");
        assert_eq!(
            filter,
            serde_json::json!({
                "property": "Status",
                "status": { "equals": "running" }
            })
        );
    }

    #[test]
    fn extract_text_from_title_property() {
        let prop = serde_json::json!({
            "type": "title",
            "title": [
                { "plain_text": "Hello " },
                { "plain_text": "World" }
            ]
        });
        assert_eq!(extract_text_from_property(Some(&prop)), "Hello World");
    }

    #[test]
    fn extract_text_from_rich_text_property() {
        let prop = serde_json::json!({
            "type": "rich_text",
            "rich_text": [{ "plain_text": "task content" }]
        });
        assert_eq!(extract_text_from_property(Some(&prop)), "task content");
    }

    #[test]
    fn extract_text_from_none() {
        assert_eq!(extract_text_from_property(None), "");
    }

    #[test]
    fn extract_text_from_unknown_type() {
        let prop = serde_json::json!({ "type": "number", "number": 42 });
        assert_eq!(extract_text_from_property(Some(&prop)), "");
    }

    #[tokio::test]
    async fn claim_task_respects_max_concurrent() {
        let channel = NotionChannel::new(
            "test-key".into(),
            "test-db".into(),
            5,
            "Status".into(),
            "Input".into(),
            "Result".into(),
            2, // max_concurrent = 2
            false,
        );

        assert!(channel.claim_task("page-1").await);
        assert!(channel.claim_task("page-2").await);
        // Third claim should be rejected (at capacity)
        assert!(!channel.claim_task("page-3").await);

        // After releasing one, can claim again
        channel.release_task("page-1").await;
        assert!(channel.claim_task("page-3").await);
    }
}
