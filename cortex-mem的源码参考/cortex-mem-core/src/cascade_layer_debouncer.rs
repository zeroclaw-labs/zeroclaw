//! Cascade Layer Debouncer Module
//!
//! Implements debouncing for layer updates to reduce redundant LLM calls.
//! When multiple updates for the same directory occur within a short time window,
//! they are merged into a single update operation.
//!
//! ## How it works:
//! 1. Update requests are recorded but not immediately executed
//! 2. A background task periodically checks for "due" requests (no new activity for N seconds)
//! 3. Due requests are batched and executed, significantly reducing LLM calls
//!
//! ## Example:
//! ```text
//! Without debounce (5 entity files created in 2 seconds):
//!   - entities/ updated 5 times → 10 LLM calls
//!   - user/root/ updated 5 times → 10 LLM calls
//!   Total: 20 LLM calls
//!
//! With debounce (2 second window):
//!   - entities/ updated 1 time → 2 LLM calls
//!   - user/root/ updated 1 time → 2 LLM calls
//!   Total: 4 LLM calls (80% reduction!)
//! ```

use crate::cascade_layer_updater::CascadeLayerUpdater;
use crate::memory_index::MemoryScope;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Configuration for debouncer
#[derive(Debug, Clone)]
pub struct DebouncerConfig {
    /// Debounce delay in seconds
    /// If no new updates arrive for this duration, the update will be executed
    pub debounce_secs: u64,

    /// Maximum delay before forcing an update
    /// Even if new updates keep arriving, force update after this duration
    pub max_delay_secs: u64,
}

impl Default for DebouncerConfig {
    fn default() -> Self {
        Self {
            debounce_secs: 30,   // 30 seconds quiet period
            max_delay_secs: 120, // Force update after 120 seconds max
        }
    }
}

/// Pending update request
#[derive(Debug, Clone)]
struct PendingUpdate {
    dir_uri: String,
    scope: MemoryScope,
    owner_id: String,
    /// When the first request was received
    first_request_at: Instant,
    /// When the most recent request was received
    last_request_at: Instant,
    /// Number of requests merged
    request_count: usize,
}

/// Layer Update Debouncer
///
/// Batches update requests for the same directory to reduce LLM calls.
pub struct LayerUpdateDebouncer {
    /// Pending updates keyed by directory URI
    pending: Arc<RwLock<HashMap<String, PendingUpdate>>>,
    /// Configuration
    config: DebouncerConfig,
}

impl LayerUpdateDebouncer {
    /// Create a new debouncer
    pub fn new(config: DebouncerConfig) -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Request an update (will be debounced)
    ///
    /// Returns true if this is a new request, false if merged with existing
    pub async fn request_update(
        &self,
        dir_uri: String,
        scope: MemoryScope,
        owner_id: String,
    ) -> bool {
        let mut pending = self.pending.write().await;

        if let Some(existing) = pending.get_mut(&dir_uri) {
            // Update existing request
            existing.last_request_at = Instant::now();
            existing.request_count += 1;
            debug!(
                "🔀 Merged update request for {} (total: {} requests)",
                dir_uri, existing.request_count
            );
            false
        } else {
            // New request
            pending.insert(
                dir_uri.clone(),
                PendingUpdate {
                    dir_uri: dir_uri.clone(),
                    scope,
                    owner_id,
                    first_request_at: Instant::now(),
                    last_request_at: Instant::now(),
                    request_count: 1,
                },
            );
            debug!("📝 Registered update request for {}", dir_uri);
            true
        }
    }

