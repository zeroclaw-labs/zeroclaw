//! Memory-backed audit log for dispatch events and their results.
//!
//! Persists every event passing through the router so that compliance, debug
//! and post-incident review can reconstruct the chain of: trigger → handlers
//! → outcomes. Storage uses the existing `Memory` trait so the audit log
//! lives wherever the user's regular memory lives (SQLite by default).
//!
//! # Storage layout
//!
//! - `dispatch_event_{event_id}` — the full `DispatchEvent` JSON
//! - `dispatch_result_{event_id}` — the full `DispatchResult` JSON
//!
//! Both are stored under the `dispatch` memory category so they can be listed
//! together with `Memory::list(Some(&Custom("dispatch")), None)`.

use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use super::types::{DispatchEvent, DispatchResult};
use crate::memory::traits::{Memory, MemoryCategory};

const DISPATCH_CATEGORY: &str = "dispatch";

/// Persists dispatch events and results to the Memory backend.
///
/// All log methods are best-effort: failures are logged via `tracing::warn!`
/// but never propagate up the dispatch path. Hardware events must never be
/// lost because of an audit failure.
pub struct DispatchAuditLogger {
    memory: Arc<dyn Memory>,
}

impl DispatchAuditLogger {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }

    /// Log a dispatch event before any handler runs.
    pub async fn log_event(&self, event: &DispatchEvent) -> Result<()> {
        let key = event_key(&event.id);
        let content = serde_json::to_string_pretty(event)?;
        self.memory.store(&key, &content, category(), None).await?;
        info!(
            event_id = %event.id,
            source = %event.source,
            topic = ?event.topic,
            "Dispatch: event recorded"
        );
        Ok(())
    }

    /// Log a dispatch result after all handlers have completed.
    pub async fn log_result(&self, result: &DispatchResult) -> Result<()> {
        let key = result_key(&result.event_id);
        let content = serde_json::to_string_pretty(result)?;
        self.memory.store(&key, &content, category(), None).await?;
        info!(
            event_id = %result.event_id,
            handled = result.handled_count(),
            failed = result.failed_count(),
            total = result.handler_outcomes.len(),
            "Dispatch: result recorded"
        );
        Ok(())
    }

    /// Retrieve a stored event by id (returns None if not found or unparseable).
    pub async fn get_event(&self, event_id: &str) -> Result<Option<DispatchEvent>> {
        let key = event_key(event_id);
        match self.memory.get(&key).await? {
            Some(entry) => match serde_json::from_str(&entry.content) {
                Ok(event) => Ok(Some(event)),
                Err(e) => {
                    warn!("Dispatch audit: failed to parse event {event_id}: {e}");
                    Ok(None)
                }
            },
            None => Ok(None),
        }
    }

    /// Retrieve a stored result by event id.
    pub async fn get_result(&self, event_id: &str) -> Result<Option<DispatchResult>> {
        let key = result_key(event_id);
        match self.memory.get(&key).await? {
            Some(entry) => match serde_json::from_str(&entry.content) {
                Ok(result) => Ok(Some(result)),
                Err(e) => {
                    warn!("Dispatch audit: failed to parse result {event_id}: {e}");
                    Ok(None)
                }
            },
            None => Ok(None),
        }
    }

    /// List all dispatch event keys currently stored.
    pub async fn list_events(&self) -> Result<Vec<String>> {
        let entries = self.memory.list(Some(&category()), None).await?;
        Ok(entries
            .into_iter()
            .filter(|e| e.key.starts_with("dispatch_event_"))
            .map(|e| e.key)
            .collect())
    }
}

fn event_key(event_id: &str) -> String {
    format!("dispatch_event_{event_id}")
}

fn result_key(event_id: &str) -> String {
    format!("dispatch_result_{event_id}")
}

fn category() -> MemoryCategory {
    MemoryCategory::Custom(DISPATCH_CATEGORY.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::types::{DispatchEvent, EventSource, HandlerOutcome};

    async fn make_logger() -> DispatchAuditLogger {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        DispatchAuditLogger::new(memory)
    }

    #[tokio::test]
    async fn event_roundtrip() {
        let logger = make_logger().await;
        let event = DispatchEvent::new(
            EventSource::Peripheral,
            Some("nucleo/pin_3".into()),
            Some("1".into()),
        );
        let event_id = event.id.clone();

        logger.log_event(&event).await.unwrap();

        let loaded = logger.get_event(&event_id).await.unwrap().unwrap();
        assert_eq!(loaded.id, event_id);
        assert_eq!(loaded.source, EventSource::Peripheral);
        assert_eq!(loaded.topic.as_deref(), Some("nucleo/pin_3"));
    }

    #[tokio::test]
    async fn result_roundtrip() {
        let logger = make_logger().await;
        let result = DispatchResult {
            event_id: "test-evt-1".into(),
            matched_handlers: vec!["alert_handler".into()],
            handler_outcomes: vec![(
                "alert_handler".into(),
                HandlerOutcome::Handled {
                    summary: "notified ops channel".into(),
                },
            )],
        };

        logger.log_result(&result).await.unwrap();

        let loaded = logger.get_result("test-evt-1").await.unwrap().unwrap();
        assert_eq!(loaded.matched_handlers, vec!["alert_handler"]);
        assert_eq!(loaded.handled_count(), 1);
    }

    #[tokio::test]
    async fn list_events_returns_only_event_keys() {
        let logger = make_logger().await;
        let event = DispatchEvent::new(EventSource::Manual, None, None);
        let result = DispatchResult {
            event_id: event.id.clone(),
            matched_handlers: vec![],
            handler_outcomes: vec![],
        };
        logger.log_event(&event).await.unwrap();
        logger.log_result(&result).await.unwrap();

        let keys = logger.list_events().await.unwrap();
        // Only event keys, not result keys
        assert_eq!(keys.len(), 1);
        assert!(keys[0].starts_with("dispatch_event_"));
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let logger = make_logger().await;
        assert!(logger.get_event("nonexistent").await.unwrap().is_none());
        assert!(logger.get_result("nonexistent").await.unwrap().is_none());
    }
}
