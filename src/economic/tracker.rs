//! Economic tracker for agent survival economics.
//!
//! Tracks balance, token costs, work income, and survival status following
//! the ClawWork LiveBench economic model. Persists state to JSONL files.

use super::costs::{
    ApiCallRecord, ApiUsageSummary, BalanceRecord, CostBreakdown, LlmCallRecord, LlmUsageSummary,
    PricingModel, TaskCompletionRecord, TaskCostRecord, TokenPricing, WorkIncomeRecord,
};
use super::status::SurvivalStatus;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;

/// Economic configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EconomicConfig {
    /// Enable economic tracking
    #[serde(default)]
    pub enabled: bool,
    /// Starting balance in USD
    #[serde(default = "default_initial_balance")]
    pub initial_balance: f64,
    /// Token pricing configuration
    #[serde(default)]
    pub token_pricing: TokenPricing,
    /// Minimum evaluation score to receive payment (0.0-1.0)
    #[serde(default = "default_min_threshold")]
    pub min_evaluation_threshold: f64,
}

fn default_initial_balance() -> f64 {
    1000.0
}

fn default_min_threshold() -> f64 {
    0.6
}

impl Default for EconomicConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            initial_balance: default_initial_balance(),
            token_pricing: TokenPricing::default(),
            min_evaluation_threshold: default_min_threshold(),
        }
    }
}

/// Task-level tracking state (in-memory during task execution).
#[derive(Debug, Clone, Default)]
struct TaskState {
    /// Current task ID
    task_id: Option<String>,
    /// Date the task was assigned
    task_date: Option<String>,
    /// Task start timestamp
    start_time: Option<DateTime<Utc>>,
    /// Costs accumulated for this task
    costs: CostBreakdown,
    /// LLM call records
    llm_calls: Vec<LlmCallRecord>,
    /// API call records
    api_calls: Vec<ApiCallRecord>,
}

impl TaskState {
    fn reset(&mut self) {
        self.task_id = None;
        self.task_date = None;
        self.start_time = None;
        self.costs.reset();
        self.llm_calls.clear();
        self.api_calls.clear();
    }
}

/// Daily tracking state (accumulated across tasks).
#[derive(Debug, Clone, Default)]
struct DailyState {
    /// Task IDs completed today
    task_ids: Vec<String>,
    /// First task start time
    first_task_start: Option<DateTime<Utc>>,
    /// Last task end time
    last_task_end: Option<DateTime<Utc>>,
    /// Daily cost accumulator
    cost: f64,
}

impl DailyState {
    fn reset(&mut self) {
        self.task_ids.clear();
        self.first_task_start = None;
        self.last_task_end = None;
        self.cost = 0.0;
    }
}

/// Session tracking state.
#[derive(Debug, Clone, Default)]
struct SessionState {
    /// Input tokens this session
    input_tokens: u64,
    /// Output tokens this session
    output_tokens: u64,
    /// Cost this session
    cost: f64,
}

impl SessionState {
    fn reset(&mut self) {
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.cost = 0.0;
    }
}

/// Economic tracker for managing agent survival economics.
///
/// Tracks:
/// - Balance (starting capital minus costs plus income)
/// - Token costs separated by channel (LLM, search, OCR, etc.)
/// - Work income with evaluation threshold
/// - Trading profits/losses
/// - Survival status
///
/// Persists records to JSONL files for durability and analysis.
pub struct EconomicTracker {
    /// Configuration
    config: EconomicConfig,
    /// Agent signature/name
    signature: String,
    /// Data directory for persistence
    data_path: PathBuf,
    /// Current balance (protected by mutex for thread safety)
    state: Arc<Mutex<TrackerState>>,
}

