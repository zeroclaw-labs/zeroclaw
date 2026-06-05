use super::types::{BudgetCheck, CostRecord, CostSummary, ModelStats, TokenUsage, UsagePeriod};
use daemonclaw_config::schema::CostConfig;
use daemonclaw_config::state_db::StateDb;
use anyhow::{Context, Result, anyhow};
use chrono::{Datelike, NaiveDate, Utc};
use parking_lot::{Mutex, MutexGuard};
use rusqlite::params;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// Cost tracker for API usage monitoring and budget enforcement.
pub struct CostTracker {
    config: CostConfig,
    storage: Arc<Mutex<CostStorage>>,
    session_id: String,
    session_costs: Arc<Mutex<Vec<CostRecord>>>,
}

impl CostTracker {
    /// Create a new cost tracker.
    pub fn new(config: CostConfig, workspace_dir: &Path) -> Result<Self> {
        let storage = CostStorage::new(workspace_dir).with_context(|| {
            format!(
                "Failed to open cost storage in {}",
                workspace_dir.display()
            )
        })?;

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

    fn lock_storage(&self) -> MutexGuard<'_, CostStorage> {
        self.storage.lock()
    }

    fn lock_session_costs(&self) -> MutexGuard<'_, Vec<CostRecord>> {
        self.session_costs.lock()
    }

    /// Check if a request is within budget.
    pub fn check_budget(&self, estimated_cost_usd: f64) -> Result<BudgetCheck> {
        if !self.config.enabled {
            return Ok(BudgetCheck::Allowed);
        }

        if !estimated_cost_usd.is_finite() || estimated_cost_usd < 0.0 {
            return Err(anyhow!(
                "Estimated cost must be a finite, non-negative value"
            ));
        }

        let storage = self.lock_storage();
        let (daily_cost, monthly_cost) = storage.get_aggregated_costs()?;

        let projected_daily = daily_cost + estimated_cost_usd;
        if projected_daily > self.config.daily_limit_usd {
            return Ok(BudgetCheck::Exceeded {
                current_usd: daily_cost,
                limit_usd: self.config.daily_limit_usd,
                period: UsagePeriod::Day,
            });
        }

        let projected_monthly = monthly_cost + estimated_cost_usd;
        if projected_monthly > self.config.monthly_limit_usd {
            return Ok(BudgetCheck::Exceeded {
                current_usd: monthly_cost,
                limit_usd: self.config.monthly_limit_usd,
                period: UsagePeriod::Month,
            });
        }

        let warn_threshold = f64::from(self.config.warn_at_percent.min(100)) / 100.0;
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

        if !usage.cost_usd.is_finite() || usage.cost_usd < 0.0 {
            return Err(anyhow!(
                "Token usage cost must be a finite, non-negative value"
            ));
        }

        let record = CostRecord::new(&self.session_id, usage);

        {
            let storage = self.lock_storage();
            storage.add_record(&record)?;
        }

        let mut session_costs = self.lock_session_costs();
        session_costs.push(record);

        Ok(())
    }

    /// Get the current cost summary.
    pub fn get_summary(&self) -> Result<CostSummary> {
        let (daily_cost, monthly_cost) = {
            let storage = self.lock_storage();
            storage.get_aggregated_costs()?
        };

        let session_costs = self.lock_session_costs();
        let session_cost: f64 = session_costs
            .iter()
            .map(|record| record.usage.cost_usd)
            .sum();
        let total_tokens: u64 = session_costs
            .iter()
            .map(|record| record.usage.total_tokens)
            .sum();
        let request_count = session_costs.len();
        let by_model = build_session_model_stats(&session_costs);

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
    pub fn get_daily_cost(&self, date: NaiveDate) -> Result<f64> {
        let storage = self.lock_storage();
        storage.get_cost_for_date(date)
    }

    /// Get the monthly cost for a specific month.
    pub fn get_monthly_cost(&self, year: i32, month: u32) -> Result<f64> {
        let storage = self.lock_storage();
        storage.get_cost_for_month(year, month)
    }
}

// ── Process-global singleton ────────────────────────────────────────

