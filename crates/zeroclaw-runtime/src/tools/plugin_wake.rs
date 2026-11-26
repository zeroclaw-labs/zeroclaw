//! Host-mediated agent wake dispatch for plugin tools.
//!
//! This is the runtime half of the tool world's `dispatch.wake` import. The
//! guest boundary in `zeroclaw-plugins` has already refused an unauthorized
//! frame, an ungranted instance, and a malformed target or prompt before a call
//! reaches here; what is left is the part only canonical host state can answer:
//!
//! - **Reach.** The target must be named by the operator's
//!   `plugins.entries[].dispatch_agents` for *this* instance key. That list is
//!   read from the live config view at call time, so an operator edit takes
//!   effect without re-instantiating the guest or rebuilding the registry.
//! - **Budget.** Each instance spends a continuous-refill token bucket at
//!   `dispatch_rate_per_min`, so a plugin cannot turn one tool call into an
//!   unbounded stream of agent turns.
//! - **Depth.** A woken agent's turn (and any detached child work it spawns,
//!   such as `send_message_to_peer` or background delegation) can itself run
//!   plugin tools that dispatch. [`MAX_PLUGIN_WAKE_DEPTH`] caps that recursion,
//!   carried in `zeroclaw_spawn`'s task-local so detached child tasks inherit
//!   it automatically while unrelated turns stay at zero.
//! - **Coalescing.** A wake targeting an agent already running a plugin turn
//!   does not start a second turn. It replaces that agent's pending wake, so
//!   five plugin notifications in a burst cost one extra turn rather than five,
//!   and the follow-up turn attributes to the *latest* caller.
//!
//! Every check runs on state the guest cannot influence. `caller_alias` is the
//! host-owned alias of the agent whose turn is executing the tool, and the
//! instance scope is host-issued; only the target and prompt cross from the
//! guest, and both were shape-validated before this module saw them.

use crate::agent::cost::{
    TOOL_LOOP_COST_TRACKING_CONTEXT, TOOL_LOOP_TURN_USAGE, ToolLoopCostTrackingContext, TurnUsage,
    tool_loop_cost_tracking_context_for_agent,
};
use crate::composition::CapabilitySlot;
use crate::control_plane::{
    ControlPlaneHandle, TaskKind, TaskRecord, TaskRegistry, TaskStatus,
    control_plane as global_control_plane,
};
use crate::live_config_authority::{
    AgentExecutionAdmission, AgentExecutionCapability, AgentExecutionError,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use futures_util::FutureExt;
use parking_lot::Mutex;
use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use zeroclaw_api::ingress::{InternalPrincipal, TurnOrigin};
use zeroclaw_config::schema::Config;
use zeroclaw_plugins::PluginPermission;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::{AgentWakeBackend, AgentWakeError};

/// Wake budget for an instance whose `dispatch_rate_per_min` is unset.
const DEFAULT_DISPATCH_RATE_PER_MIN: u32 = 30;

/// How many plugin-initiated hops a wake chain may take.
///
/// A turn started by `dispatch.wake` runs at depth 1, a wake it issues starts a
/// turn at depth 2, and a wake issued there is refused. Two hops is enough for a
/// watcher to hand work to an agent that consults a second one, and small enough
/// that a mutually recursive pair of plugins cannot spin.
const MAX_PLUGIN_WAKE_DEPTH: u32 = 2;

/// First delay before retrying a durable task-store write (registration or
/// settlement); doubles per consecutive failure up to
/// [`WAKE_STORE_MAX_RETRY_DELAY`].
const WAKE_STORE_RETRY_DELAY: Duration = Duration::from_millis(25);
const WAKE_STORE_MAX_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Backoff before the next attempt after `failures` consecutive store failures.
fn wake_store_retry_delay(failures: u32) -> Duration {
    let multiplier = 1_u32 << failures.saturating_sub(1).min(8);
    WAKE_STORE_RETRY_DELAY
        .saturating_mul(multiplier)
        .min(WAKE_STORE_MAX_RETRY_DELAY)
}

/// One coalesced wake that has not started yet.
///
/// It carries everything the follow-up turn needs, including its own hop depth,
/// caller attribution and inherited capabilities, so a coalesced turn is
/// attributed to the caller that most recently replaced it rather than to
/// whoever happened to win the slot.
struct PendingWake {
    prompt: String,
    depth: u32,
    caller_alias: Option<String>,
    capabilities: Option<crate::composition::BoundCapabilities>,
    execution_capability: Option<AgentExecutionCapability>,
    /// The target's lifecycle generation when this wake was accepted. The
    /// follow-up is admitted at exactly this generation, so a target deleted
    /// (and possibly replaced under the same alias) after acceptance cancels
    /// the wake instead of handing its prompt to the replacement. `None` when
    /// there is no execution capability, and so no lifecycle to pin.
    target_generation: Option<u64>,
    /// The headless SOP step this wake's caller ran under, restored around the
    /// turn it queues so the target cannot escape that step's tool scope.
    step_scope: Option<crate::sop::active_scope::HeadlessStepScope>,
}

/// Whether an `(instance, agent)` pair is currently held by a plugin wake.
///
/// Absence from the slot map *is* the idle state: a pair with no entry is
/// wakeable, and a wake claims it by inserting `Starting`. Keeping only the held
/// states means a released slot can never be mistaken for a claim that was never
/// taken, and the guards' `remove` is the single release path.
enum WakeSlotState {
    /// A wake claimed the pair and is admitting and registering its turn; no
    /// worker owns the slot yet, and the claim can still fail. A later wake
    /// must not be accepted here: a failed claim releases the slot, and a wake
    /// stored against it would be reported as accepted and never run. It
    /// subscribes to `gate` instead and retries once the claimant settles.
    /// Every transition out of this state drops the sender (replaced by
    /// `Running`, or removed on failure), which wakes all subscribers.
    Starting {
        gate: tokio::sync::watch::Sender<()>,
    },
    /// A worker owns the pair and runs its turns. A later wake for the same
    /// pair lands in `pending` instead of starting a second turn; the worker
    /// drains it, so a pending wake is never dropped by a failed setup.
    Running { pending: Option<Box<PendingWake>> },
}

/// Continuous-refill token bucket for one plugin instance.
struct TokenBucket {
    tokens: f64,
    rate_per_min: f64,
    last_refill: tokio::time::Instant,
}

impl TokenBucket {
    fn new(rate_per_min: u32) -> Self {
        let rate = f64::from(rate_per_min);
        Self {
            // A fresh instance may spend its whole minute's budget at once: the
            // ceiling is on sustained rate, not on the first burst.
            tokens: rate,
            rate_per_min: rate,
            last_refill: tokio::time::Instant::now(),
        }
    }

    fn refill(&mut self, now: tokio::time::Instant) {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        if elapsed <= 0.0 {
            return;
        }
        self.tokens = (self.tokens + elapsed * self.rate_per_min / 60.0).min(self.rate_per_min);
        self.last_refill = now;
    }

    /// Adopt a new configured rate without either crediting or confiscating
    /// more than the new ceiling allows.
    fn retune(&mut self, rate_per_min: u32, now: tokio::time::Instant) {
        self.refill(now);
        self.rate_per_min = f64::from(rate_per_min);
        self.tokens = self.tokens.min(self.rate_per_min);
    }

    fn try_consume(&mut self, now: tokio::time::Instant) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Process-wide wake coordination shared by every agent registry.
///
/// Registries are rebuilt per agent, per channel message and per delegate, so a
/// coordinator owned by one registry could not coalesce a wake issued from
/// agent A's registry against a turn started by agent B's. The slot and bucket
/// maps live here instead, keyed by canonical identity, and [`Self::shared`]
/// hands every registry the same one.
pub(crate) struct SharedWakeCoordinator {
    slots: Mutex<HashMap<(String, String), WakeSlotState>>,
    buckets: Mutex<HashMap<String, TokenBucket>>,
}

impl SharedWakeCoordinator {
    /// A coordinator owned by one caller, for tests and isolated fixtures.
    pub(crate) fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// The process-wide coordinator every registry in this process shares.
    pub(crate) fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<SharedWakeCoordinator>> = OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| Arc::new(Self::new())))
    }

    /// Spend one wake from `instance_key`'s budget at `rate_per_min`.
    fn try_consume_token(&self, instance_key: &str, rate_per_min: u32) -> bool {
        let now = tokio::time::Instant::now();
        let mut buckets = self.buckets.lock();
        let bucket = buckets
            .entry(instance_key.to_string())
            .or_insert_with(|| TokenBucket::new(rate_per_min));
        if bucket.rate_per_min != f64::from(rate_per_min) {
            bucket.retune(rate_per_min, now);
        }
        bucket.try_consume(now)
    }

    /// How many `(instance, agent)` pairs currently hold a slot.
    #[cfg(test)]
    fn slot_count(&self) -> usize {
        self.slots.lock().len()
    }
}

/// Clears a claimed slot unless the holder has taken ownership of it.
///
/// The pre-spawn use covers every early return between claiming the slot and
/// handing it to the spawned turn; the spawned use covers a task aborted or
/// dropped before its clean exit. Both leave the pair absent rather than wedged
/// in `Running`, so a later wake is admitted instead of coalescing forever into
/// a turn that will never read its pending wake.
struct SlotGuard {
    coordinator: Arc<SharedWakeCoordinator>,
    key: (String, String),
    armed: bool,
}

impl SlotGuard {
    fn new(coordinator: Arc<SharedWakeCoordinator>, key: (String, String)) -> Self {
        Self {
            coordinator,
            key,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if self.armed {
            self.coordinator.slots.lock().remove(&self.key);
        }
    }
}

/// The runtime's [`AgentWakeBackend`].
///
/// Holds no per-instance state of its own: grants, budgets and targets are
/// resolved from canonical config under the host-issued scope at call time, so a
/// registry built before an operator's edit still honours the edit.
pub(crate) struct RuntimeAgentWakeBackend {
    /// Config to read when no live handle was supplied (one-shot / non-daemon
    /// callers). Mutually exclusive with `live_config`.
    fallback_config: Option<Arc<Config>>,
    live_config: Option<zeroclaw_config::live::LiveConfigHandle>,
    /// Authority-owned admission for the woken agent's turn. `None` for callers
    /// without managed lifecycle state, which run without a generation lease.
    execution_capability: Option<AgentExecutionCapability>,
    /// The capabilities and principal the waking turn was built from. When
    /// bound, the woken turn runs through them, so it uses the same sources and
    /// resolves for the same principal as the turn that triggered it.
    capabilities: CapabilitySlot,
    task_control_plane: Option<ControlPlaneHandle>,
    coordinator: Arc<SharedWakeCoordinator>,
}

impl RuntimeAgentWakeBackend {
    pub(crate) fn new(
        fallback_config: Option<Arc<Config>>,
        live_config: Option<zeroclaw_config::live::LiveConfigHandle>,
        execution_capability: Option<AgentExecutionCapability>,
    ) -> Self {
        Self {
            fallback_config,
            live_config,
            execution_capability,
            capabilities: Arc::new(std::sync::OnceLock::new()),
            task_control_plane: None,
            coordinator: SharedWakeCoordinator::shared(),
        }
    }

    /// A backend over an explicit coordinator and control plane, for fixtures
    /// that need isolated slots and a deterministic task store.
    #[cfg(test)]
    fn for_tests(
        config: Arc<Config>,
        live_config: Option<zeroclaw_config::live::LiveConfigHandle>,
        execution_capability: Option<AgentExecutionCapability>,
        task_control_plane: Option<ControlPlaneHandle>,
        coordinator: Arc<SharedWakeCoordinator>,
    ) -> Self {
        Self {
            fallback_config: Some(config),
            live_config,
            execution_capability,
            capabilities: Arc::new(std::sync::OnceLock::new()),
            task_control_plane,
            coordinator,
        }
    }

    /// The slot the owning registry binds, exactly as the peer and delegate
    /// tools expose theirs.
    pub(crate) fn capabilities_slot(&self) -> CapabilitySlot {
        Arc::clone(&self.capabilities)
    }

    /// The config view this call resolves grants and targets from.
    fn config_snapshot(&self) -> Result<Arc<Config>, AgentWakeError> {
        match self.live_config.as_ref() {
            Some(handle) => Ok(Arc::new(handle.read().clone())),
            None => self
                .fallback_config
                .as_ref()
                .map(Arc::clone)
                .ok_or(AgentWakeError::Unavailable),
        }
    }

    /// Resolve this instance's live grant for `target_agent`.
    ///
    /// Read from canonical config on every call, and again after a wake waits
    /// for another claim to settle, so an operator edit made in between
    /// applies before the wake is admitted.
    fn resolve_wake_grant(
        &self,
        instance_key: &str,
        target_agent: &str,
    ) -> Result<WakeGrant, AgentWakeError> {
        let config_snapshot = self.config_snapshot()?;
        let (canonical, rate_limit) = wake_target_in(&config_snapshot, instance_key, target_agent)?;
        Ok(WakeGrant {
            config_snapshot,
            canonical,
            rate_limit,
        })
    }
}

/// Judge `instance_key`'s grant for `target_agent` against one config view.
///
/// Returns the target's canonical alias and the instance's wake rate. This is
/// the single grant-and-target check: it runs on the snapshot a wake is
/// accepted against, and again on the config a turn is admitted with, so a
/// publication landing between the two reads cannot start a turn the newer
/// config no longer permits.
fn wake_target_in(
    config: &Config,
    instance_key: &str,
    target_agent: &str,
) -> Result<(String, u32), AgentWakeError> {
    // No row means no operator grant, which is the same answer as an empty
    // `dispatch_agents`: this instance may wake nobody.
    let Some(entry) = config
        .plugins
        .entries
        .iter()
        .find(|entry| entry.name == instance_key)
    else {
        return Err(AgentWakeError::AccessDenied);
    };

    // `component.rs` already refuses padded targets, so this trim is a
    // normalization rather than a second grammar.
    let trimmed = target_agent.trim();
    if !entry
        .dispatch_agents
        .iter()
        .any(|allowed| allowed == "*" || allowed.eq_ignore_ascii_case(trimmed))
    {
        return Err(AgentWakeError::AccessDenied);
    }
    let rate_limit = entry
        .dispatch_rate_per_min
        .unwrap_or(DEFAULT_DISPATCH_RATE_PER_MIN);

    let canonical = config
        .agents
        .iter()
        .find(|(alias, agent)| agent.enabled && alias.eq_ignore_ascii_case(trimmed))
        .map(|(alias, _)| alias.clone())
        .ok_or(AgentWakeError::UnknownAgent)?;
    Ok((canonical, rate_limit))
}

/// Confirm that the config a turn was admitted with still permits it.
///
/// Admission reads config after the grant was resolved, so it can supply a
/// newer view. The turn runs on that view, so the grant and the target's
/// enabled flag are judged on it before anything is registered.
fn admitted_wake_target_permitted(
    cfg: &Config,
    instance_key: &str,
    canonical: &str,
) -> Result<(), AgentWakeError> {
    let (admitted, _) = wake_target_in(cfg, instance_key, canonical)?;
    if admitted == canonical {
        Ok(())
    } else {
        Err(AgentWakeError::UnknownAgent)
    }
}

/// One resolution of an instance's live grant for a target.
struct WakeGrant {
    config_snapshot: Arc<Config>,
    canonical: String,
    rate_limit: u32,
}

#[async_trait]
impl AgentWakeBackend for RuntimeAgentWakeBackend {
    async fn wake(
        &self,
        scope: &PluginInstanceScope,
        caller_alias: Option<&str>,
        target_agent: &str,
        prompt: &str,
    ) -> Result<(), AgentWakeError> {
        if !scope.grants().allows(PluginPermission::AgentDispatch) {
            return Err(AgentWakeError::AccessDenied);
        }

        let depth = zeroclaw_spawn::current_plugin_wake_depth();
        if depth >= MAX_PLUGIN_WAKE_DEPTH {
            return Err(AgentWakeError::AccessDenied);
        }

        let instance_key = scope
            .id()
            .config_entry_key()
            .map_err(|_| AgentWakeError::Unavailable)?;
        let mut grant = self.resolve_wake_grant(&instance_key, target_agent)?;
        // Captured here, on the caller's task: a detached spawn does not
        // inherit task-locals, so the woken turn would otherwise rebuild its
        // full tool surface and escape a headless SOP step's boundary.
        let step_scope = crate::sop::active_scope::active_headless_step_scope();
        let mut pending = Some(Box::new(PendingWake {
            prompt: prompt.to_string(),
            depth,
            caller_alias: caller_alias.map(str::to_string),
            capabilities: self.capabilities.get().cloned(),
            execution_capability: self.execution_capability.clone(),
            // Stamped when the wake is actually accepted onto a running slot.
            target_generation: None,
            step_scope: step_scope.clone(),
        }));

        // Claim the slot and reserve each turn's budget under one lock. The
        // first pending wake pays for its follow-up before we return success;
        // later overwrites share that reservation. A wake is accepted onto a
        // slot only once a worker owns it: a claim still in `Starting` can
        // fail and release the slot, so a wake meeting it waits and retries.
        let slot_key = loop {
            let slot_key = (instance_key.clone(), grant.canonical.clone());
            let mut claimant_settled = {
                let mut slots = self.coordinator.slots.lock();
                match slots.get_mut(&slot_key) {
                    Some(WakeSlotState::Running { pending: existing }) => {
                        if existing.is_none()
                            && !self
                                .coordinator
                                .try_consume_token(&instance_key, grant.rate_limit)
                        {
                            return Err(AgentWakeError::RateLimited);
                        }
                        // Pin the generation this wake is accepted against,
                        // read from the same canonical alias the slot is keyed
                        // by. A later delete advances it, so the retry cannot
                        // reach a replacement that reuses the alias.
                        if let Some(wake) = pending.as_mut() {
                            wake.target_generation =
                                self.execution_capability.as_ref().map(|capability| {
                                    capability.agent_lifecycle_generation(&grant.canonical)
                                });
                        }
                        *existing = pending.take();
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Dispatch
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Success)
                            .with_attrs(::serde_json::json!({
                                "plugin": scope.id().package(),
                                "agent": grant.canonical,
                                "depth": depth,
                            })),
                            "plugin wake coalesced onto the agent's running turn"
                        );
                        return Ok(());
                    }
                    // Subscribed under the lock, so the claimant cannot settle
                    // unobserved between this check and the wait below.
                    Some(WakeSlotState::Starting { gate }) => gate.subscribe(),
                    None => {
                        if !self
                            .coordinator
                            .try_consume_token(&instance_key, grant.rate_limit)
                        {
                            return Err(AgentWakeError::RateLimited);
                        }
                        let (gate, _) = tokio::sync::watch::channel(());
                        slots.insert(slot_key.clone(), WakeSlotState::Starting { gate });
                        break slot_key;
                    }
                }
            };
            // `Err` is the expected wake-up: the claimant dropped the gate by
            // handing the slot to its worker or releasing it.
            let _ = claimant_settled.changed().await;
            // The wait can outlast an operator edit. Resolve the grant again
            // from live config before this wake claims the slot or is queued:
            // a target revoked, disabled or removed meanwhile is refused here,
            // exactly as on a first call, and the budget follows the edit.
            grant = self.resolve_wake_grant(&instance_key, target_agent)?;
        };
        let WakeGrant {
            config_snapshot,
            canonical,
            ..
        } = grant;
        // From here every early return releases the slot again.
        let mut slot_guard = SlotGuard::new(Arc::clone(&self.coordinator), slot_key.clone());

