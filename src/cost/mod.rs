pub mod tracker;
pub mod types;

// Re-exported for potential external use (public API)
#[allow(unused_imports)]
pub use tracker::CostTracker;
#[allow(unused_imports)]
pub use types::{BudgetCheck, CostRecord, CostSummary, ModelStats, TokenUsage, UsagePeriod};

use crate::config::schema::CostConfig;
use std::path::Path;
use std::sync::Arc;

/// Build an `Arc<CostTracker>` from config, honoring `enabled` and logging
/// construction failures as warnings.
///
/// Returns `None` when cost tracking is disabled or initialization fails;
/// callers can then treat absence as "cost tracking unavailable" without
/// having to replicate the enabled check + error logging pattern.
pub fn try_build_tracker(config: &CostConfig, workspace_dir: &Path) -> Option<Arc<CostTracker>> {
    if !config.enabled {
        return None;
    }
    match CostTracker::new(config.clone(), workspace_dir) {
        Ok(ct) => Some(Arc::new(ct)),
        Err(e) => {
            tracing::warn!("Failed to initialize cost tracker: {e}");
            None
        }
    }
}