static GLOBAL_COST_TRACKER: OnceLock<Option<Arc<CostTracker>>> = OnceLock::new();

impl CostTracker {
    pub fn get_or_init_global(config: CostConfig, workspace_dir: &Path) -> Option<Arc<Self>> {
        GLOBAL_COST_TRACKER
            .get_or_init(|| {
                if !config.enabled {
                    return None;
                }
                match Self::new(config, workspace_dir) {
                    Ok(ct) => Some(Arc::new(ct)),
                    Err(e) => {
                        tracing::warn!("Failed to initialize global cost tracker: {e}");
                        None
                    }
                }
            })
            .clone()
    }
}

fn build_session_model_stats(session_costs: &[CostRecord]) -> HashMap<String, ModelStats> {
    let mut by_model: HashMap<String, ModelStats> = HashMap::new();

    for record in session_costs {
        let entry = by_model
            .entry(record.usage.model.clone())
            .or_insert_with(|| ModelStats {
                model: record.usage.model.clone(),
                cost_usd: 0.0,
                total_tokens: 0,
                request_count: 0,
            });

        entry.cost_usd += record.usage.cost_usd;
        entry.total_tokens += record.usage.total_tokens;
        entry.request_count += 1;
    }

    by_model
}

/// SQLite-backed persistent storage for cost records.
struct CostStorage {
    state_db: StateDb,
}

impl CostStorage {
    fn new(workspace_dir: &Path) -> Result<Self> {
        let state_db = StateDb::open(workspace_dir)?;
        let conn = state_db.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS costs (
                 id          TEXT PRIMARY KEY,
                 session_id  TEXT NOT NULL,
                 model       TEXT NOT NULL,
                 input_tokens  INTEGER NOT NULL,
                 output_tokens INTEGER NOT NULL,
                 total_tokens  INTEGER NOT NULL,
                 cost_usd    REAL NOT NULL,
                 timestamp   TEXT NOT NULL,
                 date_key    TEXT NOT NULL,
                 month_key   TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_costs_date ON costs(date_key);
             CREATE INDEX IF NOT EXISTS idx_costs_month ON costs(month_key);
             CREATE INDEX IF NOT EXISTS idx_costs_session ON costs(session_id);
             CREATE INDEX IF NOT EXISTS idx_costs_model ON costs(model);",
        )
        .context("Failed to create costs table")?;

        let storage = Self { state_db };
        storage.migrate_from_jsonl(workspace_dir)?;
        Ok(storage)
    }

    fn migrate_from_jsonl(&self, workspace_dir: &Path) -> Result<()> {
        let jsonl_path = workspace_dir.join("state").join("costs.jsonl");
        if !jsonl_path.exists() {
            return Ok(());
        }

        let conn = self.state_db.connect()?;
        let existing: i64 = conn
            .query_row("SELECT COUNT(*) FROM costs", [], |r| r.get(0))
            .unwrap_or(0);
        if existing > 0 {
            // Already migrated — just rename the leftover file
            let migrated = jsonl_path.with_extension("jsonl.migrated");
            let _ = std::fs::rename(&jsonl_path, &migrated);
            return Ok(());
        }

        let file = std::fs::File::open(&jsonl_path)
            .with_context(|| format!("Failed to open {}", jsonl_path.display()))?;
        let reader = std::io::BufReader::new(file);

        let tx = conn.unchecked_transaction()?;
        let mut imported = 0u64;
        for line in std::io::BufRead::lines(reader) {
            let Ok(line) = line else { continue };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<CostRecord>(trimmed) else {
                continue;
            };
            let ts = record.usage.timestamp.to_rfc3339();
            let date_key = record.usage.timestamp.format("%Y-%m-%d").to_string();
            let month_key = record.usage.timestamp.format("%Y-%m").to_string();
            tx.execute(
                "INSERT OR IGNORE INTO costs (id, session_id, model, input_tokens, output_tokens, total_tokens, cost_usd, timestamp, date_key, month_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    record.id,
                    record.session_id,
                    record.usage.model,
                    record.usage.input_tokens,
                    record.usage.output_tokens,
                    record.usage.total_tokens,
                    record.usage.cost_usd,
                    ts,
                    date_key,
                    month_key,
                ],
            )?;
            imported += 1;
        }
        tx.commit()?;

