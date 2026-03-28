//! Token cost tracking types for economic agents.
//!
//! Separates costs by channel (LLM, search API, OCR, etc.) following
//! the ClawWork economic model.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Channel-separated cost breakdown for a task or session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostBreakdown {
    /// Cost from LLM token usage
    pub llm_tokens: f64,
    /// Cost from search API calls (Brave, JINA, Tavily, etc.)
    pub search_api: f64,
    /// Cost from OCR API calls
    pub ocr_api: f64,
    /// Cost from other API calls
    pub other_api: f64,
}

impl CostBreakdown {
    /// Create a new empty cost breakdown.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get total cost across all channels.
    pub fn total(&self) -> f64 {
        self.llm_tokens + self.search_api + self.ocr_api + self.other_api
    }

    /// Add another breakdown to this one.
    pub fn add(&mut self, other: &CostBreakdown) {
        self.llm_tokens += other.llm_tokens;
        self.search_api += other.search_api;
        self.ocr_api += other.ocr_api;
        self.other_api += other.other_api;
    }

    /// Reset all costs to zero.
    pub fn reset(&mut self) {
        self.llm_tokens = 0.0;
        self.search_api = 0.0;
        self.ocr_api = 0.0;
        self.other_api = 0.0;
    }
}

/// Token pricing configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenPricing {
    /// Price per million input tokens (USD)
    pub input_price_per_million: f64,
    /// Price per million output tokens (USD)
    pub output_price_per_million: f64,
}

impl Default for TokenPricing {
    fn default() -> Self {
        // Default to Claude Sonnet 4 pricing via OpenRouter
        Self {
            input_price_per_million: 3.0,
            output_price_per_million: 15.0,
        }
    }
}

impl TokenPricing {
    /// Calculate cost for given token counts.
    pub fn calculate_cost(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        let input_cost = (input_tokens as f64 / 1_000_000.0) * self.input_price_per_million;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * self.output_price_per_million;
        input_cost + output_cost
    }
}

/// A single LLM call record with token details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmCallRecord {
    /// Timestamp of the call
    pub timestamp: DateTime<Utc>,
    /// API name/source (e.g., "agent", "wrapup", "research")
    pub api_name: String,
    /// Number of input tokens
    pub input_tokens: u64,
    /// Number of output tokens
    pub output_tokens: u64,
    /// Cost in USD
    pub cost: f64,
}

/// A single API call record (non-LLM).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCallRecord {
    /// Timestamp of the call
    pub timestamp: DateTime<Utc>,
    /// API name (e.g., "tavily_search", "jina_reader")
    pub api_name: String,
    /// Pricing model used
    pub pricing_model: PricingModel,
    /// Number of tokens (if token-based pricing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// Price per million tokens (if token-based)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_per_million: Option<f64>,
    /// Cost in USD
    pub cost: f64,
}

/// Pricing model for API calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingModel {
    /// Token-based pricing (cost = tokens / 1M * price_per_million)
    PerToken,
    /// Flat rate per call
    FlatRate,
}

/// Comprehensive task cost record (one per task).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCostRecord {
    /// Task end timestamp
    pub timestamp_end: DateTime<Utc>,
    /// Task start timestamp
    pub timestamp_start: DateTime<Utc>,
    /// Date the task was assigned (YYYY-MM-DD)
    pub date: String,
    /// Unique task identifier
    pub task_id: String,
    /// LLM usage summary
    pub llm_usage: LlmUsageSummary,
    /// API usage summary
    pub api_usage: ApiUsageSummary,
    /// Cost summary by channel
    pub cost_summary: CostBreakdown,
    /// Balance after this task
    pub balance_after: f64,
    /// Session cost so far
    pub session_cost: f64,
    /// Daily cost so far
    pub daily_cost: f64,
}

/// Aggregated LLM usage for a task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmUsageSummary {
    /// Number of LLM calls made
    pub total_calls: usize,
    /// Total input tokens
    pub total_input_tokens: u64,
    /// Total output tokens
    pub total_output_tokens: u64,
    /// Total tokens (input + output)
    pub total_tokens: u64,
    /// Total cost in USD
    pub total_cost: f64,
    /// Pricing used
    pub input_price_per_million: f64,
    pub output_price_per_million: f64,
    /// Detailed call records
    #[serde(default)]
    pub calls_detail: Vec<LlmCallRecord>,
}

/// Aggregated API usage for a task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApiUsageSummary {
    /// Number of API calls made
    pub total_calls: usize,
    /// Search API costs
    pub search_api_cost: f64,
    /// OCR API costs
    pub ocr_api_cost: f64,
    /// Other API costs
    pub other_api_cost: f64,
    /// Number of token-based calls
    pub token_based_calls: usize,
    /// Number of flat-rate calls
    pub flat_rate_calls: usize,
    /// Detailed call records
    #[serde(default)]
    pub calls_detail: Vec<ApiCallRecord>,
}