        let admission = match self
            .execution_capability
            .as_ref()
            .map(|capability| capability.resolve_and_admit(&canonical))
            .transpose()
        {
            Ok(admission) => admission,
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": scope.id().package(),
                            "agent": canonical,
                            "error": format!("{error}"),
                        })),
                    "plugin wake target could not be admitted"
                );
                return Err(map_execution_error(&error));
            }
        };
        let cfg = admission
            .as_ref()
            .map(AgentExecutionAdmission::config)
            .unwrap_or_else(|| Arc::clone(&config_snapshot));
        // Admission may have read a newer publication than the grant above.
        // The turn runs on the admitted view, so judge it there too; a refusal
        // returns before any task is registered and the guard frees the slot.
        if let Err(error) = admitted_wake_target_permitted(&cfg, &instance_key, &canonical) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "plugin": scope.id().package(),
                        "agent": canonical,
                    })),
                "plugin wake refused: admitted config no longer grants an enabled target"
            );
            return Err(error);
        }
        let control_plane = match self
            .task_control_plane
            .clone()
            .or_else(|| global_control_plane().cloned())
        {
            Some(handle) => handle,
            None => match crate::control_plane::non_daemon_control_plane(&cfg.data_dir).await {
                Ok(handle) => handle,
                Err(error) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "plugin": scope.id().package(),
                                "agent": canonical,
                                "error": format!("{error:#}"),
                            })),
                        "plugin wake control plane unavailable"
                    );
                    return Err(AgentWakeError::Unavailable);
                }
            },
        };
        let task_store = Arc::clone(&control_plane.store);
        let control_plane_boot_id = control_plane.boot_id.clone();
        let package = scope.id().package().to_string();
        let next_depth = depth.saturating_add(1);
        let task = match admit_wake_task(
            Some(task_store.as_ref()),
            &control_plane_boot_id,
            &package,
            &canonical,
            caller_alias,
            next_depth,
        )
        .await
        {
            Ok(task) => task,
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": package,
                            "agent": canonical,
                            "error": format!("{error:#}"),
                        })),
                    "plugin wake durable registration failed"
                );
                return Err(AgentWakeError::Unavailable);
            }
        };
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Dispatch)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "task_id": &task.id,
                    "plugin": &package,
                    "agent": &canonical,
                    "depth": next_depth,
                })),
            "plugin wake accepted for in-process delivery"
        );

        let live_config = self.live_config.clone();
        let coordinator = Arc::clone(&self.coordinator);
        let spawn_key = slot_key.clone();
        let spawn_canonical = canonical.clone();
        let first_plan = WakeTurnPlan {
            cfg,
            admission,
            prompt: prompt.to_string(),
            depth: next_depth,
            caller_alias: caller_alias.map(str::to_string),
            capabilities: self.capabilities.get().cloned(),
            step_scope,
            task,
        };

        // Hand the slot to the worker before it exists. Replacing `Starting`
        // drops its gate, so waiting wakes retry and coalesce onto a slot whose
        // owner can no longer fail before draining them.
        self.coordinator
            .slots
            .lock()
            .insert(slot_key.clone(), WakeSlotState::Running { pending: None });
        let _handle = zeroclaw_spawn::spawn!(async move {
            let mut spawned_guard = SlotGuard::new(Arc::clone(&coordinator), spawn_key.clone());
            let mut plan = first_plan;
            loop {
                let turn_alias = spawn_canonical.clone();
                let turn_prompt = plan.prompt.clone();
                // Build from `&plan.cfg` per turn so a coalesced follow-up sees
                // updated cost-tracking and pricing config from its own
                // admitted snapshot rather than reusing the first turn's view.
                let cost_ctx = tool_loop_cost_tracking_context_for_agent(&plan.cfg, &turn_alias);
                let turn_usage = cost_ctx
                    .as_ref()
                    .map(|_| Arc::new(Mutex::new(TurnUsage::default())));
                let internal_principal = Some(InternalPrincipal::PluginTool {
                    package: package.clone(),
                    sender_alias: plan.caller_alias.clone(),
                });
                // Each turn runs through the capabilities its own caller bound,
                // so a coalesced follow-up never borrows the slot winner's
                // sources or principal.
                let turn: Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> =
                    match plan.capabilities.clone() {
                        Some(bound) => {
                            Box::pin(crate::agent::loop_::process_message_shared_with_capabilities(
                                Arc::clone(&plan.cfg),
                                live_config.clone(),
                                bound.capabilities,
                                bound.principal,
                                &turn_alias,
                                &turn_prompt,
                                None,
                                TurnOrigin::AgentDirect,
                                plan.admission.clone(),
                                internal_principal,
                            ))
                        }
                        None => Box::pin(
                            crate::agent::loop_::process_message_shared_with_live_config_and_admission_and_principal(
                                Arc::clone(&plan.cfg),
                                live_config.clone(),
                                &turn_alias,
                                &turn_prompt,
                                None,
                                TurnOrigin::AgentDirect,
                                plan.admission.clone(),
                                internal_principal,
                            ),
                        ),
                    };
                let result = crate::sop::active_scope::with_inherited_headless_step_scope(
                    plan.step_scope.clone(),
                    zeroclaw_spawn::scope_plugin_wake_depth(
                        plan.depth,
                        run_wake_turn_with_panic_recovery(cost_ctx.clone(), turn_usage, turn),
                    ),
                )
                .await;
                // The turn is over, so release its lifecycle lease before
                // anything that can wait on the store. Settlement retries a
                // failed terminal write indefinitely, and the drain below can
                // wait out registration retries; a lease held across either
                // would count a finished turn as active and block deleting an
                // otherwise idle target or draining the generation. A
                // follow-up takes its own admission; clones still owned by
                // detached work the turn spawned keep the shared lease alive
                // on their own.
                drop(plan.admission.take());
                settle_wake_task(&task_store, &plan.task, &result).await;

                // Drain wakes that coalesced while this turn ran until one is
                // ready to run. A follow-up the operator no longer permits
                // (grant revoked, target disabled or removed, re-admission
                // refused) is cancelled, never the slot: wakes that coalesced
                // while it was being prepared stay pending and are drained
                // next. A follow-up whose durable registration fails is not
                // cancelled: its caller was already told the wake was accepted,
                // so it goes back to `pending` and is retried after a backoff,
                // re-passing every live check above first. The slot is
                // released only here, under the lock, when nothing is pending,
                // so no accepted wake is dropped with it.
                let mut registration_failures = 0_u32;
                plan = 'drain: loop {
                    let next = {
                        let mut slots = coordinator.slots.lock();
                        match slots.get_mut(&spawn_key) {
                            Some(WakeSlotState::Running { pending }) => {
                                let next = pending.take();
                                if next.is_none() {
                                    slots.remove(&spawn_key);
                                }
                                next
                            }
                            // Only this worker releases its slot, so anything
                            // else here is not ours to touch.
                            Some(WakeSlotState::Starting { .. }) | None => None,
                        }
                    };
                    let Some(next) = next else {
                        spawned_guard.disarm();
                        return;
                    };
                    let next = *next;

                    // Admit first, at the generation this wake was accepted
                    // against: a target deleted since then is stale even if a
                    // replacement now holds its alias. Then judge the grant and
                    // the target's enabled flag on the config the turn will
                    // actually run with, so a publication between reads can
                    // neither revoke nor disable the target unnoticed.
                    let admission = match next
                        .execution_capability
                        .as_ref()
                        .map(|capability| match next.target_generation {
                            Some(generation) => capability.admit_at(&spawn_canonical, generation),
                            None => capability.resolve_and_admit(&spawn_canonical),
                        })
                        .transpose()
                    {
                        Ok(admission) => admission,
                        Err(error) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Reject
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({
                                    "plugin": package,
                                    "agent": spawn_canonical,
                                    "error": format!("{error}"),
                                })),
                                "coalesced plugin wake target could not be re-admitted"
                            );
                            continue 'drain;
                        }
                    };
                    let cfg = match admission.as_ref() {
                        Some(admission) => admission.config(),
                        None => match live_config.as_ref() {
                            Some(handle) => Arc::new(handle.read().clone()),
                            None => Arc::clone(&plan.cfg),
                        },
                    };
                    if let Err(error) =
                        admitted_wake_target_permitted(&cfg, &spawn_key.0, &spawn_canonical)
                    {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Reject
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "plugin": package,
                                "agent": spawn_canonical,
                                "error": format!("{error:?}"),
                            })),
                            "coalesced plugin wake cancelled: target no longer granted or enabled"
                        );
                        continue 'drain;
                    }
                    let next_depth = next.depth.saturating_add(1);
                    let task = match admit_wake_task(
                        Some(task_store.as_ref()),
                        &control_plane_boot_id,
                        &package,
                        &spawn_canonical,
                        next.caller_alias.as_deref(),
                        next_depth,
                    )
                    .await
                    {
                        Ok(task) => task,
                        Err(error) => {
                            registration_failures = registration_failures.saturating_add(1);
                            if registration_failures == 1 || registration_failures.is_power_of_two()
                            {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Reject
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "plugin": package,
                                            "agent": spawn_canonical,
                                            "attempt": registration_failures,
                                            "error": format!("{error:#}"),
                                        })
                                    ),
                                    "coalesced plugin wake registration failed; will retry"
                                );
                            }
                            // Release any generation lease before waiting so
                            // the retry cannot hold up a reload.
                            drop(admission);
                            // Requeue under the lock. A newer wake that
                            // coalesced meanwhile already replaced this one,
                            // exactly as it would have had this one still been
                            // pending, and shares its reservation.
                            if let Some(WakeSlotState::Running { pending }) =
                                coordinator.slots.lock().get_mut(&spawn_key)
                                && pending.is_none()
                            {
                                *pending = Some(Box::new(next));
                            }
                            tokio::time::sleep(wake_store_retry_delay(registration_failures)).await;
                            continue 'drain;
                        }
                    };
                    break 'drain WakeTurnPlan {
                        cfg,
                        admission,
                        prompt: next.prompt,
                        depth: next_depth,
                        caller_alias: next.caller_alias,
                        capabilities: next.capabilities,
                        step_scope: next.step_scope,
                        task,
                    };
                };
            }
        });
        // The spawned task owns the slot now; its own guard releases it.
        slot_guard.disarm();
        Ok(())
    }
}

/// One turn's mutable inputs inside the spawned wake loop.
struct WakeTurnPlan {
    cfg: Arc<Config>,
    admission: Option<AgentExecutionAdmission>,
    prompt: String,
    depth: u32,
    caller_alias: Option<String>,
    capabilities: Option<crate::composition::BoundCapabilities>,
    step_scope: Option<crate::sop::active_scope::HeadlessStepScope>,
    task: WakeTask,
}

/// Map the authority's admission failure onto the closed guest-facing set.
///
/// The guest learns a category, never why: "the agent is being deleted" and "the
/// generation is closing" are both `Unavailable`, because neither is actionable
/// by a plugin and both invite polling if distinguished.
fn map_execution_error(error: &AgentExecutionError) -> AgentWakeError {
    match error {
        AgentExecutionError::UnknownAlias { .. } => AgentWakeError::UnknownAgent,
        AgentExecutionError::Admission(_) => AgentWakeError::Unavailable,
    }
}

/// One registered wake turn, owned by the process that admitted it.
struct WakeTask {
    id: String,
    owner_pid: u32,
    owner_boot_id: String,
}

/// Register the durable record for one wake turn before the turn runs.
///
/// Registration precedes the turn so a crash mid-turn leaves a visible row
/// rather than a silently lost notification, mirroring the peer-inbox path.
async fn admit_wake_task(
    registry: Option<&dyn TaskRegistry>,
    owner_boot_id: &str,
    package: &str,
    agent_alias: &str,
    caller_alias: Option<&str>,
    depth: u32,
) -> Result<WakeTask> {
    let registry = registry.ok_or_else(|| {
        anyhow::Error::msg("plugin wake dispatch requires an available durable task store")
    })?;
    let task = WakeTask {
        id: uuid::Uuid::new_v4().to_string(),
        owner_pid: std::process::id(),
        owner_boot_id: owner_boot_id.to_string(),
    };
    registry
        .create(TaskRecord {
            id: task.id.clone(),
            kind: TaskKind::PeerInbox,
            agent: agent_alias.to_string(),
            status: TaskStatus::Running,
            owner_pid: task.owner_pid,
            owner_boot_id: task.owner_boot_id.clone(),
            heartbeat_at: None,
            depth,
            parent_id: None,
            // The originating route is the plugin, not a channel: a wake has no
            // reply target a human could answer.
            originator_route: Some(format!("plugin:{package}")),
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: Some(
                caller_alias
                    .map(str::to_string)
                    .unwrap_or_else(|| package.to_string()),
            ),
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
        })
        .await?;
    Ok(task)
}

