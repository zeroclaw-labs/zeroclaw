//! Economic tracking module for agent survival economics.
//!
//! This module implements the ClawWork economic model for AI agents,
//! tracking balance, costs, income, and survival status. Agents start
//! with initial capital and must manage their resources while completing
//! tasks.
//!
//! ## Overview
//!
//! The economic system models agent viability:
//! - **Balance**: Starting capital minus costs plus earned income
//! - **Costs**: LLM tokens, search APIs, OCR, and other service usage
//! - **Income**: Payments for completed tasks (with quality threshold)
//! - **Status**: Health indicator based on remaining capital percentage
//!
//! ## Example
//!
//! ```rust,ignore
//! use zeroclaw::economic::{EconomicTracker, EconomicConfig, SurvivalStatus};
//!
//! let config = EconomicConfig {
//!     enabled: true,
//!     initial_balance: 1000.0,
//!     ..Default::default()
//! };
//!
//! let tracker = EconomicTracker::new("my-agent", config, None);
//! tracker.initialize()?;
//!
//! // Start a task
//! tracker.start_task("task-001", None);
//!
//! // Track LLM usage
//! let cost = tracker.track_tokens(1000, 500, "agent", None);
//!
//! // Complete task and earn income
//! tracker.end_task()?;
//! let payment = tracker.add_work_income(10.0, "task-001", 0.85, "Completed task")?;
//!
//! // Check survival status
//! match tracker.get_survival_status() {
//!     SurvivalStatus::Thriving => println!("Agent is healthy!"),
//!     SurvivalStatus::Bankrupt => println!("Agent needs intervention!"),
//!     _ => {}
//! }
//! ```
//!
//! ## Persistence
//!
//! Economic state is persisted to JSONL files:
//! - `balance.jsonl`: Daily balance snapshots and cumulative totals
//! - `token_costs.jsonl`: Detailed per-task cost records
//! - `task_completions.jsonl`: Task completion statistics
//!
//! ## Configuration
//!
//! Add to `config.toml`:
//!
//! ```toml
//! [economic]
//! enabled = true
//! initial_balance = 1000.0
//! min_evaluation_threshold = 0.6
//!
//! [economic.token_pricing]
//! input_price_per_million = 3.0
//! output_price_per_million = 15.0
//! ```

pub mod classifier;
pub mod costs;
pub mod status;
pub mod tracker;

// Re-exports for convenient access
pub use classifier::{ClassificationResult, Occupation, OccupationCategory, TaskClassifier};
pub use costs::{
    ApiCallRecord, ApiUsageSummary, BalanceRecord, CostBreakdown, DateCostSummary,
    EconomicAnalytics, LlmCallRecord, LlmUsageSummary, PricingModel, TaskCompletionRecord,
    TaskCostRecord, TaskCostSummary, TokenPricing, WorkIncomeRecord,
};
pub use status::SurvivalStatus;
pub use tracker::{EconomicConfig, EconomicSummary, EconomicTracker};