/// Work income record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkIncomeRecord {
    /// Timestamp
    pub timestamp: DateTime<Utc>,
    /// Date (YYYY-MM-DD)
    pub date: String,
    /// Task identifier
    pub task_id: String,
    /// Base payment amount offered
    pub base_amount: f64,
    /// Actual payment received (0 if below threshold)
    pub actual_payment: f64,
    /// Evaluation score (0.0-1.0)
    pub evaluation_score: f64,
    /// Minimum threshold required for payment
    pub threshold: f64,
    /// Whether payment was awarded
    pub payment_awarded: bool,
    /// Optional description
    #[serde(default)]
    pub description: String,
    /// Balance after this income
    pub balance_after: f64,
}

/// Daily balance record for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceRecord {
    /// Date (YYYY-MM-DD or "initialization")
    pub date: String,
    /// Current balance
    pub balance: f64,
    /// Token cost delta for this period
    pub token_cost_delta: f64,
    /// Work income delta for this period
    pub work_income_delta: f64,
    /// Trading profit delta for this period
    pub trading_profit_delta: f64,
    /// Cumulative total token cost
    pub total_token_cost: f64,
    /// Cumulative total work income
    pub total_work_income: f64,
    /// Cumulative total trading profit
    pub total_trading_profit: f64,
    /// Net worth (balance + portfolio value)
    pub net_worth: f64,
    /// Current survival status
    pub survival_status: String,
    /// Tasks completed in this period
    #[serde(default)]
    pub completed_tasks: Vec<String>,
    /// Primary task ID for the day
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Time to complete tasks (seconds)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_completion_time_seconds: Option<f64>,
    /// Whether session was aborted by API error
    #[serde(default)]
    pub api_error: bool,
}

/// Task completion record for analytics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCompletionRecord {
    /// Task identifier
    pub task_id: String,
    /// Date (YYYY-MM-DD)
    pub date: String,
    /// Attempt number (1-based)
    pub attempt: u32,
    /// Whether work was submitted
    pub work_submitted: bool,
    /// Evaluation score (0.0-1.0)
    pub evaluation_score: f64,
    /// Money earned from this task
    pub money_earned: f64,
    /// Wall-clock time in seconds
    pub wall_clock_seconds: f64,
    /// Timestamp of completion
    pub timestamp: DateTime<Utc>,
}

/// Economic analytics summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EconomicAnalytics {
    /// Total costs by channel
    pub total_costs: CostBreakdown,
    /// Costs broken down by date
    pub by_date: HashMap<String, DateCostSummary>,
    /// Costs broken down by task
    pub by_task: HashMap<String, TaskCostSummary>,
    /// Total number of tasks
    pub total_tasks: usize,
    /// Total income earned
    pub total_income: f64,
    /// Number of tasks that received payment
    pub tasks_paid: usize,
    /// Number of tasks rejected (below threshold)
    pub tasks_rejected: usize,
}

/// Cost summary for a single date.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DateCostSummary {
    /// Costs by channel
    #[serde(flatten)]
    pub costs: CostBreakdown,
    /// Total cost
    pub total: f64,
    /// Income earned
    pub income: f64,
}

/// Cost summary for a single task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskCostSummary {
    /// Costs by channel
    #[serde(flatten)]
    pub costs: CostBreakdown,
    /// Total cost
    pub total: f64,
    /// Date of the task
    pub date: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_breakdown_total() {
        let breakdown = CostBreakdown {
            llm_tokens: 1.0,
            search_api: 0.5,
            ocr_api: 0.25,
            other_api: 0.1,
        };
        assert!((breakdown.total() - 1.85).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_breakdown_add() {
        let mut a = CostBreakdown {
            llm_tokens: 1.0,
            search_api: 0.5,
            ocr_api: 0.0,
            other_api: 0.0,
        };
        let b = CostBreakdown {
            llm_tokens: 0.5,
            search_api: 0.25,
            ocr_api: 0.1,
            other_api: 0.05,
        };
        a.add(&b);
        assert!((a.llm_tokens - 1.5).abs() < f64::EPSILON);
        assert!((a.search_api - 0.75).abs() < f64::EPSILON);
        assert!((a.total() - 2.4).abs() < f64::EPSILON);
    }

    #[test]
    fn token_pricing_calculation() {
        let pricing = TokenPricing {
            input_price_per_million: 3.0,
            output_price_per_million: 15.0,
        };
        // 1000 input, 500 output
        // (1000/1M)*3 + (500/1M)*15 = 0.003 + 0.0075 = 0.0105
        let cost = pricing.calculate_cost(1000, 500);
        assert!((cost - 0.0105).abs() < 0.0001);
    }

    #[test]
    fn default_token_pricing() {
        let pricing = TokenPricing::default();
        assert!((pricing.input_price_per_million - 3.0).abs() < f64::EPSILON);
        assert!((pricing.output_price_per_million - 15.0).abs() < f64::EPSILON);
    }
}
