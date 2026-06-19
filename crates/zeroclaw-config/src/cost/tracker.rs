use super::types::{
    AgentCostStats, BudgetCheck, CostRecord, CostSummary, ModelStats, TokenUsage, UsagePeriod,
};
use crate::schema::CostConfig;
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use parking_lot::{Mutex, MutexGuard, RwLock};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Cost tracker for API usage monitoring and budget enforcement.
pub struct CostTracker {
    config: Arc<RwLock<CostConfig>>,
    storage: Arc<Mutex<CostStorage>>,
    session_id: String,
    /// Per-daemon-lifetime aggregates keyed by `Option<agent_alias>`,
    /// replacing the unbounded per-turn `Vec<CostRecord>`.
    session_totals: Arc<Mutex<HashMap<Option<String>, AgentTotals>>>,
}

#[derive(Default, Clone, Copy)]
struct AgentTotals {
    cost_usd: f64,
    total_tokens: u64,
    request_count: u64,
}

impl CostTracker {
    /// Create a new cost tracker.
    pub fn new(config: CostConfig, workspace_dir: &Path) -> Result<Self> {
        let storage_path = resolve_storage_path(workspace_dir)?;
        let storage = CostStorage::new(&storage_path).with_context(|| {
            format!(
                "Failed to open cost storage at {}",
                storage_path.display().to_string()
            )
        })?;

        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            storage: Arc::new(Mutex::new(storage)),
            session_id: uuid::Uuid::new_v4().to_string(),
            session_totals: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Snapshot the live cost config. Reads go through the `RwLock` so a
    /// hot-reload swap is observed without a process restart.
    fn config_snapshot(&self) -> CostConfig {
        self.config.read().clone()
    }

    /// Current live cost config snapshot.
    pub fn config(&self) -> CostConfig {
        self.config_snapshot()
    }

    /// Replace the live cost config. This is what makes budget limits
    /// reloadable: the process-global tracker stays the same `Arc`, only
    /// its config is swapped under the `RwLock`.
    pub fn update_config(&self, config: CostConfig) {
        *self.config.write() = config;
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn lock_storage(&self) -> MutexGuard<'_, CostStorage> {
        self.storage.lock()
    }

    fn lock_session_totals(&self) -> MutexGuard<'_, HashMap<Option<String>, AgentTotals>> {
        self.session_totals.lock()
    }

