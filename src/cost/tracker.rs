use super::types::{BudgetCheck, CostRecord, CostSummary, TokenUsage, UsagePeriod};
use crate::config::CostConfig;
use anyhow::{Context, Result};
use chrono::Datelike;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Cost tracker for API usage monitoring and budget enforcement.
pub struct CostTracker {
    config: CostConfig,
    storage: Arc<Mutex<CostStorage>>,
    session_id: String,
    session_costs: Arc<Mutex<Vec<CostRecord>>>,
}

impl CostTracker {
    /// Create a new cost tracker.
    pub fn new(config: CostConfig, workspace_dir: &PathBuf) -> Result<Self> {
        let storage_path = workspace_dir.join(".zeroclaw").join("costs.db");

        let storage = CostStorage::new(&storage_path)
            .with_context(|| format!("Failed to open cost storage at {}", storage_path.display()))?;

        Ok(Self {
            config,
            storage: Arc::new(Mutex::new(storage)),
            session_id: uuid::Uuid::new_v4().to_string(),
            session_costs: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Check if a request is within budget.
    pub fn check_budget(&self, estimated_cost_usd: f64) -> Result<BudgetCheck> {
        if !self.config.enabled {
            return Ok(BudgetCheck::Allowed);
        }

        let storage = self.storage.lock().unwrap();
        let (daily_cost, monthly_cost) = storage.get_aggregated_costs()?;

        // Check daily limit
        let projected_daily = daily_cost + estimated_cost_usd;
        if projected_daily > self.config.daily_limit_usd {
            return Ok(BudgetCheck::Exceeded {
                current_usd: daily_cost,
                limit_usd: self.config.daily_limit_usd,
                period: UsagePeriod::Day,
            });
        }

        // Check monthly limit
        let projected_monthly = monthly_cost + estimated_cost_usd;
        if projected_monthly > self.config.monthly_limit_usd {
            return Ok(BudgetCheck::Exceeded {
                current_usd: monthly_cost,
                limit_usd: self.config.monthly_limit_usd,
                period: UsagePeriod::Month,
            });
        }

        // Check warning thresholds
        let warn_threshold = self.config.warn_at_percent as f64 / 100.0;
        let daily_warn_threshold = self.config.daily_limit_usd * warn_threshold;
        let monthly_warn_threshold = self.config.monthly_limit_usd * warn_threshold;

        if projected_daily >= daily_warn_threshold {
            return Ok(BudgetCheck::Warning {
                current_usd: daily_cost,
                limit_usd: self.config.daily_limit_usd,
                period: UsagePeriod::Day,
            });
        }

        if projected_monthly >= monthly_warn_threshold {
            return Ok(BudgetCheck::Warning {
                current_usd: monthly_cost,
                limit_usd: self.config.monthly_limit_usd,
                period: UsagePeriod::Month,
            });
        }

        Ok(BudgetCheck::Allowed)
    }

    /// Record a usage event.
    pub fn record_usage(&self, usage: TokenUsage) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let record = CostRecord::new(&self.session_id, usage);

        // Add to session costs
        {
            let mut session_costs = self.session_costs.lock().unwrap();
            session_costs.push(record.clone());
        }

        // Persist to storage
        let mut storage = self.storage.lock().unwrap();
        storage.add_record(record)?;

        Ok(())
    }

    /// Get the current cost summary.
    pub fn get_summary(&self) -> Result<CostSummary> {
        let storage = self.storage.lock().unwrap();
        let (daily_cost, monthly_cost) = storage.get_aggregated_costs()?;
        let by_model = storage.get_costs_by_model()?;

        let session_costs = self.session_costs.lock().unwrap();
        let session_cost: f64 = session_costs.iter().map(|r| r.usage.cost_usd).sum();
        let total_tokens: u64 = session_costs.iter().map(|r| r.usage.total_tokens).sum();
        let request_count = session_costs.len();

        Ok(CostSummary {
            session_cost_usd: session_cost,
            daily_cost_usd: daily_cost,
            monthly_cost_usd: monthly_cost,
            total_tokens,
            request_count,
            by_model,
        })
    }

    /// Get the daily cost for a specific date.
    pub fn get_daily_cost(&self, date: chrono::NaiveDate) -> Result<f64> {
        let storage = self.storage.lock().unwrap();
        Ok(storage.get_cost_for_date(date)?)
    }

    /// Get the monthly cost for a specific month.
    pub fn get_monthly_cost(&self, year: i32, month: u32) -> Result<f64> {
        let storage = self.storage.lock().unwrap();
        Ok(storage.get_cost_for_month(year, month)?)
    }
}

/// Persistent storage for cost records.
struct CostStorage {
    path: PathBuf,
    records: Vec<CostRecord>,
}

impl CostStorage {
    /// Create or open cost storage.
    fn new(path: &PathBuf) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }

        let mut records = Vec::new();

        // Load existing records if file exists
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read cost storage from {}", path.display()))?;

            // Read records line by line (JSONL format)
            for line in content.lines() {
                if let Ok(record) = serde_json::from_str::<CostRecord>(line) {
                    records.push(record);
                }
            }
        }

