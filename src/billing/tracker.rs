//! Cost tracking engine for API usage billing.
//!
//! Records per-request costs in a local SQLite ledger and enforces
//! configurable spending limits.

use chrono::Datelike;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default daily spending limit in USD (0 = no limit).
const DEFAULT_DAILY_LIMIT_USD: f64 = 0.0;

/// Default monthly spending limit in USD (0 = no limit).
const DEFAULT_MONTHLY_LIMIT_USD: f64 = 0.0;

/// A single cost ledger entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEntry {
    /// Provider name (e.g., "openai", "anthropic").
    pub provider: String,
    /// Model name (e.g., "gpt-4o", "claude-sonnet-4").
    pub model: String,
    /// Input tokens consumed.
    pub input_tokens: i64,
    /// Output tokens consumed.
    pub output_tokens: i64,
    /// Estimated cost in USD.
    pub cost_usd: f64,
    /// Channel that triggered this request (optional).
    pub channel: Option<String>,
    /// Unix timestamp (seconds).
    pub timestamp: i64,
}

/// Aggregated usage summary for reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSummary {
    /// Total cost in USD.
    pub total_cost_usd: f64,
    /// Total input tokens.
    pub total_input_tokens: i64,
    /// Total output tokens.
    pub total_output_tokens: i64,
    /// Number of requests.
    pub request_count: i64,
    /// Breakdown by provider.
    pub by_provider: Vec<ProviderUsage>,
}

/// Per-provider usage breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: String,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub request_count: i64,
}

/// Cost tracker with SQLite persistence and spending limits.
pub struct CostTracker {
    /// Persistent SQLite connection (avoids per-operation open overhead).
    conn: Option<Connection>,
    daily_limit_usd: f64,
    monthly_limit_usd: f64,
    enabled: bool,
}

