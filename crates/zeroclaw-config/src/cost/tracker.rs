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

pub struct CostTracker {
    /// Live cost policy. This is hot-swapped on config reload so budget checks
    /// see new global limits without rebuilding the tracker.
    config: Arc<RwLock<CostConfig>>,
    /// Durable JSONL ledger plus cached day/month aggregates for that ledger.
    storage: Arc<Mutex<CostStorage>>,
    /// Process-local tracker session id used to group records emitted by this
    /// daemon lifetime.
    session_id: String,
    /// Per-daemon-lifetime aggregates keyed by `Option<agent_alias>`,
    /// replacing the unbounded per-turn `Vec<CostRecord>`.
    session_totals: Arc<Mutex<HashMap<Option<String>, AgentTotals>>>,
}

/// Cheap process-local totals for one optional agent attribution bucket.
/// This never replaces the persisted ledger. It only avoids rereading JSONL for
/// current-session summary fields while the daemon is alive.
#[derive(Default, Clone, Copy)]
struct AgentTotals {
    /// USD total accumulated in this process for the bucket.
    cost_usd: f64,
    /// Token total accumulated in this process for the bucket.
    total_tokens: u64,
    /// Number of usage records accumulated in this process for the bucket.
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

    fn config_snapshot(&self) -> CostConfig {
        self.config.read().clone()
    }

    pub fn config(&self) -> CostConfig {
        self.config_snapshot()
    }

    pub fn is_enabled(&self) -> bool {
        self.config.read().enabled
    }

    /// Hot-swap config so reloaded budget limits apply without a restart.
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

    fn storage_path(&self) -> PathBuf {
        self.lock_storage().path.clone()
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
        self.record_usage_with_task_attribution(usage, agent_alias, None)
    }