        Ok(Self { path: path.clone(), records })
    }

    /// Add a new record.
    fn add_record(&mut self, record: CostRecord) -> Result<()> {
        self.records.push(record.clone());

        // Append to file (JSONL format for durability)
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("Failed to open cost storage at {}", self.path.display()))?;

        use std::io::Write;
        writeln!(file, "{}", serde_json::to_string(&record)?)
            .with_context(|| format!("Failed to write cost record to {}", self.path.display()))?;

        Ok(())
    }

    /// Get aggregated costs for current day and month.
    fn get_aggregated_costs(&self) -> Result<(f64, f64)> {
        let now = chrono::Utc::now();
        let today = now.naive_utc().date();
        let current_month = now.month();
        let current_year = now.year();

        let daily_cost: f64 = self
            .records
            .iter()
            .filter(|r| r.usage.timestamp.naive_utc().date() == today)
            .map(|r| r.usage.cost_usd)
            .sum();

        let monthly_cost: f64 = self
            .records
            .iter()
            .filter(|r| {
                let ts = r.usage.timestamp.naive_utc();
                ts.year() == current_year && ts.month() == current_month
            })
            .map(|r| r.usage.cost_usd)
            .sum();

        Ok((daily_cost, monthly_cost))
    }

    /// Get costs grouped by model.
    fn get_costs_by_model(&self) -> Result<std::collections::HashMap<String, super::types::ModelStats>> {
        let mut by_model: std::collections::HashMap<String, super::types::ModelStats> =
            std::collections::HashMap::new();

        for record in &self.records {
            let entry = by_model
                .entry(record.usage.model.clone())
                .or_insert_with(|| super::types::ModelStats {
                    model: record.usage.model.clone(),
                    cost_usd: 0.0,
                    total_tokens: 0,
                    request_count: 0,
                });

            entry.cost_usd += record.usage.cost_usd;
            entry.total_tokens += record.usage.total_tokens;
            entry.request_count += 1;
        }

        Ok(by_model)
    }

    /// Get cost for a specific date.
    fn get_cost_for_date(&self, date: chrono::NaiveDate) -> Result<f64> {
        let cost: f64 = self
            .records
            .iter()
            .filter(|r| r.usage.timestamp.naive_utc().date() == date)
            .map(|r| r.usage.cost_usd)
            .sum();

        Ok(cost)
    }

    /// Get cost for a specific month.
    fn get_cost_for_month(&self, year: i32, month: u32) -> Result<f64> {
        let cost: f64 = self
            .records
            .iter()
            .filter(|r| {
                let ts = r.usage.timestamp.naive_utc();
                ts.year() == year && ts.month() == month
            })
            .map(|r| r.usage.cost_usd)
            .sum();

        Ok(cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn cost_tracker_initialization() {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: true,
            daily_limit_usd: 10.0,
            monthly_limit_usd: 100.0,
            ..Default::default()
        };

        let tracker = CostTracker::new(config, &tmp.path().into()).unwrap();
        assert!(!tracker.session_id().is_empty());
    }

    #[test]
    fn budget_check_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: false,
            ..Default::default()
        };

        let tracker = CostTracker::new(config, &tmp.path().into()).unwrap();
        let check = tracker.check_budget(1000.0).unwrap();
        assert!(matches!(check, BudgetCheck::Allowed));
    }

    #[test]
    fn record_usage_and_get_summary() {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: true,
            ..Default::default()
        };

        let tracker = CostTracker::new(config, &tmp.path().into()).unwrap();

        let usage = TokenUsage::new("test/model", 1000, 500, 1.0, 2.0);
        tracker.record_usage(usage).unwrap();

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 1);
        assert!(summary.session_cost_usd > 0.0);
    }

    #[test]
    fn budget_exceeded_daily_limit() {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: true,
            daily_limit_usd: 0.01, // Very low limit
            ..Default::default()
        };

        let tracker = CostTracker::new(config, &tmp.path().into()).unwrap();

        // Record a usage that exceeds the limit
        let usage = TokenUsage::new("test/model", 10000, 5000, 1.0, 2.0); // ~0.02 USD
        tracker.record_usage(usage).unwrap();

        let check = tracker.check_budget(0.01).unwrap();
        assert!(matches!(check, BudgetCheck::Exceeded { .. }));
    }
}
