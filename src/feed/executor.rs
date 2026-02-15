//! Feed executor — runs feed handlers and manages feed item persistence.
//!
//! Supports two execution modes:
//! - **URL feeds**: When `handler_code` is an HTTP(S) URL, the executor fetches
//!   the content and returns it as a single `FeedItem` with card_type `Text`.
//! - **Code handlers**: Dispatched to the Quilt container runtime for sandboxed
//!   execution. Requires `QUILT_API_URL` and `QUILT_API_KEY` to be configured.

use crate::aria::db::AriaDb;
use crate::aria::types::{FeedCardType, FeedItem, FeedResult};
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

/// Executes feed handlers and manages feed item storage/retention.
pub struct FeedExecutor {
    db: AriaDb,
}

impl FeedExecutor {
    pub fn new(db: AriaDb) -> Self {
        Self { db }
    }

    /// Returns `true` if `handler_code` looks like an HTTP(S) URL.
    fn is_url_feed(handler_code: &str) -> bool {
        let trimmed = handler_code.trim();
        trimmed.starts_with("http://") || trimmed.starts_with("https://")
    }

    /// Derive a human-readable title from a URL.
    ///
    /// Strips the scheme and uses the host + path as the title.
    /// Falls back to the full URL if parsing fails.
    fn title_from_url(url: &str) -> String {
        // Try to extract host + path for a concise title
        if let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) {
            let trimmed = rest.trim_end_matches('/');
            if trimmed.is_empty() {
                return url.to_string();
            }
            format!("Feed: {trimmed}")
        } else {
            format!("Feed: {url}")
        }
    }

    /// Execute a URL-based feed by fetching the content via HTTP.
    ///
    /// Returns a `FeedResult` containing a single `FeedItem` with the
    /// fetched body as text content.
    async fn execute_url_feed(
        feed_id: &str,
        url: &str,
        run_id: &str,
    ) -> Result<FeedResult> {
        let url = url.trim();

        tracing::info!(
            feed_id = feed_id,
            url = url,
            run_id = run_id,
            "Fetching URL feed"
        );

        let response = reqwest::get(url)
            .await
            .with_context(|| format!("Failed to fetch URL feed: {url}"))?;

        let status = response.status();
        if !status.is_success() {
            return Ok(FeedResult {
                success: false,
                items: Vec::new(),
                summary: None,
                metadata: None,
                error: Some(format!(
                    "HTTP {status} fetching feed URL: {url}"
                )),
            });
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/plain")
            .to_string();

        let body_text = response
            .text()
            .await
            .with_context(|| format!("Failed to read response body from: {url}"))?;

        let title = Self::title_from_url(url);

        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "content_type".to_string(),
            serde_json::json!(content_type),
        );
        metadata.insert(
            "source_url".to_string(),
            serde_json::json!(url),
        );
        metadata.insert(
            "fetched_at".to_string(),
            serde_json::json!(Utc::now().to_rfc3339()),
        );

        let item = FeedItem {
            card_type: FeedCardType::Text,
            title,
            body: Some(body_text),
            source: Some(url.to_string()),
            url: Some(url.to_string()),
            metadata: Some(metadata),
            timestamp: Some(Utc::now().timestamp()),
        };

        Ok(FeedResult {
            success: true,
            items: vec![item],
            summary: Some(format!(
                "Fetched URL feed for {feed_id} (run_id={run_id})"
            )),
            metadata: None,
            error: None,
        })
    }

    /// Execute a feed's handler and return the result.
    ///
    /// Three execution modes are supported:
    /// - **URL feeds** (`handler_code` starts with `http://` or `https://`):
    ///   Fetches the URL content and returns it as a single `FeedItem`.
    /// - **Code handlers**: Dispatched to the Quilt container runtime for
    ///   sandboxed execution. Requires a Quilt endpoint to be configured.
    /// - **Empty handler**: Returns an error.
    pub async fn execute(
        &self,
        feed_id: &str,
        tenant_id: &str,
        handler_code: &str,
        run_id: &str,
    ) -> Result<FeedResult> {
        tracing::info!(
            feed_id = feed_id,
            tenant_id = tenant_id,
            run_id = run_id,
            "Executing feed handler"
        );

        // Empty handler code is always an error.
        if handler_code.is_empty() {
            return Ok(FeedResult {
                success: false,
                items: Vec::new(),
                summary: None,
                metadata: None,
                error: Some("Empty handler code".to_string()),
            });
        }

        // URL feeds: fetch the content directly via HTTP.
        if Self::is_url_feed(handler_code) {
            return Self::execute_url_feed(feed_id, handler_code, run_id).await;
        }

        // Code handler: dispatch to Quilt container runtime.
        Self::execute_code_handler(feed_id, tenant_id, handler_code, run_id).await
    }

    /// Execute a code-based feed handler via the Quilt container runtime.
    ///
    /// Creates a sandboxed container, injects the handler code, executes it,
    /// and parses the output as feed items.
    async fn execute_code_handler(
        feed_id: &str,
        tenant_id: &str,
        handler_code: &str,
        run_id: &str,
    ) -> Result<FeedResult> {
        use crate::quilt::client::QuiltClient;

        // Attempt to connect to Quilt runtime
        let quilt = match QuiltClient::from_env() {
            Ok(client) => client,
            Err(_) => {
                return Ok(FeedResult {
                    success: false,
                    items: Vec::new(),
                    summary: None,
                    metadata: None,
                    error: Some(
                        "Code handler execution requires a running Quilt container runtime. \
                         Set QUILT_API_URL and QUILT_API_KEY, or use a URL as handler_code \
                         for basic HTTP feeds."
                            .to_string(),
                    ),
                });
            }
        };

        // Create a sandboxed container for the feed execution
        let container_name = format!("feed-{feed_id}-{run_id}");
        let params = crate::quilt::client::QuiltCreateParams {
            image: "node:20-slim".to_string(),
            name: container_name,
            command: None,
            labels: std::collections::HashMap::from([
                ("aria.feed_id".to_string(), feed_id.to_string()),
                ("aria.tenant_id".to_string(), tenant_id.to_string()),
                ("aria.run_id".to_string(), run_id.to_string()),
            ]),
            memory_limit_mb: Some(256),
            cpu_limit_percent: Some(50),
            environment: std::collections::HashMap::new(),
            volumes: Vec::new(),
            ports: Vec::new(),
            network: None,
            restart_policy: None,
        };

        let container = match quilt.create_container(params).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(FeedResult {
                    success: false,
                    items: Vec::new(),
                    summary: None,
                    metadata: None,
                    error: Some(format!("Failed to create Quilt container: {e}")),
                });
            }
        };

        // Start the container
        if let Err(e) = quilt.start_container(&container.container_id).await {
            let _ = quilt.delete_container(&container.container_id).await;
            return Ok(FeedResult {
                success: false,
                items: Vec::new(),
                summary: None,
                metadata: None,
                error: Some(format!("Failed to start container: {e}")),
            });
        }

        // Execute the handler code inside the container
        let exec_params = crate::quilt::client::QuiltExecParams {
            command: vec![
                "node".into(),
                "-e".into(),
                handler_code.to_string(),
            ],
            timeout_ms: Some(60_000), // 60 second timeout
            working_dir: Some("/app".into()),
            environment: Some(std::collections::HashMap::from([
                ("FEED_ID".to_string(), feed_id.to_string()),
                ("TENANT_ID".to_string(), tenant_id.to_string()),
                ("RUN_ID".to_string(), run_id.to_string()),
            ])),
        };

        let exec_result = quilt.exec(&container.container_id, exec_params).await;

        // Clean up the container regardless of result
        let _ = quilt.stop_container(&container.container_id).await;
        let _ = quilt.delete_container(&container.container_id).await;

        match exec_result {
            Ok(result) => {
                if result.exit_code != 0 {
                    return Ok(FeedResult {
                        success: false,
                        items: Vec::new(),
                        summary: None,
                        metadata: None,
                        error: Some(format!(
                            "Handler exited with code {}: {}",
                            result.exit_code,
                            result.stderr
                        )),
                    });
                }

                // Parse stdout as JSON feed items
                let item = FeedItem {
                    card_type: FeedCardType::Text,
                    title: format!("Feed {feed_id} run {run_id}"),
                    body: Some(result.stdout),
                    source: Some("quilt".to_string()),
                    url: None,
                    metadata: Some(std::collections::HashMap::from([
                        ("run_id".to_string(), serde_json::json!(run_id)),
                        (
                            "executed_at".to_string(),
                            serde_json::json!(Utc::now().to_rfc3339()),
                        ),
                    ])),
                    timestamp: Some(Utc::now().timestamp()),
                };

                Ok(FeedResult {
                    success: true,
                    items: vec![item],
                    summary: Some(format!("Executed code handler for {feed_id}")),
                    metadata: None,
                    error: None,
                })
            }
            Err(e) => Ok(FeedResult {
                success: false,
                items: Vec::new(),
                summary: None,
                metadata: None,
                error: Some(format!("Handler execution failed: {e}")),
            }),
        }
    }

    /// Store feed items in the database.
    pub fn store_items(
        &self,
        tenant_id: &str,
        feed_id: &str,
        run_id: &str,
        items: &[FeedItem],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "INSERT INTO aria_feed_items
                 (id, tenant_id, feed_id, run_id, card_type, title, body, source, url, metadata, timestamp, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;

            for item in items {
                let id = Uuid::new_v4().to_string();
                let card_type = serde_json::to_string(&item.card_type)
                    .unwrap_or_else(|_| "\"text\"".to_string());
                // Strip surrounding quotes from the serialized card_type
                let card_type = card_type.trim_matches('"');
                let metadata_json = item
                    .metadata
                    .as_ref()
                    .map(|m| serde_json::to_string(m).unwrap_or_default());

                stmt.execute(params![
                    id,
                    tenant_id,
                    feed_id,
                    run_id,
                    card_type,
                    item.title,
                    item.body,
                    item.source,
                    item.url,
                    metadata_json,
                    item.timestamp,
                    now,
                ])?;
            }
            Ok(())
        })
    }

    /// Prune items older than retention policy.
    ///
    /// Returns the number of items deleted.
    /// - `max_items`: keep only this many most recent items per feed
    /// - `max_age_days`: remove items older than N days
    pub fn prune_by_retention(
        &self,
        feed_id: &str,
        max_items: Option<u32>,
        max_age_days: Option<u32>,
    ) -> Result<u64> {
        let mut total_pruned: u64 = 0;

        // Prune by max age first
        if let Some(days) = max_age_days {
            let cutoff = Utc::now()
                .checked_sub_signed(chrono::Duration::days(i64::from(days)))
                .context("Failed to compute age cutoff")?
                .to_rfc3339();

            let pruned = self.db.with_conn(|conn| {
                let deleted = conn.execute(
                    "DELETE FROM aria_feed_items WHERE feed_id = ?1 AND created_at < ?2",
                    params![feed_id, cutoff],
                )?;
                Ok(deleted as u64)
            })?;
            total_pruned += pruned;
        }

        // Prune by max items (keep most recent N)
        if let Some(max) = max_items {
            let pruned = self.db.with_conn(|conn| {
                // Delete items beyond the max count, keeping the most recent
                let deleted = conn.execute(
                    "DELETE FROM aria_feed_items WHERE feed_id = ?1 AND id NOT IN (
                        SELECT id FROM aria_feed_items WHERE feed_id = ?1
                        ORDER BY created_at DESC LIMIT ?2
                    )",
                    params![feed_id, max],
                )?;
                Ok(deleted as u64)
            })?;
            total_pruned += pruned;
        }

        if total_pruned > 0 {
            tracing::debug!(
                feed_id = feed_id,
                pruned = total_pruned,
                "Pruned feed items by retention policy"
            );
        }

        Ok(total_pruned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;
    use crate::aria::types::{FeedCardType, FeedItem};
    use std::collections::HashMap;

    fn setup() -> (AriaDb, FeedExecutor) {
        let db = AriaDb::open_in_memory().unwrap();
        let executor = FeedExecutor::new(db.clone());
        (db, executor)
    }

    fn sample_items(count: usize) -> Vec<FeedItem> {
        (0..count)
            .map(|i| FeedItem {
                card_type: FeedCardType::Text,
                title: format!("Item {i}"),
                body: Some(format!("Body of item {i}")),
                source: Some("test".to_string()),
                url: Some(format!("https://example.com/{i}")),
                metadata: Some(HashMap::from([(
                    "index".to_string(),
                    serde_json::json!(i),
                )])),
                timestamp: Some(Utc::now().timestamp()),
            })
            .collect()
    }

    #[tokio::test]
    async fn execute_returns_error_for_empty_handler() {
        let (_db, executor) = setup();
        let result = executor
            .execute("feed-1", "tenant-1", "", "run-1")
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("Empty handler code"));
    }

    #[tokio::test]
    async fn execute_returns_quilt_error_for_code_handler() {
        let (_db, executor) = setup();
        let result = executor
            .execute("feed-1", "tenant-1", "console.log('hello')", "run-1")
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.items.is_empty());
        let error = result.error.as_deref().unwrap();
        assert!(
            error.contains("Quilt container runtime"),
            "Expected Quilt error message, got: {error}"
        );
    }

    #[test]
    fn is_url_feed_detects_http_urls() {
        assert!(FeedExecutor::is_url_feed("https://example.com/feed.json"));
        assert!(FeedExecutor::is_url_feed("http://example.com/rss"));
        assert!(FeedExecutor::is_url_feed("  https://example.com/feed  "));
        assert!(!FeedExecutor::is_url_feed("console.log('hello')"));
        assert!(!FeedExecutor::is_url_feed(""));
        assert!(!FeedExecutor::is_url_feed("ftp://example.com/file"));
        assert!(!FeedExecutor::is_url_feed("httpx://not-a-url"));
    }

    #[test]
    fn title_from_url_extracts_host_and_path() {
        assert_eq!(
            FeedExecutor::title_from_url("https://example.com/feed.json"),
            "Feed: example.com/feed.json"
        );
        assert_eq!(
            FeedExecutor::title_from_url("http://api.example.com/v1/data"),
            "Feed: api.example.com/v1/data"
        );
        assert_eq!(
            FeedExecutor::title_from_url("https://example.com/"),
            "Feed: example.com"
        );
    }

    #[test]
    fn store_items_persists_to_db() {
        let (db, executor) = setup();
        let items = sample_items(3);
        executor
            .store_items("tenant-1", "feed-1", "run-1", &items)
            .unwrap();

        let count: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM aria_feed_items WHERE feed_id = 'feed-1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn store_items_records_correct_fields() {
        let (db, executor) = setup();
        let items = vec![FeedItem {
            card_type: FeedCardType::Link,
            title: "Test Link".to_string(),
            body: Some("A link body".to_string()),
            source: Some("reddit".to_string()),
            url: Some("https://reddit.com/r/rust".to_string()),
            metadata: None,
            timestamp: Some(1_700_000_000),
        }];
        executor
            .store_items("t1", "f1", "r1", &items)
            .unwrap();

        db.with_conn(|conn| {
            let (title, card_type, source): (String, String, String) = conn.query_row(
                "SELECT title, card_type, source FROM aria_feed_items WHERE feed_id = 'f1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(title, "Test Link");
            assert_eq!(card_type, "link");
            assert_eq!(source, "reddit");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn store_empty_items_is_noop() {
        let (_db, executor) = setup();
        executor
            .store_items("t1", "f1", "r1", &[])
            .unwrap();
    }

    #[test]
    fn prune_by_max_items_keeps_most_recent() {
        let (db, executor) = setup();

        // Insert 5 items with staggered timestamps
        for i in 0..5 {
            let created = format!("2025-01-{:02}T00:00:00+00:00", i + 1);
            db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_feed_items (id, tenant_id, feed_id, run_id, card_type, title, created_at)
                     VALUES (?1, 't1', 'f1', 'r1', 'text', ?2, ?3)",
                    params![format!("item-{i}"), format!("Item {i}"), created],
                )?;
                Ok(())
            })
            .unwrap();
        }

        let pruned = executor.prune_by_retention("f1", Some(3), None).unwrap();
        assert_eq!(pruned, 2);

        let remaining: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM aria_feed_items WHERE feed_id = 'f1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(remaining, 3);
    }

    #[test]
    fn prune_by_max_age_removes_old_items() {
        let (db, executor) = setup();

        // Insert an old item and a recent item
        let old_date = "2020-01-01T00:00:00+00:00";
        let recent_date = Utc::now().to_rfc3339();

        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO aria_feed_items (id, tenant_id, feed_id, run_id, card_type, title, created_at)
                 VALUES ('old', 't1', 'f1', 'r1', 'text', 'Old Item', ?1)",
                params![old_date],
            )?;
            conn.execute(
                "INSERT INTO aria_feed_items (id, tenant_id, feed_id, run_id, card_type, title, created_at)
                 VALUES ('new', 't1', 'f1', 'r1', 'text', 'New Item', ?1)",
                params![recent_date],
            )?;
            Ok(())
        })
        .unwrap();

        let pruned = executor.prune_by_retention("f1", None, Some(30)).unwrap();
        assert_eq!(pruned, 1);

        let remaining: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM aria_feed_items WHERE feed_id = 'f1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn prune_with_no_policy_is_noop() {
        let (db, executor) = setup();
        let items = sample_items(5);
        executor.store_items("t1", "f1", "r1", &items).unwrap();

        let pruned = executor.prune_by_retention("f1", None, None).unwrap();
        assert_eq!(pruned, 0);

        let remaining: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM aria_feed_items WHERE feed_id = 'f1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(remaining, 5);
    }

    #[test]
    fn prune_nonexistent_feed_returns_zero() {
        let (_db, executor) = setup();
        let pruned = executor
            .prune_by_retention("nonexistent", Some(10), Some(7))
            .unwrap();
        assert_eq!(pruned, 0);
    }
}