    /// Record a usage event attributed to a specific agent alias and/or task.
    /// Agent attribution still follows `[cost].track_per_agent`; task
    /// attribution is independent because it keys feature-level usage back to
    /// the durable control-plane task that spent it.
    pub fn record_usage_with_task_attribution(
        &self,
        usage: TokenUsage,
        agent_alias: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<()> {
        self.record_usage_with_owned_task_attribution(
            usage,
            agent_alias,
            task_id.map(str::to_string),
        )
    }

    /// Record a usage event with an already-owned task id. Runtime attribution
    /// resolves the id from durable task state, so this avoids cloning that id
    /// again before persistence.
    pub fn record_usage_with_owned_task_attribution(
        &self,
        usage: TokenUsage,
        agent_alias: Option<&str>,
        task_id: Option<String>,
    ) -> Result<()> {
        self.record_usage_with_owned_task_attribution_inner(usage, agent_alias, task_id, true)
    }

    pub fn record_scoped_usage_with_owned_task_attribution(
        &self,
        usage: TokenUsage,
        agent_alias: Option<&str>,
        task_id: Option<String>,
    ) -> Result<()> {
        self.record_usage_with_owned_task_attribution_inner(usage, agent_alias, task_id, false)
    }

    fn record_usage_with_owned_task_attribution_inner(
        &self,
        usage: TokenUsage,
        agent_alias: Option<&str>,
        task_id: Option<String>,
        honor_enabled: bool,
    ) -> Result<()> {
        let (enabled, track_per_agent) = {
            let config = self.config.read();
            (config.enabled, config.track_per_agent)
        };
        if honor_enabled && !enabled {
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

        let effective_alias = if track_per_agent {
            agent_alias.map(str::to_string)
        } else {
            None
        };
        let cost_usd = usage.cost_usd;
        let total_tokens = usage.total_tokens;
        let record =
            CostRecord::with_attribution(&self.session_id, effective_alias.clone(), task_id, usage);

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

    /// Get usage summary for a durable attributed task. Totals are derived from
    /// persisted ledger rows carrying the task-attribution key; no consumed
    /// counters are stored on feature-specific extension records.
    pub fn get_summary_for_task(&self, task_id: &str) -> Result<CostSummary> {
        let mut storage = self.lock_storage();
        storage.summary_for_task(task_id)
    }

    /// Get usage totals for a durable attributed task without building model/agent
    /// rollups. Totals are still derived from the canonical persisted ledger.
    pub fn get_usage_totals_for_task(&self, task_id: &str) -> Result<(u64, f64)> {
        let mut storage = self.lock_storage();
        storage.usage_totals_for_task(task_id)
    }

    pub fn get_usage_totals_for_task_with_pricing(
        &self,
        task_id: &str,
    ) -> Result<(u64, f64, bool)> {
        let mut storage = self.lock_storage();
        storage.usage_totals_for_task_with_pricing(task_id)
    }

    fn get_summary_filtered(&self, agent_filter: Option<&str>) -> Result<CostSummary> {
        let (daily_cost, monthly_cost, daily_records) = {
            let mut storage = self.lock_storage();
            let (d, m) = storage.get_aggregated_costs()?;
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

static GLOBAL_COST_TRACKER: OnceLock<RwLock<Option<Arc<CostTracker>>>> = OnceLock::new();

impl CostTracker {
    /// Return the process-global `CostTracker`, applying `config` to the
    /// existing tracker on later calls and reusing the same `Arc`. Returns
    /// `None` while cost tracking is disabled and no tracker exists yet; a
    /// later reload flipping `enabled` to `true` constructs it on demand.
    pub fn get_or_init_global(config: CostConfig, workspace_dir: &Path) -> Option<Arc<Self>> {
        let slot = GLOBAL_COST_TRACKER.get_or_init(|| RwLock::new(None));
        Self::resolve_global(slot, config, workspace_dir)
    }

    fn resolve_global(
        slot: &RwLock<Option<Arc<CostTracker>>>,
        config: CostConfig,
        workspace_dir: &Path,
    ) -> Option<Arc<Self>> {
        let storage_path = match resolve_storage_path(workspace_dir) {
            Ok(path) => path,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Failed to resolve global cost tracker storage path"
                );
                return None;
            }
        };

        if let Some(ct) = slot.read().as_ref().cloned()
            && (ct.storage_path() == storage_path || !config.enabled)
        {
            ct.update_config(config);
            return Some(ct);
        }

        if !config.enabled {
            return None;
        }

        let mut guard = slot.write();
        if let Some(ct) = guard.as_ref().cloned()
            && (ct.storage_path() == storage_path || !config.enabled)
        {
            ct.update_config(config);
            return Some(ct);
        }

        match Self::new(config, workspace_dir) {
            Ok(ct) => {
                let ct = Arc::new(ct);
                *guard = Some(ct.clone());
                Some(ct)
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Failed to initialize global cost tracker"
                );
                None
            }
        }
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
        add_model_stats(&mut by_model, record);
    }

    by_model
}

fn add_model_stats(by_model: &mut HashMap<String, ModelStats>, record: &CostRecord) {
    if let Some(entry) = by_model.get_mut(record.usage.model.as_str()) {
        add_usage_to_model_stats(entry, record);
        return;
    }
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
    add_usage_to_model_stats(entry, record);
}

fn add_usage_to_model_stats(entry: &mut ModelStats, record: &CostRecord) {
    entry.cost_usd += record.usage.cost_usd;
    entry.total_tokens += record.usage.total_tokens;
    entry.input_tokens += record.usage.input_tokens;
    entry.output_tokens += record.usage.output_tokens;
    entry.cached_input_tokens += record.usage.cached_input_tokens;
    entry.request_count += 1;
}

fn build_agent_stats(records: &[CostRecord]) -> HashMap<String, AgentCostStats> {
    let mut by_agent: HashMap<String, AgentCostStats> = HashMap::new();

    for record in records {
        add_agent_stats(&mut by_agent, record);
    }

    by_agent
}

fn add_agent_stats(by_agent: &mut HashMap<String, AgentCostStats>, record: &CostRecord) {
    let Some(alias) = record.agent_alias.as_deref() else {
        return;
    };
    if let Some(entry) = by_agent.get_mut(alias) {
        add_usage_to_agent_stats(entry, record);
        return;
    }
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
    add_usage_to_agent_stats(entry, record);
}

fn add_usage_to_agent_stats(entry: &mut AgentCostStats, record: &CostRecord) {
    entry.cost_usd += record.usage.cost_usd;
    entry.total_tokens += record.usage.total_tokens;
    entry.input_tokens += record.usage.input_tokens;
    entry.output_tokens += record.usage.output_tokens;
    entry.cached_input_tokens += record.usage.cached_input_tokens;
    entry.request_count += 1;
}

#[derive(Default)]
struct CostSummaryAccumulator {
    /// Aggregated USD cost for the scanned records.
    total_cost: f64,
    /// Aggregated token count for the scanned records.
    total_tokens: u64,
    /// Number of scanned usage records.
    request_count: usize,
    /// Per-model rollup keyed by model id.
    by_model: HashMap<String, ModelStats>,
    /// Per-agent rollup keyed by agent alias.
    by_agent: HashMap<String, AgentCostStats>,
}

impl CostSummaryAccumulator {
    fn record(&mut self, record: &CostRecord) {
        self.total_cost += record.usage.cost_usd;
        self.total_tokens += record.usage.total_tokens;
        self.request_count += 1;
        add_model_stats(&mut self.by_model, record);
        add_agent_stats(&mut self.by_agent, record);
    }

    fn finish(self) -> CostSummary {
        CostSummary {
            session_cost_usd: self.total_cost,
            daily_cost_usd: self.total_cost,
            monthly_cost_usd: self.total_cost,
            total_tokens: self.total_tokens,
            request_count: self.request_count,
            by_model: self.by_model,
            by_agent: self.by_agent,
        }
    }
}

struct CostStorage {
    /// JSONL ledger path.
    path: PathBuf,
    /// Cached total for the current UTC day.
    daily_cost_usd: f64,
    /// Cached total for the current UTC month.
    monthly_cost_usd: f64,
    /// Day represented by `daily_cost_usd`.
    cached_day: NaiveDate,
    /// Year represented by `monthly_cost_usd`.
    cached_year: i32,
    /// Month represented by `monthly_cost_usd`.
    cached_month: u32,
    /// Whether the cached day/month aggregates reflect the current ledger.
    aggregates_current: bool,
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
        Ok(Self {
            path: path.to_path_buf(),
            daily_cost_usd: 0.0,
            monthly_cost_usd: 0.0,
            cached_day: now.date_naive(),
            cached_year: now.year(),
            cached_month: now.month(),
            aggregates_current: false,
        })
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
                    let mut recovered = 0usize;
                    let stream =
                        serde_json::Deserializer::from_str(trimmed).into_iter::<CostRecord>();
                    for value in stream {
                        match value {
                            Ok(record) => {
                                on_record(record);
                                recovered += 1;
                            }
                            Err(_) => break,
                        }
                    }
                    if recovered == 0 {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "path": self.path.display().to_string(),
                                "line": line_number + 1,
                                "error": error.to_string(),
                            })),
                            "skipping malformed cost record"
                        );
                    }
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
        self.aggregates_current = true;

        Ok(())
    }

    fn ensure_period_cache_current(&mut self) -> Result<()> {
        let now = Utc::now();
        let day = now.date_naive();
        let year = now.year();
        let month = now.month();

        if !self.aggregates_current
            || day != self.cached_day
            || year != self.cached_year
            || month != self.cached_month
        {
            self.rebuild_aggregates(day, year, month)?;
        }

        Ok(())
    }

    /// Add a new record.
    fn add_record(&mut self, record: CostRecord) -> Result<()> {
        self.ensure_period_cache_current()?;

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

        let mut line = serde_json::to_string(&record)?;
        line.push('\n');
        file.write_all(line.as_bytes()).with_context(|| {
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

    fn summary_for_task(&mut self, task_id: &str) -> Result<CostSummary> {
        let mut summary = CostSummaryAccumulator::default();
        self.for_each_record(|record| {
            if record.task_id.as_deref() == Some(task_id) {
                summary.record(&record);
            }
        })?;
        Ok(summary.finish())
    }

    fn usage_totals_for_task(&mut self, task_id: &str) -> Result<(u64, f64)> {
        let (total_tokens, cost_usd, _pricing_available) =
            self.usage_totals_for_task_with_pricing(task_id)?;
        Ok((total_tokens, cost_usd))
    }

    fn usage_totals_for_task_with_pricing(&mut self, task_id: &str) -> Result<(u64, f64, bool)> {
        let mut total_tokens = 0_u64;
        let mut cost_usd = 0.0_f64;
        let mut pricing_available = true;
        self.for_each_record(|record| {
            if record.task_id.as_deref() == Some(task_id) {
                total_tokens = total_tokens.saturating_add(record.usage.total_tokens);
                cost_usd += record.usage.cost_usd;
                if !record.usage.pricing_available {
                    pricing_available = false;
                }
            }
        })?;
        Ok((total_tokens, cost_usd, pricing_available))
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
    fn recovers_concatenated_records_from_legacy_ledger() {
        let tmp = TempDir::new().unwrap();
        // Write two real, valid records through the normal (now-atomic) path.
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        tracker
            .record_usage(TokenUsage::new("test/model", 1000, 500, 0, 1.0, 2.0, 0.0))
            .unwrap();
        tracker
            .record_usage(TokenUsage::new("test/model", 2000, 800, 0, 1.0, 2.0, 0.0))
            .unwrap();

        // Simulate the legacy interleaved-write artifact: collapse the two
        // newline-separated records into one concatenated `{..}{..}` line.
        let path = resolve_storage_path(tmp.path()).unwrap();
        let joined: String = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .collect::<Vec<_>>()
            .join("");
        std::fs::write(&path, format!("{joined}\n")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            1,
            "ledger should now be a single concatenated line"
        );

        // A fresh storage over the corrupted ledger still recovers both records.
        let storage = CostStorage::new(&path).unwrap();
        let mut count = 0usize;
        storage.for_each_record(|_| count += 1).unwrap();
        assert_eq!(count, 2, "both concatenated records should be recovered");
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
    fn first_record_after_lazy_init_is_counted_once() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        let usage = TokenUsage::new("test/model", 1000, 500, 0, 1.0, 2.0, 0.0);
        let expected_cost = usage.cost_usd;
        tracker.record_usage(usage).unwrap();

        let summary = tracker.get_summary().unwrap();
        assert!((summary.daily_cost_usd - expected_cost).abs() < 1e-9);
        assert!((summary.monthly_cost_usd - expected_cost).abs() < 1e-9);
    }

    #[test]
    fn record_usage_with_task_attribution_summarizes_from_ledger() {
        let tmp = TempDir::new().unwrap();
        let mut config = enabled_config();
        config.track_per_agent = true;
        let tracker = CostTracker::new(config, tmp.path()).unwrap();

        tracker
            .record_usage_with_task_attribution(
                TokenUsage::new("test/model", 1000, 500, 0, 1.0, 2.0, 0.0),
                Some("agent-a"),
                Some("goal-a"),
            )
            .unwrap();
        tracker
            .record_usage_with_task_attribution(
                TokenUsage::new("test/model", 2000, 500, 0, 1.0, 2.0, 0.0),
                Some("agent-a"),
                Some("goal-b"),
            )
            .unwrap();

        let summary = tracker.get_summary_for_task("goal-a").unwrap();

        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.total_tokens, 1500);
        assert!(summary.session_cost_usd > 0.0);
        assert_eq!(
            summary
                .by_agent
                .get("agent-a")
                .map(|stats| stats.request_count),
            Some(1)
        );
    }

    #[test]
    fn task_usage_totals_report_pricing_reliability_from_ledger() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        let mut unpriced = TokenUsage::new("test/unpriced", 1000, 500, 0, 0.0, 0.0, 0.0);
        unpriced.pricing_available = false;

        tracker
            .record_usage_with_task_attribution(unpriced, Some("agent-a"), Some("goal-a"))
            .unwrap();
        tracker
            .record_usage_with_task_attribution(
                TokenUsage::new("test/priced", 500, 250, 0, 1.0, 2.0, 0.0),
                Some("agent-a"),
                Some("goal-a"),
            )
            .unwrap();

        let (tokens, cost, pricing_available) = tracker
            .get_usage_totals_for_task_with_pricing("goal-a")
            .unwrap();

        assert_eq!(tokens, 2_250);
        assert!(cost > 0.0);
        assert!(!pricing_available);
    }

    #[test]
    fn task_usage_totals_include_appended_records() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        let empty = tracker.get_summary_for_task("goal-a").unwrap();
        assert_eq!(empty.request_count, 0);

        tracker
            .record_usage_with_task_attribution(
                TokenUsage::new("test/priced", 1000, 500, 0, 1.0, 2.0, 0.0),
                Some("agent-a"),
                Some("goal-a"),
            )
            .unwrap();
        let (tokens, cost, pricing_available) = tracker
            .get_usage_totals_for_task_with_pricing("goal-a")
            .unwrap();
        assert_eq!(tokens, 1_500);
        assert!(cost > 0.0);
        assert!(pricing_available);

        let mut unpriced = TokenUsage::new("test/unpriced", 500, 250, 0, 0.0, 0.0, 0.0);
        unpriced.pricing_available = false;
        tracker
            .record_usage_with_task_attribution(unpriced, Some("agent-a"), Some("goal-a"))
            .unwrap();

        let summary = tracker.get_summary_for_task("goal-a").unwrap();
        assert_eq!(summary.request_count, 2);
        assert_eq!(summary.total_tokens, 2_250);
        assert_eq!(
            summary
                .by_agent
                .get("agent-a")
                .map(|stats| stats.request_count),
            Some(2)
        );
        let (tokens, _cost, pricing_available) = tracker
            .get_usage_totals_for_task_with_pricing("goal-a")
            .unwrap();
        assert_eq!(tokens, 2_250);
        assert!(
            !pricing_available,
            "one unpriced row must make task cost-budget enforcement fail closed"
        );
    }

    #[test]
    fn task_usage_totals_read_legacy_recovered_records() {
        let tmp = TempDir::new().unwrap();
        let path = resolve_storage_path(tmp.path()).unwrap();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let first = CostRecord::with_attribution(
            "legacy-session",
            Some("agent-a".into()),
            Some("goal-a".into()),
            TokenUsage::new("test/model-a", 1000, 500, 0, 1.0, 2.0, 0.0),
        );
        let second = CostRecord::with_attribution(
            "legacy-session",
            Some("agent-a".into()),
            Some("goal-a".into()),
            TokenUsage::new("test/model-b", 2000, 250, 0, 1.0, 2.0, 0.0),
        );
        let joined = format!(
            "{}{}\n",
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
        std::fs::write(&path, joined).unwrap();

        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        let summary = tracker.get_summary_for_task("goal-a").unwrap();
        assert_eq!(summary.request_count, 2);
        assert_eq!(summary.total_tokens, 3_750);
        assert_eq!(summary.by_model.len(), 2);
        assert_eq!(
            summary
                .by_agent
                .get("agent-a")
                .map(|stats| stats.request_count),
            Some(2)
        );
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
    fn record_usage_reads_one_config_generation() {
        let tmp = TempDir::new().unwrap();

        let tracker = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                ..Default::default()
            },
            tmp.path(),
        )
        .expect("boot tracker");

        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000, 1_000, 0, 1.0, 1.0, 0.0),
                Some("agent-a"),
            )
            .expect("record under enabled+track_per_agent");

        let summary = tracker.get_summary().expect("summary");
        assert!(
            summary.by_agent.contains_key("agent-a"),
            "with enabled+track_per_agent both read from one snapshot, the alias must be attributed"
        );
    }

    #[test]
    fn scoped_usage_persists_after_tracking_is_disabled_for_future_turns() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                ..Default::default()
            },
            tmp.path(),
        )
        .expect("boot tracker");

        tracker.update_config(CostConfig {
            enabled: false,
            track_per_agent: true,
            ..Default::default()
        });

        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000, 500, 0, 1.0, 2.0, 0.0),
                Some("future-turn"),
            )
            .expect("disabled future turn should skip without error");
        tracker
            .record_scoped_usage_with_owned_task_attribution(
                TokenUsage::new("test/model", 2_000, 500, 0, 1.0, 2.0, 0.0),
                Some("in-flight"),
                Some("goal-a".into()),
            )
            .expect("in-flight scoped turn should persist");

        let skipped = tracker.get_summary_for_agent("future-turn").unwrap();
        assert_eq!(skipped.request_count, 0);
        let scoped = tracker.get_summary_for_task("goal-a").unwrap();
        assert_eq!(scoped.request_count, 1);
        assert_eq!(scoped.total_tokens, 2_500);
        assert_eq!(
            scoped
                .by_agent
                .get("in-flight")
                .map(|stats| stats.request_count),
            Some(1)
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
        let slot = RwLock::new(None);

        let boot = CostConfig {
            enabled: true,
            daily_limit_usd: 10.0,
            ..Default::default()
        };
        let first = CostTracker::resolve_global(&slot, boot, tmp.path())
            .expect("first init yields a tracker");

        let reloaded = CostConfig {
            enabled: true,
            daily_limit_usd: 14000.0,
            ..Default::default()
        };
        let after = CostTracker::resolve_global(&slot, reloaded, tmp.path())
            .expect("reload yields a tracker");

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

    #[test]
    fn get_or_init_global_replaces_tracker_when_data_dir_changes() {
        let first_tmp = TempDir::new().unwrap();
        let second_tmp = TempDir::new().unwrap();
        let slot = RwLock::new(None);

        let first = CostTracker::resolve_global(&slot, enabled_config(), first_tmp.path())
            .expect("first init yields a tracker");
        let after = CostTracker::resolve_global(&slot, enabled_config(), second_tmp.path())
            .expect("data-dir change yields a tracker");

        assert!(
            !Arc::ptr_eq(&first, &after),
            "a process-global tracker must not keep a stale ledger path when config.data_dir changes"
        );

        after
            .record_usage(TokenUsage::new("test/model", 10, 5, 0, 1.0, 2.0, 0.0))
            .unwrap();
        assert!(
            resolve_storage_path(second_tmp.path()).unwrap().exists(),
            "usage after data-dir change must land in the new canonical ledger"
        );
    }

    #[test]
    fn get_or_init_global_constructs_tracker_when_enabled_after_disabled_boot() {
        let tmp = TempDir::new().unwrap();
        let slot = RwLock::new(None);

        let disabled_boot = CostConfig {
            enabled: false,
            daily_limit_usd: 10.0,
            ..Default::default()
        };
        assert!(
            CostTracker::resolve_global(&slot, disabled_boot, tmp.path()).is_none(),
            "disabled boot must not construct a tracker"
        );

        let enable = CostConfig {
            enabled: true,
            daily_limit_usd: 14000.0,
            ..Default::default()
        };
        let constructed = CostTracker::resolve_global(&slot, enable, tmp.path())
            .expect("reload enabling cost tracking must construct the tracker");
        assert_eq!(
            constructed.config().daily_limit_usd,
            14000.0,
            "the on-demand tracker must adopt the reloaded daily limit"
        );

        let again = CostTracker::resolve_global(
            &slot,
            CostConfig {
                enabled: true,
                daily_limit_usd: 14000.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .expect("subsequent call yields a tracker");
        assert!(
            Arc::ptr_eq(&constructed, &again),
            "once constructed the tracker must be reused, not rebuilt"
        );
    }

    #[test]
    fn get_or_init_global_leaves_tracker_resident_when_disabled_on_reload() {
        let tmp = TempDir::new().unwrap();
        let slot = RwLock::new(None);

        let enabled_boot = CostConfig {
            enabled: true,
            daily_limit_usd: 14000.0,
            ..Default::default()
        };
        let tracker = CostTracker::resolve_global(&slot, enabled_boot, tmp.path())
            .expect("enabled boot yields a tracker");

        let disable = CostConfig {
            enabled: false,
            daily_limit_usd: 14000.0,
            ..Default::default()
        };
        let after = CostTracker::resolve_global(&slot, disable, tmp.path())
            .expect("disable reload leaves the tracker resident");
        assert!(
            Arc::ptr_eq(&tracker, &after),
            "disabling on reload must not tear down the resident tracker"
        );
        assert!(
            !after.config().enabled,
            "the resident tracker must adopt the disabled config so enforcement is neutralised"
        );
        assert!(
            matches!(after.check_budget(0.0).unwrap(), BudgetCheck::Allowed),
            "a disabled resident tracker must short-circuit enforcement"
        );
    }
}