        if imported > 0 {
            tracing::info!("💰 Migrated {imported} cost record(s) from costs.jsonl to state.db");
            let migrated = jsonl_path.with_extension("jsonl.migrated");
            if let Err(e) = std::fs::rename(&jsonl_path, &migrated) {
                tracing::warn!("Failed to rename costs.jsonl to .migrated: {e}");
            }
        }

        Ok(())
    }

    fn add_record(&self, record: &CostRecord) -> Result<()> {
        let conn = self.state_db.connect()?;
        let ts = record.usage.timestamp.to_rfc3339();
        let date_key = record.usage.timestamp.format("%Y-%m-%d").to_string();
        let month_key = record.usage.timestamp.format("%Y-%m").to_string();
        conn.execute(
            "INSERT INTO costs (id, session_id, model, input_tokens, output_tokens, total_tokens, cost_usd, timestamp, date_key, month_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.id,
                record.session_id,
                record.usage.model,
                record.usage.input_tokens,
                record.usage.output_tokens,
                record.usage.total_tokens,
                record.usage.cost_usd,
                ts,
                date_key,
                month_key,
            ],
        )
        .context("Failed to insert cost record")?;
        Ok(())
    }

    fn get_aggregated_costs(&self) -> Result<(f64, f64)> {
        let conn = self.state_db.connect()?;
        let now = Utc::now();
        let today = now.format("%Y-%m-%d").to_string();
        let this_month = now.format("%Y-%m").to_string();

        let daily: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM costs WHERE date_key = ?1",
                params![today],
                |r| r.get(0),
            )
            .unwrap_or(0.0);

        let monthly: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM costs WHERE month_key = ?1",
                params![this_month],
                |r| r.get(0),
            )
            .unwrap_or(0.0);

        Ok((daily, monthly))
    }

    fn get_cost_for_date(&self, date: NaiveDate) -> Result<f64> {
        let conn = self.state_db.connect()?;
        let date_key = date.format("%Y-%m-%d").to_string();
        let cost: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM costs WHERE date_key = ?1",
                params![date_key],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        Ok(cost)
    }

    fn get_cost_for_month(&self, year: i32, month: u32) -> Result<f64> {
        let conn = self.state_db.connect()?;
        let month_key = format!("{year:04}-{month:02}");
        let cost: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM costs WHERE month_key = ?1",
                params![month_key],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        Ok(cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn enabled_config() -> CostConfig {
        CostConfig {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn cost_tracker_initialization() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        assert!(!tracker.session_id().is_empty());
    }

    #[test]
    fn budget_check_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: false,
            ..Default::default()
        };

        let tracker = CostTracker::new(config, tmp.path()).unwrap();
        let check = tracker.check_budget(1000.0).unwrap();
        assert!(matches!(check, BudgetCheck::Allowed));
    }

    #[test]
    fn record_usage_and_get_summary() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        let usage = TokenUsage::new("test/model", 1000, 500, 1.0, 2.0);
        tracker.record_usage(usage).unwrap();

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 1);
        assert!(summary.session_cost_usd > 0.0);
        assert_eq!(summary.by_model.len(), 1);
    }

    #[test]
    fn budget_exceeded_daily_limit() {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: true,
            daily_limit_usd: 0.01,
            ..Default::default()
        };

        let tracker = CostTracker::new(config, tmp.path()).unwrap();

        let usage = TokenUsage::new("test/model", 10000, 5000, 1.0, 2.0);
        tracker.record_usage(usage).unwrap();

        let check = tracker.check_budget(0.01).unwrap();
        assert!(matches!(check, BudgetCheck::Exceeded { .. }));
    }

    #[test]
    fn summary_by_model_is_session_scoped() {
        let tmp = TempDir::new().unwrap();

        // Pre-seed state.db with an old-session record
        let storage = CostStorage::new(tmp.path()).unwrap();
        let old_usage = TokenUsage::new("legacy/model", 500, 500, 1.0, 1.0);
        let old_record = CostRecord::new("old-session", old_usage);
        storage.add_record(&old_record).unwrap();
        drop(storage);

        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        tracker
            .record_usage(TokenUsage::new("session/model", 1000, 1000, 1.0, 1.0))
            .unwrap();

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.by_model.len(), 1);
        assert!(summary.by_model.contains_key("session/model"));
        assert!(!summary.by_model.contains_key("legacy/model"));
    }

    #[test]
    fn state_db_is_created_with_wal() {
        let tmp = TempDir::new().unwrap();
        let _tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        let db_path = tmp.path().join("state").join("state.db");
        assert!(db_path.exists(), "state.db should exist");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn migrate_from_jsonl_imports_and_renames() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();

        let jsonl_path = state_dir.join("costs.jsonl");
        let usage = TokenUsage::new("test/model", 1000, 500, 3.0, 15.0);
        let record = CostRecord::new("session-1", usage);
        let mut file = fs::File::create(&jsonl_path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&record).unwrap()).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let _tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        assert!(!jsonl_path.exists(), "costs.jsonl should be renamed");
        assert!(
            state_dir.join("costs.jsonl.migrated").exists(),
            ".migrated file should exist"
        );

        let db_path = state_dir.join("state.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM costs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn migrate_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();

        let jsonl_path = state_dir.join("costs.jsonl");
        let usage = TokenUsage::new("test/model", 100, 50, 1.0, 1.0);
        let record = CostRecord::new("s1", usage);
        let mut file = fs::File::create(&jsonl_path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&record).unwrap()).unwrap();
        drop(file);

        // First open: imports and renames
        let _t1 = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        drop(_t1);

        let db_path = state_dir.join("state.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count1: i64 = conn
            .query_row("SELECT COUNT(*) FROM costs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count1, 1);
        drop(conn);

        // Second open: no jsonl, no re-import
        let _t2 = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        let conn2 = rusqlite::Connection::open(&db_path).unwrap();
        let count2: i64 = conn2
            .query_row("SELECT COUNT(*) FROM costs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count2, 1, "Should not double-count on second open");
    }

    #[test]
    fn malformed_jsonl_lines_skipped_during_migration() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();

        let jsonl_path = state_dir.join("costs.jsonl");
        let valid_usage = TokenUsage::new("test/model", 1000, 0, 1.0, 1.0);
        let valid_record = CostRecord::new("session-a", valid_usage.clone());

        let mut file = fs::File::create(&jsonl_path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&valid_record).unwrap()).unwrap();
        writeln!(file, "not-a-json-line").unwrap();
        writeln!(file).unwrap();
        drop(file);

        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        let today_cost = tracker.get_daily_cost(Utc::now().date_naive()).unwrap();
        assert!((today_cost - valid_usage.cost_usd).abs() < f64::EPSILON);
    }

    #[test]
    fn invalid_budget_estimate_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        let err = tracker.check_budget(f64::NAN).unwrap_err();
        assert!(
            err.to_string()
                .contains("Estimated cost must be a finite, non-negative value")
        );
    }

    #[test]
    fn per_date_and_per_month_queries_use_indexed_sql() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        tracker
            .record_usage(TokenUsage::new("m1", 1000, 500, 3.0, 15.0))
            .unwrap();
        tracker
            .record_usage(TokenUsage::new("m2", 2000, 1000, 3.0, 15.0))
            .unwrap();

        let today = Utc::now().date_naive();
        let daily = tracker.get_daily_cost(today).unwrap();
        let monthly = tracker
            .get_monthly_cost(today.year(), today.month())
            .unwrap();

        let expected = (1000.0 + 2000.0) / 1_000_000.0 * 3.0
            + (500.0 + 1000.0) / 1_000_000.0 * 15.0;
        assert!(
            (daily - expected).abs() < 1e-6,
            "daily={daily}, expected={expected}"
        );
        assert!(
            (monthly - expected).abs() < 1e-6,
            "monthly={monthly}, expected={expected}"
        );

        // Verify no full-scan path remains (costs.jsonl does not exist)
        assert!(
            !tmp.path().join("state").join("costs.jsonl").exists(),
            "No JSONL file should exist"
        );
    }
}