    /// Check if a request is within budget.
    pub fn check_budget(&self, estimated_cost_usd: f64) -> Result<BudgetCheck> {
        let config = self.config_snapshot();
        if !config.enabled {
            return Ok(BudgetCheck::Allowed);
        }

        if !estimated_cost_usd.is_finite() || estimated_cost_usd < 0.0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"estimated_cost_usd": estimated_cost_usd})),
                "cost budget check rejected: estimated cost is not finite or is negative"
            );
            anyhow::bail!("Estimated cost must be a finite, non-negative value");
        }

        let mut storage = self.lock_storage();
        let (daily_cost, monthly_cost) = storage.get_aggregated_costs()?;

        // Check daily limit
        let projected_daily = daily_cost + estimated_cost_usd;
        if projected_daily > config.daily_limit_usd {
            return Ok(BudgetCheck::Exceeded {
                current_usd: daily_cost,
                limit_usd: config.daily_limit_usd,
                period: UsagePeriod::Day,
            });
        }

        // Check monthly limit
        let projected_monthly = monthly_cost + estimated_cost_usd;
        if projected_monthly > config.monthly_limit_usd {
            return Ok(BudgetCheck::Exceeded {
                current_usd: monthly_cost,
                limit_usd: config.monthly_limit_usd,
                period: UsagePeriod::Month,
            });
        }

        // Check warning thresholds
        let warn_threshold = f64::from(config.warn_at_percent.min(100)) / 100.0;
        let daily_warn_threshold = config.daily_limit_usd * warn_threshold;
        let monthly_warn_threshold = config.monthly_limit_usd * warn_threshold;

        if projected_daily >= daily_warn_threshold {
            return Ok(BudgetCheck::Warning {
                current_usd: daily_cost,
                limit_usd: config.daily_limit_usd,
                period: UsagePeriod::Day,
            });
        }

        if projected_monthly >= monthly_warn_threshold {
            return Ok(BudgetCheck::Warning {
                current_usd: monthly_cost,
                limit_usd: config.monthly_limit_usd,
                period: UsagePeriod::Month,
            });
        }

        Ok(BudgetCheck::Allowed)
    }

    /// Record a usage event without per-agent attribution.
    pub fn record_usage(&self, usage: TokenUsage) -> Result<()> {
        self.record_usage_with_agent(usage, None)
    }

    /// Record a usage event attributed to a specific agent alias. When
    /// `[cost].track_per_agent` is false the alias is dropped before
    /// persistence.
    pub fn record_usage_with_agent(
        &self,
        usage: TokenUsage,
        agent_alias: Option<&str>,
    ) -> Result<()> {
        if !self.config_snapshot().enabled {
            return Ok(());
        }

        if !usage.cost_usd.is_finite() || usage.cost_usd < 0.0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"cost_usd": usage.cost_usd})),
                "token usage record rejected: cost is not finite or is negative"
            );
            anyhow::bail!("Token usage cost must be a finite, non-negative value");
        }

        let effective_alias = if self.config_snapshot().track_per_agent {
            agent_alias.map(str::to_string)
        } else {
            None
        };
        let cost_usd = usage.cost_usd;
        let total_tokens = usage.total_tokens;
        let record = CostRecord::with_agent(&self.session_id, effective_alias.clone(), usage);

        {
            let mut storage = self.lock_storage();
            storage.add_record(record)?;
        }

        {
            let mut totals = self.lock_session_totals();
            let entry = totals.entry(effective_alias).or_default();
            entry.cost_usd += cost_usd;
            entry.total_tokens += total_tokens;
            entry.request_count += 1;
        }

        Ok(())
    }

    /// Get the current cost summary. When `[cost].track_per_agent` is
    /// enabled, the response includes a `by_agent` rollup over today's
    /// records.
    pub fn get_summary(&self) -> Result<CostSummary> {
        self.get_summary_filtered(None)
    }

    /// Filter persisted records by `[from, to)` (either side `None` is
    /// unbounded) and roll up by_model / by_agent / window totals.
    /// Bounds come from the caller (the dashboard computes them in the
    /// operator's local timezone); the tracker doesn't decide what
    /// "today" means.
    pub fn get_summary_in_bounds(
        &self,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<CostSummary> {
        let (daily_cost, monthly_cost, records) = {
            let mut storage = self.lock_storage();
            let (d, m) = storage.get_aggregated_costs()?;
            let recs = storage.records_in_bounds(from, to)?;
            (d, m, recs)
        };
        let total_cost: f64 = records.iter().map(|r| r.usage.cost_usd).sum();
        let total_tokens: u64 = records.iter().map(|r| r.usage.total_tokens).sum();
        let request_count = records.len();
        let by_model = build_model_stats(records.iter());
        let by_agent = if self.config_snapshot().track_per_agent {
            build_agent_stats(&records)
        } else {
            HashMap::new()
        };
        Ok(CostSummary {
            session_cost_usd: total_cost,
            daily_cost_usd: daily_cost,
            monthly_cost_usd: monthly_cost,
            total_tokens,
            request_count,
            by_model,
            by_agent,
        })
    }

    /// Get the current cost summary scoped to a single agent alias. The
    /// session/day/month figures and `by_model` are filtered to records
    /// attributed to that alias; `by_agent` is left empty since the
    /// caller already chose the dimension.
    pub fn get_summary_for_agent(&self, agent_alias: &str) -> Result<CostSummary> {
        self.get_summary_filtered(Some(agent_alias))
    }

    fn get_summary_filtered(&self, agent_filter: Option<&str>) -> Result<CostSummary> {
        let (daily_cost, monthly_cost, daily_records) = {
            let mut storage = self.lock_storage();
            let (d, m) = storage.get_aggregated_costs()?;
            // Always pull daily_records: per-model and per-agent rollups
            // both want today's slice. The optional-skip optimisation tied
            // to `track_per_agent` made the by-model rollup session-scoped,
            // which surprised operators after a daemon restart and clashes
            // with the daily totals in the same response.
            (d, m, storage.daily_records()?)
        };

        let (session_cost, total_tokens, request_count) = {
            let totals = self.lock_session_totals();
            totals
                .iter()
                .filter(|(alias, _)| match agent_filter {
                    Some(want) => alias.as_deref() == Some(want),
                    None => true,
                })
                .fold((0.0_f64, 0_u64, 0_usize), |(c, t, r), (_, v)| {
                    (
                        c + v.cost_usd,
                        t + v.total_tokens,
                        r + v.request_count as usize,
                    )
                })
        };

        let matches_agent = |record: &CostRecord| match agent_filter {
            Some(alias) => record.agent_alias.as_deref() == Some(alias),
            None => true,
        };

        // Daily-scoped per-model rollup. Filter by agent when scoped.
        let model_records: Vec<&CostRecord> =
            daily_records.iter().filter(|r| matches_agent(r)).collect();
        let by_model = build_model_stats(model_records.iter().copied());

        let (daily_total, monthly_total, by_agent) = if let Some(alias) = agent_filter {
            // Per-agent view: re-aggregate day/month from persisted records.
            let mut daily_total = 0.0;
            let mut monthly_total = 0.0;
            let today = Utc::now().date_naive();
            let now = Utc::now();
            for record in &daily_records {
                if record.agent_alias.as_deref() != Some(alias) {
                    continue;
                }
                let ts = record.usage.timestamp.naive_utc();
                if ts.date() == today {
                    daily_total += record.usage.cost_usd;
                }
                if ts.year() == now.year() && ts.month() == now.month() {
                    monthly_total += record.usage.cost_usd;
                }
            }
            (daily_total, monthly_total, HashMap::new())
        } else if self.config_snapshot().track_per_agent {
            let by_agent = build_agent_stats(&daily_records);
            (daily_cost, monthly_cost, by_agent)
        } else {
            (daily_cost, monthly_cost, HashMap::new())
        };

        Ok(CostSummary {
            session_cost_usd: session_cost,
            daily_cost_usd: daily_total,
            monthly_cost_usd: monthly_total,
            total_tokens,
            request_count,
            by_model,
            by_agent,
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
// Both the gateway and the channels supervisor share a single CostTracker
// so that budget enforcement is consistent across all paths.

static GLOBAL_COST_TRACKER: OnceLock<Option<Arc<CostTracker>>> = OnceLock::new();

impl CostTracker {
    /// Return the process-global `CostTracker`, creating it on first call.
    /// Subsequent calls (from gateway or channels, whichever starts second,
    /// or a hot-reload re-invocation) apply the supplied `config` to the
    /// existing tracker and receive the same `Arc`.  The tracker holds its
    /// config behind a `RwLock`, so reloaded budget limits take effect
    /// without a process restart.  Returns `None` when cost tracking is
    /// disabled or initialisation fails.
    ///
    /// Note: if cost tracking was disabled at first init the global stores
    /// `None` permanently; enabling it later still requires a restart,
    /// because the `OnceLock` cannot retroactively construct storage.
    pub fn get_or_init_global(config: CostConfig, workspace_dir: &Path) -> Option<Arc<Self>> {
        let tracker = GLOBAL_COST_TRACKER
            .get_or_init(|| {
                if !config.enabled {
                    return None;
                }
                match Self::new(config.clone(), workspace_dir) {
                    Ok(ct) => Some(Arc::new(ct)),
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "Failed to initialize global cost tracker"
                        );
                        None
                    }
                }
            })
            .clone();

        if let Some(ct) = tracker.as_ref() {
            ct.update_config(config);
        }

        tracker
    }
}

fn resolve_storage_path(workspace_dir: &Path) -> Result<PathBuf> {
    let storage_path = workspace_dir.join("state").join("costs.jsonl");
    let legacy_path = workspace_dir.join(".zeroclaw").join("costs.db");

    if !storage_path.exists() && legacy_path.exists() {
        if let Some(parent) = storage_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create directory {}",
                    parent.display().to_string()
                )
            })?;
        }

        if let Err(error) = fs::rename(&legacy_path, &storage_path) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "Failed to move legacy cost storage from {} to {}: {error}; falling back to copy",
                    legacy_path.display().to_string(),
                    storage_path.display().to_string()
                )
            );
            fs::copy(&legacy_path, &storage_path).with_context(|| {
                format!(
                    "Failed to copy legacy cost storage from {} to {}",
                    legacy_path.display().to_string(),
                    storage_path.display()
                )
            })?;
        }
    }

    Ok(storage_path)
}

