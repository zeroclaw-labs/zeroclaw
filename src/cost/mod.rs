pub mod tracker;
pub mod types;

pub use tracker::CostTracker;
pub use types::{BudgetCheck, CostRecord, CostSummary, ModelStats, TokenUsage, UsagePeriod};

pub fn create_tracker_from_json(
    cost_config_json: &serde_json::Value,
    workspace_dir: &std::path::Path,
) -> anyhow::Result<CostTracker> {
    let config: crate::config::schema::CostConfig = serde_json::from_value(cost_config_json.clone())?;
    CostTracker::new(config, workspace_dir)
}