/// Internal mutable state.
struct TrackerState {
    /// Current balance
    balance: f64,
    /// Initial balance (for status calculation)
    initial_balance: f64,
    /// Cumulative totals
    total_token_cost: f64,
    total_work_income: f64,
    total_trading_profit: f64,
    /// Task-level tracking
    task: TaskState,
    /// Daily tracking
    daily: DailyState,
    /// Session tracking
    session: SessionState,
}

impl EconomicTracker {
    /// Create a new economic tracker.
    ///
    /// # Arguments
    /// * `signature` - Agent signature/name for identification
    /// * `config` - Economic configuration
    /// * `data_path` - Optional custom data path (defaults to `./data/agent_data/{signature}/economic`)
    pub fn new(
        signature: impl Into<String>,
        config: EconomicConfig,
        data_path: Option<PathBuf>,
    ) -> Self {
        let signature = signature.into();
        let data_path = data_path
            .unwrap_or_else(|| PathBuf::from(format!("./data/agent_data/{}/economic", signature)));

        Self {
            signature,
            state: Arc::new(Mutex::new(TrackerState {
                balance: config.initial_balance,
                initial_balance: config.initial_balance,
                total_token_cost: 0.0,
                total_work_income: 0.0,
                total_trading_profit: 0.0,
                task: TaskState::default(),
                daily: DailyState::default(),
                session: SessionState::default(),
            })),
            config,
            data_path,
        }
    }

    /// Initialize the tracker, loading existing state or creating new.
    pub fn initialize(&self) -> Result<()> {
        fs::create_dir_all(&self.data_path).with_context(|| {
            format!(
                "Failed to create data directory: {}",
                self.data_path.display()
            )
        })?;

        let balance_file = self.balance_file_path();

        if balance_file.exists() {
            self.load_latest_state()?;
            let state = self.state.lock();
            tracing::info!(
                "📊 Loaded economic state for {}: balance=${:.2}, status={}",
                self.signature,
                state.balance,
                self.get_survival_status_inner(&state)
            );
        } else {
            self.save_balance_record("initialization", 0.0, 0.0, 0.0, Vec::new(), false)?;
            tracing::info!(
                "✅ Initialized economic tracker for {}: starting balance=${:.2}",
                self.signature,
                self.config.initial_balance
            );
        }

        Ok(())
    }

    /// Start tracking costs for a new task.
    pub fn start_task(&self, task_id: impl Into<String>, date: Option<String>) {
        let task_id = task_id.into();
        let date = date.unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
        let now = Utc::now();

        let mut state = self.state.lock();
        state.task.task_id = Some(task_id.clone());
        state.task.task_date = Some(date);
        state.task.start_time = Some(now);
        state.task.costs.reset();
        state.task.llm_calls.clear();
        state.task.api_calls.clear();

        // Track daily window
        if state.daily.first_task_start.is_none() {
            state.daily.first_task_start = Some(now);
        }
        state.daily.task_ids.push(task_id);
    }

    /// End tracking for current task and save consolidated record.
    pub fn end_task(&self) -> Result<()> {
        let mut state = self.state.lock();

        if state.task.task_id.is_some() {
            self.save_task_record_inner(&state)?;
            state.daily.last_task_end = Some(Utc::now());
            state.task.reset();
        }

        Ok(())
    }

    /// Track LLM token usage.
    ///
    /// # Arguments
    /// * `input_tokens` - Number of input tokens
    /// * `output_tokens` - Number of output tokens
    /// * `api_name` - Origin of the call (e.g., "agent", "wrapup")
    /// * `cost` - Pre-computed cost (if provided, skips local calculation)
    ///
    /// # Returns
    /// The cost in USD for this call.
    pub fn track_tokens(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        api_name: impl Into<String>,
        cost: Option<f64>,
    ) -> f64 {
        let api_name = api_name.into();
        let cost = cost.unwrap_or_else(|| {
            self.config
                .token_pricing
                .calculate_cost(input_tokens, output_tokens)
        });

        let mut state = self.state.lock();

        // Update session tracking
        state.session.input_tokens += input_tokens;
        state.session.output_tokens += output_tokens;
        state.session.cost += cost;
        state.daily.cost += cost;

        // Update task-level tracking
        state.task.costs.llm_tokens += cost;
        state.task.llm_calls.push(LlmCallRecord {
            timestamp: Utc::now(),
            api_name,
            input_tokens,
            output_tokens,
            cost,
        });

        // Update totals
        state.total_token_cost += cost;
        state.balance -= cost;

        cost
    }

