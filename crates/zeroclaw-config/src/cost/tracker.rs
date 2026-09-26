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

/// Process-local subtree accounting for one hop of a delegation chain.
///
/// Single source of truth: the durable ledger remains the source of truth
/// for every alias's own daily spend, which budget checks read per alias;
/// the descendant accumulator holds only what THIS delegation chain's
/// descendants recorded during this process lifetime. It exists because
/// descendants attribute their ledger records to their own alias, not the
/// ancestor's, so an ancestor's per-alias daily total alone cannot see
/// them. Descendant accounting is per process lifetime and per UTC day:
/// each record lands in the accumulator under the UTC day of its own
/// timestamp, the same day the ledger's daily aggregates fold that row
/// into, so the accumulator rolls over with the ledger's daily totals at
/// UTC midnight instead of carrying one day's descendant spend into the
/// next day's ceiling check. A daemon restart therefore forgets
/// descendant spend while every alias's own ledger total persists.
///
/// Shared by `Arc` into the budget scopes of the whole chain: the entry's
/// owning scope checks it in `check_budget`, and every descendant tracker
/// recording under an inherited chain adds its recorded cost to the
/// accumulator.
pub struct SubtreeSpend {
    alias: String,
    daily_ceiling_usd: f64,
    descendant_spend: Mutex<DescendantSpend>,
}

/// One UTC day's worth of descendant spend in a [`SubtreeSpend`] entry.
/// The stored total counts toward `day`'s ceiling check only: reads for
/// any other day see zero, so a stale day's spend never leaks into
/// another day's check. `day` is `None` until the first record opens the
/// slot: the entry is created when its tracker is derived, not when its
/// first descendant spend arrives, so an unopened slot must not behave
/// as if the derivation day had already accumulated spend (a record
/// stamped before the derivation would otherwise be dropped as older
/// than a day that never opened).
#[derive(Clone, Copy)]
struct DescendantSpend {
    day: Option<NaiveDate>,
    usd: f64,
}

impl SubtreeSpend {
    fn new(alias: &str, daily_ceiling_usd: f64) -> Self {
        Self {
            alias: alias.to_string(),
            daily_ceiling_usd,
            descendant_spend: Mutex::new(DescendantSpend {
                day: None,
                usd: 0.0,
            }),
        }
    }

    /// Descendant spend counted toward `day`'s ceiling check: the stored
    /// total when the stored day is `day`, zero for any other day. Read
    /// only, so a reader on a stale day just sees zero and never disturbs
    /// the stored slot.
    fn descendants_usd_for(&self, day: NaiveDate) -> f64 {
        let spend = *self.descendant_spend.lock();
        if spend.day == Some(day) {
            spend.usd
        } else {
            0.0
        }
    }

    /// Add `cost_usd` of descendant spend to `day`'s total. The first
    /// record opens the slot on its own day. After a day is open, a
    /// record for a NEWER day (the UTC rollover) resets the stored slot
    /// to `day` at zero first, so each UTC day starts from zero exactly
    /// like the ledger's daily aggregates; a record for the SAME day
    /// adds into that day's total; and a record for an OLDER day is
    /// dropped: the slot was already opened by a newer record (a usage
    /// stamped just before UTC midnight can be persisted just after
    /// another record already opened the new day), and resetting the
    /// slot back to the older day would lose the new day's accumulated
    /// spend at the next add. The older day's ceiling checks are over,
    /// and its ledger row still lands on its own day either way, so
    /// nothing else needs the stale amount.
    fn add_descendant_spend(&self, day: NaiveDate, cost_usd: f64) {
        let mut spend = self.descendant_spend.lock();
        if spend.day.is_none_or(|stored| stored < day) {
            *spend = DescendantSpend {
                day: Some(day),
                usd: 0.0,
            };
        }
        if spend.day == Some(day) {
            spend.usd += cost_usd;
        }
    }
}

/// Budget scope a derived tracker enforces its substituted daily limit
/// against. Private to the cost module: the shared ledger stays the source
/// of truth either way; the scope only chooses which total the substituted
/// daily limit compares against.
enum BudgetScope {
    /// Compare the substituted daily limit against the shared process-wide
    /// daily total (previous derived-tracker behavior).
    Shared,
    /// Compare the shared process-wide daily total against the global daily
    /// limit tightened by this per-hop ceiling
    /// (`config.daily_limit_usd.min(daily_ceiling_usd)`, read live each
    /// check). This is the `track_per_agent = false` degrade: per-alias
    /// daily totals cannot exist, so the per-hop ceiling can only tighten
    /// the shared check. No subtree chain rides on this scope: chains only
    /// apply to per-agent scopes, and the shared cap stays shared.
    SharedCapped { daily_ceiling_usd: f64 },
    /// Compare the named agent's OWN daily spend against this ceiling, so a
    /// per-profile cost ceiling means that agent's usage for the day. The
    /// shared daily/monthly limits still apply to the shared totals on top.
    /// `own` is this hop's subtree entry; `inherited` is the delegation
    /// chain's ancestor entries (nearest first), so a delegated descendant's
    /// spend also counts against every ancestor's ceiling. Chains only
    /// exist on per-agent scopes; the shared-cap degrade stays shared.
    Agent {
        own: Arc<SubtreeSpend>,
        inherited: Vec<Arc<SubtreeSpend>>,
    },
}

