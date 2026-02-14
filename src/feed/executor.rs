//! Feed executor — runs feed handlers and manages feed item persistence.
//!
//! Currently provides a stub execution path that builds SDK context.
//! When Quilt integration lands, this will create/reuse tenant-scoped
//! containers and execute handler code in a sandboxed environment.

use crate::aria::db::AriaDb;
use crate::aria::types::{FeedItem, FeedResult};
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

    /// Execute a feed's handler and return the result.
    ///
    /// Currently builds SDK context with access to registries.
    /// When Quilt integration lands, this will:
    /// 1. Create/reuse tenant-scoped container
    /// 2. Inject handler code + SDK runtime
    /// 3. Execute handler(context)
    /// 4. Return typed FeedResult
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

        // TODO: When Quilt lands, replace this stub with container execution.
        // For now, return a placeholder result indicating the handler was invoked.
        if handler_code.is_empty() {
            return Ok(FeedResult {
                success: false,
                items: Vec::new(),
                summary: None,
                metadata: None,
                error: Some("Empty handler code".to_string()),
            });
        }

        // Stub: acknowledge handler code exists but cannot execute natively yet
        Ok(FeedResult {
            success: true,
            items: Vec::new(),
            summary: Some(format!(
                "Feed {feed_id} executed (stub, run_id={run_id}). Handler code length: {} bytes.",
                handler_code.len()
            )),
            metadata: None,
            error: None,
        })
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
    async fn execute_returns_stub_result_for_valid_handler() {
        let (_db, executor) = setup();
        let result = executor
            .execute("feed-1", "tenant-1", "console.log('hello')", "run-1")
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.summary.is_some());
        assert!(result.items.is_empty()); // Stub returns no items
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