impl CostTracker {
    /// Create a new cost tracker for the given workspace.
    pub fn new(workspace_dir: &Path, enabled: bool) -> anyhow::Result<Self> {
        let conn = if enabled {
            let db_path = workspace_dir.join("billing.db");
            let conn = Connection::open(&db_path)?;
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA busy_timeout = 5000;",
            )?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS cost_ledger (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    provider TEXT NOT NULL,
                    model TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cost_usd REAL NOT NULL DEFAULT 0.0,
                    channel TEXT,
                    timestamp INTEGER NOT NULL,
                    created_at TEXT DEFAULT (datetime('now'))
                );
                CREATE INDEX IF NOT EXISTS idx_cost_timestamp ON cost_ledger(timestamp);
                CREATE INDEX IF NOT EXISTS idx_cost_provider ON cost_ledger(provider);",
            )?;
            Some(conn)
        } else {
            None
        };

        Ok(Self {
            conn,
            daily_limit_usd: DEFAULT_DAILY_LIMIT_USD,
            monthly_limit_usd: DEFAULT_MONTHLY_LIMIT_USD,
            enabled,
        })
    }

    /// Set daily spending limit in USD.
    pub fn set_daily_limit(&mut self, limit_usd: f64) {
        self.daily_limit_usd = limit_usd;
    }

    /// Set monthly spending limit in USD.
    pub fn set_monthly_limit(&mut self, limit_usd: f64) {
        self.monthly_limit_usd = limit_usd;
    }

    /// Record a cost entry in the ledger.
    pub fn record(&self, entry: &CostEntry) -> anyhow::Result<()> {
        let Some(ref conn) = self.conn else {
            return Ok(());
        };

        conn.execute(
            "INSERT INTO cost_ledger (provider, model, input_tokens, output_tokens, cost_usd, channel, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.provider,
                entry.model,
                entry.input_tokens,
                entry.output_tokens,
                entry.cost_usd,
                entry.channel,
                entry.timestamp,
            ],
        )?;

        Ok(())
    }

    /// Check if daily spending limit has been exceeded.
    pub fn check_daily_limit(&self) -> anyhow::Result<bool> {
        if self.daily_limit_usd <= 0.0 {
            return Ok(false);
        }

        let Some(ref conn) = self.conn else {
            return Ok(false);
        };

        let today_start = today_start_epoch();
        let total: f64 = conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM cost_ledger WHERE timestamp >= ?1",
            params![today_start],
            |row| row.get(0),
        )?;

        Ok(total >= self.daily_limit_usd)
    }

    /// Check if monthly spending limit has been exceeded.
    pub fn check_monthly_limit(&self) -> anyhow::Result<bool> {
        if self.monthly_limit_usd <= 0.0 {
            return Ok(false);
        }

        let Some(ref conn) = self.conn else {
            return Ok(false);
        };

        let month_start = month_start_epoch();
        let total: f64 = conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM cost_ledger WHERE timestamp >= ?1",
            params![month_start],
            |row| row.get(0),
        )?;

        Ok(total >= self.monthly_limit_usd)
    }

    /// Get today's total spending.
    pub fn today_total(&self) -> anyhow::Result<f64> {
        let Some(ref conn) = self.conn else {
            return Ok(0.0);
        };

        let today_start = today_start_epoch();
        let total: f64 = conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM cost_ledger WHERE timestamp >= ?1",
            params![today_start],
            |row| row.get(0),
        )?;

        Ok(total)
    }

    /// Get this month's total spending.
    pub fn month_total(&self) -> anyhow::Result<f64> {
        let Some(ref conn) = self.conn else {
            return Ok(0.0);
        };

        let month_start = month_start_epoch();
        let total: f64 = conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM cost_ledger WHERE timestamp >= ?1",
            params![month_start],
            |row| row.get(0),
        )?;

        Ok(total)
    }

    /// Get a usage summary for a given time range.
    pub fn summary(&self, from_timestamp: i64, to_timestamp: i64) -> anyhow::Result<UsageSummary> {
        let Some(ref conn) = self.conn else {
            return Ok(UsageSummary {
                total_cost_usd: 0.0,
                total_input_tokens: 0,
                total_output_tokens: 0,
                request_count: 0,
                by_provider: Vec::new(),
            });
        };

        // Overall totals
        let (total_cost, total_input, total_output, count): (f64, i64, i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0), COALESCE(SUM(input_tokens), 0),
                        COALESCE(SUM(output_tokens), 0), COUNT(*)
                 FROM cost_ledger WHERE timestamp >= ?1 AND timestamp <= ?2",
            params![from_timestamp, to_timestamp],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;

        // Per-provider breakdown
        let mut stmt = conn.prepare_cached(
            "SELECT provider, COALESCE(SUM(cost_usd), 0.0), COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0), COUNT(*)
             FROM cost_ledger WHERE timestamp >= ?1 AND timestamp <= ?2
             GROUP BY provider ORDER BY SUM(cost_usd) DESC",
        )?;

        let by_provider: Vec<ProviderUsage> = stmt
            .query_map(params![from_timestamp, to_timestamp], |row| {
                Ok(ProviderUsage {
                    provider: row.get(0)?,
                    cost_usd: row.get(1)?,
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                    request_count: row.get(4)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(UsageSummary {
            total_cost_usd: total_cost,
            total_input_tokens: total_input,
            total_output_tokens: total_output,
            request_count: count,
            by_provider,
        })
    }

    /// Estimate cost for a given model based on token counts.
    ///
    /// Uses the comprehensive pricing registry for accurate per-model rates.
    /// Falls back to conservative defaults ($1.00/$3.00 per 1M) for unknown models.
    pub fn estimate_cost(model: &str, input_tokens: i64, output_tokens: i64) -> f64 {
        let registry = super::pricing::PricingRegistry::defaults();
        let (cost, _found) = registry.estimate_cost(model, input_tokens, output_tokens);
        cost
    }
}

/// Get the Unix epoch timestamp for the start of today (UTC).
fn today_start_epoch() -> i64 {
    let now = chrono::Utc::now();
    let today = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
    today.and_utc().timestamp()
}

/// Get the Unix epoch timestamp for the start of this month (UTC).
fn month_start_epoch() -> i64 {
    let now = chrono::Utc::now();
    let month_start = now
        .date_naive()
        .with_day(1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    month_start.and_utc().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_tracker() -> (TempDir, CostTracker) {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(tmp.path(), true).unwrap();
        (tmp, tracker)
    }

    fn now_epoch() -> i64 {
        chrono::Utc::now().timestamp()
    }

    #[test]
    fn record_and_query_costs() {
        let (_tmp, tracker) = make_tracker();
        let now = now_epoch();

        tracker
            .record(&CostEntry {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                input_tokens: 1000,
                output_tokens: 500,
                cost_usd: 0.0075,
                channel: Some("telegram".into()),
                timestamp: now,
            })
            .unwrap();

        let today = tracker.today_total().unwrap();
        assert!(today > 0.0);
    }

    #[test]
    fn daily_limit_enforcement() {
        let (_tmp, mut tracker) = make_tracker();
        tracker.set_daily_limit(0.01);
        let now = now_epoch();

        // Under limit
        tracker
            .record(&CostEntry {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                input_tokens: 100,
                output_tokens: 50,
                cost_usd: 0.005,
                channel: None,
                timestamp: now,
            })
            .unwrap();
        assert!(!tracker.check_daily_limit().unwrap());

        // Over limit
        tracker
            .record(&CostEntry {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                input_tokens: 100,
                output_tokens: 50,
                cost_usd: 0.006,
                channel: None,
                timestamp: now,
            })
            .unwrap();
        assert!(tracker.check_daily_limit().unwrap());
    }

    #[test]
    fn summary_aggregation() {
        let (_tmp, tracker) = make_tracker();
        let now = now_epoch();

        tracker
            .record(&CostEntry {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                input_tokens: 1000,
                output_tokens: 500,
                cost_usd: 0.01,
                channel: None,
                timestamp: now,
            })
            .unwrap();

        tracker
            .record(&CostEntry {
                provider: "anthropic".into(),
                model: "claude-sonnet-4".into(),
                input_tokens: 2000,
                output_tokens: 1000,
                cost_usd: 0.02,
                channel: None,
                timestamp: now,
            })
            .unwrap();

        let summary = tracker.summary(now - 3600, now + 3600).unwrap();
        assert_eq!(summary.request_count, 2);
        assert!((summary.total_cost_usd - 0.03).abs() < 0.001);
        assert_eq!(summary.by_provider.len(), 2);
    }

    #[test]
    fn estimate_cost_known_models() {
        let cost = CostTracker::estimate_cost("gpt-4o", 1_000_000, 1_000_000);
        assert!((cost - 12.50).abs() < 0.01); // $2.50 input + $10.00 output

        let cost = CostTracker::estimate_cost("claude-sonnet-4", 1_000_000, 1_000_000);
        assert!((cost - 18.00).abs() < 0.01); // $3.00 input + $15.00 output
    }

    #[test]
    fn disabled_tracker_returns_defaults() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(tmp.path(), false).unwrap();

        assert_eq!(tracker.today_total().unwrap(), 0.0);
        assert!(!tracker.check_daily_limit().unwrap());

        let summary = tracker.summary(0, i64::MAX).unwrap();
        assert_eq!(summary.request_count, 0);
    }

    #[test]
    fn record_disabled_is_noop() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(tmp.path(), false).unwrap();

        tracker
            .record(&CostEntry {
                provider: "test".into(),
                model: "test".into(),
                input_tokens: 100,
                output_tokens: 50,
                cost_usd: 0.01,
                channel: None,
                timestamp: 12345,
            })
            .unwrap();
    }
}