    /// Track token-based API call cost.
    ///
    /// # Arguments
    /// * `tokens` - Number of tokens used
    /// * `price_per_million` - Price per million tokens
    /// * `api_name` - Name of the API
    ///
    /// # Returns
    /// The cost in USD for this call.
    pub fn track_api_call(
        &self,
        tokens: u64,
        price_per_million: f64,
        api_name: impl Into<String>,
    ) -> f64 {
        let api_name = api_name.into();
        let cost = (tokens as f64 / 1_000_000.0) * price_per_million;

        self.record_api_cost(
            &api_name,
            cost,
            Some(tokens),
            Some(price_per_million),
            PricingModel::PerToken,
        );

        cost
    }

    /// Track flat-rate API call cost.
    ///
    /// # Arguments
    /// * `cost` - Flat cost in USD
    /// * `api_name` - Name of the API
    ///
    /// # Returns
    /// The cost (same as input).
    pub fn track_flat_api_call(&self, cost: f64, api_name: impl Into<String>) -> f64 {
        let api_name = api_name.into();
        self.record_api_cost(&api_name, cost, None, None, PricingModel::FlatRate);
        cost
    }

    fn record_api_cost(
        &self,
        api_name: &str,
        cost: f64,
        tokens: Option<u64>,
        price_per_million: Option<f64>,
        pricing_model: PricingModel,
    ) {
        let mut state = self.state.lock();

        // Update session/daily
        state.session.cost += cost;
        state.daily.cost += cost;

        // Categorize by API type
        let api_lower = api_name.to_lowercase();
        if api_lower.contains("search")
            || api_lower.contains("jina")
            || api_lower.contains("tavily")
        {
            state.task.costs.search_api += cost;
        } else if api_lower.contains("ocr") {
            state.task.costs.ocr_api += cost;
        } else {
            state.task.costs.other_api += cost;
        }

        // Record detailed call
        state.task.api_calls.push(ApiCallRecord {
            timestamp: Utc::now(),
            api_name: api_name.to_string(),
            pricing_model,
            tokens,
            price_per_million,
            cost,
        });

        // Update totals
        state.total_token_cost += cost;
        state.balance -= cost;
    }

    /// Add income from completed work with evaluation threshold.
    ///
    /// Payment is only awarded if `evaluation_score >= min_evaluation_threshold`.
    ///
    /// # Arguments
    /// * `amount` - Base payment amount in USD
    /// * `task_id` - Task identifier
    /// * `evaluation_score` - Score from 0.0 to 1.0
    /// * `description` - Optional description
    ///
    /// # Returns
    /// Actual payment received (0.0 if below threshold).
    pub fn add_work_income(
        &self,
        amount: f64,
        task_id: impl Into<String>,
        evaluation_score: f64,
        description: impl Into<String>,
    ) -> Result<f64> {
        let task_id = task_id.into();
        let description = description.into();
        let threshold = self.config.min_evaluation_threshold;

        let actual_payment = if evaluation_score >= threshold {
            amount
        } else {
            0.0
        };

        {
            let mut state = self.state.lock();
            if actual_payment > 0.0 {
                state.balance += actual_payment;
                state.total_work_income += actual_payment;
                tracing::info!(
                    "💰 Work income: +${:.2} (Task: {}, Score: {:.2})",
                    actual_payment,
                    task_id,
                    evaluation_score
                );
            } else {
                tracing::warn!(
                    "⚠️ Work below threshold (score: {:.2} < {:.2}), no payment for task: {}",
                    evaluation_score,
                    threshold,
                    task_id
                );
            }
        }

        self.log_work_income(
            &task_id,
            amount,
            actual_payment,
            evaluation_score,
            &description,
        )?;

        Ok(actual_payment)
    }