fn build_model_stats<'a, I>(records: I) -> HashMap<String, ModelStats>
where
    I: IntoIterator<Item = &'a CostRecord>,
{
    let mut by_model: HashMap<String, ModelStats> = HashMap::new();

    for record in records {
        let entry = by_model
            .entry(record.usage.model.clone())
            .or_insert_with(|| ModelStats {
                model: record.usage.model.clone(),
                cost_usd: 0.0,
                total_tokens: 0,
                input_tokens: 0,
                output_tokens: 0,
                cached_input_tokens: 0,
                request_count: 0,
            });

        entry.cost_usd += record.usage.cost_usd;
        entry.total_tokens += record.usage.total_tokens;
        entry.input_tokens += record.usage.input_tokens;
        entry.output_tokens += record.usage.output_tokens;
        entry.cached_input_tokens += record.usage.cached_input_tokens;
        entry.request_count += 1;
    }

    by_model
}

fn build_agent_stats(records: &[CostRecord]) -> HashMap<String, AgentCostStats> {
    let mut by_agent: HashMap<String, AgentCostStats> = HashMap::new();

    for record in records {
        let Some(alias) = record.agent_alias.as_deref() else {
            continue;
        };
        let entry = by_agent
            .entry(alias.to_string())
            .or_insert_with(|| AgentCostStats {
                agent_alias: alias.to_string(),
                cost_usd: 0.0,
                total_tokens: 0,
                input_tokens: 0,
                output_tokens: 0,
                cached_input_tokens: 0,
                request_count: 0,
            });

        entry.cost_usd += record.usage.cost_usd;
        entry.total_tokens += record.usage.total_tokens;
        entry.input_tokens += record.usage.input_tokens;
        entry.output_tokens += record.usage.output_tokens;
        entry.cached_input_tokens += record.usage.cached_input_tokens;
        entry.request_count += 1;
    }

    by_agent
}