/// Settle one wake turn's durable record, retrying transient store failures.
///
/// The same shape as peer-inbox settlement: the delay is bounded and the loop is
/// not, because a terminal transition never written leaves a row `Running`
/// forever, and the store is the only durable witness that the wake ran.
async fn settle_wake_task(
    registry: &Arc<dyn TaskRegistry>,
    task: &WakeTask,
    result: &Result<String>,
) {
    let (status, output, error) = match result {
        Ok(response) => (TaskStatus::Completed, Some(response.clone()), None),
        Err(error) => (TaskStatus::Failed, None, Some(format!("{error:#}"))),
    };

    let mut failures = 0_u32;
    loop {
        match registry
            .transition_terminal_if_owner(
                &task.id,
                task.owner_pid,
                &task.owner_boot_id,
                status,
                output.clone(),
                error.clone(),
            )
            .await
        {
            Ok(won) => {
                if failures > 0 {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                            .with_outcome(::zeroclaw_log::EventOutcome::Success)
                            .with_attrs(::serde_json::json!({
                                "task_id": &task.id,
                                "attempts": failures + 1,
                                "transition_won": won,
                            })),
                        "plugin wake terminal transition recovered"
                    );
                }
                return;
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                if failures == 1 || failures.is_power_of_two() {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "task_id": &task.id,
                                "attempt": failures,
                                "error": format!("{error:#}"),
                            })),
                        "plugin wake terminal transition will retry"
                    );
                }
            }
        }

        tokio::time::sleep(wake_store_retry_delay(failures)).await;
    }
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".to_string())
}

/// Run one woken turn under its cost scope, converting a panic into a failure.
///
/// A panicking woken turn must not take the process down and must not leave the
/// slot wedged: it becomes an ordinary `Err`, which settles the task `Failed`
/// and lets the loop release the slot.
async fn run_wake_turn_with_panic_recovery<F>(
    cost_ctx: Option<ToolLoopCostTrackingContext>,
    turn_usage: Option<Arc<Mutex<TurnUsage>>>,
    inner: F,
) -> Result<String>
where
    F: Future<Output = Result<String>>,
{
    match AssertUnwindSafe(deliver_wake_turn_with_cost_scope(
        cost_ctx, turn_usage, inner,
    ))
    .catch_unwind()
    .await
    {
        Ok(result) => result,
        Err(payload) => Err(anyhow::Error::msg(format!(
            "plugin wake turn panicked: {}",
            panic_payload_message(payload.as_ref())
        ))),
    }
}