    /// Add profit/loss from trading.
    pub fn add_trading_profit(&self, profit: f64, _description: impl Into<String>) {
        let mut state = self.state.lock();
        state.balance += profit;
        state.total_trading_profit += profit;

        let sign = if profit >= 0.0 { "+" } else { "" };
        tracing::info!(
            "📈 Trading P&L: {}${:.2}, new balance: ${:.2}",
            sign,
            profit,
            state.balance
        );
    }

    /// Save end-of-day economic state.
    pub fn save_daily_state(
        &self,
        date: &str,
        work_income: f64,
        trading_profit: f64,
        completed_tasks: Vec<String>,
        api_error: bool,
    ) -> Result<()> {
        let daily_cost = {
            let state = self.state.lock();
            state.daily.cost
        };

        self.save_balance_record(
            date,
            daily_cost,
            work_income,
            trading_profit,
            completed_tasks,
            api_error,
        )?;

        // Reset daily tracking
        {
            let mut state = self.state.lock();
            state.daily.reset();
            state.session.reset();
        }

        tracing::info!("💾 Saved daily state for {}", date);

        Ok(())
    }

    /// Get current balance.
    pub fn get_balance(&self) -> f64 {
        self.state.lock().balance
    }

    /// Get net worth (balance + portfolio value).
    pub fn get_net_worth(&self) -> f64 {
        // TODO: Add trading portfolio value
        self.get_balance()
    }

    /// Get current survival status.
    pub fn get_survival_status(&self) -> SurvivalStatus {
        let state = self.state.lock();
        self.get_survival_status_inner(&state)
    }

    fn get_survival_status_inner(&self, state: &TrackerState) -> SurvivalStatus {
        SurvivalStatus::from_balance(state.balance, state.initial_balance)
    }

    /// Check if agent is bankrupt.
    pub fn is_bankrupt(&self) -> bool {
        self.get_survival_status() == SurvivalStatus::Bankrupt
    }

    /// Get session cost so far.
    pub fn get_session_cost(&self) -> f64 {
        self.state.lock().session.cost
    }

    /// Get daily cost so far.
    pub fn get_daily_cost(&self) -> f64 {
        self.state.lock().daily.cost
    }

    /// Get comprehensive economic summary.
    pub fn get_summary(&self) -> EconomicSummary {
        let state = self.state.lock();
        EconomicSummary {
            signature: self.signature.clone(),
            balance: state.balance,
            initial_balance: state.initial_balance,
            net_worth: state.balance, // TODO: Add portfolio
            total_token_cost: state.total_token_cost,
            total_work_income: state.total_work_income,
            total_trading_profit: state.total_trading_profit,
            session_cost: state.session.cost,
            daily_cost: state.daily.cost,
            session_input_tokens: state.session.input_tokens,
            session_output_tokens: state.session.output_tokens,
            survival_status: self.get_survival_status_inner(&state),
            is_bankrupt: self.get_survival_status_inner(&state) == SurvivalStatus::Bankrupt,
            min_evaluation_threshold: self.config.min_evaluation_threshold,
        }
    }

    /// Reset session tracking (for new decision/activity).
    pub fn reset_session(&self) {
        self.state.lock().session.reset();
    }

    /// Record task completion statistics.
    pub fn record_task_completion(
        &self,
        task_id: impl Into<String>,
        work_submitted: bool,
        wall_clock_seconds: f64,
        evaluation_score: f64,
        money_earned: f64,
        attempt: u32,
        date: Option<String>,
    ) -> Result<()> {
        let task_id = task_id.into();
        let date = date
            .or_else(|| self.state.lock().task.task_date.clone())
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());