/// Where a tracker reads the two mutable mode flags that decide whether
/// it enforces and records at all (`enabled`) and whether its recorded
/// rows carry an agent alias (`track_per_agent`). Split from the limits
/// on purpose: the limits (`daily_limit_usd`, `monthly_limit_usd`,
/// `warn_at_percent`) always come from the live config handle, while the
/// mode is decided once for a derived tracker so a running delegation's
/// checks and records cannot be desynchronized by a mid-run reload.
#[derive(Clone, Copy)]
enum EnforcementMode {
    /// Read both flags from the live config on every use. The
    /// process-global tracker is live: an operator reload applies to its
    /// next check and its next record.
    Live,
    /// Both flags frozen for the derived tracker's whole lifetime:
    /// captured from the live config when deriving from a live base (the
    /// process-global tracker), and inherited verbatim from an
    /// already-frozen base, so the mode is fixed at the ROOT delegation
    /// of a tree and every nested hop of that tree derives with the same
    /// pair. A delegation that started scoped keeps checking and
    /// recording under the mode it started with, and one that started
    /// dropping aliases keeps dropping them, for the delegation's whole
    /// lifetime.
    Frozen {
        enabled: bool,
        track_per_agent: bool,
    },
}

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
    /// Which total the substituted daily limit compares against. The global
    /// tracker always enforces `Shared`; only derived trackers can carry an
    /// agent-scoped ceiling.
    budget_scope: BudgetScope,
    /// Where the enforcement mode comes from. The mode flags (`enabled`,
    /// `track_per_agent`) decide whether this tracker enforces and records
    /// at all and whether recorded rows carry an agent alias; the limits
    /// (`daily_limit_usd`, `monthly_limit_usd`, `warn_at_percent`) always
    /// come from the live config handle. The global tracker reads the mode
    /// live as well; every derived tracker freezes it at derivation so a
    /// delegation's checks and records keep agreeing for its whole
    /// lifetime.
    enforcement_mode: EnforcementMode,
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
            budget_scope: BudgetScope::Shared,
            enforcement_mode: EnforcementMode::Live,
        })
    }

    fn config_snapshot(&self) -> CostConfig {
        self.config.read().clone()
    }

    pub fn config(&self) -> CostConfig {
        self.config_snapshot()
    }

    /// Whether this tracker enforces and records spend. The
    /// process-global tracker reads the live config, so a reload applies
    /// at its next check; a derived tracker reports the mode frozen at
    /// derivation time, matching what its checks and records actually
    /// honor for the delegation's whole lifetime.
    pub fn is_enabled(&self) -> bool {
        self.mode().0
    }

    /// The effective `(enabled, track_per_agent)` pair for this tracker,
    /// as the runtime reads it when deciding how a delegated sub-loop is
    /// scoped: the process-global tracker reports the live pair, and a
    /// derived tracker the pair frozen at derivation (for a nested hop,
    /// inherited from its delegating parent). Exposed as a plain pair
    /// rather than the mode itself so the public surface stays two
    /// booleans.
    pub fn enforcement_flags(&self) -> (bool, bool) {
        self.mode()
    }

    /// Hot-swap config so reloaded budget limits apply without a restart.
    pub fn update_config(&self, config: CostConfig) {
        *self.config.write() = config;
    }

    /// Derive an ephemeral tracker whose budget checks read only the
    /// shared process-wide daily and monthly totals against the global
    /// limits (read live each check), with no per-agent ceiling and no
    /// subtree chain: the frozen-mode equivalent of the global tracker.
    /// An independent nested delegation with no per-hop ceiling (`0` =
    /// inherit the global limit) uses this so the target runs under the
    /// shared global limits instead of its delegating parent's
    /// agent-scoped tracker, while the enforcement mode stays frozen
    /// from the base exactly as for every other derived tracker.
    pub fn derived_shared(&self) -> Self {
        self.derived_with_scope(BudgetScope::Shared)
    }

    /// Derive an ephemeral tracker whose shared daily check uses the base
    /// tracker's global daily limit tightened by `daily_ceiling_usd`
    /// (`config.daily_limit_usd.min(daily_ceiling_usd)`, read live each
    /// check). Delegated sub-loops use this when
    /// `[cost].track_per_agent` is false: per-alias daily totals cannot
    /// exist, so the per-hop ceiling can only tighten the shared check, and
    /// their recorded spend lands on the same durable ledger as every other
    /// path. The captured `track_per_agent = false` is frozen for the
    /// derived tracker's lifetime, so a mid-run reload enabling per-agent
    /// attribution does not start attributing this delegation's rows
    /// halfway through a run. The derived tracker is never registered as
    /// the process-global one; it lives only as long as the delegation
    /// that created it.
    pub fn derived_shared_capped(&self, daily_ceiling_usd: f64) -> Self {
        self.derived_with_scope(BudgetScope::SharedCapped { daily_ceiling_usd })
    }

    /// Derive an ephemeral tracker whose `daily_ceiling_usd` is compared
    /// against `agent_alias`'s OWN daily spend on the shared ledger, so a
    /// per-profile cost ceiling means that agent's usage for the day. The
    /// tracker's own shared daily and monthly limits still apply to the
    /// shared totals on top, so the derived tracker can only ever be
    /// stricter than the base tracker, never looser. The scope kind and
    /// the enforcement mode (`enabled`, `track_per_agent`, captured at
    /// derivation) are fixed for the derived tracker's lifetime; the
    /// limits themselves are read live from the base tracker's shared
    /// config handle, so config reloads apply at the next `check_budget`.
    pub fn derived_for_agent(&self, agent_alias: &str, daily_ceiling_usd: f64) -> Self {
        self.derived_for_agent_in_chain(agent_alias, daily_ceiling_usd, Vec::new())
    }

    /// `derived_for_agent` with an inherited delegation chain: `inherited`
    /// carries the ancestor subtree entries (nearest first) collected with
    /// [`Self::subtree_chain_for_children`] on the delegating parent's
    /// tracker. Budget checks through the derived tracker then also count
    /// this agent's spend against every ancestor's per-hop ceiling, and
    /// usage recorded through it accumulates into each ancestor entry's
    /// descendant total. Chains only apply to per-agent scopes.
    /// The scope kind and the enforcement mode (`enabled`,
    /// `track_per_agent`, captured at derivation) are fixed for the
    /// derived tracker's lifetime; the limits themselves are read live
    /// from the base tracker's shared config handle, so config reloads
    /// apply at the next `check_budget`.
    pub fn derived_for_agent_in_chain(
        &self,
        agent_alias: &str,
        daily_ceiling_usd: f64,
        inherited: Vec<Arc<SubtreeSpend>>,
    ) -> Self {
        self.derived_with_scope(BudgetScope::Agent {
            own: Arc::new(SubtreeSpend::new(agent_alias, daily_ceiling_usd)),
            inherited,
        })
    }

    /// The subtree entries a delegation FROM this tracker's agent passes to
    /// the child's budget scope: this agent's own entry first, then the
    /// inherited ancestor chain (nearest first). Empty when this tracker is
    /// not agent-scoped, so unscoped and shared-cap delegations carry no
    /// chain. Delegation plumbing calls this on the tracker of the loop a
    /// delegated sub-agent runs under when building that sub-agent's own
    /// delegate tool.
    pub fn subtree_chain_for_children(&self) -> Vec<Arc<SubtreeSpend>> {
        match &self.budget_scope {
            BudgetScope::Agent { own, inherited } => {
                let mut chain = Vec::with_capacity(inherited.len() + 1);
                chain.push(Arc::clone(own));
                chain.extend(inherited.iter().cloned());
                chain
            }
            BudgetScope::Shared | BudgetScope::SharedCapped { .. } => Vec::new(),
        }
    }

    fn derived_with_scope(&self, budget_scope: BudgetScope) -> Self {
        // Two sources of truth, split on purpose. The config handle is
        // shared, not snapshotted: a derived tracker sees every config
        // reload the base tracker sees (`update_config` writes the one
        // live `CostConfig`), so an operator lowering `daily_limit_usd`
        // mid-delegation binds the delegate's next `check_budget`. The
        // enforcement MODE is frozen instead, and where the frozen pair
        // comes from depends on the base: deriving from a LIVE base (the
        // process-global tracker, the root of a delegation tree)
        // captures `enabled` and `track_per_agent` from the config here,
        // because whether a delegation enforces at all and how its rows
        // are attributed are decisions taken at delegation start, same
        // as the scope kind fixed by `budget_scope`; deriving from a
        // FROZEN base (any nested hop inside a running delegation)
        // inherits the base's captured pair verbatim, so the mode is
        // fixed at the root of the tree and a mid-run reload of either
        // flag can neither untrack a scoped run, whose spend would then
        // escape every ceiling if tracking were re-enabled, nor start
        // reattributing an unattributed one halfway through a run.
        let enforcement_mode = match self.enforcement_mode {
            EnforcementMode::Live => {
                let (enabled, track_per_agent) = {
                    let config = self.config.read();
                    (config.enabled, config.track_per_agent)
                };
                EnforcementMode::Frozen {
                    enabled,
                    track_per_agent,
                }
            }
            frozen @ EnforcementMode::Frozen { .. } => frozen,
        };
        Self {
            config: Arc::clone(&self.config),
            storage: Arc::clone(&self.storage),
            session_id: self.session_id.clone(),
            session_totals: Arc::clone(&self.session_totals),
            budget_scope,
            enforcement_mode,
        }
    }

    /// The `(enabled, track_per_agent)` pair every enforcement and
    /// attribution decision on this tracker reads: the early return in
    /// `check_budget`, the record path's enabled gate, the alias
    /// stamping, and the ancestor-accumulator gate. A live tracker reads
    /// the pair from the config; a frozen one returns the pair captured
    /// at derivation, so a derived tracker keeps honoring the mode its
    /// delegation started under across later reloads.
    fn mode(&self) -> (bool, bool) {
        match &self.enforcement_mode {
            EnforcementMode::Live => {
                let config = self.config.read();
                (config.enabled, config.track_per_agent)
            }
            EnforcementMode::Frozen {
                enabled,
                track_per_agent,
            } => (*enabled, *track_per_agent),
        }
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
        self.check_budget_at_period(estimated_cost_usd, ReportingPeriod::current())
    }

    /// [`Self::check_budget`] against an injected reporting period, so
    /// tests can drive which UTC day the per-day accounting reads without
    /// waiting for a real midnight rollover.
    fn check_budget_at_period(
        &self,
        estimated_cost_usd: f64,
        period: ReportingPeriod,
    ) -> Result<BudgetCheck> {
        let config = self.config_snapshot();
        let (enabled, _) = self.mode();
        if !enabled {
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
        let (daily_cost, monthly_cost) = storage.get_aggregated_costs_at_period(period)?;
        let day = storage.reporting_period().day;

        // Check daily limit (shared). A shared-capped scope tightens the
        // global daily limit by its per-hop ceiling; the global limit is
        // read live from the shared config handle each call, so an operator
        // reload applies to running delegates at their next check.
        let shared_daily_limit = match &self.budget_scope {
            BudgetScope::SharedCapped { daily_ceiling_usd } => {
                config.daily_limit_usd.min(*daily_ceiling_usd)
            }
            BudgetScope::Shared | BudgetScope::Agent { .. } => config.daily_limit_usd,
        };
        let projected_daily = daily_cost + estimated_cost_usd;
        if projected_daily > shared_daily_limit {
            return Ok(BudgetCheck::Exceeded {
                current_usd: daily_cost,
                limit_usd: shared_daily_limit,
                period: UsagePeriod::Day,
                agent_alias: None,
            });
        }

        // Per-agent daily ceiling: when this tracker was derived for a
        // specific agent, its ceiling means THAT agent's own spend for the
        // day, not the shared total, and the ceilings of every delegation
        // ancestor bind on top: a delegated descendant's spend counts
        // against each ancestor's ceiling through the chain's shared
        // subtree entries. The descendant total is read for the check's
        // accounting day, so it rolls over with the ledger's daily totals
        // at UTC midnight. Runs after the shared daily check, so the
        // derived tracker can only be stricter than the base tracker,
        // never looser. Own entry first, then ancestors nearest first, so
        // the nearest violated ceiling wins and names its agent.
        if let BudgetScope::Agent { own, inherited } = &self.budget_scope {
            for entry in std::iter::once(own).chain(inherited.iter()) {
                let agent_daily = storage.get_daily_cost_for_agent(&entry.alias);
                let descendants_usd = entry.descendants_usd_for(day);
                let projected = agent_daily + descendants_usd + estimated_cost_usd;
                if projected > entry.daily_ceiling_usd {
                    return Ok(BudgetCheck::Exceeded {
                        current_usd: agent_daily + descendants_usd,
                        limit_usd: entry.daily_ceiling_usd,
                        period: UsagePeriod::Day,
                        agent_alias: Some(entry.alias.clone()),
                    });
                }
            }
        }

        // Check monthly limit
        let projected_monthly = monthly_cost + estimated_cost_usd;
        if projected_monthly > config.monthly_limit_usd {
            return Ok(BudgetCheck::Exceeded {
                current_usd: monthly_cost,
                limit_usd: config.monthly_limit_usd,
                period: UsagePeriod::Month,
                agent_alias: None,
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

    /// Record a usage event attributed to an agent, a durable task, and the
    /// chat session that incurred it. `conversation_id` is the runtime
    /// session key scoped around the turn; `None` keeps the record
    /// attributable only to the daemon-lifetime tracker id.
    pub fn record_usage_attributed(
        &self,
        usage: TokenUsage,
        agent_alias: Option<&str>,
        task_id: Option<String>,
        conversation_id: Option<String>,
    ) -> Result<()> {
        self.record_usage_with_owned_task_attribution_inner_with_sync(
            usage,
            agent_alias,
            task_id,
            conversation_id,
            true,
            File::sync_all,
        )
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
        self.record_usage_with_owned_task_attribution_inner_with_sync(
            usage,
            agent_alias,
            task_id,
            None,
            honor_enabled,
            File::sync_all,
        )
    }

    fn record_usage_with_owned_task_attribution_inner_with_sync(
        &self,
        usage: TokenUsage,
        agent_alias: Option<&str>,
        task_id: Option<String>,
        conversation_id: Option<String>,
        honor_enabled: bool,
        sync_file: fn(&File) -> std::io::Result<()>,
    ) -> Result<()> {
        let (enabled, track_per_agent) = self.mode();
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
            CostRecord::with_attribution(&self.session_id, effective_alias.clone(), task_id, usage)
                .with_conversation_id(conversation_id);
        // The UTC day this record belongs to. The ledger's rebuild folds
        // the row into this day's aggregates and no other day's, so the
        // descendant accumulator keys on the same day and the two agree
        // on which day a record's spend counts under.
        let record_day = record.usage.timestamp.naive_utc().date();

        let mut storage = self.lock_storage();
        let append_outcome = storage.add_record_with_sync(record, sync_file)?;

        {
            let mut totals = self.lock_session_totals();
            let entry = totals.entry(effective_alias).or_default();
            entry.cost_usd += cost_usd;
            entry.total_tokens += total_tokens;
            entry.request_count += 1;
        }

        drop(storage);

        // Delegation-chain accounting: usage recorded through an
        // agent-scoped tracker with an inherited chain lands on the ledger
        // under this tracker's own alias, so the ancestors' per-alias daily
        // totals never see it. Add it to each ancestor entry's
        // process-local descendant accumulator under the record's own UTC
        // day, so the ancestor's next `check_budget` counts it against the
        // ancestor's ceiling for that day and the accumulator rolls over
        // with the ledger's daily totals. Gated on the same
        // `track_per_agent` flag as the attribution above: chains only
        // exist on per-agent scopes.
        if track_per_agent && let BudgetScope::Agent { inherited, .. } = &self.budget_scope {
            for entry in inherited {
                entry.add_descendant_spend(record_day, cost_usd);
            }
        }

        append_outcome.into_result()
    }

    /// Get the current cost summary. When `[cost].track_per_agent` is
    /// enabled, the response includes a `by_agent` rollup over the current
    /// month's records.
    pub fn get_summary(&self) -> Result<CostSummary> {
        self.get_summary_filtered(None)
    }

    /// Per-model rollup over every record in the current UTC month.
    ///
    /// [`CostSummary::by_model`] stays daily-scoped for dashboard and RPC
    /// consumers. Operator surfaces that qualify the monthly total, such as
    /// the `zeroclaw status` pricing-unavailable warning, need the whole
    /// month's recorded provenance so unpriced usage from an earlier day
    /// does not disappear at UTC day rollover while the monthly spend still
    /// omits its cost. Derived from the persisted ledger on demand; nothing
    /// is cached or duplicated.
    pub fn get_current_month_model_stats(&self) -> Result<HashMap<String, ModelStats>> {
        self.get_current_month_model_stats_at_period(ReportingPeriod::current())
    }

    fn get_current_month_model_stats_at_period(
        &self,
        period: ReportingPeriod,
    ) -> Result<HashMap<String, ModelStats>> {
        let mut storage = self.lock_storage();
        storage.ensure_period_cache_current_at(period)?;
        let period = storage.reporting_period();
        let records = storage.current_month_records(period)?;
        Ok(build_model_stats(records.iter()))
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
        self.get_summary_for_task_at_period(task_id, ReportingPeriod::current())
    }

    fn get_summary_for_task_at_period(
        &self,
        task_id: &str,
        period: ReportingPeriod,
    ) -> Result<CostSummary> {
        let mut storage = self.lock_storage();
        storage.summary_for_task(task_id, period)
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
        self.get_summary_filtered_at_period(agent_filter, ReportingPeriod::current())
    }

    fn get_summary_filtered_at_period(
        &self,
        agent_filter: Option<&str>,
        period: ReportingPeriod,
    ) -> Result<CostSummary> {
        let (daily_cost, monthly_cost, period, current_month_records) = {
            let mut storage = self.lock_storage();
            storage.ensure_period_cache_current_at(period)?;
            let period = storage.reporting_period();
            let daily_cost = storage.daily_cost_usd;
            let monthly_cost = storage.monthly_cost_usd;
            let records = storage.current_month_records(period)?;
            (daily_cost, monthly_cost, period, records)
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
        let by_model = build_model_stats(
            current_month_records
                .iter()
                .filter(|r| matches_agent(r))
                .filter(|r| period.contains_day(r.usage.timestamp)),
        );

        let (daily_total, monthly_total, by_agent) = if let Some(alias) = agent_filter {
            // Per-agent view: re-aggregate day/month from persisted records.
            let mut daily_total = 0.0;
            let mut monthly_total = 0.0;
            for record in &current_month_records {
                if record.agent_alias.as_deref() != Some(alias) {
                    continue;
                }
                if period.contains_day(record.usage.timestamp) {
                    daily_total += record.usage.cost_usd;
                }
                if period.contains_month(record.usage.timestamp) {
                    monthly_total += record.usage.cost_usd;
                }
            }
            (daily_total, monthly_total, HashMap::new())
        } else if self.config_snapshot().track_per_agent {
            let by_agent = build_agent_stats(&current_month_records);
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
            unpriced_tokens: 0,
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
    if record.usage.unpriced_tokens > 0 {
        entry.unpriced_tokens = entry
            .unpriced_tokens
            .saturating_add(record.usage.unpriced_tokens);
    } else if !record.usage.pricing_available {
        // Compatibility with rows written by the first provenance format,
        // which had only a record-level boolean. Rows older than that omit the
        // boolean too and deserialize as priced by the existing default.
        entry.unpriced_tokens = entry
            .unpriced_tokens
            .saturating_add(record.usage.total_tokens);
    }
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
    /// Aggregated USD cost for records on the cached UTC day.
    daily_cost: f64,
    /// Aggregated USD cost for records in the cached UTC month.
    monthly_cost: f64,
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
    fn record(&mut self, record: &CostRecord, period: ReportingPeriod) {
        self.total_cost += record.usage.cost_usd;
        if period.contains_day(record.usage.timestamp) {
            self.daily_cost += record.usage.cost_usd;
        }
        if period.contains_month(record.usage.timestamp) {
            self.monthly_cost += record.usage.cost_usd;
        }
        self.total_tokens += record.usage.total_tokens;
        self.request_count += 1;
        add_model_stats(&mut self.by_model, record);
        add_agent_stats(&mut self.by_agent, record);
    }

    fn finish(self) -> CostSummary {
        CostSummary {
            session_cost_usd: self.total_cost,
            daily_cost_usd: self.daily_cost,
            monthly_cost_usd: self.monthly_cost,
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
    /// Cached per-alias spend for the current UTC day. Records with an
    /// unassigned alias count toward `daily_cost_usd` only. Maintained by
    /// the append path and rebuilt in the same pass as `daily_cost_usd`,
    /// so per-agent budget checks never rescan the ledger.
    daily_cost_by_agent: HashMap<String, f64>,
    /// Day represented by `daily_cost_usd`.
    cached_day: NaiveDate,
    /// Year represented by `monthly_cost_usd`.
    cached_year: i32,
    /// Month represented by `monthly_cost_usd`.
    cached_month: u32,
    /// Whether the cached day/month aggregates reflect the current ledger.
    aggregates_current: bool,
}

enum AppendOutcome {
    Synced,
    AppendedButSyncFailed(anyhow::Error),
}

impl AppendOutcome {
    fn into_result(self) -> Result<()> {
        match self {
            Self::Synced => Ok(()),
            Self::AppendedButSyncFailed(error) => Err(error),
        }
    }
}

#[derive(Clone, Copy)]
struct ReportingPeriod {
    day: NaiveDate,
    year: i32,
    month: u32,
}

impl ReportingPeriod {
    fn current() -> Self {
        let now = Utc::now();
        Self {
            day: now.date_naive(),
            year: now.year(),
            month: now.month(),
        }
    }

    fn contains_day(self, timestamp: DateTime<Utc>) -> bool {
        timestamp.naive_utc().date() == self.day
    }

    fn contains_month(self, timestamp: DateTime<Utc>) -> bool {
        let timestamp = timestamp.naive_utc();
        timestamp.year() == self.year && timestamp.month() == self.month
    }
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
            daily_cost_by_agent: HashMap::new(),
            cached_day: now.date_naive(),
            cached_year: now.year(),
            cached_month: now.month(),
            aggregates_current: false,
        })
    }

    fn recover_concatenated_records<F>(
        input: &str,
        mut on_record: F,
    ) -> Result<(), serde_json::Error>
    where
        F: FnMut(CostRecord),
    {
        for value in serde_json::Deserializer::from_str(input).into_iter::<CostRecord>() {
            on_record(value?);
        }
        Ok(())
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
                Err(_) => {
                    if let Err(error) = Self::recover_concatenated_records(trimmed, &mut on_record)
                    {
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
        let mut daily_by_agent: HashMap<String, f64> = HashMap::new();

        self.for_each_record(|record| {
            let timestamp = record.usage.timestamp.naive_utc();

            if timestamp.date() == day {
                daily_cost += record.usage.cost_usd;
                if let Some(agent_alias) = &record.agent_alias {
                    *daily_by_agent.entry(agent_alias.clone()).or_insert(0.0) +=
                        record.usage.cost_usd;
                }
            }

            if timestamp.year() == year && timestamp.month() == month {
                monthly_cost += record.usage.cost_usd;
            }
        })?;

        self.daily_cost_usd = daily_cost;
        self.monthly_cost_usd = monthly_cost;
        self.daily_cost_by_agent = daily_by_agent;
        self.cached_day = day;
        self.cached_year = year;
        self.cached_month = month;
        self.aggregates_current = true;

        Ok(())
    }

    fn ensure_period_cache_current(&mut self) -> Result<()> {
        self.ensure_period_cache_current_at(ReportingPeriod::current())
    }

    fn ensure_period_cache_current_at(&mut self, period: ReportingPeriod) -> Result<()> {
        if !self.aggregates_current
            || period.day != self.cached_day
            || period.year != self.cached_year
            || period.month != self.cached_month
        {
            self.rebuild_aggregates(period.day, period.year, period.month)?;
        }

        Ok(())
    }

    fn add_record_with_sync(
        &mut self,
        record: CostRecord,
        sync_file: fn(&File) -> std::io::Result<()>,
    ) -> Result<AppendOutcome> {
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

        let timestamp = record.usage.timestamp.naive_utc();
        if timestamp.date() == self.cached_day {
            self.daily_cost_usd += record.usage.cost_usd;
            if let Some(agent_alias) = &record.agent_alias {
                *self
                    .daily_cost_by_agent
                    .entry(agent_alias.clone())
                    .or_insert(0.0) += record.usage.cost_usd;
            }
        } else {
            // A record for a prior day landed outside its day's cache: the
            // per-alias day buckets would go stale, so drop the whole cache
            // and let the next check rebuild it.
            self.aggregates_current = false;
        }
        if timestamp.year() == self.cached_year && timestamp.month() == self.cached_month {
            self.monthly_cost_usd += record.usage.cost_usd;
        }

        let sync_result = sync_file(&file).with_context(|| {
            format!(
                "Failed to sync cost storage at {}",
                self.path.display().to_string()
            )
        });

        Ok(match sync_result {
            Ok(()) => AppendOutcome::Synced,
            Err(error) => AppendOutcome::AppendedButSyncFailed(error),
        })
    }

    /// Get aggregated costs for current day and month.
    fn get_aggregated_costs(&mut self) -> Result<(f64, f64)> {
        self.get_aggregated_costs_at_period(ReportingPeriod::current())
    }

    /// Get aggregated costs for the given reporting period's day and
    /// month, rebuilding the caches first when the period moved.
    fn get_aggregated_costs_at_period(&mut self, period: ReportingPeriod) -> Result<(f64, f64)> {
        self.ensure_period_cache_current_at(period)?;
        Ok((self.daily_cost_usd, self.monthly_cost_usd))
    }

    /// Per-alias spend for the cached current day. Reads only the in-memory
    /// bucket maintained by the append path / rebuild, so per-agent budget
    /// checks never rescan the ledger.
    fn get_daily_cost_for_agent(&self, agent_alias: &str) -> f64 {
        self.daily_cost_by_agent
            .get(agent_alias)
            .copied()
            .unwrap_or(0.0)
    }

    fn reporting_period(&self) -> ReportingPeriod {
        ReportingPeriod {
            day: self.cached_day,
            year: self.cached_year,
            month: self.cached_month,
        }
    }

    /// Snapshot every record whose timestamp falls within the current
    /// calendar month. Used to build per-agent rollups without folding a
    /// new aggregate table into the JSONL file.
    fn current_month_records(&self, period: ReportingPeriod) -> Result<Vec<CostRecord>> {
        let mut out = Vec::new();
        self.for_each_record(|record| {
            if period.contains_month(record.usage.timestamp) {
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

    fn summary_for_task(&mut self, task_id: &str, period: ReportingPeriod) -> Result<CostSummary> {
        self.ensure_period_cache_current_at(period)?;
        let period = self.reporting_period();
        let mut summary = CostSummaryAccumulator::default();
        self.for_each_record(|record| {
            if record.task_id.as_deref() == Some(task_id) {
                summary.record(&record, period);
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
    use chrono::{Duration, TimeZone, Utc};
    use tempfile::TempDir;

    fn enabled_config() -> CostConfig {
        CostConfig {
            enabled: true,
            ..Default::default()
        }
    }

    fn record_at(
        model: &str,
        cost_usd: f64,
        timestamp: DateTime<Utc>,
        task_id: Option<&str>,
    ) -> CostRecord {
        let usage = TokenUsage {
            model: model.to_string(),
            input_tokens: 10,
            output_tokens: 10,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            total_tokens: 20,
            cost_usd,
            pricing_available: true,
            unpriced_tokens: 0,
            timestamp,
        };
        CostRecord::with_attribution(
            "fixture-session",
            Some("fixture-agent".to_string()),
            task_id.map(str::to_string),
            usage,
        )
    }

    fn unpriced_record_at(
        model: &str,
        unpriced_tokens: u64,
        timestamp: DateTime<Utc>,
    ) -> CostRecord {
        let usage = TokenUsage {
            model: model.to_string(),
            input_tokens: unpriced_tokens,
            output_tokens: 0,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            total_tokens: unpriced_tokens,
            cost_usd: 0.0,
            pricing_available: false,
            unpriced_tokens,
            timestamp,
        };
        CostRecord::with_attribution(
            "fixture-session",
            Some("fixture-agent".to_string()),
            None,
            usage,
        )
    }

    /// A `TokenUsage` priced at exactly `cost_usd` and stamped with the
    /// given timestamp, for routing a timestamped record through a
    /// tracker's record path (the record path takes `TokenUsage`, so the
    /// `record_at` fixture's `CostRecord` cannot ride it directly).
    fn usage_costing_at(cost_usd: f64, timestamp: DateTime<Utc>) -> TokenUsage {
        let tokens = (cost_usd * 1_000_000.0).round() as u64;
        let mut usage = TokenUsage::new("test/model", tokens, 0, 0, 1.0, 1.0, 0.0);
        usage.timestamp = timestamp;
        usage
    }

    fn write_records(path: &Path, records: &[CostRecord]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for record in records {
            writeln!(file, "{}", serde_json::to_string(record).unwrap()).unwrap();
        }
        file.sync_all().unwrap();
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
    fn recovery_helper_accepts_clean_concatenated_records() {
        let first = record_at("test/model-a", 1.0, Utc::now(), Some("task-a"));
        let second = record_at("test/model-b", 2.0, Utc::now(), Some("task-b"));
        let input = format!(
            "{}{}",
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
        let mut recovered = Vec::new();

        let result =
            CostStorage::recover_concatenated_records(&input, |record| recovered.push(record));

        assert!(result.is_ok());
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].id, first.id);
        assert_eq!(recovered[1].id, second.id);
    }

    #[test]
    fn recovery_helper_returns_error_after_recovering_valid_prefix() {
        let record = record_at("test/model", 1.0, Utc::now(), Some("task-a"));
        let input = format!("{}{{malformed", serde_json::to_string(&record).unwrap());
        let mut recovered = Vec::new();

        let result =
            CostStorage::recover_concatenated_records(&input, |record| recovered.push(record));

        assert!(result.is_err());
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].id, record.id);
    }

    #[test]
    fn recovery_helper_returns_error_without_recovering_wholly_malformed_input() {
        let mut recovered = Vec::new();

        let result =
            CostStorage::recover_concatenated_records("not-json", |record| recovered.push(record));

        assert!(result.is_err());
        assert!(recovered.is_empty());
    }

    #[test]
    fn sync_failure_keeps_all_process_visible_totals_consistent() {
        fn fail_sync(_: &File) -> std::io::Result<()> {
            Err(std::io::Error::other("forced sync failure"))
        }

        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        let usage = TokenUsage {
            model: "test/model".to_string(),
            input_tokens: 10,
            output_tokens: 10,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            total_tokens: 20,
            cost_usd: 1.0,
            pricing_available: true,
            unpriced_tokens: 0,
            timestamp: Utc::now(),
        };

        let error = tracker
            .record_usage_with_owned_task_attribution_inner_with_sync(
                usage,
                None,
                Some("task-a".to_string()),
                None,
                true,
                fail_sync,
            )
            .unwrap_err();

        assert!(error.to_string().contains("Failed to sync cost storage"));
        let summary = tracker.get_summary().unwrap();
        assert!((summary.session_cost_usd - 1.0).abs() < f64::EPSILON);
        assert!((summary.daily_cost_usd - 1.0).abs() < f64::EPSILON);
        assert!((summary.monthly_cost_usd - 1.0).abs() < f64::EPSILON);
        assert_eq!(summary.total_tokens, 20);
        assert_eq!(summary.request_count, 1);

        let model = summary.by_model.get("test/model").unwrap();
        assert!((model.cost_usd - 1.0).abs() < f64::EPSILON);
        assert_eq!(model.total_tokens, 20);
        assert_eq!(model.request_count, 1);
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
    fn model_summary_counts_only_explicitly_unpriced_tokens() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        let configured_free = TokenUsage::new("test/model", 100, 50, 0, 0.0, 0.0, 0.0);
        let mut unpriced = TokenUsage::new("test/model", 200, 75, 0, 0.0, 0.0, 0.0);
        unpriced.pricing_available = false;

        tracker.record_usage(configured_free).unwrap();
        tracker.record_usage(unpriced).unwrap();

        let summary = tracker.get_summary().unwrap();
        let model = summary.by_model.get("test/model").unwrap();
        assert_eq!(model.total_tokens, 425);
        assert_eq!(model.unpriced_tokens, 275);
        assert_eq!(model.cost_usd, 0.0);
    }

    #[test]
    fn model_summary_prefers_dimension_level_unpriced_count() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();
        let mut partial = TokenUsage::new("test/model", 100, 20, 0, 2.0, 0.0, 0.0);
        partial.unpriced_tokens = 20;
        partial.pricing_available = false;

        tracker.record_usage(partial).unwrap();

        let summary = tracker.get_summary().unwrap();
        let model = summary.by_model.get("test/model").unwrap();
        assert_eq!(model.total_tokens, 120);
        assert_eq!(model.unpriced_tokens, 20);
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
    fn summaries_use_one_cached_period_for_task_and_model_rollups() {
        let tmp = TempDir::new().unwrap();
        let storage_path = resolve_storage_path(tmp.path()).unwrap();
        let period = ReportingPeriod {
            day: NaiveDate::from_ymd_opt(2025, 6, 15).unwrap(),
            year: 2025,
            month: 6,
        };
        let month_start = period.day.with_day(1).unwrap();
        let other_current_month = month_start;
        let prior_month = month_start - Duration::days(1);
        write_records(
            &storage_path,
            &[
                record_at(
                    "today/model",
                    1.0,
                    Utc.from_utc_datetime(&period.day.and_hms_opt(12, 0, 0).unwrap()),
                    Some("task-periods"),
                ),
                record_at(
                    "earlier-month/model",
                    2.0,
                    Utc.from_utc_datetime(&other_current_month.and_hms_opt(0, 0, 0).unwrap()),
                    Some("task-periods"),
                ),
                record_at(
                    "prior-month/model",
                    4.0,
                    Utc.from_utc_datetime(&prior_month.and_hms_opt(0, 0, 0).unwrap()),
                    Some("task-periods"),
                ),
            ],
        );

        let mut config = enabled_config();
        config.track_per_agent = true;
        let tracker = CostTracker::new(config, tmp.path()).unwrap();

        let task = tracker
            .get_summary_for_task_at_period("task-periods", period)
            .unwrap();
        assert_eq!(task.request_count, 3);
        assert!((task.session_cost_usd - 7.0).abs() < f64::EPSILON);
        assert!((task.monthly_cost_usd - 3.0).abs() < f64::EPSILON);
        assert!((task.daily_cost_usd - 1.0).abs() < f64::EPSILON);
        assert!(task.by_model.contains_key("prior-month/model"));

        let summary = tracker
            .get_summary_filtered_at_period(None, period)
            .unwrap();
        assert_eq!(summary.by_model.len(), 1);
        assert!(summary.by_model.contains_key("today/model"));
        assert!(!summary.by_model.contains_key("earlier-month/model"));
        assert!(!summary.by_model.contains_key("prior-month/model"));
        assert_eq!(
            summary
                .by_agent
                .get("fixture-agent")
                .map(|stats| stats.request_count),
            Some(2),
            "per-agent aggregation retains current-month rows"
        );

        let filtered = tracker
            .get_summary_filtered_at_period(Some("fixture-agent"), period)
            .unwrap();
        assert!((filtered.daily_cost_usd - 1.0).abs() < f64::EPSILON);
        assert!((filtered.monthly_cost_usd - 3.0).abs() < f64::EPSILON);
        assert_eq!(filtered.by_model.len(), 1);
        assert!(filtered.by_model.contains_key("today/model"));
        assert!(!filtered.by_model.contains_key("earlier-month/model"));
        assert!(!filtered.by_model.contains_key("prior-month/model"));
    }

    #[test]
    fn current_month_model_stats_keep_earlier_month_unpriced_usage_visible() {
        let tmp = TempDir::new().unwrap();
        let storage_path = resolve_storage_path(tmp.path()).unwrap();
        let period = ReportingPeriod {
            day: NaiveDate::from_ymd_opt(2025, 6, 15).unwrap(),
            year: 2025,
            month: 6,
        };
        let month_start = period.day.with_day(1).unwrap();
        let prior_month = month_start - Duration::days(1);
        write_records(
            &storage_path,
            &[
                record_at(
                    "today/model",
                    1.0,
                    Utc.from_utc_datetime(&period.day.and_hms_opt(12, 0, 0).unwrap()),
                    None,
                ),
                unpriced_record_at(
                    "earlier-month/model",
                    150,
                    Utc.from_utc_datetime(&month_start.and_hms_opt(0, 0, 0).unwrap()),
                ),
                unpriced_record_at(
                    "prior-month/model",
                    75,
                    Utc.from_utc_datetime(&prior_month.and_hms_opt(23, 59, 59).unwrap()),
                ),
            ],
        );

        // A fresh tracker reloads the ledger from disk the same way the
        // status command does after a restart.
        let tracker = CostTracker::new(enabled_config(), tmp.path()).unwrap();

        let summary = tracker
            .get_summary_filtered_at_period(None, period)
            .unwrap();
        assert_eq!(
            summary.by_model.len(),
            1,
            "the daily by_model contract for other consumers is unchanged"
        );
        assert!(summary.by_model.contains_key("today/model"));
        assert!((summary.monthly_cost_usd - 1.0).abs() < f64::EPSILON);

        let month = tracker
            .get_current_month_model_stats_at_period(period)
            .unwrap();
        assert_eq!(month.len(), 2);
        assert_eq!(month["today/model"].unpriced_tokens, 0);
        assert!((month["today/model"].cost_usd - 1.0).abs() < f64::EPSILON);
        assert_eq!(
            month["earlier-month/model"].unpriced_tokens, 150,
            "earlier-this-month unpriced usage must stay visible after day rollover"
        );
        assert!(
            !month.contains_key("prior-month/model"),
            "previous-month rows are outside the monthly cap window"
        );
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

    #[test]
    fn shared_capped_derived_tracker_enforces_cap_over_shared_ledger() {
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();

        // The derived tracker shares the base's live config (the global
        // limits stay the base's) and tightens only the shared daily check
        // to the $0.50 per-hop cap.
        let derived = base.derived_shared_capped(0.5);
        assert_eq!(derived.session_id(), base.session_id());
        assert!(
            matches!(derived.check_budget(0.0).unwrap(), BudgetCheck::Allowed),
            "an empty ledger must pass the capped shared check"
        );

        // Spend recorded through the base tracker is immediately visible to
        // the derived tracker's budget check (shared storage, no stale fork).
        base.record_usage(TokenUsage::new(
            "test/model",
            2_000_000,
            0,
            0,
            3.0,
            3.0,
            0.0,
        ))
        .unwrap();
        assert!(
            matches!(
                derived.check_budget(0.0).unwrap(),
                BudgetCheck::Exceeded {
                    limit_usd,
                    agent_alias: None,
                    ..
                } if (limit_usd - 0.5).abs() < 1e-9
            ),
            "the derived tracker must see shared-ledger spend against its cap"
        );
        assert!(
            matches!(base.check_budget(0.0).unwrap(), BudgetCheck::Allowed),
            "the base tracker must still enforce its own (looser) limit"
        );

        // Spend recorded through the derived tracker lands on the same
        // durable ledger the base tracker reads.
        derived
            .record_usage(TokenUsage::new("test/model", 1000, 500, 0, 1.0, 2.0, 0.0))
            .unwrap();
        let day = Utc::now().date_naive();
        let daily = base.get_daily_cost(day).unwrap();
        assert!(
            daily > 0.0,
            "usage recorded through the derived tracker must reach the shared ledger"
        );
        assert!(
            (derived.get_daily_cost(day).unwrap() - daily).abs() < f64::EPSILON,
            "both trackers must read the same ledger file"
        );
    }

    #[test]
    fn derived_tracker_sees_base_config_reload() {
        // Base daily $10, derived for an agent with a $5 ceiling; $1 is
        // recorded through the derived tracker, then the base's global
        // daily limit is reloaded to $0.50. The derived tracker shares the
        // base's live config handle, so its next check must refuse on the
        // SHARED Day limit (agent_alias None), not the agent ceiling.
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 10.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();
        let derived = base.derived_for_agent("opus", 5.0);
        derived
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000_000, 0, 0, 1.0, 1.0, 0.0),
                Some("opus"),
            )
            .unwrap();
        assert!(
            matches!(derived.check_budget(0.1).unwrap(), BudgetCheck::Allowed),
            "under the original $10 shared limit the $1 spend must pass"
        );

        let mut reloaded = base.config();
        reloaded.daily_limit_usd = 0.5;
        base.update_config(reloaded);
        match derived.check_budget(0.1).unwrap() {
            BudgetCheck::Exceeded {
                current_usd,
                limit_usd,
                period,
                agent_alias,
            } => {
                assert!((current_usd - 1.0).abs() < 1e-9);
                assert!((limit_usd - 0.5).abs() < 1e-9);
                assert_eq!(period, UsagePeriod::Day);
                assert_eq!(
                    agent_alias, None,
                    "the reload must refuse through the shared limit, not the agent ceiling"
                );
            }
            other => panic!("expected shared-limit Exceeded after reload, got {other:?}"),
        }
    }

    #[test]
    fn shared_capped_derived_tracker_reads_live_global_limit() {
        // A $2 per-hop cap over a base global daily limit of $10: the
        // effective shared limit is the cap. Reload the base's global limit
        // down to $1 and the effective limit becomes the LIVE global $1,
        // not the cap: the capped scope reads the global limit from the
        // shared config handle on every check.
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: false,
                daily_limit_usd: 10.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();
        base.record_usage(TokenUsage::new("test/model", 800_000, 0, 0, 1.0, 1.0, 0.0))
            .unwrap();
        let derived = base.derived_shared_capped(2.0);
        assert!(
            matches!(derived.check_budget(0.5).unwrap(), BudgetCheck::Allowed),
            "ledger $0.80 against the effective min(10, 2) = $2 cap must pass"
        );

        let mut reloaded = base.config();
        reloaded.daily_limit_usd = 1.0;
        base.update_config(reloaded);
        match derived.check_budget(0.5).unwrap() {
            BudgetCheck::Exceeded {
                current_usd,
                limit_usd,
                agent_alias,
                ..
            } => {
                assert!((current_usd - 0.8).abs() < 1e-9);
                assert!(
                    (limit_usd - 1.0).abs() < 1e-9,
                    "the effective limit must be the live global $1, not the $2 cap"
                );
                assert_eq!(agent_alias, None);
            }
            other => panic!("expected Exceeded on the reloaded global limit, got {other:?}"),
        }
    }

    #[test]
    fn derived_tracker_keeps_enforcing_after_global_disable() {
        // A delegation that started scoped stays scoped: the derived
        // tracker froze `enabled` at derivation, so an operator disabling
        // cost tracking mid-run stops the GLOBAL tracker's checks but
        // not the running delegate's, and the delegate's records keep
        // landing on the ledger instead of vanishing from every ceiling.
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();
        let derived = base.derived_for_agent("target", 5.0);
        derived
            .record_usage_with_agent(
                TokenUsage::new("test/model", 6_000_000, 0, 0, 1.0, 1.0, 0.0),
                Some("target"),
            )
            .unwrap();

        let mut disabled = base.config();
        disabled.enabled = false;
        base.update_config(disabled);

        match derived.check_budget(0.0).unwrap() {
            BudgetCheck::Exceeded {
                current_usd,
                limit_usd,
                agent_alias,
                ..
            } => {
                assert!((current_usd - 6.0).abs() < 1e-9);
                assert!((limit_usd - 5.0).abs() < 1e-9);
                assert_eq!(
                    agent_alias.as_deref(),
                    Some("target"),
                    "the frozen mode must keep the agent ceiling enforcing"
                );
            }
            other => panic!("expected agent-scoped Exceeded after disable, got {other:?}"),
        }
        assert!(
            matches!(base.check_budget(0.0).unwrap(), BudgetCheck::Allowed),
            "the global tracker honors the live disabled flag"
        );

        // Recording through the derived tracker is gated on the frozen
        // mode too, so the delegation's later spend still lands under
        // its alias instead of disappearing.
        derived
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000_000, 0, 0, 1.0, 1.0, 0.0),
                Some("target"),
            )
            .unwrap();
        let daily = base.get_summary_for_agent("target").unwrap().daily_cost_usd;
        assert!(
            (daily - 7.0).abs() < 1e-9,
            "post-disable records must keep landing under the alias: {daily}"
        );
    }

    #[test]
    fn derived_from_frozen_base_inherits_frozen_mode() {
        // The mode a nested delegation runs under is fixed at the ROOT of
        // its tree: deriving from an already-frozen tracker propagates the
        // frozen pair instead of re-reading the live config, so an
        // operator reload between a parent's provider calls cannot flip
        // the mode the parent's descendants derive with. A derivation
        // from the still-live base keeps capturing the live pair, so
        // root behaviour is unchanged.
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();
        let derived_a = base.derived_for_agent("parent", 5.0);
        assert_eq!(derived_a.enforcement_flags(), (true, true));

        let mut reloaded = base.config();
        reloaded.enabled = false;
        reloaded.track_per_agent = false;
        base.update_config(reloaded);

        // The nested hop derives from the frozen parent, not from the
        // live config the parent's base still shares.
        let derived_b = derived_a.derived_for_agent("child", 8.0);
        assert_eq!(
            derived_b.enforcement_flags(),
            (true, true),
            "a frozen base must propagate its own pair to further derivations"
        );
        // A derivation straight from the live base still captures the
        // reloaded pair.
        let derived_c = base.derived_for_agent("other", 8.0);
        assert_eq!(
            derived_c.enforcement_flags(),
            (false, false),
            "a live base keeps capturing the live pair"
        );
    }

    #[test]
    fn derived_agent_scope_keeps_attribution_after_track_per_agent_disabled() {
        // Flipping `track_per_agent` off mid-run must not reattribute a
        // per-agent-scoped delegation: the child's rows keep their own
        // alias and keep counting into the ancestor's descendant total,
        // so the ancestor's ceiling still refuses once the subtree is
        // over it.
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();
        base.record_usage_with_agent(
            TokenUsage::new("test/model", 6_000_000, 0, 0, 1.0, 1.0, 0.0),
            Some("parent"),
        )
        .unwrap();
        let parent_scope = base.derived_for_agent("parent", 5.0);
        let child = base.derived_for_agent_in_chain(
            "child",
            8.0,
            parent_scope.subtree_chain_for_children(),
        );

        let mut reloaded = base.config();
        reloaded.track_per_agent = false;
        base.update_config(reloaded);

        child
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000_000, 0, 0, 1.0, 1.0, 0.0),
                Some("child"),
            )
            .unwrap();
        let child_daily = base.get_summary_for_agent("child").unwrap().daily_cost_usd;
        assert!(
            (child_daily - 1.0).abs() < 1e-9,
            "the frozen mode must keep attributing the child's rows: {child_daily}"
        );
        let parent_daily = base.get_summary_for_agent("parent").unwrap().daily_cost_usd;
        assert!(
            (parent_daily - 6.0).abs() < 1e-9,
            "the child's row must stay attributed to the child, not the parent: {parent_daily}"
        );

        // The ancestor entry counted the post-flip record: the parent's
        // own $6 plus the descendant's $1 exceeds the $5 ceiling, and
        // both the parent's scoped tracker and the child's (through the
        // inherited chain) refuse naming the ancestor.
        match parent_scope.check_budget(0.0).unwrap() {
            BudgetCheck::Exceeded {
                current_usd,
                agent_alias,
                ..
            } => {
                assert!(
                    (current_usd - 7.0).abs() < 1e-9,
                    "the ancestor's check must count the descendant's post-flip $1: {current_usd}"
                );
                assert_eq!(agent_alias.as_deref(), Some("parent"));
            }
            other => panic!("expected ancestor-scoped Exceeded after flip, got {other:?}"),
        }
        match child.check_budget(0.0).unwrap() {
            BudgetCheck::Exceeded {
                current_usd,
                agent_alias,
                ..
            } => {
                assert!(
                    (current_usd - 7.0).abs() < 1e-9,
                    "the child's check must see the ancestor subtree total: {current_usd}"
                );
                assert_eq!(agent_alias.as_deref(), Some("parent"));
            }
            other => panic!("expected ancestor-scoped Exceeded through the child, got {other:?}"),
        }
    }

    #[test]
    fn shared_capped_scope_stays_unattributed_after_track_per_agent_enabled() {
        // The SharedCapped degrade froze `track_per_agent = false` at
        // derivation: enabling per-agent attribution mid-run must not
        // start attributing this delegation's rows halfway through, so
        // the spend stays in the unattributed bucket of the shared
        // ledger.
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: false,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();
        let derived = base.derived_shared_capped(2.0);

        let mut reloaded = base.config();
        reloaded.track_per_agent = true;
        base.update_config(reloaded);

        derived
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000_000, 0, 0, 1.0, 1.0, 0.0),
                Some("x"),
            )
            .unwrap();
        let attributed = base.get_summary_for_agent("x").unwrap().daily_cost_usd;
        assert!(
            attributed.abs() < 1e-9,
            "the frozen SharedCapped mode must keep dropping the alias: {attributed}"
        );
        let day = Utc::now().date_naive();
        let daily = base.get_daily_cost(day).unwrap();
        assert!(
            (daily - 1.0).abs() < 1e-9,
            "the row must still land on the shared ledger, unattributed: {daily}"
        );
    }

    /// Fold the day's per-alias spend straight from the ledger file - the
    /// oracle `daily_cost_by_agent` must match after appends, forced cache
    /// rebuilds, and day rollovers.
    fn ledger_daily_by_agent(path: &Path) -> HashMap<String, f64> {
        let mut out: HashMap<String, f64> = HashMap::new();
        if !path.exists() {
            return out;
        }
        let file = File::open(path).unwrap();
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let record: CostRecord = serde_json::from_str(trimmed).unwrap();
            if record.usage.timestamp.naive_utc().date() == Utc::now().date_naive()
                && let Some(alias) = &record.agent_alias
            {
                *out.entry(alias.clone()).or_insert(0.0) += record.usage.cost_usd;
            }
        }
        out
    }

    #[test]
    fn daily_cost_by_agent_cache_tracks_appends_rebuilds_and_rollover() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();

        // (a) appends keep the per-alias day buckets in step with the ledger.
        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000_000, 0, 0, 3.0, 3.0, 0.0),
                Some("opus"),
            )
            .unwrap();
        tracker
            .record_usage_with_agent(
                TokenUsage::new("test/model", 500_000, 0, 0, 3.0, 3.0, 0.0),
                Some("sonnet"),
            )
            .unwrap();
        // Unattributed spend counts toward the shared total only.
        tracker
            .record_usage(TokenUsage::new("test/model", 250_000, 0, 0, 3.0, 3.0, 0.0))
            .unwrap();

        let mut storage = tracker.lock_storage();
        storage.ensure_period_cache_current().unwrap();
        assert_eq!(
            storage.daily_cost_by_agent,
            ledger_daily_by_agent(&storage.path),
            "per-alias day buckets must match a from-scratch ledger fold after appends"
        );
        drop(storage);

        // (b) a forced cache rebuild produces the same map.
        tracker
            .lock_storage()
            .rebuild_aggregates(
                Utc::now().date_naive(),
                Utc::now().year(),
                Utc::now().month(),
            )
            .unwrap();
        let storage = tracker.lock_storage();
        assert_eq!(
            storage.daily_cost_by_agent,
            ledger_daily_by_agent(&storage.path),
            "per-alias day buckets must match a from-scratch ledger fold after a rebuild"
        );
        assert!(
            storage.daily_cost_by_agent.contains_key("sonnet"),
            "both attributed aliases must appear after the rebuild"
        );
        drop(storage);

        // (c) a simulated day rollover clears the per-alias buckets with the
        // rest of the day cache.
        tracker
            .lock_storage()
            .rebuild_aggregates(
                Utc::now().date_naive() + Duration::days(1),
                Utc::now().year(),
                Utc::now().month(),
            )
            .unwrap();
        let storage = tracker.lock_storage();
        assert!(
            storage.daily_cost_by_agent.is_empty(),
            "day rollover must clear the per-alias day buckets"
        );
        assert!(
            storage.daily_cost_usd == 0.0,
            "day rollover clears the day total"
        );
    }

    #[test]
    fn agent_scoped_derived_tracker_enforces_alias_own_spend() {
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();

        // opus spent $6 today; the shared daily total is the same $6.
        base.record_usage_with_agent(
            TokenUsage::new("test/model", 6_000_000, 0, 0, 1.0, 1.0, 0.0),
            Some("opus"),
        )
        .unwrap();

        // opus's own $6 already trips a $5 ceiling...
        let opus_derived = base.derived_for_agent("opus", 5.0);
        let check = opus_derived.check_budget(0.0).unwrap();
        match check {
            BudgetCheck::Exceeded {
                current_usd,
                limit_usd,
                period,
                agent_alias,
            } => {
                assert!((current_usd - 6.0).abs() < 1e-9);
                assert!((limit_usd - 5.0).abs() < 1e-9);
                assert_eq!(period, UsagePeriod::Day);
                assert_eq!(agent_alias.as_deref(), Some("opus"));
            }
            other => panic!("expected agent-scoped Exceeded, got {other:?}"),
        }

        // ...but sonnet's own $0 does not, even though the shared total is $6.
        let sonnet_derived = base.derived_for_agent("sonnet", 5.0);
        assert!(
            matches!(
                sonnet_derived.check_budget(0.0).unwrap(),
                BudgetCheck::Allowed
            ),
            "the per-agent ceiling must compare the agent's OWN daily spend, \
             not the shared process-wide total"
        );
    }

    #[test]
    fn derived_for_agent_in_chain_checks_ancestor_ceiling() {
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();

        // The ancestor spent $6 of its own today under its own alias.
        base.record_usage_with_agent(
            TokenUsage::new("test/model", 6_000_000, 0, 0, 1.0, 1.0, 0.0),
            Some("parent"),
        )
        .unwrap();

        // The parent's own scoped tracker, then the child's tracker derived
        // with the parent's chain-for-children (own entry plus ancestors,
        // exactly as delegation plumbing threads it).
        let parent_scope = base.derived_for_agent("parent", 5.0);
        let chain = parent_scope.subtree_chain_for_children();
        let child = base.derived_for_agent_in_chain("child", 8.0, chain);

        // The child's own $0 passes its own $8 ceiling, but the ancestor's
        // own $6 already exceeds its $5 ceiling, so the child is refused
        // with the ANCESTOR named and the ancestor's limit.
        match child.check_budget(0.0).unwrap() {
            BudgetCheck::Exceeded {
                current_usd,
                limit_usd,
                period,
                agent_alias,
            } => {
                assert!((current_usd - 6.0).abs() < 1e-9);
                assert!((limit_usd - 5.0).abs() < 1e-9);
                assert_eq!(period, UsagePeriod::Day);
                assert_eq!(agent_alias.as_deref(), Some("parent"));
            }
            other => panic!("expected ancestor-scoped Exceeded, got {other:?}"),
        }

        // A descendant's $1 recorded through the child accumulates into the
        // ancestor entry while staying attributed to the child's own alias.
        child
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000_000, 0, 0, 1.0, 1.0, 0.0),
                Some("child"),
            )
            .unwrap();
        match child.check_budget(0.0).unwrap() {
            BudgetCheck::Exceeded { current_usd, .. } => {
                assert!(
                    (current_usd - 7.0).abs() < 1e-9,
                    "ancestor current must count its own $6 plus the \
                     descendant's $1: {current_usd}"
                );
            }
            other => panic!("expected ancestor-scoped Exceeded, got {other:?}"),
        }

        // Attribution stays per alias: the ancestor's own ledger total is
        // still $6, and the child's own total is $1.
        let parent_daily = base.get_summary_for_agent("parent").unwrap().daily_cost_usd;
        assert!((parent_daily - 6.0).abs() < 1e-9);
        let child_daily = base.get_summary_for_agent("child").unwrap().daily_cost_usd;
        assert!((child_daily - 1.0).abs() < 1e-9);

        // With headroom everywhere the chain admits: a fresh ancestor
        // ledger at $1 against a $5 ceiling plus a $1 descendant is fine.
        let tmp2 = TempDir::new().unwrap();
        let base2 = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp2.path(),
        )
        .unwrap();
        base2
            .record_usage_with_agent(
                TokenUsage::new("test/model", 1_000_000, 0, 0, 1.0, 1.0, 0.0),
                Some("parent"),
            )
            .unwrap();
        let parent_scope2 = base2.derived_for_agent("parent", 5.0);
        let child2 = base2.derived_for_agent_in_chain(
            "child",
            8.0,
            parent_scope2.subtree_chain_for_children(),
        );
        assert!(
            matches!(child2.check_budget(0.0).unwrap(), BudgetCheck::Allowed),
            "headroom on every ceiling must admit the chained tracker"
        );
    }

    #[test]
    fn subtree_descendant_spend_resets_on_utc_day_rollover() {
        // A descendant record counts against the ancestor's ceiling only
        // on the UTC day the record was stamped with: the accumulator
        // must roll over with the ledger's daily totals, so a chain alive
        // across UTC midnight does not carry yesterday's descendant spend
        // into today's ceiling check.
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();

        let day_d = NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
        let day_d1 = NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        let period = |day: NaiveDate| ReportingPeriod {
            day,
            year: day.year(),
            month: day.month(),
        };
        let usage_on = |cost_usd: f64, day: NaiveDate| {
            usage_costing_at(
                cost_usd,
                Utc.from_utc_datetime(&day.and_hms_opt(12, 0, 0).unwrap()),
            )
        };

        let parent_scope = base.derived_for_agent("parent", 1.0);
        let child = base.derived_for_agent_in_chain(
            "child",
            10.0,
            parent_scope.subtree_chain_for_children(),
        );

        // $0.80 of descendant spend recorded through the child on day D.
        child
            .record_usage_with_agent(usage_on(0.80, day_d), Some("child"))
            .unwrap();

        // Same-day accounting is intact: the ancestor's day-D check counts
        // the descendant's $0.80 against its $1.00 ceiling.
        match parent_scope
            .check_budget_at_period(0.30, period(day_d))
            .unwrap()
        {
            BudgetCheck::Exceeded {
                current_usd,
                agent_alias,
                ..
            } => {
                assert!(
                    (current_usd - 0.80).abs() < 1e-9,
                    "day D must count the descendant's $0.80: {current_usd}"
                );
                assert_eq!(agent_alias.as_deref(), Some("parent"));
            }
            other => panic!("expected ancestor-scoped Exceeded on day D, got {other:?}"),
        }

        // Day D+1 starts from zero: the same check one day later must not
        // carry day D's descendant spend into the fresh day's ceiling.
        assert!(
            matches!(
                parent_scope
                    .check_budget_at_period(0.30, period(day_d1))
                    .unwrap(),
                BudgetCheck::Allowed
            ),
            "day D+1 must not count day D's descendant spend against the ancestor ceiling"
        );

        // The rollover is a reset-then-add, not a read-side zero: new
        // descendant spend on day D+1 lands in the fresh slot, so the
        // ancestor's day-D+1 ceiling sees only day D+1's $0.10.
        child
            .record_usage_with_agent(usage_on(0.10, day_d1), Some("child"))
            .unwrap();
        match parent_scope
            .check_budget_at_period(0.95, period(day_d1))
            .unwrap()
        {
            BudgetCheck::Exceeded {
                current_usd,
                agent_alias,
                ..
            } => {
                assert!(
                    (current_usd - 0.10).abs() < 1e-9,
                    "day D+1 must count only day D+1's descendant $0.10, not the \
                     carried-over $0.90: {current_usd}"
                );
                assert_eq!(agent_alias.as_deref(), Some("parent"));
            }
            other => panic!("expected ancestor-scoped Exceeded on day D+1, got {other:?}"),
        }
    }

    #[test]
    fn subtree_descendant_spend_same_day_accumulates() {
        // Two records on the same UTC day through the same chain add into
        // one day total: the ancestor's check on that day sees the sum.
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();

        let day = NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
        let period = ReportingPeriod {
            day,
            year: day.year(),
            month: day.month(),
        };

        let parent_scope = base.derived_for_agent("parent", 2.0);
        let child = base.derived_for_agent_in_chain(
            "child",
            10.0,
            parent_scope.subtree_chain_for_children(),
        );
        child
            .record_usage_with_agent(
                usage_costing_at(
                    0.80,
                    Utc.from_utc_datetime(&day.and_hms_opt(12, 0, 0).unwrap()),
                ),
                Some("child"),
            )
            .unwrap();
        child
            .record_usage_with_agent(
                usage_costing_at(
                    0.70,
                    Utc.from_utc_datetime(&day.and_hms_opt(13, 0, 0).unwrap()),
                ),
                Some("child"),
            )
            .unwrap();

        // $0.80 plus $0.70 of same-day descendant spend with the $0.60
        // estimate exceeds the $2.00 ceiling, and the refusal's current is
        // the full same-day sum.
        match parent_scope.check_budget_at_period(0.60, period).unwrap() {
            BudgetCheck::Exceeded {
                current_usd,
                agent_alias,
                ..
            } => {
                assert!(
                    (current_usd - 1.50).abs() < 1e-9,
                    "same-day descendant records must add into one day total: {current_usd}"
                );
                assert_eq!(agent_alias.as_deref(), Some("parent"));
            }
            other => panic!("expected ancestor-scoped Exceeded, got {other:?}"),
        }
    }

    #[test]
    fn subtree_descendant_spend_ignores_record_from_earlier_day() {
        // A record stamped on an EARLIER day than the accumulator's
        // stored slot (a usage recorded just before UTC midnight,
        // persisted just after another record already opened the new day)
        // must not reset the slot back to the older day: the new day's
        // accumulated spend survives, and the older day reads zero
        // because its record was dropped from the accumulator (its
        // ledger row still lands on its own day).
        let tmp = TempDir::new().unwrap();
        let base = CostTracker::new(
            CostConfig {
                enabled: true,
                track_per_agent: true,
                daily_limit_usd: 100.0,
                monthly_limit_usd: 500.0,
                ..Default::default()
            },
            tmp.path(),
        )
        .unwrap();

        let day_d = NaiveDate::from_ymd_opt(2026, 9, 20).unwrap();
        let day_d1 = NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        let period = |day: NaiveDate| ReportingPeriod {
            day,
            year: day.year(),
            month: day.month(),
        };
        let usage_on = |cost_usd: f64, day: NaiveDate| {
            usage_costing_at(
                cost_usd,
                Utc.from_utc_datetime(&day.and_hms_opt(12, 0, 0).unwrap()),
            )
        };

        let parent_scope = base.derived_for_agent("parent", 1.0);
        let child = base.derived_for_agent_in_chain(
            "child",
            10.0,
            parent_scope.subtree_chain_for_children(),
        );

        // Day D+1's slot opens with $0.50 of descendant spend, then a
        // day-D record arrives late (out-of-order persistence around UTC
        // midnight) with $0.80.
        child
            .record_usage_with_agent(usage_on(0.50, day_d1), Some("child"))
            .unwrap();
        child
            .record_usage_with_agent(usage_on(0.80, day_d), Some("child"))
            .unwrap();

        // Day D+1's total is unchanged: the ancestor's day-D+1 check
        // still counts exactly the $0.50 that landed on day D+1, so the
        // $0.60 estimate is refused with the $0.50 current.
        match parent_scope
            .check_budget_at_period(0.60, period(day_d1))
            .unwrap()
        {
            BudgetCheck::Exceeded {
                current_usd,
                agent_alias,
                ..
            } => {
                assert!(
                    (current_usd - 0.50).abs() < 1e-9,
                    "day D+1 must keep its own $0.50 total: {current_usd}"
                );
                assert_eq!(agent_alias.as_deref(), Some("parent"));
            }
            other => panic!("expected ancestor-scoped Exceeded on day D+1, got {other:?}"),
        }

        // Day D reads zero: the late day-D record was dropped from the
        // accumulator, so the same estimate passes the ancestor's day-D
        // check instead of seeing the stale $0.80.
        assert!(
            matches!(
                parent_scope
                    .check_budget_at_period(0.60, period(day_d))
                    .unwrap(),
                BudgetCheck::Allowed
            ),
            "day D must read zero descendant spend: the older record was dropped"
        );
    }
}