/// Run `inner` under the woken agent's cost-tracking task-locals so its usage is
/// attributed to that agent and its provider calls join the shared budget.
///
/// Split out from the spawned loop so the scope install can be exercised
/// directly, independent of the detached-spawn plumbing around it.
async fn deliver_wake_turn_with_cost_scope<F, T>(
    cost_ctx: Option<ToolLoopCostTrackingContext>,
    turn_usage: Option<Arc<Mutex<TurnUsage>>>,
    inner: F,
) -> T
where
    F: Future<Output = T>,
{
    TOOL_LOOP_TURN_USAGE
        .scope(
            turn_usage,
            TOOL_LOOP_COST_TRACKING_CONTEXT.scope(cost_ctx, inner),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::TaskSnapshot;
    use std::path::Path;
    use zeroclaw_config::schema::{
        AliasedAgentConfig, CostConfig, ModelProviderConfig, OllamaModelProviderConfig,
        PluginEntryConfig, RiskProfileConfig,
    };
    use zeroclaw_plugins::{PluginCapability, PluginManifest};

    /// A manifest that requests exactly the permission a waking tool needs.
    fn manifest() -> PluginManifest {
        PluginManifest {
            name: "waker".to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: None,
            wasm_sha256: None,
            capabilities: vec![PluginCapability::Tool],
            provides: None,
            permissions: vec![PluginPermission::AgentDispatch],
            config_schema: None,
            signature: None,
            publisher_key: None,
            egress: Default::default(),
        }
    }

    fn scope() -> PluginInstanceScope {
        PluginInstanceScope::from_manifest(
            &manifest(),
            PluginCapability::Tool,
            "main",
            [PluginPermission::AgentDispatch],
        )
        .expect("host-issued scope")
    }

    fn ungranted_scope() -> PluginInstanceScope {
        PluginInstanceScope::from_manifest(&manifest(), PluginCapability::Tool, "main", [])
            .expect("host-issued scope")
    }

    fn entry(name: &str, dispatch_agents: &[&str], rate: Option<u32>) -> PluginEntryConfig {
        PluginEntryConfig {
            name: name.to_string(),
            dispatch_agents: dispatch_agents
                .iter()
                .map(|alias| (*alias).to_string())
                .collect(),
            dispatch_rate_per_min: rate,
            ..PluginEntryConfig::default()
        }
    }

    /// A config with one enabled `recipient` agent whose provider is unreachable
    /// unless a test overrides `uri`.
    ///
    /// `127.0.0.1:1` refuses a connection in microseconds, so a test that only
    /// needs the wake *admitted* does not wait on a network timeout for the turn
    /// it started.
    fn base_config(dir: &Path) -> Config {
        let mut config = Config {
            data_dir: dir.to_path_buf(),
            config_path: dir.join("config.toml"),
            ..Config::default()
        };
        config
            .risk_profiles
            .insert("default".to_string(), RiskProfileConfig::default());
        config.agents.insert(
            "recipient".to_string(),
            AliasedAgentConfig {
                model_provider: "ollama.default".into(),
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config.providers.models.ollama.insert(
            "default".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("mock-model".to_string()),
                    uri: Some("http://127.0.0.1:1".to_string()),
                    timeout_secs: Some(5),
                    ..ModelProviderConfig::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        config
    }

    fn instance_key() -> String {
        scope()
            .id()
            .config_entry_key()
            .expect("valid instance identity")
    }

    fn pair() -> (String, String) {
        (instance_key(), "recipient".to_string())
    }

    /// Wait for a task to reach a terminal state and return its snapshot.
    async fn await_terminal(store: &dyn TaskRegistry, task_id: &str) -> TaskSnapshot {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let snapshot = store
                    .get_snapshot(task_id)
                    .await
                    .expect("task snapshot should be queryable")
                    .expect("registered task should remain in the control plane");
                if snapshot.task.status.is_terminal() {
                    return snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("wake task should reach a terminal state")
    }

    /// Wait for every `(instance, agent)` slot to be released.
    async fn await_slots_released(coordinator: &Arc<SharedWakeCoordinator>) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while coordinator.slot_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("every running pair should release its slot");
    }

    /// Every wake task the store recorded for `agent`, oldest first.
    async fn tasks_for(store: &dyn TaskRegistry, agent: &str) -> Vec<TaskRecord> {
        let mut tasks = store
            .list_by_agent(agent)
            .await
            .expect("agent task list should be queryable");
        tasks.sort_by(|left, right| left.started_at.cmp(&right.started_at));
        tasks
    }

    #[tokio::test]
    async fn default_empty_dispatch_agents_denies_wake() {
        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(base_config(workspace.path())),
            None,
            None,
            None,
            Arc::new(SharedWakeCoordinator::new()),
        );

        // No row at all: the operator granted this instance nothing.
        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "recipient", "look at this")
                .await,
            Err(AgentWakeError::AccessDenied),
            "an instance with no config row may wake nobody"
        );

        // A row whose `dispatch_agents` is unset is the documented deny-all.
        let mut config = base_config(workspace.path());
        config.plugins.entries = vec![entry(&instance_key(), &[], None)];
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            None,
            Arc::new(SharedWakeCoordinator::new()),
        );
        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "recipient", "look at this")
                .await,
            Err(AgentWakeError::AccessDenied),
            "an empty dispatch_agents list denies every target"
        );
        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "recipient", "look at this")
                .await,
            Err(AgentWakeError::AccessDenied),
            "a denied wake must not consume the instance's budget"
        );

        // The grant is checked before the target list, so an ungranted scope is
        // refused even when the operator would have allowed the target.
        let mut config = base_config(workspace.path());
        config.plugins.entries = vec![entry(&instance_key(), &["*"], None)];
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            None,
            Arc::new(SharedWakeCoordinator::new()),
        );
        assert_eq!(
            backend
                .wake(&ungranted_scope(), Some("sender"), "recipient", "look")
                .await,
            Err(AgentWakeError::AccessDenied),
            "a scope without agent_dispatch is refused before its target list"
        );
    }

    #[tokio::test]
    async fn live_config_update_allows_subsequent_wake_without_rebuild() {
        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let key = instance_key();
        let mut initial = base_config(workspace.path());
        initial.plugins.entries = vec![entry(&key, &[], None)];
        let live = zeroclaw_config::live::LiveConfig::new(initial);
        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let store = Arc::clone(&control_plane.store);
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(base_config(workspace.path())),
            Some(live.handle()),
            None,
            Some(control_plane),
            Arc::new(SharedWakeCoordinator::new()),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "recipient", "first")
                .await,
            Err(AgentWakeError::AccessDenied),
            "the published config grants nothing yet"
        );

        // The operator grants the target. Nothing is rebuilt: the same backend,
        // scope and coordinator serve the next call.
        let mut updated = live.snapshot();
        updated.plugins.entries = vec![entry(&key, &["recipient"], None)];
        let revision = live.next_revision().expect("revision available");
        live.publish(revision, updated)
            .expect("publication accepted");

        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "recipient", "second")
                .await,
            Ok(()),
            "a live grant must take effect without re-instantiating the guest"
        );

        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(tasks.len(), 1, "one admitted wake registers one task");
        assert_eq!(tasks[0].originator_route.as_deref(), Some("plugin:waker"));
        assert_eq!(tasks[0].principal_id.as_deref(), Some("sender"));
        assert_eq!(tasks[0].depth, 1, "a root wake runs at depth one");
        await_terminal(store.as_ref(), &tasks[0].id).await;
    }

    #[tokio::test]
    async fn unknown_or_disabled_target_returns_unknown_agent() {
        let workspace = tempfile::TempDir::new().expect("temp data dir");

        // Wildcard grant: reach is not the question, resolvability is.
        let mut config = base_config(workspace.path());
        config.plugins.entries = vec![entry(&instance_key(), &["*"], None)];
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config.clone()),
            None,
            None,
            None,
            Arc::new(SharedWakeCoordinator::new()),
        );
        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "ghost", "hello")
                .await,
            Err(AgentWakeError::UnknownAgent),
            "an alias naming no configured agent is unknown, not denied"
        );

        // A configured but disabled agent is likewise not a wake target.
        config
            .agents
            .get_mut("recipient")
            .expect("recipient configured")
            .enabled = false;
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config.clone()),
            None,
            None,
            None,
            Arc::new(SharedWakeCoordinator::new()),
        );
        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "recipient", "hello")
                .await,
            Err(AgentWakeError::UnknownAgent),
            "a disabled agent is not a wake target"
        );

        // The target list still runs first: a disabled agent the operator never
        // granted is denied rather than reported as unknown, so an ungranted
        // instance cannot probe which aliases exist.
        config.plugins.entries = vec![entry(&instance_key(), &[], None)];
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            None,
            Arc::new(SharedWakeCoordinator::new()),
        );
        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "recipient", "hello")
                .await,
            Err(AgentWakeError::AccessDenied),
            "reach is decided before target existence"
        );
    }

    #[tokio::test]
    async fn rate_limit_enforces_dispatch_rate_per_min() {
        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = base_config(workspace.path());
        config.plugins.entries = vec![entry(&instance_key(), &["recipient"], Some(1))];
        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let store = Arc::clone(&control_plane.store);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "recipient", "one")
                .await,
            Ok(()),
            "the first wake spends the instance's only token"
        );
        // Wait for the admitted turn to finish so the slot is free again: the
        // rate limiter, not coalescing, is what this test is about.
        await_slots_released(&coordinator).await;

        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "recipient", "two")
                .await,
            Err(AgentWakeError::RateLimited),
            "the second wake exceeds one dispatch per minute"
        );
        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "recipient", "three")
                .await,
            Err(AgentWakeError::RateLimited),
            "a refused wake must not itself spend budget"
        );

        // A refused wake registers nothing.
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(tasks.len(), 1, "only the admitted wake registered a task");
    }

    #[tokio::test]
    async fn token_bucket_refills_continuously_and_retunes_cleanly() {
        tokio::time::pause();
        let mut bucket = TokenBucket::new(1);
        assert!(bucket.try_consume(tokio::time::Instant::now()));
        assert!(
            !bucket.try_consume(tokio::time::Instant::now()),
            "one dispatch per minute cannot spend twice in the same instant"
        );

        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(
            bucket.try_consume(tokio::time::Instant::now()),
            "a full minute refills one token at one dispatch per minute"
        );

        // Raising the ceiling mid-window clamps the balance to the new ceiling
        // instead of crediting the difference.
        let mut bucket = TokenBucket::new(600);
        assert!(bucket.try_consume(tokio::time::Instant::now()));
        bucket.retune(2, tokio::time::Instant::now());
        assert_eq!(bucket.rate_per_min, 2.0);
        assert!(
            bucket.tokens <= 2.0,
            "a lowered ceiling must not leave a larger balance: {}",
            bucket.tokens
        );

        // A zero rate is refused by config validation; the bucket stays closed
        // rather than dividing by it.
        let mut bucket = TokenBucket::new(0);
        assert!(!bucket.try_consume(tokio::time::Instant::now()));
    }

    #[tokio::test]
    async fn hop_depth_ceiling_rejects_recursive_wake() {
        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = base_config(workspace.path());
        // `ghost` names no enabled agent, so every call that clears the depth
        // gate ends at `UnknownAgent` and starts no turn.
        config.plugins.entries = vec![entry(&instance_key(), &["ghost"], None)];
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            None,
            Arc::new(SharedWakeCoordinator::new()),
        );

        let at_ceiling = zeroclaw_spawn::scope_plugin_wake_depth(MAX_PLUGIN_WAKE_DEPTH, async {
            backend
                .wake(&scope(), Some("sender"), "ghost", "recurse")
                .await
        })
        .await;
        assert_eq!(
            at_ceiling,
            Err(AgentWakeError::AccessDenied),
            "a wake at the hop ceiling is refused"
        );

        let beyond_ceiling =
            zeroclaw_spawn::scope_plugin_wake_depth(MAX_PLUGIN_WAKE_DEPTH + 7, async {
                backend
                    .wake(&scope(), Some("sender"), "ghost", "recurse")
                    .await
            })
            .await;
        assert_eq!(
            beyond_ceiling,
            Err(AgentWakeError::AccessDenied),
            "the ceiling holds above itself too"
        );

        // One hop below the ceiling the depth gate passes, so the same call
        // reaches target resolution: proof the two refusals above came from
        // depth and not from an unrelated check.
        let below_ceiling =
            zeroclaw_spawn::scope_plugin_wake_depth(MAX_PLUGIN_WAKE_DEPTH - 1, async {
                backend
                    .wake(&scope(), Some("sender"), "ghost", "recurse")
                    .await
            })
            .await;
        assert_eq!(
            below_ceiling,
            Err(AgentWakeError::UnknownAgent),
            "below the ceiling the depth gate lets the call through"
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender"), "ghost", "recurse")
                .await,
            Err(AgentWakeError::UnknownAgent),
            "a root turn outside any plugin scope is depth zero"
        );
    }

    /// Five wakes in a burst cost one initial turn plus one coalesced follow-up,
    /// and the follow-up attributes to the latest caller.
    ///
    /// The mock stands in at the OpenAI-compatible `/v1/chat/completions`
    /// boundary the shared Ollama client speaks, exactly as the peer-delivery
    /// regression does, so the woken turns are real `process_message` calls
    /// reaching a real provider dispatch.
    #[tokio::test]
    async fn coalesces_concurrent_wakes_and_recovers_from_panic() {
        use crate::cost::CostTracker;
        use axum::{Json, Router, extract::State, routing::post};

        #[derive(Clone)]
        struct RequestState {
            count: Arc<Mutex<u32>>,
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }
        async fn capture_chat(
            State(state): State<RequestState>,
            Json(_body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            *state.count.lock() += 1;
            state.entered.notify_one();
            state.release.notified().await;
            Json(serde_json::json!({
                "choices": [{"message": {"content": "wake turn complete"}}],
                "usage": {"prompt_tokens": 1_000, "completion_tokens": 200}
            }))
        }

        let request_count = Arc::new(Mutex::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock provider listener should bind");
        let mock_addr = listener.local_addr().expect("mock provider addr");
        let app = Router::new()
            .route("/v1/chat/completions", post(capture_chat))
            .with_state(RequestState {
                count: Arc::clone(&request_count),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            });
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("mock provider server should run");
        });

        // Isolated data_dir: the process-global cost tracker and the durable task
        // store are both keyed by it, so a unique path keeps this fixture from
        // touching real user state.
        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = base_config(workspace.path());
        config.cost = CostConfig {
            enabled: true,
            track_per_agent: true,
            ..CostConfig::default()
        };
        config.providers.models.ollama.insert(
            "default".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("mock-model".to_string()),
                    uri: Some(format!("http://{mock_addr}")),
                    timeout_secs: Some(10),
                    pricing: HashMap::from([
                        ("mock-model.input".to_string(), 3.0),
                        ("mock-model.output".to_string(), 15.0),
                    ]),
                    ..ModelProviderConfig::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        // A budget wide enough that a burst of notifications is never confused
        // with a rate-limit refusal.
        config.plugins.entries = vec![entry(&instance_key(), &["recipient"], Some(600))];

        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let store = Arc::clone(&control_plane.store);
        let authority = crate::LiveConfigAuthority::new(config.clone());
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config.clone()),
            None,
            Some(authority.execution_capability()),
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        // The first wake claims the slot and starts a turn, which parks at the
        // mock provider. The remaining four arrive while it runs.
        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("the woken turn reaches the paused provider");
        for caller in ["sender-2", "sender-3", "sender-4", "sender-5"] {
            assert_eq!(
                backend
                    .wake(&scope(), Some(caller), "recipient", caller)
                    .await,
                Ok(()),
                "a wake for a running target coalesces rather than failing"
            );
        }
        assert_eq!(
            *request_count.lock(),
            1,
            "coalesced wakes must not each start a provider call"
        );
        assert_eq!(
            coordinator.slot_count(),
            1,
            "one slot is held for the running pair"
        );

        release.notify_one();
        // The follow-up turn: the loop takes the coalesced wake and runs it.
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("the coalesced follow-up turn reaches the provider");
        release.notify_one();

        let tasks = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let tasks = tasks_for(store.as_ref(), "recipient").await;
                if tasks.len() >= 2 && tasks.iter().all(|task| task.status.is_terminal()) {
                    return tasks;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both wake turns should settle");

        assert_eq!(
            tasks.len(),
            2,
            "five coalesced wakes register exactly two turns: {tasks:?}"
        );
        for task in &tasks {
            assert_eq!(
                task.status,
                TaskStatus::Completed,
                "each wake turn completes: {task:?}"
            );
            assert_eq!(task.kind, TaskKind::PeerInbox);
            assert_eq!(task.agent, "recipient");
            assert_eq!(task.originator_route.as_deref(), Some("plugin:waker"));
            assert_eq!(task.depth, 1);
        }
        assert!(
            tasks
                .iter()
                .any(|task| task.principal_id.as_deref() == Some("sender-5")),
            "the coalesced turn attributes to the caller that most recently replaced it: {tasks:?}"
        );
        assert_eq!(
            *request_count.lock(),
            2,
            "one initial turn plus one coalesced follow-up, never five"
        );

        // Both turns' spend landed on the woken agent, not on any caller.
        let tracker = CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir)
            .expect("an enabled cost config resolves a tracker");
        let mut recipient_summary = None;
        for _ in 0..20_000 {
            let summary = tracker
                .get_summary_for_agent("recipient")
                .expect("recipient cost summary should be queryable");
            if summary.request_count >= 2 {
                recipient_summary = Some(summary);
                break;
            }
            tokio::task::yield_now().await;
        }
        let recipient_summary =
            recipient_summary.expect("both woken turns must record usage on the recipient");
        assert_eq!(recipient_summary.request_count, 2);
        assert_eq!(recipient_summary.total_tokens, 2_400);
        let expected_cost = 2.0 * ((1_000.0 * 3.0 / 1_000_000.0) + (200.0 * 15.0 / 1_000_000.0));
        assert!(
            (recipient_summary.session_cost_usd - expected_cost).abs() < 1e-9,
            "unexpected recipient cost: {} vs expected {expected_cost}",
            recipient_summary.session_cost_usd
        );
        for caller in ["sender-1", "sender-5"] {
            let summary = tracker
                .get_summary_for_agent(caller)
                .expect("caller cost summary should be queryable");
            assert_eq!(
                summary.request_count, 0,
                "wake spend must not bleed onto {caller}'s alias"
            );
        }

        // The slot is released once the last coalesced turn finishes.
        await_slots_released(&coordinator).await;

        // A panicked woken turn becomes a `Failed` settlement rather than a
        // process abort, and the slot guard clears the claim so the pair is
        // wakeable again.
        let panic_task = admit_wake_task(
            Some(store.as_ref()),
            "boot-test",
            "waker",
            "recipient",
            Some("sender-1"),
            1,
        )
        .await
        .expect("registration should succeed");
        let panicked = run_wake_turn_with_panic_recovery(None, None, async {
            panic!("synthetic woken-turn panic");
        })
        .await;
        let panic_text = format!(
            "{:#}",
            panicked.as_ref().expect_err("a panic must fail the turn")
        );
        assert!(
            panic_text.contains("plugin wake turn panicked: synthetic woken-turn panic"),
            "the panic must be reported with its payload: {panic_text}"
        );
        settle_wake_task(&store, &panic_task, &panicked).await;
        let snapshot = await_terminal(store.as_ref(), &panic_task.id).await;
        assert_eq!(
            snapshot.task.status,
            TaskStatus::Failed,
            "a panicked wake turn settles failed"
        );
        assert!(
            snapshot
                .error
                .as_deref()
                .is_some_and(|error| error.contains("synthetic woken-turn panic")),
            "the settlement carries the panic text: {snapshot:?}"
        );

        // The guard is what keeps a dropped turn from wedging the pair: claim a
        // slot, drop the armed guard, and the pair is idle again.
        coordinator
            .slots
            .lock()
            .insert(pair(), WakeSlotState::Running { pending: None });
        assert_eq!(coordinator.slot_count(), 1);
        drop(SlotGuard::new(Arc::clone(&coordinator), pair()));
        assert_eq!(
            coordinator.slot_count(),
            0,
            "an armed guard clears the slot it owns"
        );

        // And the pair accepts a new wake afterwards.
        assert_eq!(
            backend
                .wake(&scope(), Some("sender-6"), "recipient", "after the panic")
                .await,
            Ok(()),
            "a cleared slot admits the next wake"
        );
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("the post-panic wake reaches the provider");
        release.notify_one();
        await_slots_released(&coordinator).await;

        server.abort();
    }

    /// A coalesced wake must carry the latest caller's attribution, prompt and
    /// hop depth rather than the ones that won the slot.
    #[tokio::test]
    async fn coalesced_wake_carries_the_latest_callers_identity() {
        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = base_config(workspace.path());
        config.plugins.entries = vec![entry(&instance_key(), &["recipient"], Some(1))];
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            None,
            Arc::clone(&coordinator),
        );

        // Claim the slot as a running turn so every wake below coalesces.
        coordinator
            .slots
            .lock()
            .insert(pair(), WakeSlotState::Running { pending: None });
        for caller in ["sender-1", "sender-2"] {
            assert_eq!(
                backend
                    .wake(&scope(), Some(caller), "recipient", caller)
                    .await,
                Ok(())
            );
        }

        let slots = coordinator.slots.lock();
        let Some(WakeSlotState::Running {
            pending: Some(pending),
        }) = slots.get(&pair())
        else {
            panic!("the pair should hold a pending coalesced wake");
        };
        assert_eq!(pending.prompt, "sender-2");
        assert_eq!(pending.caller_alias.as_deref(), Some("sender-2"));
        assert_eq!(pending.depth, 0, "a coalesced wake keeps its own hop depth");
        assert!(
            pending.capabilities.is_none(),
            "an unbound registry inherits no capabilities"
        );
        drop(slots);
        assert!(
            !coordinator.try_consume_token(&instance_key(), 1),
            "the first pending wake reserves one token and overwrites share it"
        );
    }

    /// Build (or reuse) the compiled `tool-dispatch-fixture` WASM component
    /// and stage a ready-to-load plugin root directory for tests.
    #[cfg(feature = "plugins-wasm-cranelift")]
    fn stage_dispatch_fixture_plugin() -> (tempfile::TempDir, PluginManifest) {
        use std::path::PathBuf;
        use std::process::Command;

        let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../zeroclaw-plugins/tests/fixtures/tool-dispatch-fixture");
        let target_dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/plugin-wake-fixture");
        let status = Command::new(env!("CARGO"))
            .current_dir(&fixture_dir)
            .args([
                "build",
                "--locked",
                "--quiet",
                "--package",
                "zeroclaw-tool-dispatch-plugin-fixture",
                "--target",
                "wasm32-wasip2",
                "--target-dir",
            ])
            .arg(&target_dir)
            .status()
            .expect("run Cargo for the tool dispatch component fixture");
        assert!(
            status.success(),
            "the tool dispatch fixture must build; install the wasm32-wasip2 target"
        );
        let wasm =
            target_dir.join("wasm32-wasip2/debug/zeroclaw_tool_dispatch_plugin_fixture.wasm");
        assert!(wasm.is_file(), "the fixture WASM was not produced");

        let plugin_manifest = PluginManifest {
            name: "tool-dispatch-fixture".to_string(),
            wasm_path: Some("tool-dispatch-fixture.wasm".to_string()),
            ..manifest()
        };
        let root = tempfile::TempDir::new().expect("fixture package root");
        let package_dir = root.path().join(&plugin_manifest.name);
        std::fs::create_dir_all(&package_dir).expect("fixture package directory");
        std::fs::copy(&wasm, package_dir.join("tool-dispatch-fixture.wasm"))
            .expect("copy fixture payload");
        std::fs::write(
            package_dir.join("manifest.toml"),
            toml::to_string(&plugin_manifest).expect("serialize fixture manifest"),
        )
        .expect("write fixture manifest");
        (root, plugin_manifest)
    }

    /// A [`WasmTool`] over the compiled dispatch fixture, granted
    /// `agent-dispatch` and wired to a real [`RuntimeAgentWakeBackend`] whose
    /// operator grant names `recipient`. The guards keep the staged package,
    /// control plane and plugin state alive for the caller.
    #[cfg(feature = "plugins-wasm-cranelift")]
    struct DispatchFixture {
        tool: zeroclaw_plugins::wasm_tool::WasmTool,
        store: Arc<dyn TaskRegistry>,
        _guards: [tempfile::TempDir; 3],
    }

    #[cfg(feature = "plugins-wasm-cranelift")]
    fn dispatch_fixture_tool() -> DispatchFixture {
        use zeroclaw_plugins::component::PluginLimits;
        use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
        use zeroclaw_plugins::host::{AdmittedComponent, PluginHost};
        use zeroclaw_plugins::services::{AgentWakeService, PluginHostServices};
        use zeroclaw_plugins::wasm_tool::WasmTool;

        let (root, plugin_manifest) = stage_dispatch_fixture_plugin();
        let host = Arc::new(PluginHost::from_plugins_dir(root.path()).expect("admit fixture"));
        let details = host.tool_plugin_details();
        assert_eq!(
            details.len(),
            1,
            "the fixture package must be admitted once"
        );
        let component: AdmittedComponent = details[0].1.clone();

        let scope = PluginInstanceScope::from_manifest(
            &plugin_manifest,
            PluginCapability::Tool,
            "alpha",
            [PluginPermission::AgentDispatch],
        )
        .expect("a dispatch-granted scope");

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let store = Arc::clone(&control_plane.store);

        // The grant the operator publishes, keyed on the admitted instance.
        let mut config = base_config(workspace.path());
        config.plugins.entries = vec![entry(
            &scope.id().config_entry_key().expect("instance key"),
            &["recipient"],
            None,
        )];
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            Some(control_plane),
            Arc::new(SharedWakeCoordinator::new()),
        );

        let state_dir = tempfile::TempDir::new().expect("plugin state dir");
        let resolver_manifest = plugin_manifest.clone();
        let services = PluginHostServices::new(
            PluginConfigResolver::new(move |plug_scope| {
                resolve_plugin_config(&resolver_manifest, plug_scope, None)
            }),
            zeroclaw_plugins::services::PluginStateService::new(
                crate::plugin_state::PluginStateStore::new(state_dir.path(), state_dir.path()),
            ),
        )
        .with_wake(AgentWakeService::new(backend));

        let tool = WasmTool::from_wasm(
            component,
            scope,
            services,
            PluginLimits {
                call_fuel: 1_000_000_000,
                max_memory_bytes: 64 * 1024 * 1024,
                max_table_elements: 10_000,
                max_instances: 10,
                call_timeout: Duration::from_secs(20),
            },
            None,
        )
        .expect("a dispatch-granted scope instantiates the importing component")
        .with_caller_alias("alpha");
        DispatchFixture {
            tool,
            store,
            _guards: [root, workspace, state_dir],
        }
    }

    /// The real component boundary: a compiled `tool` guest calls
    /// `dispatch.wake`, and the host answers through
    /// [`RuntimeAgentWakeBackend`] rather than a recording stub.
    ///
    /// This is the only test that pins the *seam* instead of the backend: the
    /// grant, the frame, the shape validation and the caller attribution all
    /// live in `zeroclaw-plugins`, and what crosses into this module is exactly
    /// what an installed plugin would send.
    #[cfg(feature = "plugins-wasm-cranelift")]
    #[tokio::test]
    async fn wasm_tool_dispatch_reaches_the_runtime_backend_end_to_end() {
        use zeroclaw_api::tool::Tool as _;

        let DispatchFixture {
            tool,
            store,
            _guards,
        } = dispatch_fixture_tool();
        assert_eq!(tool.name(), "dispatch_fixture");

        let result = tool
            .execute(serde_json::json!({"target": "recipient", "prompt": "crossbar is up"}))
            .await
            .expect("the guest returns a tool result, not a trap");
        assert!(
            result.success,
            "the admitted wake must report success: {result:?}"
        );
        assert_eq!(result.output.as_str(), "ok");
        assert_eq!(result.error, None);

        // The guest's target reached a real turn, attributed to the host-owned
        // caller alias rather than to anything the guest could name.
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(tasks.len(), 1, "the guest's wake registered one task");
        assert_eq!(
            tasks[0].originator_route.as_deref(),
            Some("plugin:tool-dispatch-fixture")
        );
        assert_eq!(tasks[0].principal_id.as_deref(), Some("alpha"));
        await_terminal(store.as_ref(), &tasks[0].id).await;

        // A target the operator never granted is refused by the same path, and
        // the guest sees the WIT category rather than a host detail.
        let refused = tool
            .execute(serde_json::json!({"target": "ghost", "prompt": "crossbar is up"}))
            .await
            .expect("the guest returns a tool result, not a trap");
        assert!(!refused.success, "an ungranted target must be refused");
        assert!(
            format!("{:?}", refused.error).contains("access-denied"),
            "the guest must observe the access-denied case: {:?}",
            refused.error
        );
        assert_eq!(
            tasks_for(store.as_ref(), "recipient").await.len(),
            1,
            "a refused wake registers nothing further"
        );
    }

    /// The principal boundary at the real Agent/WASM seam: dispatch carries the
    /// caller's identity into the target turn but not its live agent and tool
    /// selectors, so the non-admin posture must withhold a dispatch-granted
    /// plugin even when an explicit tool ceiling names it.
    ///
    /// An unrestricted Agent first runs the compiled guest and completes one
    /// target turn, proving the tool is live. After the restricted posture is
    /// applied, the same dispatch must be unreachable and register nothing.
    #[cfg(feature = "plugins-wasm-cranelift")]
    #[tokio::test]
    async fn restricted_principal_posture_withholds_dispatch_capable_plugin() {
        use crate::agent::{Agent, SteeringPosture, dispatcher::NativeToolDispatcher};
        use crate::composition::test_support::StubProvider;
        use crate::tools::scoped::ScopedToolRegistry;
        use zeroclaw_api::tool::Tool as _;

        let DispatchFixture {
            tool,
            store,
            _guards,
        } = dispatch_fixture_tool();
        let tool_name = tool.name().to_string();
        assert!(
            tool.requires_unrestricted_principal(),
            "a dispatch-granted plugin is marked for unrestricted principals only"
        );

        let workspace = tempfile::TempDir::new().expect("agent workspace");
        let mut agent = Agent::builder()
            .model_provider(Box::new(StubProvider))
            .tools(ScopedToolRegistry::from_raw_for_test(vec![Box::new(tool)]))
            .memory(Arc::new(zeroclaw_memory::NoneMemory::new("none")))
            .observer(Arc::new(crate::observability::NoopObserver {}))
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(workspace.path().to_path_buf())
            .build()
            .expect("agent builds");
        let args = serde_json::json!({"target": "recipient", "prompt": "crossbar is up"});

        // Unrestricted: the guest runs and the target turn completes.
        let result = agent
            .execute_tool_for_test(&tool_name, args.clone())
            .await
            .expect("an unrestricted Agent holds the dispatch plugin")
            .expect("the guest returns a tool result, not a trap");
        assert!(result.success, "the admitted wake must succeed: {result:?}");
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(tasks.len(), 1, "the unrestricted wake registered one task");
        await_terminal(store.as_ref(), &tasks[0].id).await;

        // Restricted: an explicit ceiling naming the tool does not restore it.
        agent.apply_principal_tool_posture(SteeringPosture {
            tool_ceiling: Some(vec![tool_name.clone()]),
            disable_principal_unaware_nested_tools: true,
            withhold_unrestricted_tools: true,
        });
        assert!(
            agent
                .execute_tool_for_test(&tool_name, args)
                .await
                .is_none(),
            "a restricted principal must not reach a dispatch-capable plugin"
        );
        assert_eq!(
            tasks_for(store.as_ref(), "recipient").await.len(),
            1,
            "the refused dispatch registers no further target task"
        );
    }

    #[tokio::test]
    async fn coalesced_followup_enforces_rate_limit() {
        use axum::{Json, Router, extract::State, routing::post};

        #[derive(Clone)]
        struct RequestState {
            count: Arc<Mutex<u32>>,
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }
        async fn capture_chat(
            State(state): State<RequestState>,
            Json(_body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            *state.count.lock() += 1;
            state.entered.notify_one();
            state.release.notified().await;
            Json(serde_json::json!({
                "choices": [{"message": {"content": "ok"}}]
            }))
        }

        let request_count = Arc::new(Mutex::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock provider listener should bind");
        let mock_addr = listener.local_addr().expect("mock provider addr");
        let app = Router::new()
            .route("/v1/chat/completions", post(capture_chat))
            .with_state(RequestState {
                count: Arc::clone(&request_count),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            });
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("mock provider server should run");
        });

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = base_config(workspace.path());
        config.providers.models.ollama.insert(
            "default".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("mock-model".to_string()),
                    uri: Some(format!("http://{mock_addr}")),
                    timeout_secs: Some(10),
                    ..ModelProviderConfig::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        // Budget = 1 token/min: the initial turn spends it, so a later wake
        // must report rate-limited rather than claim a follow-up it will drop.
        config.plugins.entries = vec![entry(&instance_key(), &["recipient"], Some(1))];

        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let store = Arc::clone(&control_plane.store);
        let authority = crate::LiveConfigAuthority::new(config.clone());
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            Some(authority.live_handle()),
            Some(authority.execution_capability()),
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("first turn reaches mock provider");

        // No token remains for a pending follow-up. Every call must receive
        // rate-limited while the first turn remains in progress.
        for caller in ["sender-2", "sender-3", "sender-4"] {
            assert_eq!(
                backend
                    .wake(&scope(), Some(caller), "recipient", caller)
                    .await,
                Err(AgentWakeError::RateLimited)
            );
        }
        release.notify_one();

        await_slots_released(&coordinator).await;
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(
            tasks.len(),
            1,
            "rate=1 admits only the initial turn: {tasks:?}"
        );
        assert_eq!(*request_count.lock(), 1);
        server.abort();
    }

    /// Delegates to a real store, except that one numbered `create` waits for
    /// `release` and then fails. That pins a registration failure to a chosen
    /// wake while the test arranges other wakes around it. Independently,
    /// every `create` fails immediately while `failing` is set, signalling
    /// `failed` first, so a test can hold the worker in registration backoff.
    /// Likewise every owner-checked terminal write fails, signalling
    /// `settle_failed`, while `settle_failing` is set.
    struct GatedCreateRegistry {
        inner: Arc<dyn TaskRegistry>,
        creates: std::sync::atomic::AtomicUsize,
        gated_call: usize,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        failing: Arc<std::sync::atomic::AtomicBool>,
        failed: Arc<tokio::sync::Notify>,
        settle_failing: Arc<std::sync::atomic::AtomicBool>,
        settle_failed: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl TaskRegistry for GatedCreateRegistry {
        async fn create(&self, rec: TaskRecord) -> anyhow::Result<()> {
            let call = self
                .creates
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if call == self.gated_call {
                self.entered.notify_one();
                self.release.notified().await;
                anyhow::bail!("gated registration failure");
            }
            if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
                self.failed.notify_one();
                anyhow::bail!("registration store unavailable");
            }
            self.inner.create(rec).await
        }
        async fn heartbeat(&self, id: &str, owner_boot_id: &str) -> anyhow::Result<()> {
            self.inner.heartbeat(id, owner_boot_id).await
        }
        async fn update_status(
            &self,
            id: &str,
            status: TaskStatus,
            output: Option<String>,
            error: Option<String>,
        ) -> anyhow::Result<()> {
            self.inner.update_status(id, status, output, error).await
        }
        async fn transition_terminal(
            &self,
            id: &str,
            status: TaskStatus,
            output: Option<String>,
            error: Option<String>,
        ) -> anyhow::Result<bool> {
            self.inner
                .transition_terminal(id, status, output, error)
                .await
        }
        async fn transition_terminal_if_owner(
            &self,
            id: &str,
            owner_pid: u32,
            owner_boot_id: &str,
            status: TaskStatus,
            output: Option<String>,
            error: Option<String>,
        ) -> anyhow::Result<bool> {
            if self
                .settle_failing
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                self.settle_failed.notify_one();
                anyhow::bail!("settlement store unavailable");
            }
            self.inner
                .transition_terminal_if_owner(id, owner_pid, owner_boot_id, status, output, error)
                .await
        }
        async fn persist_terminal_settlement_intent(
            &self,
            intent: crate::control_plane::task_registry::TerminalSettlementIntent,
        ) -> anyhow::Result<bool> {
            self.inner.persist_terminal_settlement_intent(intent).await
        }
        async fn list_terminal_settlement_intents(
            &self,
        ) -> anyhow::Result<Vec<crate::control_plane::task_registry::TerminalSettlementIntent>>
        {
            self.inner.list_terminal_settlement_intents().await
        }
        async fn promote_terminal_settlement(
            &self,
            intent: &crate::control_plane::task_registry::TerminalSettlementIntent,
            resolved_status: TaskStatus,
            output: Option<String>,
            error: Option<String>,
        ) -> anyhow::Result<bool> {
            self.inner
                .promote_terminal_settlement(intent, resolved_status, output, error)
                .await
        }
        async fn discard_terminal_settlement_intent(
            &self,
            intent: &crate::control_plane::task_registry::TerminalSettlementIntent,
        ) -> anyhow::Result<bool> {
            self.inner.discard_terminal_settlement_intent(intent).await
        }
        async fn claim_owner(
            &self,
            id: &str,
            owner_pid: u32,
            owner_boot_id: &str,
        ) -> anyhow::Result<()> {
            self.inner.claim_owner(id, owner_pid, owner_boot_id).await
        }
        async fn get(&self, id: &str) -> anyhow::Result<Option<TaskRecord>> {
            self.inner.get(id).await
        }
        async fn get_snapshot(&self, id: &str) -> anyhow::Result<Option<TaskSnapshot>> {
            self.inner.get_snapshot(id).await
        }
        async fn record_progress(
            &self,
            id: &str,
            owner_boot_id: &str,
            progress: &crate::control_plane::TaskProgress,
        ) -> anyhow::Result<bool> {
            self.inner
                .record_progress(id, owner_boot_id, progress)
                .await
        }
        async fn list_running(&self) -> anyhow::Result<Vec<TaskRecord>> {
            self.inner.list_running().await
        }
        async fn list_by_agent(&self, agent: &str) -> anyhow::Result<Vec<TaskRecord>> {
            self.inner.list_by_agent(agent).await
        }
        async fn reconcile_lost(&self, id: &str, now_boot_id: &str) -> anyhow::Result<bool> {
            self.inner.reconcile_lost(id, now_boot_id).await
        }
        async fn reconcile_timed_out(
            &self,
            id: &str,
            owner_pid: u32,
            owner_boot_id: &str,
            heartbeat_at: &str,
        ) -> anyhow::Result<bool> {
            self.inner
                .reconcile_timed_out(id, owner_pid, owner_boot_id, heartbeat_at)
                .await
        }
    }

    /// A control plane over a real store whose `gated_call`-th registration
    /// waits for `release` and then fails. Returns the handle and the real
    /// store the test reads back from.
    fn gated_control_plane(
        dir: &Path,
        gated_call: usize,
        entered: &Arc<tokio::sync::Notify>,
        release: &Arc<tokio::sync::Notify>,
    ) -> (ControlPlaneHandle, Arc<dyn TaskRegistry>) {
        let real = ControlPlaneHandle::open(dir).expect("isolated control plane");
        let store = Arc::clone(&real.store);
        let gated = ControlPlaneHandle {
            store: Arc::new(GatedCreateRegistry {
                inner: Arc::clone(&store),
                creates: std::sync::atomic::AtomicUsize::new(0),
                gated_call,
                entered: Arc::clone(entered),
                release: Arc::clone(release),
                failing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                failed: Arc::new(tokio::sync::Notify::new()),
                settle_failing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                settle_failed: Arc::new(tokio::sync::Notify::new()),
            }),
            boot_id: real.boot_id,
        };
        (gated, store)
    }

    /// A control plane over a real store whose registrations fail immediately,
    /// signalling `failed`, for as long as `failing` is set.
    fn failing_control_plane(
        dir: &Path,
        failing: &Arc<std::sync::atomic::AtomicBool>,
        failed: &Arc<tokio::sync::Notify>,
    ) -> (ControlPlaneHandle, Arc<dyn TaskRegistry>) {
        let real = ControlPlaneHandle::open(dir).expect("isolated control plane");
        let store = Arc::clone(&real.store);
        let flaky = ControlPlaneHandle {
            store: Arc::new(GatedCreateRegistry {
                inner: Arc::clone(&store),
                creates: std::sync::atomic::AtomicUsize::new(0),
                gated_call: 0,
                entered: Arc::new(tokio::sync::Notify::new()),
                release: Arc::new(tokio::sync::Notify::new()),
                failing: Arc::clone(failing),
                failed: Arc::clone(failed),
                settle_failing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                settle_failed: Arc::new(tokio::sync::Notify::new()),
            }),
            boot_id: real.boot_id,
        };
        (flaky, store)
    }

    /// A control plane over a real store whose owner-checked terminal writes
    /// fail immediately, signalling `settle_failed`, for as long as
    /// `settle_failing` is set. Registration is unaffected.
    fn failing_settlement_control_plane(
        dir: &Path,
        settle_failing: &Arc<std::sync::atomic::AtomicBool>,
        settle_failed: &Arc<tokio::sync::Notify>,
    ) -> (ControlPlaneHandle, Arc<dyn TaskRegistry>) {
        let real = ControlPlaneHandle::open(dir).expect("isolated control plane");
        let store = Arc::clone(&real.store);
        let flaky = ControlPlaneHandle {
            store: Arc::new(GatedCreateRegistry {
                inner: Arc::clone(&store),
                creates: std::sync::atomic::AtomicUsize::new(0),
                gated_call: 0,
                entered: Arc::new(tokio::sync::Notify::new()),
                release: Arc::new(tokio::sync::Notify::new()),
                failing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                failed: Arc::new(tokio::sync::Notify::new()),
                settle_failing: Arc::clone(settle_failing),
                settle_failed: Arc::clone(settle_failed),
            }),
            boot_id: real.boot_id,
        };
        (flaky, store)
    }

    /// Wait until another wake is held against `key`: parked as a pending
    /// follow-up, or waiting for the slot's claimant to settle.
    async fn await_contended(coordinator: &Arc<SharedWakeCoordinator>, key: &(String, String)) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let contended = match coordinator.slots.lock().get(key) {
                    Some(WakeSlotState::Running { pending }) => pending.is_some(),
                    Some(WakeSlotState::Starting { gate }) => gate.receiver_count() > 0,
                    None => false,
                };
                if contended {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the second wake must reach the held slot");
    }

    /// A wake that arrives while the first wake is still registering its task
    /// must not be accepted and then dropped when that registration fails. The
    /// first caller learns `unavailable`; the second caller's success must mean
    /// a turn actually runs for it.
    #[tokio::test]
    async fn wake_accepted_during_a_failed_claim_still_runs() {
        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = base_config(workspace.path());
        config.plugins.entries = vec![entry(&instance_key(), &["recipient"], None)];

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (control_plane, store) = gated_control_plane(workspace.path(), 1, &entered, &release);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = Arc::new(RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            Some(control_plane),
            Arc::clone(&coordinator),
        ));

        // The first wake claims the slot and stalls inside its registration.
        let first = {
            let backend = Arc::clone(&backend);
            zeroclaw_spawn::spawn!(async move {
                backend
                    .wake(&scope(), Some("sender-1"), "recipient", "first")
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("the first wake reaches its registration");

        // A second wake meets the held slot before the first one settles.
        let second = {
            let backend = Arc::clone(&backend);
            zeroclaw_spawn::spawn!(async move {
                backend
                    .wake(&scope(), Some("sender-2"), "recipient", "second")
                    .await
            })
        };
        await_contended(&coordinator, &pair()).await;

        // The first registration fails.
        release.notify_one();
        assert_eq!(
            first.await.expect("first wake task"),
            Err(AgentWakeError::Unavailable),
            "the caller whose registration failed learns it"
        );
        let second = tokio::time::timeout(Duration::from_secs(10), second)
            .await
            .expect("the second wake settles")
            .expect("second wake task");
        assert_eq!(second, Ok(()), "the second wake was not itself refused");

        await_slots_released(&coordinator).await;
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(
            tasks.len(),
            1,
            "a wake reported as accepted must run a turn: {tasks:?}"
        );
        assert_eq!(tasks[0].principal_id.as_deref(), Some("sender-2"));
        await_terminal(store.as_ref(), &tasks[0].id).await;
    }

    /// A wake that waited for another claim to settle must be judged on the
    /// grant in force when it claims, not the one it saw before waiting. Here
    /// the operator revokes the target while the second wake waits; when the
    /// first claim fails and frees the slot, the waiting wake must be refused
    /// and start nothing.
    #[tokio::test]
    async fn waiting_wake_rechecks_a_grant_revoked_while_it_waited() {
        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let key = instance_key();
        let mut initial = base_config(workspace.path());
        initial.plugins.entries = vec![entry(&key, &["recipient"], None)];
        let live = zeroclaw_config::live::LiveConfig::new(initial);

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (control_plane, store) = gated_control_plane(workspace.path(), 1, &entered, &release);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = Arc::new(RuntimeAgentWakeBackend::for_tests(
            Arc::new(base_config(workspace.path())),
            Some(live.handle()),
            None,
            Some(control_plane),
            Arc::clone(&coordinator),
        ));

        // The first wake claims the slot and stalls inside its registration.
        let first = {
            let backend = Arc::clone(&backend);
            zeroclaw_spawn::spawn!(async move {
                backend
                    .wake(&scope(), Some("sender-1"), "recipient", "first")
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("the first wake reaches its registration");

        // A second wake, granted when it arrives, waits on the held claim.
        let second = {
            let backend = Arc::clone(&backend);
            zeroclaw_spawn::spawn!(async move {
                backend
                    .wake(&scope(), Some("sender-2"), "recipient", "second")
                    .await
            })
        };
        await_contended(&coordinator, &pair()).await;

        // The operator revokes the target while the second wake waits.
        let mut revoked = live.snapshot();
        revoked.plugins.entries = vec![entry(&key, &[], None)];
        let revision = live.next_revision().expect("revision available");
        live.publish(revision, revoked)
            .expect("publication accepted");

        // The first registration fails and frees the slot.
        release.notify_one();
        assert_eq!(
            first.await.expect("first wake task"),
            Err(AgentWakeError::Unavailable)
        );
        let second = tokio::time::timeout(Duration::from_secs(10), second)
            .await
            .expect("the second wake settles")
            .expect("second wake task");
        assert_eq!(
            second,
            Err(AgentWakeError::AccessDenied),
            "a wake must not claim a slot under a grant revoked while it waited"
        );

        await_slots_released(&coordinator).await;
        assert!(
            tasks_for(store.as_ref(), "recipient").await.is_empty(),
            "a revoked wake starts no turn"
        );
    }

    /// The worker's own follow-up registration can fail too. A wake that
    /// coalesced while that registration was in flight must still run after
    /// it: the worker keeps draining instead of releasing the slot. The newer
    /// wake supersedes the failed one in `pending`, exactly as coalescing
    /// would have had the failed one still been waiting there, so it runs in
    /// that one's place rather than in addition to it.
    #[tokio::test]
    async fn wake_coalesced_during_a_failed_followup_registration_still_runs() {
        use axum::{Json, Router, extract::State, routing::post};

        #[derive(Clone)]
        struct RequestState {
            count: Arc<Mutex<u32>>,
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }
        async fn capture_chat(
            State(state): State<RequestState>,
            Json(_body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            *state.count.lock() += 1;
            state.entered.notify_one();
            state.release.notified().await;
            Json(serde_json::json!({
                "choices": [{"message": {"content": "ok"}}]
            }))
        }

        let request_count = Arc::new(Mutex::new(0));
        let provider_entered = Arc::new(tokio::sync::Notify::new());
        let provider_release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock provider listener should bind");
        let mock_addr = listener.local_addr().expect("mock provider addr");
        let app = Router::new()
            .route("/v1/chat/completions", post(capture_chat))
            .with_state(RequestState {
                count: Arc::clone(&request_count),
                entered: Arc::clone(&provider_entered),
                release: Arc::clone(&provider_release),
            });
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("mock provider server should run");
        });

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = base_config(workspace.path());
        config.providers.models.ollama.insert(
            "default".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("mock-model".to_string()),
                    uri: Some(format!("http://{mock_addr}")),
                    timeout_secs: Some(10),
                    ..ModelProviderConfig::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        config.plugins.entries = vec![entry(&instance_key(), &["recipient"], None)];

        // Registration 1 is the first turn; 2 is the first follow-up and fails.
        let registry_entered = Arc::new(tokio::sync::Notify::new());
        let registry_release = Arc::new(tokio::sync::Notify::new());
        let (control_plane, store) =
            gated_control_plane(workspace.path(), 2, &registry_entered, &registry_release);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("first turn reaches mock provider");
        assert_eq!(
            backend
                .wake(&scope(), Some("sender-2"), "recipient", "second")
                .await,
            Ok(()),
            "the second wake coalesces onto the running turn"
        );

        // The first turn ends; the worker starts registering the follow-up.
        provider_release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), registry_entered.notified())
            .await
            .expect("the worker reaches the follow-up registration");

        // A third wake coalesces while that registration is in flight.
        assert_eq!(
            backend
                .wake(&scope(), Some("sender-3"), "recipient", "third")
                .await,
            Ok(())
        );

        // The follow-up registration fails; the third wake must still run.
        registry_release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("the third wake's turn must reach the provider");
        provider_release.notify_one();

        await_slots_released(&coordinator).await;
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        let principals: Vec<_> = tasks
            .iter()
            .map(|task| task.principal_id.as_deref())
            .collect();
        assert_eq!(
            tasks.len(),
            2,
            "the first turn and the third wake run; the third superseded the \
             second while its registration failed: {principals:?}"
        );
        assert!(principals.contains(&Some("sender-1")));
        assert!(principals.contains(&Some("sender-3")));
        assert_eq!(*request_count.lock(), 2);
        server.abort();
    }

    /// Mock OpenAI-compatible provider whose every chat request signals
    /// `entered` and then waits for `release`. Returns the server handle, its
    /// address and the request counter.
    async fn gated_mock_provider(
        entered: &Arc<tokio::sync::Notify>,
        release: &Arc<tokio::sync::Notify>,
    ) -> (
        tokio::task::JoinHandle<()>,
        std::net::SocketAddr,
        Arc<Mutex<u32>>,
    ) {
        use axum::{Json, Router, extract::State, routing::post};

        #[derive(Clone)]
        struct RequestState {
            count: Arc<Mutex<u32>>,
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }
        async fn capture_chat(
            State(state): State<RequestState>,
            Json(_body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            *state.count.lock() += 1;
            state.entered.notify_one();
            state.release.notified().await;
            Json(serde_json::json!({
                "choices": [{"message": {"content": "ok"}}]
            }))
        }

        let count = Arc::new(Mutex::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock provider listener should bind");
        let addr = listener.local_addr().expect("mock provider addr");
        let app = Router::new()
            .route("/v1/chat/completions", post(capture_chat))
            .with_state(RequestState {
                count: Arc::clone(&count),
                entered: Arc::clone(entered),
                release: Arc::clone(release),
            });
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("mock provider server should run");
        });
        (server, addr, count)
    }

    /// `base_config` pointed at a mock provider, granting `recipient`.
    fn mock_provider_config(dir: &Path, addr: std::net::SocketAddr) -> Config {
        let mut config = base_config(dir);
        config.providers.models.ollama.insert(
            "default".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("mock-model".to_string()),
                    uri: Some(format!("http://{addr}")),
                    timeout_secs: Some(10),
                    ..ModelProviderConfig::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        config.plugins.entries = vec![entry(&instance_key(), &["recipient"], None)];
        config
    }

    /// A wake that coalesced onto a running turn was reported as accepted, so
    /// a transient failure registering its follow-up must not lose it. With
    /// no later wake to carry the slot, the worker must requeue the accepted
    /// wake, retry its registration, and run it exactly once.
    #[tokio::test]
    async fn accepted_wake_survives_a_transient_followup_registration_failure() {
        let provider_entered = Arc::new(tokio::sync::Notify::new());
        let provider_release = Arc::new(tokio::sync::Notify::new());
        let (server, mock_addr, request_count) =
            gated_mock_provider(&provider_entered, &provider_release).await;

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let config = mock_provider_config(workspace.path(), mock_addr);

        // Registration 1 is the first turn; 2 is the follow-up's first attempt
        // and fails once. Every later registration succeeds.
        let registry_entered = Arc::new(tokio::sync::Notify::new());
        let registry_release = Arc::new(tokio::sync::Notify::new());
        let (control_plane, store) =
            gated_control_plane(workspace.path(), 2, &registry_entered, &registry_release);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("first turn reaches mock provider");
        assert_eq!(
            backend
                .wake(&scope(), Some("sender-2"), "recipient", "second")
                .await,
            Ok(()),
            "the second wake is accepted onto the running turn"
        );

        // The first turn ends; the follow-up's registration fails once. No
        // later wake arrives to keep the slot busy.
        provider_release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), registry_entered.notified())
            .await
            .expect("the worker reaches the follow-up registration");
        registry_release.notify_one();

        // The accepted wake must still reach the provider after the retry.
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("the accepted wake's turn must run after a transient failure");
        provider_release.notify_one();

        await_slots_released(&coordinator).await;
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        let principals: Vec<_> = tasks
            .iter()
            .map(|task| task.principal_id.as_deref())
            .collect();
        assert_eq!(
            tasks.len(),
            2,
            "the first turn and the accepted wake each register once: {principals:?}"
        );
        assert!(principals.contains(&Some("sender-1")));
        assert!(principals.contains(&Some("sender-2")));
        for task in &tasks {
            let snapshot = await_terminal(store.as_ref(), &task.id).await;
            assert_eq!(snapshot.task.status, TaskStatus::Completed);
        }

        // Exactly once: nothing else runs after the slot is released.
        tokio::time::sleep(WAKE_STORE_RETRY_DELAY * 4).await;
        assert_eq!(*request_count.lock(), 2);
        assert_eq!(tasks_for(store.as_ref(), "recipient").await.len(), 2);
        server.abort();
    }

    /// A requeued wake is re-judged on live config before each retry. If the
    /// operator revokes the target while the failed registration is in
    /// flight, the retry must cancel the wake rather than run it.
    #[tokio::test]
    async fn requeued_wake_rechecks_a_grant_revoked_before_its_retry() {
        let provider_entered = Arc::new(tokio::sync::Notify::new());
        let provider_release = Arc::new(tokio::sync::Notify::new());
        let (server, mock_addr, request_count) =
            gated_mock_provider(&provider_entered, &provider_release).await;

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let key = instance_key();
        let live = zeroclaw_config::live::LiveConfig::new(mock_provider_config(
            workspace.path(),
            mock_addr,
        ));

        let registry_entered = Arc::new(tokio::sync::Notify::new());
        let registry_release = Arc::new(tokio::sync::Notify::new());
        let (control_plane, store) =
            gated_control_plane(workspace.path(), 2, &registry_entered, &registry_release);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(base_config(workspace.path())),
            Some(live.handle()),
            None,
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("first turn reaches mock provider");
        assert_eq!(
            backend
                .wake(&scope(), Some("sender-2"), "recipient", "second")
                .await,
            Ok(())
        );

        provider_release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), registry_entered.notified())
            .await
            .expect("the worker reaches the follow-up registration");

        // The operator revokes the target before the registration fails.
        let mut revoked = live.snapshot();
        revoked.plugins.entries = vec![entry(&key, &[], None)];
        let revision = live.next_revision().expect("revision available");
        live.publish(revision, revoked)
            .expect("publication accepted");
        registry_release.notify_one();

        await_slots_released(&coordinator).await;
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(
            tasks.len(),
            1,
            "a wake revoked before its retry registers nothing: {tasks:?}"
        );
        assert_eq!(tasks[0].principal_id.as_deref(), Some("sender-1"));
        assert_eq!(*request_count.lock(), 1, "the revoked wake never runs");
        server.abort();
    }

    /// A finished turn must not keep its lifecycle lease while the worker
    /// retries registering the next wake. Runs against the real lifecycle
    /// coordinator: while the store keeps refusing the follow-up's
    /// registration, the idle target must have no active turns and deleting
    /// it must be admitted. Once the store recovers, the accepted wake still
    /// runs.
    #[tokio::test]
    async fn registration_backoff_releases_the_finished_turns_lifecycle_lease() {
        let provider_entered = Arc::new(tokio::sync::Notify::new());
        let provider_release = Arc::new(tokio::sync::Notify::new());
        let (server, mock_addr, request_count) =
            gated_mock_provider(&provider_entered, &provider_release).await;

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let config = mock_provider_config(workspace.path(), mock_addr);
        let authority = crate::LiveConfigAuthority::new(config.clone());
        let lifecycle = authority.agent_lifecycle();

        let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failed = Arc::new(tokio::sync::Notify::new());
        let (control_plane, store) = failing_control_plane(workspace.path(), &failing, &failed);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            Some(authority.live_handle()),
            Some(authority.execution_capability()),
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("first turn reaches mock provider");
        assert_eq!(
            lifecycle.active_turn_count("recipient"),
            1,
            "the running turn holds a real lifecycle lease"
        );

        // The store goes down before the accepted follow-up registers.
        failing.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            backend
                .wake(&scope(), Some("sender-2"), "recipient", "second")
                .await,
            Ok(())
        );
        provider_release.notify_one();

        // Two failed attempts: the worker has settled the first turn and is
        // retrying. On this single-threaded runtime it is parked in its
        // backoff sleep whenever this task runs.
        for attempt in 1..=2 {
            tokio::time::timeout(Duration::from_secs(10), failed.notified())
                .await
                .unwrap_or_else(|_| panic!("registration attempt {attempt} must fail"));
        }
        assert_eq!(
            lifecycle.active_turn_count("recipient"),
            0,
            "a finished turn must not count as active during registration backoff"
        );
        let delete = lifecycle
            .begin_delete("recipient")
            .expect("an idle agent can be deleted during registration backoff");
        // Abandon the delete uncommitted: the alias stays and the probe ends.
        drop(delete);

        // The store recovers; the accepted wake retries and runs.
        failing.store(false, std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("the accepted wake runs once registration recovers");
        provider_release.notify_one();

        await_slots_released(&coordinator).await;
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        let principals: Vec<_> = tasks
            .iter()
            .map(|task| task.principal_id.as_deref())
            .collect();
        assert_eq!(tasks.len(), 2, "{principals:?}");
        assert!(principals.contains(&Some("sender-1")));
        assert!(principals.contains(&Some("sender-2")));
        for task in &tasks {
            let snapshot = await_terminal(store.as_ref(), &task.id).await;
            assert_eq!(snapshot.task.status, TaskStatus::Completed);
        }
        assert_eq!(*request_count.lock(), 2);
        assert_eq!(lifecycle.active_turn_count("recipient"), 0);
        server.abort();
    }

    /// A finished turn must not keep its lifecycle lease while the worker
    /// retries the turn's terminal write. Runs against the real lifecycle
    /// coordinator: while the store refuses the owner-checked settlement, the
    /// idle target must have no active turns and deleting it must be admitted.
    /// Once the deletion rolls back and the store recovers, the original task
    /// still settles.
    #[tokio::test]
    async fn settlement_backoff_releases_the_finished_turns_lifecycle_lease() {
        let provider_entered = Arc::new(tokio::sync::Notify::new());
        let provider_release = Arc::new(tokio::sync::Notify::new());
        let (server, mock_addr, request_count) =
            gated_mock_provider(&provider_entered, &provider_release).await;

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let config = mock_provider_config(workspace.path(), mock_addr);
        let authority = crate::LiveConfigAuthority::new(config.clone());
        let lifecycle = authority.agent_lifecycle();

        let settle_failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let settle_failed = Arc::new(tokio::sync::Notify::new());
        let (control_plane, store) =
            failing_settlement_control_plane(workspace.path(), &settle_failing, &settle_failed);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            Some(authority.live_handle()),
            Some(authority.execution_capability()),
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("the turn reaches mock provider");
        assert_eq!(lifecycle.active_turn_count("recipient"), 1);

        // The store goes down before the finished turn settles.
        settle_failing.store(true, std::sync::atomic::Ordering::SeqCst);
        provider_release.notify_one();
        for attempt in 1..=2 {
            tokio::time::timeout(Duration::from_secs(10), settle_failed.notified())
                .await
                .unwrap_or_else(|_| panic!("terminal write attempt {attempt} must fail"));
        }
        assert_eq!(
            lifecycle.active_turn_count("recipient"),
            0,
            "a finished turn must not count as active during settlement backoff"
        );
        let delete = lifecycle
            .begin_delete("recipient")
            .expect("an idle agent can be deleted during settlement backoff");
        // Roll the reservation back: the alias and its generation stay.
        drop(delete);

        // The store recovers; the original task settles with the turn's result.
        settle_failing.store(false, std::sync::atomic::Ordering::SeqCst);
        await_slots_released(&coordinator).await;
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        let snapshot = await_terminal(store.as_ref(), &tasks[0].id).await;
        assert_eq!(snapshot.task.status, TaskStatus::Completed);
        assert_eq!(*request_count.lock(), 1);
        assert_eq!(lifecycle.active_turn_count("recipient"), 0);
        server.abort();
    }

    /// A queued wake is pinned to the target generation it was accepted
    /// against. If the target is deleted (committed) and a replacement takes
    /// the alias before the wake's retry, a wildcard grant still matches the
    /// replacement, but the stale wake must be cancelled rather than hand the
    /// old caller's prompt to a different agent. A wake issued after the
    /// replacement exists reaches it normally.
    #[tokio::test]
    async fn committed_delete_cancels_a_queued_wake_but_not_a_fresh_one() {
        let provider_entered = Arc::new(tokio::sync::Notify::new());
        let provider_release = Arc::new(tokio::sync::Notify::new());
        let (server, mock_addr, request_count) =
            gated_mock_provider(&provider_entered, &provider_release).await;

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = mock_provider_config(workspace.path(), mock_addr);
        config.plugins.entries = vec![entry(&instance_key(), &["*"], None)];
        let authority = crate::LiveConfigAuthority::new(config.clone());
        let lifecycle = authority.agent_lifecycle();

        let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failed = Arc::new(tokio::sync::Notify::new());
        let (control_plane, store) = failing_control_plane(workspace.path(), &failing, &failed);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config.clone()),
            Some(authority.live_handle()),
            Some(authority.execution_capability()),
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("first turn reaches mock provider");

        // The store goes down; the second wake is accepted onto the running
        // turn and its follow-up registration keeps failing.
        failing.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            backend
                .wake(&scope(), Some("sender-2"), "recipient", "stale")
                .await,
            Ok(())
        );
        provider_release.notify_one();
        for attempt in 1..=2 {
            tokio::time::timeout(Duration::from_secs(10), failed.notified())
                .await
                .unwrap_or_else(|_| panic!("registration attempt {attempt} must fail"));
        }

        // Commit the deletion, then create a replacement under the same alias.
        // No await between these steps, so the worker cannot retry in between.
        let mut delete = lifecycle
            .begin_delete("recipient")
            .expect("an idle agent can be deleted during registration backoff");
        let mut removed = authority.snapshot_config();
        removed.agents.remove("recipient");
        authority.publish_for_test(removed);
        delete.commit_destructive_mutation();
        drop(delete);
        authority.publish_for_test(config.clone());

        // The store recovers. The stale wake's retry is refused at its pinned
        // generation and the worker releases the slot without running it.
        failing.store(false, std::sync::atomic::Ordering::SeqCst);
        await_slots_released(&coordinator).await;
        assert_eq!(
            *request_count.lock(),
            1,
            "the stale wake must never reach the replacement"
        );
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        assert_eq!(tasks[0].principal_id.as_deref(), Some("sender-1"));

        // A wake issued against the replacement runs on it.
        assert_eq!(
            backend
                .wake(&scope(), Some("sender-3"), "recipient", "fresh")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("a fresh wake reaches the replacement");
        provider_release.notify_one();
        await_slots_released(&coordinator).await;

        let tasks = tasks_for(store.as_ref(), "recipient").await;
        let principals: Vec<_> = tasks
            .iter()
            .map(|task| task.principal_id.as_deref())
            .collect();
        assert_eq!(tasks.len(), 2, "{principals:?}");
        assert!(principals.contains(&Some("sender-1")));
        assert!(principals.contains(&Some("sender-3")));
        for task in &tasks {
            let snapshot = await_terminal(store.as_ref(), &task.id).await;
            assert_eq!(snapshot.task.status, TaskStatus::Completed);
        }
        assert_eq!(*request_count.lock(), 2);
        server.abort();
    }

    /// The grant is resolved from one config read and admission supplies
    /// another. A publication landing between the two can disable the target
    /// or revoke its grant; the idle path must judge the admitted config and
    /// refuse before registering anything.
    ///
    /// The two reads are adjacent and synchronous in production, so the test
    /// gives the backend a pre-publication view while the authority already
    /// holds the published one: exactly what the wake observes when a
    /// publication lands between them.
    #[tokio::test]
    async fn idle_wake_judges_the_admitted_config_before_registering() {
        fn disable_target(config: &mut Config) {
            config
                .agents
                .get_mut("recipient")
                .expect("fixture defines recipient")
                .enabled = false;
        }
        fn revoke_grant(config: &mut Config) {
            config.plugins.entries = vec![entry(&instance_key(), &[], None)];
        }
        type Case = (&'static str, fn(&mut Config), AgentWakeError);
        let cases: [Case; 2] = [
            ("disabled", disable_target, AgentWakeError::UnknownAgent),
            ("revoked", revoke_grant, AgentWakeError::AccessDenied),
        ];

        for (label, publish, expected) in cases {
            let workspace = tempfile::TempDir::new().expect("temp data dir");
            let mut granted = base_config(workspace.path());
            granted.plugins.entries = vec![entry(&instance_key(), &["recipient"], None)];
            let mut published = granted.clone();
            publish(&mut published);
            let authority = crate::LiveConfigAuthority::new(published);
            let lifecycle = authority.agent_lifecycle();

            let control_plane =
                ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
            let store = Arc::clone(&control_plane.store);
            let coordinator = Arc::new(SharedWakeCoordinator::new());
            let backend = RuntimeAgentWakeBackend::for_tests(
                Arc::new(granted),
                None,
                Some(authority.execution_capability()),
                Some(control_plane),
                Arc::clone(&coordinator),
            );

            assert_eq!(
                backend
                    .wake(&scope(), Some("sender"), "recipient", "go")
                    .await,
                Err(expected),
                "{label}: the admitted config must refuse the wake"
            );
            assert_eq!(coordinator.slot_count(), 0, "{label}: slot released");
            assert!(
                tasks_for(store.as_ref(), "recipient").await.is_empty(),
                "{label}: a refused wake registers no task"
            );
            assert_eq!(
                lifecycle.active_turn_count("recipient"),
                0,
                "{label}: a refused wake keeps no lease"
            );
        }
    }

    /// A coalesced follow-up must judge its grant on the config it is admitted
    /// with, not on an earlier read. The backend's live view never sees the
    /// revocation; only the authority that admits the follow-up does, which is
    /// what the worker observes when the revocation lands between its reads.
    #[tokio::test]
    async fn followup_judges_its_grant_on_the_admitted_config() {
        let provider_entered = Arc::new(tokio::sync::Notify::new());
        let provider_release = Arc::new(tokio::sync::Notify::new());
        let (server, mock_addr, request_count) =
            gated_mock_provider(&provider_entered, &provider_release).await;

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let config = mock_provider_config(workspace.path(), mock_addr);
        let earlier_view = zeroclaw_config::live::LiveConfig::new(config.clone());
        let authority = crate::LiveConfigAuthority::new(config.clone());

        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let store = Arc::clone(&control_plane.store);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            Some(earlier_view.handle()),
            Some(authority.execution_capability()),
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), provider_entered.notified())
            .await
            .expect("first turn reaches mock provider");
        assert_eq!(
            backend
                .wake(&scope(), Some("sender-2"), "recipient", "second")
                .await,
            Ok(())
        );

        let mut revoked = authority.snapshot_config();
        revoked.plugins.entries = vec![entry(&instance_key(), &[], None)];
        authority.publish_for_test(revoked);
        provider_release.notify_one();

        await_slots_released(&coordinator).await;
        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(tasks.len(), 1, "the revoked follow-up registers nothing");
        assert_eq!(tasks[0].principal_id.as_deref(), Some("sender-1"));
        assert_eq!(*request_count.lock(), 1, "the revoked follow-up never runs");
        server.abort();
    }

    /// A headless SOP step that runs a dispatch-capable plugin must keep the
    /// woken turn inside the step's tool scope. Detached spawn drops
    /// task-locals, so without restoring the scope the target rebuilds its full
    /// surface (`send_message_to_peer`, `delegate` and `spawn_subagent` already
    /// carry it). Each wake carries its own caller's scope, so a coalesced
    /// follow-up runs under the scope of the call that queued it.
    #[tokio::test]
    async fn woken_turns_keep_each_callers_headless_step_scope() {
        use axum::{Json, Router, extract::State, routing::post};
        use zeroclaw_config::schema::SopConfig;

        #[derive(Clone)]
        struct RequestState {
            bodies: Arc<Mutex<Vec<serde_json::Value>>>,
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }
        async fn capture_chat(
            State(state): State<RequestState>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            state.bodies.lock().push(body);
            state.entered.notify_one();
            state.release.notified().await;
            Json(serde_json::json!({
                "choices": [{"message": {"content": "ok"}}]
            }))
        }
        fn offered(body: &serde_json::Value) -> Vec<String> {
            body["tools"]
                .as_array()
                .map(|tools| {
                    tools
                        .iter()
                        .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        }
        fn step_allowing(tool: &str) -> crate::sop::active_scope::HeadlessStepScope {
            crate::sop::active_scope::HeadlessStepScope {
                run_id: format!("run-{tool}"),
                step: crate::sop::SopStep {
                    number: 1,
                    scope: Some(crate::sop::StepToolScope {
                        allow: Some(vec![tool.to_string()]),
                        deny: Vec::new(),
                    }),
                    ..crate::sop::SopStep::default()
                },
                config: SopConfig {
                    step_scope_enforce: true,
                    ..SopConfig::default()
                },
            }
        }

        let bodies = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock provider listener should bind");
        let mock_addr = listener.local_addr().expect("mock provider addr");
        let app = Router::new()
            .route("/v1/chat/completions", post(capture_chat))
            .with_state(RequestState {
                bodies: Arc::clone(&bodies),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            });
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("mock provider server should run");
        });

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = base_config(workspace.path());
        config.providers.models.ollama.insert(
            "default".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("mock-model".to_string()),
                    uri: Some(format!("http://{mock_addr}")),
                    timeout_secs: Some(10),
                    ..ModelProviderConfig::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        config.plugins.entries = vec![entry(&instance_key(), &["recipient"], None)];
        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        // The first wake comes from a step that may only read files.
        assert_eq!(
            crate::sop::active_scope::with_active_headless_step_scope(
                step_allowing("file_read"),
                backend.wake(&scope(), Some("sender-1"), "recipient", "first"),
            )
            .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("first turn reaches mock provider");

        // A second wake from a step that may only search coalesces meanwhile.
        assert_eq!(
            crate::sop::active_scope::with_active_headless_step_scope(
                step_allowing("glob_search"),
                backend.wake(&scope(), Some("sender-2"), "recipient", "second"),
            )
            .await,
            Ok(())
        );
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("the coalesced follow-up reaches mock provider");
        release.notify_one();
        await_slots_released(&coordinator).await;

        let bodies = bodies.lock().clone();
        assert_eq!(bodies.len(), 2, "one request per woken turn");
        assert_eq!(
            offered(&bodies[0]),
            vec!["file_read".to_string()],
            "the first woken turn escaped its caller's step scope"
        );
        assert_eq!(
            offered(&bodies[1]),
            vec!["glob_search".to_string()],
            "the coalesced follow-up must run under the scope of the call that queued it"
        );
        server.abort();
    }

    #[tokio::test]
    async fn coalesced_followup_is_cancelled_when_target_is_disabled_mid_turn() {
        use axum::{Json, Router, extract::State, routing::post};

        #[derive(Clone)]
        struct RequestState {
            count: Arc<Mutex<u32>>,
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }
        async fn capture_chat(
            State(state): State<RequestState>,
            Json(_body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            *state.count.lock() += 1;
            state.entered.notify_one();
            state.release.notified().await;
            Json(serde_json::json!({
                "choices": [{"message": {"content": "ok"}}]
            }))
        }

        let request_count = Arc::new(Mutex::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock provider listener should bind");
        let mock_addr = listener.local_addr().expect("mock provider addr");
        let app = Router::new()
            .route("/v1/chat/completions", post(capture_chat))
            .with_state(RequestState {
                count: Arc::clone(&request_count),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            });
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("mock provider server should run");
        });

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = base_config(workspace.path());
        config.providers.models.ollama.insert(
            "default".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("mock-model".to_string()),
                    uri: Some(format!("http://{mock_addr}")),
                    timeout_secs: Some(10),
                    ..ModelProviderConfig::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        config.plugins.entries = vec![entry(&instance_key(), &["recipient"], Some(600))];

        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let store = Arc::clone(&control_plane.store);
        let authority = crate::LiveConfigAuthority::new(config.clone());
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            Some(authority.live_handle()),
            Some(authority.execution_capability()),
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("first turn reaches mock provider");

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-2"), "recipient", "second")
                .await,
            Ok(())
        );

        // Disable the target agent in live config while the first turn runs.
        let mut updated = authority.live_handle().read().clone();
        updated
            .agents
            .get_mut("recipient")
            .expect("recipient agent")
            .enabled = false;
        authority.publish_for_test(updated);

        release.notify_one();
        await_slots_released(&coordinator).await;

        let tasks = tasks_for(store.as_ref(), "recipient").await;
        assert_eq!(
            tasks.len(),
            1,
            "disabled target must cancel the pending follow-up turn: {tasks:?}"
        );
        assert_eq!(*request_count.lock(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn coalesced_followup_rebuilds_cost_context_from_fresh_config() {
        use crate::cost::CostTracker;
        use axum::{Json, Router, extract::State, routing::post};

        #[derive(Clone)]
        struct RequestState {
            count: Arc<Mutex<u32>>,
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }
        async fn capture_chat(
            State(state): State<RequestState>,
            Json(_body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            *state.count.lock() += 1;
            state.entered.notify_one();
            state.release.notified().await;
            Json(serde_json::json!({
                "choices": [{"message": {"content": "ok"}}],
                "usage": {"prompt_tokens": 1_000, "completion_tokens": 200}
            }))
        }

        let request_count = Arc::new(Mutex::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock provider listener should bind");
        let mock_addr = listener.local_addr().expect("mock provider addr");
        let app = Router::new()
            .route("/v1/chat/completions", post(capture_chat))
            .with_state(RequestState {
                count: Arc::clone(&request_count),
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            });
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("mock provider server should run");
        });

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = base_config(workspace.path());
        // First turn starts with cost tracking disabled (so first turn's
        // cost_ctx is None).
        config.cost = CostConfig {
            enabled: false,
            track_per_agent: true,
            ..CostConfig::default()
        };
        config.providers.models.ollama.insert(
            "default".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("mock-model".to_string()),
                    uri: Some(format!("http://{mock_addr}")),
                    timeout_secs: Some(10),
                    pricing: HashMap::from([
                        ("mock-model.input".to_string(), 3.0),
                        ("mock-model.output".to_string(), 15.0),
                    ]),
                    ..ModelProviderConfig::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        config.plugins.entries = vec![entry(&instance_key(), &["recipient"], Some(600))];

        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let store = Arc::clone(&control_plane.store);
        let authority = crate::LiveConfigAuthority::new(config.clone());
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config.clone()),
            Some(authority.live_handle()),
            Some(authority.execution_capability()),
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-1"), "recipient", "first")
                .await,
            Ok(())
        );
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("first turn reaches mock provider");

        assert_eq!(
            backend
                .wake(&scope(), Some("sender-2"), "recipient", "second")
                .await,
            Ok(())
        );

        // Enable cost tracking + updated pricing while the first turn runs.
        let mut updated = authority.live_handle().read().clone();
        updated.cost.enabled = true;
        updated.cost.track_per_agent = true;
        updated
            .providers
            .models
            .ollama
            .get_mut("default")
            .expect("ollama default")
            .base
            .pricing = HashMap::from([
            ("mock-model.input".to_string(), 6.0),
            ("mock-model.output".to_string(), 30.0),
        ]);
        authority.publish_for_test(updated.clone());

        release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("follow-up turn reaches mock provider");
        release.notify_one();

        let tasks = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let tasks = tasks_for(store.as_ref(), "recipient").await;
                if tasks.len() >= 2 && tasks.iter().all(|task| task.status.is_terminal()) {
                    return tasks;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both turns should complete");
        assert_eq!(tasks.len(), 2);
        await_slots_released(&coordinator).await;

        let tracker = CostTracker::get_or_init_global(updated.cost.clone(), &updated.data_dir)
            .expect("cost tracker should initialize once enabled");
        let mut recipient_summary = None;
        for _ in 0..20_000 {
            let summary = tracker
                .get_summary_for_agent("recipient")
                .expect("recipient cost summary should be queryable");
            if summary.request_count >= 1 {
                recipient_summary = Some(summary);
                break;
            }
            tokio::task::yield_now().await;
        }
        let recipient_summary =
            recipient_summary.expect("follow-up turn must record cost under fresh config");
        assert_eq!(
            recipient_summary.request_count, 1,
            "only the follow-up turn ran with cost tracking enabled"
        );
        let expected_cost = (1_000.0 * 6.0 / 1_000_000.0) + (200.0 * 30.0 / 1_000_000.0);
        assert!(
            (recipient_summary.session_cost_usd - expected_cost).abs() < 1e-9,
            "follow-up turn must use updated pricing ({} vs {expected_cost})",
            recipient_summary.session_cost_usd
        );
        server.abort();
    }

    /// Regression: an authorized cycle of `plugin wake -> peer message ->
    /// plugin wake` must preserve `PLUGIN_WAKE_DEPTH` across the detached
    /// `send_message_to_peer` recipient turn rather than resetting to `0` on
    /// the fresh Tokio task.
    #[cfg(feature = "plugins-wasm-cranelift")]
    #[tokio::test]
    async fn peer_message_turn_preserves_wake_depth_and_stops_cycle_at_ceiling() {
        use axum::{Json, Router, extract::State, routing::post};
        use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig};
        use zeroclaw_config::policy::AutonomyLevel;
        use zeroclaw_config::schema::{OpenAIModelProviderConfig, WireApi};

        let (plugin_root, plugin_manifest) = stage_dispatch_fixture_plugin();

        #[derive(Clone)]
        struct CycleState {
            alpha_calls: Arc<Mutex<u32>>,
            beta_calls: Arc<Mutex<u32>>,
            tool_results: Arc<Mutex<Vec<String>>>,
        }

        async fn scripted_alpha(
            State(state): State<CycleState>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            let has_tool_reply = messages.iter().any(|m| m["role"] == "tool");
            if has_tool_reply {
                return Json(serde_json::json!({
                    "choices": [{"message": {"content": "alpha done"}}]
                }));
            }
            let turn_num = {
                let mut count = state.alpha_calls.lock();
                *count += 1;
                *count
            };
            // Bound runaway loops in RED before the fix so the test finishes fast.
            if turn_num > 4 {
                return Json(serde_json::json!({
                    "choices": [{"message": {"content": "alpha safety stop"}}]
                }));
            }
            Json(serde_json::json!({
                "choices": [{"message": {
                    "content": "",
                    "tool_calls": [{
                        "id": format!("alpha-peer-{turn_num}"),
                        "type": "function",
                        "function": {
                            "name": "send_message_to_peer",
                            "arguments": serde_json::json!({
                                "channel": "telegram.prod",
                                "target": "beta",
                                "message": format!("hop-{turn_num}")
                            }).to_string()
                        }
                    }]
                }}]
            }))
        }

        async fn scripted_beta(
            State(state): State<CycleState>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            if let Some(tool_msg) = messages.iter().rev().find(|m| m["role"] == "tool") {
                if let Some(content) = tool_msg["content"].as_str() {
                    state.tool_results.lock().push(content.to_string());
                }
                return Json(serde_json::json!({
                    "choices": [{"message": {"content": "beta done"}}]
                }));
            }
            let turn_num = {
                let mut count = state.beta_calls.lock();
                *count += 1;
                *count
            };
            if turn_num > 4 {
                return Json(serde_json::json!({
                    "choices": [{"message": {"content": "beta safety stop"}}]
                }));
            }
            Json(serde_json::json!({
                "choices": [{"message": {
                    "content": "",
                    "tool_calls": [{
                        "id": format!("beta-wake-{turn_num}"),
                        "type": "function",
                        "function": {
                            "name": "dispatch_fixture",
                            "arguments": serde_json::json!({
                                "target": "alpha",
                                "prompt": format!("wake-alpha-{turn_num}")
                            }).to_string()
                        }
                    }]
                }}]
            }))
        }

        let alpha_calls = Arc::new(Mutex::new(0));
        let beta_calls = Arc::new(Mutex::new(0));
        let tool_results = Arc::new(Mutex::new(Vec::new()));
        let cycle_state = CycleState {
            alpha_calls: Arc::clone(&alpha_calls),
            beta_calls: Arc::clone(&beta_calls),
            tool_results: Arc::clone(&tool_results),
        };

        let alpha_state = cycle_state.clone();
        let alpha_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("alpha listener");
        let alpha_addr = alpha_listener.local_addr().expect("alpha addr");
        let alpha_server = zeroclaw_spawn::spawn!(async move {
            let app = Router::new()
                .route("/chat/completions", post(scripted_alpha))
                .with_state(alpha_state);
            axum::serve(alpha_listener, app)
                .await
                .expect("alpha mock server");
        });

        let beta_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("beta listener");
        let beta_addr = beta_listener.local_addr().expect("beta addr");
        let beta_server = zeroclaw_spawn::spawn!(async move {
            let app = Router::new()
                .route("/chat/completions", post(scripted_beta))
                .with_state(cycle_state);
            axum::serve(beta_listener, app)
                .await
                .expect("beta mock server");
        });

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = Config {
            data_dir: workspace.path().to_path_buf(),
            config_path: workspace.path().join("config.toml"),
            ..Config::default()
        };
        config.reliability.provider_retries = 0;
        config.plugins.enabled = true;
        config.plugins.auto_discover = true;
        config.plugins.plugins_dir = plugin_root.path().to_string_lossy().into_owned();
        config.tools.optional = vec!["send_message_to_peer".to_string()];

        let fixture_scope = PluginInstanceScope::for_package_binding(
            &plugin_manifest,
            PluginCapability::Tool,
            [PluginPermission::AgentDispatch],
        )
        .expect("fixture scope");
        let fixture_key = fixture_scope
            .id()
            .config_entry_key()
            .expect("fixture entry key");
        config.plugins.entries = vec![
            entry(&instance_key(), &["alpha"], Some(600)),
            entry(&fixture_key, &["alpha"], Some(600)),
        ];

        config.risk_profiles.insert(
            "full".to_string(),
            RiskProfileConfig {
                level: AutonomyLevel::Full,
                ..RiskProfileConfig::default()
            },
        );
        for (alias, addr) in [("alpha", alpha_addr), ("beta", beta_addr)] {
            config.providers.models.openai.insert(
                alias.to_string(),
                OpenAIModelProviderConfig {
                    base: ModelProviderConfig {
                        model: Some("mock-model".to_string()),
                        uri: Some(format!("http://{addr}")),
                        api_key: Some("fixture-key".to_string()),
                        wire_api: Some(WireApi::ChatCompletions),
                        timeout_secs: Some(10),
                        ..ModelProviderConfig::default()
                    },
                },
            );
            config.agents.insert(
                alias.to_string(),
                AliasedAgentConfig {
                    enabled: true,
                    channels: vec!["telegram.prod".into()],
                    model_provider: format!("openai.{alias}").into(),
                    risk_profile: "full".into(),
                    ..AliasedAgentConfig::default()
                },
            );
        }
        config.peer_groups.insert(
            "mesh".to_string(),
            PeerGroupConfig {
                channel: "telegram".into(),
                agents: vec![AgentAlias::new("alpha"), AgentAlias::new("beta")],
                ..PeerGroupConfig::default()
            },
        );

        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let store = Arc::clone(&control_plane.store);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        // Kick off the cycle:
        //   wake(alpha) -> alpha runs at depth 1 -> send_message_to_peer(beta)
        //   -> beta runs at inherited depth 1 -> dispatch_fixture(alpha)
        //   -> alpha runs at depth 2 -> send_message_to_peer(beta)
        //   -> beta runs at inherited depth 2 -> dispatch_fixture(alpha)
        //   -> MUST be refused with access-denied (depth == MAX_PLUGIN_WAKE_DEPTH == 2).
        assert_eq!(
            backend
                .wake(&scope(), Some("seed"), "alpha", "kickoff")
                .await,
            Ok(())
        );

        // Wait until beta has completed its second turn (recording two tool
        // results from `dispatch_fixture`: first `ok`, second `access-denied`)
        // and all tasks in `workspace` have settled.
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let results_len = tool_results.lock().len();
                let alpha_tasks = tasks_for(store.as_ref(), "alpha").await;
                let beta_tasks = tasks_for(store.as_ref(), "beta").await;
                let all_settled = !alpha_tasks.is_empty()
                    && alpha_tasks.iter().all(|t| t.status.is_terminal())
                    && beta_tasks.len() >= 2
                    && beta_tasks.iter().all(|t| t.status.is_terminal());
                if results_len >= 2 && all_settled {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("cycle should reach beta's second turn and settle");
        await_slots_released(&coordinator).await;

        let alpha_tasks = tasks_for(store.as_ref(), "alpha").await;
        let results = tool_results.lock().clone();

        alpha_server.abort();
        beta_server.abort();

        assert_eq!(
            alpha_tasks.len(),
            2,
            "alpha must run at depth 1 and depth 2 only, never beyond MAX_PLUGIN_WAKE_DEPTH: {alpha_tasks:?}"
        );
        assert_eq!(
            alpha_tasks.iter().map(|t| t.depth).collect::<Vec<_>>(),
            vec![1, 2],
            "second wake through peer turn must advance depth from 1 to 2 rather than resetting to 1"
        );
        assert_eq!(
            results.len(),
            2,
            "beta must invoke dispatch_fixture twice (accepted at depth 1, denied at depth 2): {results:?}"
        );
        assert!(
            results[1].contains("access-denied"),
            "wake from peer turn at MAX_PLUGIN_WAKE_DEPTH must fail with access-denied, got: {:?}",
            results
        );
    }

    /// Regression: background delegation (`delegate` with `background: true`)
    /// spawns a detached Tokio task; it must also inherit `PLUGIN_WAKE_DEPTH`
    /// rather than resetting to `0`.
    #[cfg(feature = "plugins-wasm-cranelift")]
    #[tokio::test]
    async fn background_delegate_turn_preserves_wake_depth_ceiling() {
        use axum::{Json, Router, extract::State, routing::post};
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::policy::AutonomyLevel;
        use zeroclaw_config::schema::{
            DelegateExecutionMode, DelegateTargetConfig, OpenAIModelProviderConfig,
            RuntimeProfileConfig, WireApi,
        };

        let (plugin_root, plugin_manifest) = stage_dispatch_fixture_plugin();

        #[derive(Clone)]
        struct BgState {
            worker_tool_results: Arc<Mutex<Vec<String>>>,
        }

        async fn scripted_leader(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            if messages.iter().any(|m| m["role"] == "tool") {
                return Json(serde_json::json!({
                    "choices": [{"message": {"content": "leader done"}}]
                }));
            }
            Json(serde_json::json!({
                "choices": [{"message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "bg-del-1",
                        "type": "function",
                        "function": {
                            "name": "delegate",
                            "arguments": serde_json::json!({
                                "agent": "worker",
                                "prompt": "try waking sink",
                                "background": true
                            }).to_string()
                        }
                    }]
                }}]
            }))
        }

        async fn scripted_worker(
            State(state): State<BgState>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            if let Some(tool_msg) = messages.iter().rev().find(|m| m["role"] == "tool") {
                if let Some(content) = tool_msg["content"].as_str() {
                    state.worker_tool_results.lock().push(content.to_string());
                }
                return Json(serde_json::json!({
                    "choices": [{"message": {"content": "worker done"}}]
                }));
            }
            Json(serde_json::json!({
                "choices": [{"message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "worker-wake-1",
                        "type": "function",
                        "function": {
                            "name": "dispatch_fixture",
                            "arguments": serde_json::json!({
                                "target": "sink",
                                "prompt": "should be denied at depth 2"
                            }).to_string()
                        }
                    }]
                }}]
            }))
        }

        let worker_tool_results = Arc::new(Mutex::new(Vec::new()));
        let leader_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let leader_addr = leader_listener.local_addr().unwrap();
        let leader_server = zeroclaw_spawn::spawn!(async move {
            let app = Router::new().route("/chat/completions", post(scripted_leader));
            axum::serve(leader_listener, app).await.unwrap();
        });

        let worker_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let worker_addr = worker_listener.local_addr().unwrap();
        let bg_state = BgState {
            worker_tool_results: Arc::clone(&worker_tool_results),
        };
        let worker_server = zeroclaw_spawn::spawn!(async move {
            let app = Router::new()
                .route("/chat/completions", post(scripted_worker))
                .with_state(bg_state);
            axum::serve(worker_listener, app).await.unwrap();
        });

        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = Config {
            data_dir: workspace.path().to_path_buf(),
            config_path: workspace.path().join("config.toml"),
            ..Config::default()
        };
        config.reliability.provider_retries = 0;
        config.plugins.enabled = true;
        config.plugins.auto_discover = true;
        config.plugins.plugins_dir = plugin_root.path().to_string_lossy().into_owned();
        config.tools.optional = vec!["delegate".to_string()];

        let fixture_scope = PluginInstanceScope::for_package_binding(
            &plugin_manifest,
            PluginCapability::Tool,
            [PluginPermission::AgentDispatch],
        )
        .expect("fixture scope");
        let fixture_key = fixture_scope
            .id()
            .config_entry_key()
            .expect("fixture entry key");
        config.plugins.entries = vec![
            entry(&instance_key(), &["leader"], Some(600)),
            entry(&fixture_key, &["sink"], Some(600)),
        ];

        config.risk_profiles.insert(
            "full".to_string(),
            RiskProfileConfig {
                level: AutonomyLevel::Full,
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "agentic".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                ..RuntimeProfileConfig::default()
            },
        );

        for (alias, uri) in [
            ("leader", format!("http://{leader_addr}")),
            ("worker", format!("http://{worker_addr}")),
            ("sink", "http://127.0.0.1:1".to_string()),
        ] {
            config.providers.models.openai.insert(
                alias.to_string(),
                OpenAIModelProviderConfig {
                    base: ModelProviderConfig {
                        model: Some("mock-model".to_string()),
                        uri: Some(uri),
                        api_key: Some("fixture-key".to_string()),
                        wire_api: Some(WireApi::ChatCompletions),
                        timeout_secs: Some(10),
                        ..ModelProviderConfig::default()
                    },
                },
            );
            let delegates = if alias == "leader" {
                vec![DelegateTargetConfig {
                    agent: "worker".into(),
                    mode: DelegateExecutionMode::Independent,
                }]
            } else {
                Vec::new()
            };
            config.agents.insert(
                alias.to_string(),
                AliasedAgentConfig {
                    enabled: true,
                    model_provider: format!("openai.{alias}").into(),
                    risk_profile: "full".into(),
                    runtime_profile: "agentic".into(),
                    delegates,
                    ..AliasedAgentConfig::default()
                },
            );
        }

        let control_plane =
            ControlPlaneHandle::open(workspace.path()).expect("isolated control plane");
        let store = Arc::clone(&control_plane.store);
        let coordinator = Arc::new(SharedWakeCoordinator::new());
        let backend = RuntimeAgentWakeBackend::for_tests(
            Arc::new(config),
            None,
            None,
            Some(control_plane),
            Arc::clone(&coordinator),
        );

        // Start `leader` at depth 2 by issuing wake from depth 1:
        // leader runs at depth 2 -> spawns background delegate `worker`
        // -> `worker` must inherit depth 2 and be denied when waking `sink`.
        zeroclaw_spawn::scope_plugin_wake_depth(MAX_PLUGIN_WAKE_DEPTH - 1, async {
            assert_eq!(
                backend
                    .wake(&scope(), Some("seed"), "leader", "run background worker")
                    .await,
                Ok(())
            );
        })
        .await;

        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if !worker_tool_results.lock().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("background worker should invoke dispatch_fixture");
        await_slots_released(&coordinator).await;

        let sink_tasks = tasks_for(store.as_ref(), "sink").await;
        let results = worker_tool_results.lock().clone();

        leader_server.abort();
        worker_server.abort();

        assert!(
            sink_tasks.is_empty(),
            "background delegate spawned from depth-2 wake turn must not wake sink: {sink_tasks:?}"
        );
        assert!(
            results[0].contains("access-denied"),
            "background delegate at MAX_PLUGIN_WAKE_DEPTH must observe access-denied, got: {results:?}"
        );
    }
}