        let record = TaskCompletionRecord {
            task_id: task_id.clone(),
            date,
            attempt,
            work_submitted,
            evaluation_score,
            money_earned,
            wall_clock_seconds,
            timestamp: Utc::now(),
        };

        // Read existing records, filter out this task_id
        let completions_file = self.task_completions_file_path();
        let mut existing: Vec<String> = Vec::new();

        if completions_file.exists() {
            let file = File::open(&completions_file)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<TaskCompletionRecord>(&line) {
                    if entry.task_id != task_id {
                        existing.push(line);
                    }
                } else {
                    existing.push(line);
                }
            }
        }

        // Rewrite with updated record
        let mut file = File::create(&completions_file)?;
        for line in existing {
            writeln!(file, "{}", line)?;
        }
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        file.sync_all()?;

        Ok(())
    }

    // ── Private helpers ──

    fn balance_file_path(&self) -> PathBuf {
        self.data_path.join("balance.jsonl")
    }

    fn token_costs_file_path(&self) -> PathBuf {
        self.data_path.join("token_costs.jsonl")
    }

    fn task_completions_file_path(&self) -> PathBuf {
        self.data_path.join("task_completions.jsonl")
    }

    fn load_latest_state(&self) -> Result<()> {
        let balance_file = self.balance_file_path();
        let file = File::open(&balance_file)?;
        let reader = BufReader::new(file);

        let mut last_record: Option<BalanceRecord> = None;
        for line in reader.lines() {
            let line = line?;
            if let Ok(record) = serde_json::from_str::<BalanceRecord>(&line) {
                last_record = Some(record);
            }
        }

        if let Some(record) = last_record {
            let mut state = self.state.lock();
            state.balance = record.balance;
            state.total_token_cost = record.total_token_cost;
            state.total_work_income = record.total_work_income;
            state.total_trading_profit = record.total_trading_profit;
        }

        Ok(())
    }

    fn save_task_record_inner(&self, state: &TrackerState) -> Result<()> {
        let Some(ref task_id) = state.task.task_id else {
            return Ok(());
        };

        let total_input = state.task.llm_calls.iter().map(|c| c.input_tokens).sum();
        let total_output = state.task.llm_calls.iter().map(|c| c.output_tokens).sum();
        let llm_call_count = state.task.llm_calls.len();

        let token_based = state
            .task
            .api_calls
            .iter()
            .filter(|c| c.pricing_model == PricingModel::PerToken)
            .count();
        let flat_rate = state
            .task
            .api_calls
            .iter()
            .filter(|c| c.pricing_model == PricingModel::FlatRate)
            .count();

        let record = TaskCostRecord {
            timestamp_end: Utc::now(),
            timestamp_start: state.task.start_time.unwrap_or_else(Utc::now),
            date: state
                .task
                .task_date
                .clone()
                .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string()),
            task_id: task_id.clone(),
            llm_usage: LlmUsageSummary {
                total_calls: llm_call_count,
                total_input_tokens: total_input,
                total_output_tokens: total_output,
                total_tokens: total_input + total_output,
                total_cost: state.task.costs.llm_tokens,
                input_price_per_million: self.config.token_pricing.input_price_per_million,
                output_price_per_million: self.config.token_pricing.output_price_per_million,
                calls_detail: state.task.llm_calls.clone(),
            },
            api_usage: ApiUsageSummary {
                total_calls: state.task.api_calls.len(),
                search_api_cost: state.task.costs.search_api,
                ocr_api_cost: state.task.costs.ocr_api,
                other_api_cost: state.task.costs.other_api,
                token_based_calls: token_based,
                flat_rate_calls: flat_rate,
                calls_detail: state.task.api_calls.clone(),
            },
            cost_summary: state.task.costs.clone(),
            balance_after: state.balance,
            session_cost: state.session.cost,
            daily_cost: state.daily.cost,
        };

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.token_costs_file_path())?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        file.sync_all()?;

        Ok(())
    }

    fn save_balance_record(
        &self,
        date: &str,
        token_cost_delta: f64,
        work_income_delta: f64,
        trading_profit_delta: f64,
        completed_tasks: Vec<String>,
        api_error: bool,
    ) -> Result<()> {
        let state = self.state.lock();

        let task_completion_time = match (state.daily.first_task_start, state.daily.last_task_end) {
            (Some(start), Some(end)) => Some((end - start).num_seconds() as f64),
            _ => None,
        };

        let record = BalanceRecord {
            date: date.to_string(),
            balance: state.balance,
            token_cost_delta,
            work_income_delta,
            trading_profit_delta,
            total_token_cost: state.total_token_cost,
            total_work_income: state.total_work_income,
            total_trading_profit: state.total_trading_profit,
            net_worth: state.balance,
            survival_status: self.get_survival_status_inner(&state).to_string(),
            completed_tasks,
            task_id: state.daily.task_ids.first().cloned(),
            task_completion_time_seconds: task_completion_time,
            api_error,
        };

        drop(state); // Release lock before IO

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.balance_file_path())?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        file.sync_all()?;

        Ok(())
    }

    fn log_work_income(
        &self,
        task_id: &str,
        base_amount: f64,
        actual_payment: f64,
        evaluation_score: f64,
        description: &str,
    ) -> Result<()> {
        let state = self.state.lock();

        let record = WorkIncomeRecord {
            timestamp: Utc::now(),
            date: state
                .task
                .task_date
                .clone()
                .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string()),
            task_id: task_id.to_string(),
            base_amount,
            actual_payment,
            evaluation_score,
            threshold: self.config.min_evaluation_threshold,
            payment_awarded: actual_payment > 0.0,
            description: description.to_string(),
            balance_after: state.balance,
        };

        drop(state);

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.token_costs_file_path())?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        file.sync_all()?;

        Ok(())
    }
}