/// Persistent storage for cost records.
struct CostStorage {
    path: PathBuf,
    daily_cost_usd: f64,
    monthly_cost_usd: f64,
    cached_day: NaiveDate,
    cached_year: i32,
    cached_month: u32,
}

impl CostStorage {
    /// Create or open cost storage.
    fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create directory {}",
                    parent.display().to_string()
                )
            })?;
        }
        let now = Utc::now();
        let mut storage = Self {
            path: path.to_path_buf(),
            daily_cost_usd: 0.0,
            monthly_cost_usd: 0.0,
            cached_day: now.date_naive(),
            cached_year: now.year(),
            cached_month: now.month(),
        };
        storage.rebuild_aggregates(
            storage.cached_day,
            storage.cached_year,
            storage.cached_month,
        )?;

        Ok(storage)
    }

    fn for_each_record<F>(&self, mut on_record: F) -> Result<()>
    where
        F: FnMut(CostRecord),
    {
        if !self.path.exists() {
            return Ok(());
        }

        let file = File::open(&self.path).with_context(|| {
            format!(
                "Failed to read cost storage from {}",
                self.path.display().to_string()
            )
        })?;
        let reader = BufReader::new(file);

        for (line_number, line) in reader.lines().enumerate() {
            let raw_line = line.with_context(|| {
                format!(
                    "Failed to read line {} from cost storage {}",
                    line_number + 1,
                    self.path.display()
                )
            })?;

            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }

            match serde_json::from_str::<CostRecord>(trimmed) {
                Ok(record) => on_record(record),
                Err(error) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "Skipping malformed cost record at {}:{}: {error}",
                            self.path.display().to_string(),
                            line_number + 1
                        )
                    );
                }
            }
        }

        Ok(())
    }

    fn rebuild_aggregates(&mut self, day: NaiveDate, year: i32, month: u32) -> Result<()> {
        let mut daily_cost = 0.0;
        let mut monthly_cost = 0.0;

        self.for_each_record(|record| {
            let timestamp = record.usage.timestamp.naive_utc();

            if timestamp.date() == day {
                daily_cost += record.usage.cost_usd;
            }

            if timestamp.year() == year && timestamp.month() == month {
                monthly_cost += record.usage.cost_usd;
            }
        })?;

        self.daily_cost_usd = daily_cost;
        self.monthly_cost_usd = monthly_cost;
        self.cached_day = day;
        self.cached_year = year;
        self.cached_month = month;

        Ok(())
    }

    fn ensure_period_cache_current(&mut self) -> Result<()> {
        let now = Utc::now();
        let day = now.date_naive();
        let year = now.year();
        let month = now.month();

        if day != self.cached_day || year != self.cached_year || month != self.cached_month {
            self.rebuild_aggregates(day, year, month)?;
        }

        Ok(())
    }

    /// Add a new record.
    fn add_record(&mut self, record: CostRecord) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| {
                format!(
                    "Failed to open cost storage at {}",
                    self.path.display().to_string()
                )
            })?;

        writeln!(file, "{}", serde_json::to_string(&record)?).with_context(|| {
            format!(
                "Failed to write cost record to {}",
                self.path.display().to_string()
            )
        })?;
        file.sync_all().with_context(|| {
            format!(
                "Failed to sync cost storage at {}",
                self.path.display().to_string()
            )
        })?;

        self.ensure_period_cache_current()?;

        let timestamp = record.usage.timestamp.naive_utc();
        if timestamp.date() == self.cached_day {
            self.daily_cost_usd += record.usage.cost_usd;
        }
        if timestamp.year() == self.cached_year && timestamp.month() == self.cached_month {
            self.monthly_cost_usd += record.usage.cost_usd;
        }

        Ok(())
    }

    /// Get aggregated costs for current day and month.
    fn get_aggregated_costs(&mut self) -> Result<(f64, f64)> {
        self.ensure_period_cache_current()?;
        Ok((self.daily_cost_usd, self.monthly_cost_usd))
    }

    /// Snapshot every record whose timestamp falls within the current
    /// calendar month. Used to build per-agent rollups without folding a
    /// new aggregate table into the JSONL file.
    fn daily_records(&mut self) -> Result<Vec<CostRecord>> {
        self.ensure_period_cache_current()?;
        let year = self.cached_year;
        let month = self.cached_month;
        let mut out = Vec::new();
        self.for_each_record(|record| {
            let ts = record.usage.timestamp.naive_utc();
            if ts.year() == year && ts.month() == month {
                out.push(record);
            }
        })?;
        Ok(out)
    }

    fn records_in_bounds(
        &mut self,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Vec<CostRecord>> {
        let mut out = Vec::new();
        self.for_each_record(|record| {
            let ts = record.usage.timestamp;
            if from.is_some_and(|f| ts < f) {
                return;
            }
            if to.is_some_and(|t| ts >= t) {
                return;
            }
            out.push(record);
        })?;
        Ok(out)
    }

    /// Get cost for a specific date.
    fn get_cost_for_date(&self, date: NaiveDate) -> Result<f64> {
        let mut cost = 0.0;

        self.for_each_record(|record| {
            if record.usage.timestamp.naive_utc().date() == date {
                cost += record.usage.cost_usd;
            }
        })?;

        Ok(cost)
    }

    /// Get cost for a specific month.
    fn get_cost_for_month(&self, year: i32, month: u32) -> Result<f64> {
        let mut cost = 0.0;

        self.for_each_record(|record| {
            let timestamp = record.usage.timestamp.naive_utc();
            if timestamp.year() == year && timestamp.month() == month {
                cost += record.usage.cost_usd;
            }
        })?;

        Ok(cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
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

        let usage = TokenUsage::new("test/model", 1000, 500, 0, 1.0, 2.0, 0.0);
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
            daily_limit_usd: 0.01, // Very low limit
            ..Default::default()
        };

        let tracker = CostTracker::new(config, tmp.path()).unwrap();

        // Record a usage that exceeds the limit
        let usage = TokenUsage::new("test/model", 10000, 5000, 0, 1.0, 2.0, 0.0); // ~0.02 USD
        tracker.record_usage(usage).unwrap();

        let check = tracker.check_budget(0.01).unwrap();
        assert!(matches!(check, BudgetCheck::Exceeded { .. }));
    }

    #[test]
    fn summary_by_model_is_daily_scoped() {
        // by_model rollup pulls from today's persisted records so the
        // dashboard's per-model breakdown survives daemon restarts (matches
        // by_agent's behaviour). A record from another session that
        // happened today still shows up; only ones outside the day fall
        // off — exercised by the storage layer's get_aggregated_costs.
        let tmp = TempDir::new().unwrap();
        let storage_path = resolve_storage_path(tmp.path()).unwrap();
        if let Some(parent) = storage_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let prior_today = CostRecord::new(
            "prior-session",
            TokenUsage::new("prior/model", 500, 500, 0, 1.0, 1.0, 0.0),
        );
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(storage_path)
            .unwrap();
        writeln!(file, "{}", serde_json::to_string(&prior_today).unwrap()).unwrap();
        file.sync_all().unwrap();

        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        tracker
            .record_usage(TokenUsage::new(
                "session/model",
                1000,
                1000,
                0,
                1.0,
                1.0,
                0.0,
            ))
            .unwrap();

        let summary = tracker.get_summary().unwrap();
        assert_eq!(
            summary.by_model.len(),
            2,
            "by_model must include every model that recorded today, \
             regardless of which session wrote the record"
        );
        assert!(summary.by_model.contains_key("session/model"));
        assert!(summary.by_model.contains_key("prior/model"));
    }

    #[test]
    fn malformed_lines_are_ignored_while_loading() {
        let tmp = TempDir::new().unwrap();
        let storage_path = resolve_storage_path(tmp.path()).unwrap();
        if let Some(parent) = storage_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let valid_usage = TokenUsage::new("test/model", 1000, 0, 0, 1.0, 1.0, 0.0);
        let valid_record = CostRecord::new("session-a", valid_usage.clone());

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(storage_path)
            .unwrap();
        writeln!(file, "{}", serde_json::to_string(&valid_record).unwrap()).unwrap();
        writeln!(file, "not-a-json-line").unwrap();
        writeln!(file).unwrap();
        file.sync_all().unwrap();

        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        let today_cost = tracker.get_daily_cost(Utc::now().date_naive()).unwrap();
        assert!((today_cost - valid_usage.cost_usd).abs() < f64::EPSILON);
    }

    #[test]
    fn per_agent_aggregation_buckets_by_alias() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000, 1_000, 0, 1.0, 1.0, 0.0),
                Some("scout"),
            )
            .unwrap();
        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 2_000, 0, 0, 1.0, 1.0, 0.0),
                Some("scout"),
            )
            .unwrap();
        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 500, 500, 0, 1.0, 1.0, 0.0),
                Some("scribe"),
            )
            .unwrap();

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.by_agent.len(), 2);
        let scout = summary.by_agent.get("scout").unwrap();
        assert_eq!(scout.request_count, 2);
        assert_eq!(scout.total_tokens, 4_000);
        let scribe = summary.by_agent.get("scribe").unwrap();
        assert_eq!(scribe.request_count, 1);
        assert_eq!(scribe.total_tokens, 1_000);

        let scoped = tracker.get_summary_for_agent("scout").unwrap();
        assert_eq!(scoped.request_count, 2);
        assert!(
            scoped.by_agent.is_empty(),
            "per-agent view doesn't re-bucket"
        );
        assert!(
            (scoped.daily_cost_usd - scout.cost_usd).abs() < 1e-9,
            "daily filtered to alias must match by_agent bucket"
        );
    }

    #[test]
    fn track_per_agent_disabled_strips_alias() {
        let tmp = TempDir::new().unwrap();
        let config = CostConfig {
            enabled: true,
            track_per_agent: false,
            ..Default::default()
        };
        let tracker = CostTracker::new(config, tmp.path()).unwrap();

        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000, 1_000, 0, 1.0, 1.0, 0.0),
                Some("scout"),
            )
            .unwrap();

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 1);
        assert!(
            summary.by_agent.is_empty(),
            "track_per_agent=false must not surface per-agent rollups"
        );
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
    fn cost_reload_applies_new_daily_limit() {
        let tmp = TempDir::new().unwrap();

        let boot = CostConfig {
            enabled: true,
            daily_limit_usd: 10.0,
            ..Default::default()
        };
        let tracker = CostTracker::new(boot, tmp.path()).expect("boot tracker");
        assert_eq!(tracker.config().daily_limit_usd, 10.0);

        tracker.update_config(CostConfig {
            enabled: true,
            daily_limit_usd: 14000.0,
            ..Default::default()
        });

        assert_eq!(
            tracker.config().daily_limit_usd,
            14000.0,
            "reload must apply the new daily limit through the RwLock"
        );
    }

    #[test]
    fn get_or_init_global_applies_reloaded_config_to_existing_tracker() {
        let tmp = TempDir::new().unwrap();

        let boot = CostConfig {
            enabled: true,
            daily_limit_usd: 10.0,
            ..Default::default()
        };
        let first =
            CostTracker::get_or_init_global(boot, tmp.path()).expect("first init yields a tracker");

        let reloaded = CostConfig {
            enabled: true,
            daily_limit_usd: 14000.0,
            ..Default::default()
        };
        let after =
            CostTracker::get_or_init_global(reloaded, tmp.path()).expect("reload yields a tracker");

        assert_eq!(
            after.config().daily_limit_usd,
            14000.0,
            "the process-global tracker must adopt the reloaded daily limit"
        );
        assert!(
            Arc::ptr_eq(&first, &after),
            "reload must reuse the same global Arc, not construct a second tracker"
        );
    }
}