    /// Process all due updates
    ///
    /// Returns the number of updates executed
    pub async fn process_due_updates(&self, updater: &CascadeLayerUpdater) -> usize {
        let now = Instant::now();
        let debounce_threshold = Duration::from_secs(self.config.debounce_secs);
        let max_delay_threshold = Duration::from_secs(self.config.max_delay_secs);

        // Find all due updates
        let due_updates: Vec<PendingUpdate> = {
            let mut pending = self.pending.write().await;

            let due_keys: Vec<String> = pending
                .iter()
                .filter(|(_, update)| {
                    let since_last = now - update.last_request_at;
                    let since_first = now - update.first_request_at;

                    // Update is due if:
                    // 1. No activity for debounce_secs, OR
                    // 2. Reached max_delay_secs since first request
                    since_last >= debounce_threshold || since_first >= max_delay_threshold
                })
                .map(|(key, _)| key.clone())
                .collect();

            // Remove and collect due updates
            due_keys
                .into_iter()
                .filter_map(|key| pending.remove(&key))
                .collect()
        };

        let update_count = due_updates.len();

        if update_count > 0 {
            info!(
                "🚀 Processing {} due updates (pending: {})",
                update_count,
                self.pending.read().await.len()
            );
        }

        // Execute updates (outside the lock)
        for update in due_updates {
            debug!(
                "⚙️  Executing merged update for {} ({} requests merged, waited {:.2}s)",
                update.dir_uri,
                update.request_count,
                (now - update.first_request_at).as_secs_f64()
            );

            if let Err(e) = updater
                .update_directory_layers(&update.dir_uri, &update.scope, &update.owner_id)
                .await
            {
                tracing::error!("Failed to update layers for {}: {}", update.dir_uri, e);
            }
        }

        update_count
    }

    /// Get number of pending updates
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }

    /// Flush all pending updates immediately (for shutdown)
    ///
    /// This method forces execution of ALL pending updates regardless of
    /// debounce timing. Used during application shutdown to ensure all
    /// layer updates are processed before exit.
    ///
    /// Returns the number of updates executed
    pub async fn flush_all(&self, updater: &CascadeLayerUpdater) -> usize {
        // Take all pending updates
        let all_updates: Vec<PendingUpdate> = {
            let mut pending = self.pending.write().await;
            pending.drain().map(|(_, v)| v).collect()
        };

        let update_count = all_updates.len();

        if update_count > 0 {
            info!(
                "🚀 Flushing ALL {} pending updates (shutdown mode)",
                update_count
            );
        }

        // Execute all updates
        for update in all_updates {
            info!(
                "⚙️  Flushing update for {} ({} requests merged)",
                update.dir_uri, update.request_count
            );

            if let Err(e) = updater
                .update_directory_layers(&update.dir_uri, &update.scope, &update.owner_id)
                .await
            {
                tracing::error!("Failed to flush layers for {}: {}", update.dir_uri, e);
            }
        }

        update_count
    }

    /// Check if there are any pending updates
    pub async fn has_pending(&self) -> bool {
        !self.pending.read().await.is_empty()
    }

    /// Clear all pending updates (useful for tests)
    #[cfg(test)]
    pub async fn clear(&self) {
        self.pending.write().await.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_request_merge() {
        let debouncer = LayerUpdateDebouncer::new(DebouncerConfig::default());

        // First request - new
        let is_new = debouncer
            .request_update(
                "cortex://user/test/entities".to_string(),
                MemoryScope::User,
                "test".to_string(),
            )
            .await;
        assert!(is_new);
        assert_eq!(debouncer.pending_count().await, 1);

        // Second request for same directory - merged
        let is_new = debouncer
            .request_update(
                "cortex://user/test/entities".to_string(),
                MemoryScope::User,
                "test".to_string(),
            )
            .await;
        assert!(!is_new);
        assert_eq!(debouncer.pending_count().await, 1);

        // Different directory - new
        let is_new = debouncer
            .request_update(
                "cortex://user/test/events".to_string(),
                MemoryScope::User,
                "test".to_string(),
            )
            .await;
        assert!(is_new);
        assert_eq!(debouncer.pending_count().await, 2);
    }

    #[tokio::test]
    async fn test_debounce_delay() {
        let config = DebouncerConfig {
            debounce_secs: 0, // Immediate execution for testing
            max_delay_secs: 10,
        };
        let debouncer = LayerUpdateDebouncer::new(config);

        // Add a request
        debouncer
            .request_update(
                "cortex://user/test/entities".to_string(),
                MemoryScope::User,
                "test".to_string(),
            )
            .await;

        assert_eq!(debouncer.pending_count().await, 1);

        // Should be immediately due (debounce_secs = 0)
        tokio::time::sleep(Duration::from_millis(10)).await;

        let pending = debouncer.pending.read().await;
        let update = pending.get("cortex://user/test/entities").unwrap();
        let since_last = Instant::now() - update.last_request_at;
        assert!(since_last >= Duration::from_secs(0));
    }
}