impl std::fmt::Display for EconomicTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        write!(
            f,
            "EconomicTracker(signature='{}', balance=${:.2}, status={})",
            self.signature,
            state.balance,
            self.get_survival_status_inner(&state)
        )
    }
}

/// Comprehensive economic summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EconomicSummary {
    pub signature: String,
    pub balance: f64,
    pub initial_balance: f64,
    pub net_worth: f64,
    pub total_token_cost: f64,
    pub total_work_income: f64,
    pub total_trading_profit: f64,
    pub session_cost: f64,
    pub daily_cost: f64,
    pub session_input_tokens: u64,
    pub session_output_tokens: u64,
    pub survival_status: SurvivalStatus,
    pub is_bankrupt: bool,
    pub min_evaluation_threshold: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config() -> EconomicConfig {
        EconomicConfig {
            enabled: true,
            initial_balance: 1000.0,
            token_pricing: TokenPricing {
                input_price_per_million: 3.0,
                output_price_per_million: 15.0,
            },
            min_evaluation_threshold: 0.6,
        }
    }

    #[test]
    fn tracker_initialization() {
        let tmp = TempDir::new().unwrap();
        let config = test_config();
        let tracker = EconomicTracker::new("test-agent", config, Some(tmp.path().to_path_buf()));

        tracker.initialize().unwrap();

        assert!((tracker.get_balance() - 1000.0).abs() < f64::EPSILON);
        assert_eq!(tracker.get_survival_status(), SurvivalStatus::Thriving);
    }

    #[test]
    fn track_tokens_reduces_balance() {
        let tmp = TempDir::new().unwrap();
        let tracker =
            EconomicTracker::new("test-agent", test_config(), Some(tmp.path().to_path_buf()));
        tracker.initialize().unwrap();

        tracker.start_task("task-1", None);
        let cost = tracker.track_tokens(1000, 500, "agent", None);
        tracker.end_task().unwrap();

        // (1000/1M)*3 + (500/1M)*15 = 0.003 + 0.0075 = 0.0105
        assert!((cost - 0.0105).abs() < 0.0001);
        assert!((tracker.get_balance() - (1000.0 - 0.0105)).abs() < 0.0001);
    }

    #[test]
    fn work_income_with_threshold() {
        let tmp = TempDir::new().unwrap();
        let tracker =
            EconomicTracker::new("test-agent", test_config(), Some(tmp.path().to_path_buf()));
        tracker.initialize().unwrap();

        // Below threshold - no payment
        let payment = tracker.add_work_income(100.0, "task-1", 0.5, "").unwrap();
        assert!((payment - 0.0).abs() < f64::EPSILON);
        assert!((tracker.get_balance() - 1000.0).abs() < f64::EPSILON);

        // At threshold - payment awarded
        let payment = tracker.add_work_income(100.0, "task-2", 0.6, "").unwrap();
        assert!((payment - 100.0).abs() < f64::EPSILON);
        assert!((tracker.get_balance() - 1100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn survival_status_changes() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config();
        config.initial_balance = 100.0;

        let tracker = EconomicTracker::new("test-agent", config, Some(tmp.path().to_path_buf()));
        tracker.initialize().unwrap();

        assert_eq!(tracker.get_survival_status(), SurvivalStatus::Thriving);

        // Spend 30% - should be stable
        tracker.track_tokens(10_000_000, 0, "agent", Some(30.0));
        assert_eq!(tracker.get_survival_status(), SurvivalStatus::Stable);

        // Spend more to reach struggling
        tracker.track_tokens(10_000_000, 0, "agent", Some(35.0));
        assert_eq!(tracker.get_survival_status(), SurvivalStatus::Struggling);

        // Spend more to reach critical
        tracker.track_tokens(10_000_000, 0, "agent", Some(25.0));
        assert_eq!(tracker.get_survival_status(), SurvivalStatus::Critical);

        // Bankrupt
        tracker.track_tokens(10_000_000, 0, "agent", Some(20.0));
        assert_eq!(tracker.get_survival_status(), SurvivalStatus::Bankrupt);
        assert!(tracker.is_bankrupt());
    }

    #[test]
    fn state_persistence() {
        let tmp = TempDir::new().unwrap();
        let config = test_config();

        // Create tracker, do some work, save state
        {
            let tracker =
                EconomicTracker::new("test-agent", config.clone(), Some(tmp.path().to_path_buf()));
            tracker.initialize().unwrap();
            tracker.track_tokens(1000, 500, "agent", Some(10.0));
            tracker
                .save_daily_state("2025-01-01", 0.0, 0.0, vec![], false)
                .unwrap();
        }

        // Create new tracker, should load state
        {
            let tracker =
                EconomicTracker::new("test-agent", config, Some(tmp.path().to_path_buf()));
            tracker.initialize().unwrap();
            assert!((tracker.get_balance() - 990.0).abs() < 0.01);
        }
    }

    #[test]
    fn api_call_categorization() {
        let tmp = TempDir::new().unwrap();
        let tracker =
            EconomicTracker::new("test-agent", test_config(), Some(tmp.path().to_path_buf()));
        tracker.initialize().unwrap();

        tracker.start_task("task-1", None);

        // Search API
        tracker.track_flat_api_call(0.001, "tavily_search");

        // OCR API
        tracker.track_api_call(1000, 1.0, "ocr_reader");

        // Other API
        tracker.track_flat_api_call(0.01, "some_api");

        tracker.end_task().unwrap();

        // Balance should reflect all costs
        let expected_reduction = 0.001 + 0.001 + 0.01; // search + ocr + other
        assert!((tracker.get_balance() - (1000.0 - expected_reduction)).abs() < 0.0001);
    }
}
