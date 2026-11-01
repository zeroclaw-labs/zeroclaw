//! Transport-neutral Goal execution boundary.
//!
//! This module deliberately does not select a transport driver from route
//! text. A trusted Matrix or ZeroCode adapter must supply the exact live
//! session driver with the typed ingress it derived before mutable hooks or
//! prompt handling. The host owns typed admission and the controller owns
//! guarded durable lifecycle transitions; later stages add execution,
//! accounting, and transport adapters behind those boundaries.

use std::{fmt, sync::Arc};

use anyhow::{Context, Error, Result, bail};
use async_trait::async_trait;
use chrono::Utc;
use uuid::Uuid;
use zeroclaw_api::{
    model_provider::ChatMessage,
    session_keys::sanitize_session_key,
    task::{TaskKind, TaskRecord, TaskStatus},
};
use zeroclaw_commands::goal::{
    GoalBudgetLimits, GoalBudgetSelection, GoalCommand, is_valid_finite_goal_cost_limit,
    is_valid_finite_goal_token_limit, validate_goal_objective,
};
use zeroclaw_config::goal::{GoalBudgetLimits as ConfigGoalBudgetLimits, GoalDefaultBudget};

use crate::task::{
    GoalAccountingState, GoalPauseReason, GoalPauseState, GoalTaskRecord, GoalTaskRegistry,
    GoalTransitionResult,
};

/// The only V1 surfaces permitted to admit a Goal command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalSurface {
    Matrix,
    ZeroCode,
}

/// A canonical session key owned by the trusted ingress surface.
///
/// Matrix supplies its existing canonical conversation-history key. ZeroCode
/// supplies the raw RPC session identifier, which is namespaced as `rpc_` for
/// the durable task-plane binding. The variants prevent an adapter from
/// mistaking a route-shaped string for a different transport's session.
#[derive(Clone, PartialEq, Eq)]
pub enum GoalSessionKey {
    Matrix { history_key: String },
    ZeroCode { raw_session_id: String },
}

impl fmt::Debug for GoalSessionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoalSessionKey")
            .field("surface", &self.surface())
            .finish()
    }
}

impl GoalSessionKey {
    /// Build a Matrix key from the adapter's existing canonical history key.
    ///
    /// Callers must pass the already-canonical Matrix key and must not
    /// pre-sanitize it. This preserves a one-to-one binding to the session
    /// identity produced by the Matrix history owner. That owner should scope
    /// Goal-capable Matrix history to the raw sender so independent Matrix
    /// users do not contend for one session. Goal control additionally checks
    /// the persisted exact raw MXID, because sanitized history-key components
    /// are not a sound authorization boundary.
    ///
    /// # Errors
    ///
    /// Returns an error for a blank or padded key, a key whose existing shared
    /// sanitizer would change it, or a key outside the `matrix_` namespace.
    pub fn matrix(history_key: impl Into<String>) -> Result<Self> {
        let key = Self::Matrix {
            history_key: history_key.into(),
        };
        key.validate_canonical()?;
        Ok(key)
    }

    /// Build a ZeroCode key from the adapter's canonical raw session ID.
    ///
    /// Callers must pass the already-canonical ID and must not pre-sanitize
    /// it. The durable `rpc_` namespace is applied only by [`Self::durable_id`]
    /// so this raw identifier cannot be confused with a Matrix history key.
    ///
    /// # Errors
    ///
    /// Returns an error for a blank or padded ID, or an ID whose existing
    /// shared sanitizer would change it.
    pub fn zero_code(raw_session_id: impl Into<String>) -> Result<Self> {
        let key = Self::ZeroCode {
            raw_session_id: raw_session_id.into(),
        };
        key.validate_canonical()?;
        Ok(key)
    }

    fn validate_canonical(&self) -> Result<()> {
        match self {
            Self::Matrix { history_key } => {
                require_canonical_nonblank("Matrix history key", history_key)?;
                if sanitize_session_key(history_key) != *history_key {
                    bail!("Matrix Goal history key is not canonical");
                }
                if !history_key.starts_with("matrix_") {
                    bail!("Matrix Goal history key is outside the Matrix namespace");
                }
            }
            Self::ZeroCode { raw_session_id } => {
                require_canonical_nonblank("ZeroCode session id", raw_session_id)?;
                if sanitize_session_key(raw_session_id) != *raw_session_id {
                    bail!("ZeroCode Goal session id is not canonical");
                }
            }
        }
        Ok(())
    }

    pub const fn surface(&self) -> GoalSurface {
        match self {
            Self::Matrix { .. } => GoalSurface::Matrix,
            Self::ZeroCode { .. } => GoalSurface::ZeroCode,
        }
    }

    /// Canonical task-plane session binding. This deliberately reuses the
    /// existing session namespaces rather than creating a Goal identity store.
    pub fn durable_id(&self) -> String {
        match self {
            Self::Matrix { history_key } => history_key.clone(),
            Self::ZeroCode { raw_session_id } => format!("rpc_{raw_session_id}"),
        }
    }
}

/// Immutable source identity captured by the trusted adapter. It is never a
/// hook-mutable generic channel field and `tui_id` is never durable identity.
#[derive(Clone, PartialEq, Eq)]
pub enum GoalIngressPrincipal {
    Matrix { raw_mxid: String },
    ZeroCode { tui_id: String },
}

impl fmt::Debug for GoalIngressPrincipal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoalIngressPrincipal")
            .field("surface", &self.surface())
            .finish()
    }
}

impl GoalIngressPrincipal {
    const fn surface(&self) -> GoalSurface {
        match self {
            Self::Matrix { .. } => GoalSurface::Matrix,
            Self::ZeroCode { .. } => GoalSurface::ZeroCode,
        }
    }

    fn validate(&self) -> Result<()> {
        let value = match self {
            Self::Matrix { raw_mxid } => raw_mxid,
            Self::ZeroCode { tui_id } => tui_id,
        };
        require_canonical_nonblank("Goal ingress principal", value)
    }
}

/// Trusted authority supplied alongside a parsed [`GoalCommand`].
#[derive(Clone, PartialEq, Eq)]
pub struct GoalIngressContext {
    session_key: GoalSessionKey,
    agent: String,
    route: String,
    principal: GoalIngressPrincipal,
}

impl fmt::Debug for GoalIngressContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoalIngressContext")
            .field("surface", &self.surface())
            .finish()
    }
}

impl GoalIngressContext {
    /// Build trusted ingress facts after an adapter has parsed a Goal command.
    ///
    /// The adapter, rather than user command text or a mutable hook, supplies
    /// every authority-bearing value.
    ///
    /// # Errors
    ///
    /// Returns an error if the principal surface differs from the session key,
    /// the directly constructed session key is not canonical, or if the agent,
    /// route, or principal is blank.
    pub fn trusted(
        session_key: GoalSessionKey,
        agent: impl Into<String>,
        route: impl Into<String>,
        principal: GoalIngressPrincipal,
    ) -> Result<Self> {
        if session_key.surface() != principal.surface() {
            bail!("Goal ingress principal does not match the session surface");
        }
        session_key.validate_canonical()?;
        principal.validate()?;
        Ok(Self {
            session_key,
            agent: required("Goal ingress agent", agent.into())?,
            route: required("Goal ingress route", route.into())?,
            principal,
        })
    }

    pub fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    pub const fn surface(&self) -> GoalSurface {
        self.session_key.surface()
    }

    pub fn agent(&self) -> &str {
        &self.agent
    }

    pub fn route(&self) -> &str {
        &self.route
    }

    fn durable_principal_id(&self) -> Option<&str> {
        match &self.principal {
            GoalIngressPrincipal::Matrix { raw_mxid } => Some(raw_mxid),
            GoalIngressPrincipal::ZeroCode { .. } => None,
        }
    }
}

/// Metadata about a live session returned by an already-selected driver.
///
/// The paired [`GoalSessionLease`] owns the guard that keeps the session
/// authoritative through a controller transition. The binding intentionally
/// carries no independent freshness state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalSessionBinding {
    session_key: GoalSessionKey,
}

impl GoalSessionBinding {
    /// Build the live-session binding returned by a selected surface driver.
    pub const fn new(session_key: GoalSessionKey) -> Self {
        Self { session_key }
    }

    pub fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }
}

/// Surface-owned live-session mechanics used by the shared Goal host.
///
/// The adapter chooses this object from trusted live-session state. The host
/// verifies its typed surface and canonical key before asking it to bind; it
/// never probes a registry using user-controlled route or session text.
#[async_trait]
pub trait GoalSessionDriver: Send + Sync {
    fn session_key(&self) -> &GoalSessionKey;

    /// Revalidate the exact live session and acquire its validity lease.
    ///
    /// The returned lease must keep the driver's authoritative live-session
    /// guard held until it is dropped. In particular, a reconnect or session
    /// replacement must not become effective while a submitted Goal command
    /// still owns this lease. The guard remains held across durable controller
    /// I/O, so it must be async-safe and must not be a lock that a Goal
    /// lifecycle path needs to acquire again.
    async fn bind(&self, ingress: &GoalIngressContext) -> Result<GoalSessionLease>;

    /// Acquire the exact session's foreground execution lease.
    ///
    /// The returned lease must retain the driver's authoritative live-session
    /// guard until it is dropped, exclusively serialize foreground work for
    /// this session, and tolerate being held across the executor's durable
    /// lifecycle and accounting I/O. It must not re-enter a lock that the
    /// Goal lifecycle needs. The executor treats `ingress` as admission-time
    /// audit data only; live policy stays with the driver and is revalidated
    /// there. `canonical_history` is read-only, parent and verifier calls
    /// mutate only process-local working state, and `append_verified_candidate`
    /// appends and delivers exactly one already-verified final candidate.
    /// The object is the sole transport bridge used by the later Goal executor.
    async fn acquire_execution(
        &self,
        ingress: &GoalIngressContext,
        scope: &GoalExecutionScope,
    ) -> Result<Box<dyn GoalSessionExecutionLease>>;
}

/// Controller-owned facts carried for one fenced Goal execution epoch.
///
/// The host validates the session binding only. The later executor performs
/// the durable task, epoch, and ingress-ownership fence before it asks a
/// driver for this lease.
#[derive(Clone)]
pub struct GoalExecutionScope {
    task_id: String,
    session_id: String,
    execution_epoch: i64,
}

impl fmt::Debug for GoalExecutionScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoalExecutionScope")
            .field("execution_epoch", &self.execution_epoch)
            .finish()
    }
}

impl GoalExecutionScope {
    /// Build controller-owned facts for one fenced Goal execution epoch.
    ///
    /// # Errors
    ///
    /// Returns an error for blank task/session IDs or a non-positive epoch.
    pub fn new(
        task_id: impl Into<String>,
        session_id: impl Into<String>,
        execution_epoch: i64,
    ) -> Result<Self> {
        if execution_epoch <= 0 {
            bail!("Goal execution epoch must be positive");
        }
        Ok(Self {
            task_id: canonical_nonblank("Goal execution task id", task_id.into())?,
            session_id: canonical_nonblank("Goal execution session id", session_id.into())?,
            execution_epoch,
        })
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub const fn execution_epoch(&self) -> i64 {
        self.execution_epoch
    }
}

/// Trusted input for a parent Goal turn.
#[derive(Clone)]
pub struct GoalParentTurn {
    pub objective: String,
    pub working_history: Vec<ChatMessage>,
}

impl fmt::Debug for GoalParentTurn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoalParentTurn")
            .field("working_history_len", &self.working_history.len())
            .finish()
    }
}

/// Trusted, isolated input for the mandatory Goal verifier.
#[derive(Clone)]
pub struct GoalVerifierTurn {
    pub objective: String,
    pub candidate: String,
}

impl fmt::Debug for GoalVerifierTurn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("GoalVerifierTurn").finish()
    }
}

/// Surface-owned foreground execution bridge. It retains the live-session
/// guard and one foreground slot for its lifetime, but has no lifecycle or
/// ledger authority; the later executor owns both around these calls.
#[async_trait]
pub trait GoalSessionExecutionLease: Send {
    /// Exact live session retained by this foreground lease.
    fn session_key(&self) -> &GoalSessionKey;
    fn canonical_history(&self) -> Result<Vec<ChatMessage>>;

    /// Append an operator response to the same canonical session before a
    /// resumed Goal is allowed to issue its next parent turn.  Implementors
    /// must make this durable using the exact surface session that their
    /// foreground lease has already revalidated.
    ///
    /// The conservative default deliberately rejects the operation: silently
    /// treating a resume response as process-local context would let it vanish
    /// across a restart and break the session-continuity contract.
    async fn append_resume_response(&mut self, _response: String) -> Result<()> {
        bail!("Goal session driver cannot durably append a resume response")
    }
    async fn run_parent_turn(
        &mut self,
        scope: &GoalExecutionScope,
        turn: GoalParentTurn,
    ) -> Result<String>;
    async fn run_verifier(
        &mut self,
        scope: &GoalExecutionScope,
        turn: GoalVerifierTurn,
    ) -> Result<String>;
    async fn append_verified_candidate(&mut self, candidate: String) -> Result<()>;
}

/// A driver's owned proof that a live session remains authoritative.
///
/// The guard is intentionally opaque to the Goal controller.  Its producer
/// owns the surface-specific reconnect and replacement mechanics, while the
/// host keeps it alive for the complete lifecycle transition.
pub struct GoalSessionLease {
    binding: GoalSessionBinding,
    validity_guard: Box<dyn Send + Sync>,
}

impl std::fmt::Debug for GoalSessionLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoalSessionLease")
            .field("binding", &self.binding)
            .field(
                "validity_guard_size",
                &std::mem::size_of_val(self.validity_guard.as_ref()),
            )
            .finish_non_exhaustive()
    }
}

impl GoalSessionLease {
    /// Construct a lease from a validated binding and surface-owned guard.
    ///
    /// `validity_guard` may be an owned lock, generation lease, or another
    /// surface-specific resource whose lifetime prevents a stale session from
    /// mutating Goal lifecycle state. The controller retains it across durable
    /// I/O and the submission may be borrowed by a `Send` future, so the
    /// resource must be `Send + Sync` and must not require a Goal lifecycle
    /// path to acquire the same lock re-entrantly.
    pub fn new<G>(binding: GoalSessionBinding, validity_guard: G) -> Self
    where
        G: Send + Sync + 'static,
    {
        Self {
            binding,
            validity_guard: Box::new(validity_guard),
        }
    }

    pub fn binding(&self) -> &GoalSessionBinding {
        &self.binding
    }
}

/// A Goal command admitted by the host for one controller decision.
///
/// An unavailable submission intentionally carries no driver or session lease:
/// disabled Goal Mode and the local help command must not contend a channel's
/// live-session guard. This is intentionally non-cloneable: duplicating a
/// bound submission could allow a stale authority proof to outlive the session
/// transition it protects.
pub struct GoalSubmission(GoalSubmissionState);

enum GoalSubmissionState {
    Unavailable {
        ingress: GoalIngressContext,
        command: GoalCommand,
    },
    Bound {
        ingress: GoalIngressContext,
        command: GoalCommand,
        /// The exact driver validated alongside `ingress`. Future execution
        /// must retain this object rather than resolving a new driver from
        /// route or session text after the durable lifecycle transition.
        driver: Arc<dyn GoalSessionDriver>,
        lease: GoalSessionLease,
    },
}

impl std::fmt::Debug for GoalSubmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (ingress, command, admitted) = match &self.0 {
            GoalSubmissionState::Unavailable { ingress, command } => (ingress, command, false),
            GoalSubmissionState::Bound {
                ingress, command, ..
            } => (ingress, command, true),
        };
        formatter
            .debug_struct("GoalSubmission")
            .field("surface", &ingress.surface())
            .field("command_kind", &goal_command_kind(command))
            .field("admitted", &admitted)
            .finish()
    }
}

fn goal_command_kind(command: &GoalCommand) -> &'static str {
    match command {
        GoalCommand::Start { .. } => "start",
        GoalCommand::Status => "status",
        GoalCommand::Budget => "budget",
        GoalCommand::SetBudget(_) => "budget_set",
        GoalCommand::Pause { .. } => "pause",
        GoalCommand::Resume { .. } => "resume",
        GoalCommand::Cancel => "cancel",
        GoalCommand::Help => "help",
    }
}

impl GoalSubmission {
    pub fn ingress(&self) -> &GoalIngressContext {
        match self {
            Self(GoalSubmissionState::Unavailable { ingress, .. })
            | Self(GoalSubmissionState::Bound { ingress, .. }) => ingress,
        }
    }

    pub fn command(&self) -> &GoalCommand {
        match self {
            Self(GoalSubmissionState::Unavailable { command, .. })
            | Self(GoalSubmissionState::Bound { command, .. }) => command,
        }
    }

    fn is_unavailable(&self) -> bool {
        matches!(self, Self(GoalSubmissionState::Unavailable { .. }))
    }
}

/// The transport-neutral authority boundary for Goal admission.
#[derive(Debug, Default)]
pub struct GoalExecutionHost;

impl GoalExecutionHost {
    pub const fn new() -> Self {
        Self
    }

    /// Validate a typed command against the exact trusted driver supplied by
    /// the caller. Local disabled/help responses return without binding; all
    /// other commands do not mutate lifecycle state or start model work.
    /// Build settings from the current canonical
    /// [`zeroclaw_config::schema::Config`] for each command;
    /// never reuse them for a later command or hold a config lock across an
    /// await. Start and Resume fail closed with the canonical typed Goal
    /// configuration error, while administrative commands do not consult the
    /// verifier.
    /// [`GoalController`] borrows the returned submission for durable
    /// transitions. A later executor consumes it to acquire the same exact
    /// driver; it must never resolve a value-equivalent replacement driver.
    pub async fn submit(
        &self,
        settings: &GoalHostSettings<'_>,
        ingress: GoalIngressContext,
        driver: Arc<dyn GoalSessionDriver>,
        command: GoalCommand,
    ) -> Result<GoalSubmission> {
        if !settings.enabled() || matches!(command, GoalCommand::Help) {
            return Ok(GoalSubmission(GoalSubmissionState::Unavailable {
                ingress,
                command,
            }));
        }
        if driver.session_key() != ingress.session_key() {
            bail!("Goal session driver does not match trusted ingress");
        }

        if matches!(
            command,
            GoalCommand::Start { .. } | GoalCommand::Resume { .. }
        ) {
            settings.require_execution_admissible()?;
        }

        let lease = driver.bind(&ingress).await?;
        if lease.binding().session_key() != ingress.session_key() {
            bail!("Goal session binding does not match trusted ingress");
        }

        Ok(GoalSubmission(GoalSubmissionState::Bound {
            ingress,
            command,
            driver,
            lease,
        }))
    }

    /// Acquire execution only through the same exact trusted driver boundary
    /// used for command admission. This never selects a driver from route text
    /// and refuses while Goal Mode is disabled. Build settings from the current
    /// canonical [`zeroclaw_config::schema::Config`] separately for each
    /// acquisition; never reuse them
    /// from command admission or hold a config lock across an await. Invalid
    /// Goal configuration fails closed with its canonical typed error.
    ///
    /// The admission lease is released immediately before the driver acquires
    /// foreground execution so a driver can take its own session guard. The
    /// driver must therefore revalidate the live session while acquiring that
    /// guard; it may not treat the earlier admission as a durable snapshot.
    /// This consumes `submission` even if acquisition fails; retry requires a
    /// fresh host admission after the executor has rechecked durable task,
    /// epoch, and ingress ownership.
    pub async fn acquire_execution(
        &self,
        settings: &GoalHostSettings<'_>,
        submission: GoalSubmission,
        scope: &GoalExecutionScope,
    ) -> Result<Box<dyn GoalSessionExecutionLease>> {
        if !settings.enabled() {
            bail!("Goal Mode is disabled");
        }
        let GoalSubmission(GoalSubmissionState::Bound {
            ingress,
            driver,
            lease,
            ..
        }) = submission
        else {
            bail!("Goal submission was not admitted");
        };
        if driver.session_key() != ingress.session_key() {
            bail!("Goal execution driver session key does not match trusted ingress");
        }
        if scope.session_id() != ingress.session_key().durable_id() {
            bail!("Goal execution scope session does not match trusted ingress");
        }
        settings.require_execution_admissible()?;
        drop(lease);
        let lease = driver.acquire_execution(&ingress, scope).await?;
        if lease.session_key() != ingress.session_key() {
            bail!("Goal execution lease session key does not match trusted ingress");
        }
        Ok(lease)
    }
}

fn required(name: &str, value: String) -> Result<String> {
    require_nonblank(name, &value)?;
    Ok(value.trim().to_owned())
}

fn require_nonblank(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{name} is blank");
    }
    Ok(())
}

fn canonical_nonblank(name: &str, value: String) -> Result<String> {
    require_canonical_nonblank(name, &value)?;
    Ok(value)
}

fn require_canonical_nonblank(name: &str, value: &str) -> Result<()> {
    require_nonblank(name, value)?;
    if value.trim() != value {
        bail!("{name} is not canonical");
    }
    Ok(())
}

/// Borrowed settings for one operation against the current canonical config.
pub struct GoalHostSettings<'a> {
    config: &'a zeroclaw_config::schema::Config,
    owner_pid: u32,
    owner_boot_id: String,
}

impl fmt::Debug for GoalHostSettings<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoalHostSettings")
            .field("enabled", &self.enabled())
            .field("owner_pid", &self.owner_pid)
            .finish()
    }
}

impl<'a> GoalHostSettings<'a> {
    /// Borrow settings from the canonical config current for this operation.
    ///
    /// Construct a fresh value for every command and every execution
    /// acquisition. Never reuse it across operations or borrow from a config
    /// lock held across `.await`.
    ///
    /// # Errors
    ///
    /// Returns an error if the owner boot ID is blank.
    pub fn from_config(
        config: &'a zeroclaw_config::schema::Config,
        owner_pid: u32,
        owner_boot_id: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            config,
            owner_pid,
            owner_boot_id: required("Goal owner boot id", owner_boot_id.into())?,
        })
    }

    fn enabled(&self) -> bool {
        self.config.goal.enabled
    }

    fn require_execution_admissible(&self) -> Result<()> {
        self.config
            .validate_goal_config()
            .context("Goal configuration does not admit execution")
    }

    fn default_limits(&self) -> Result<Option<GoalBudgetLimits>> {
        self.require_execution_admissible()?;
        match self
            .config
            .goal
            .default_budget()
            .map_err(|_| Error::msg("Goal default limits are invalid"))?
        {
            GoalDefaultBudget::Configured(limits) => {
                validate_default_limits(limits)?;
                Ok(Some(GoalBudgetLimits {
                    token_limit: limits.token_limit,
                    cost_limit_usd: limits.cost_limit_usd,
                }))
            }
            GoalDefaultBudget::Unconfigured => Ok(None),
        }
    }
}

fn validate_default_limits(limits: ConfigGoalBudgetLimits) -> Result<()> {
    if limits
        .token_limit
        .is_some_and(|limit| !is_valid_finite_goal_token_limit(limit))
    {
        bail!("Goal default token limit must be positive and SQLite-representable when finite");
    }
    if let Some(cost_limit_usd) = limits.cost_limit_usd
        && !is_valid_finite_goal_cost_limit(cost_limit_usd)
    {
        bail!("Goal default cost limit must be finite and positive when finite");
    }
    Ok(())
}

/// Controller response before a transport renders it through Fluent.
#[derive(Debug, Clone, PartialEq)]
pub enum GoalResponse {
    Help,
    Disabled,
    Started(GoalStatusProjection),
    Status(GoalStatusProjection),
    Budget(GoalStatusProjection),
    BudgetUpdated(GoalStatusProjection),
    Paused(GoalStatusProjection),
    AlreadyPaused(GoalStatusProjection),
    Resumed(GoalStatusProjection),
    Cancelled(GoalStatusProjection),
    AlreadyCancelled(GoalStatusProjection),
    NoCurrentGoal,
    AlreadyActive,
    /// A start without budget flags found no configured default budget; no Goal was created.
    BudgetRequired,
    Terminal(GoalStatusProjection),
    Stale,
}

/// Durable state visible to the future Matrix and ZeroCode renderers.
#[derive(Debug, Clone, PartialEq)]
pub struct GoalStatusProjection {
    pub task_id: String,
    pub status: TaskStatus,
    pub execution_epoch: i64,
    pub token_limit: Option<u64>,
    pub cost_limit_usd: Option<f64>,
    pub accounting_state: GoalAccountingState,
    pub pause_reason: Option<GoalPauseReason>,
    pub resumable: bool,
}

impl GoalStatusProjection {
    fn from_parts(task: &TaskRecord, goal: &GoalTaskRecord) -> Self {
        Self {
            task_id: task.id.clone(),
            status: task.status,
            execution_epoch: task.execution_epoch,
            token_limit: goal.effective_token_limit,
            cost_limit_usd: goal.effective_cost_limit_usd,
            accounting_state: goal.accounting_state,
            pause_reason: goal.pause_reason,
            resumable: goal_is_resumable(task, goal),
        }
    }
}

/// Keep the renderer-facing resume hint aligned with the durable resume CAS.
/// It is deliberately conservative: a paired pending operation and an
/// exhausted epoch are both non-resumable even though the SQLite constraints
/// normally make those combinations unreachable in a readable paused row.
fn goal_is_resumable(task: &TaskRecord, goal: &GoalTaskRecord) -> bool {
    task.status == TaskStatus::Paused
        && task.execution_epoch < i64::MAX
        && goal.accounting_state == GoalAccountingState::Complete
        && goal.pending_call_id.is_none()
        && goal.pending_call_epoch.is_none()
}

/// Transport-neutral lifecycle controller. Its input is opaque outside this
/// module, so callers must pass through [`GoalExecutionHost`] first.
pub struct GoalController {
    registry: Arc<dyn GoalTaskRegistry>,
}

impl GoalController {
    pub fn new(registry: Arc<dyn GoalTaskRegistry>) -> Self {
        Self { registry }
    }

    /// Apply a previously host-validated Goal submission through guarded
    /// durable transitions without consuming its exact driver proof. Construct
    /// settings from the current canonical
    /// [`zeroclaw_config::schema::Config`] for each command and do
    /// not reuse them across commands or hold a config lock across `.await`.
    /// Start and Resume fail closed with the canonical typed Goal
    /// configuration error; administrative commands do not consult the
    /// verifier.
    ///
    /// The caller must keep `settings` current for this admission. A disabled
    /// setting returns [`GoalResponse::Disabled`] for every command, local help
    /// included, without a lifecycle change.
    pub async fn submit(
        &self,
        settings: &GoalHostSettings<'_>,
        submission: &GoalSubmission,
    ) -> Result<GoalResponse> {
        if !settings.enabled() {
            return Ok(GoalResponse::Disabled);
        }
        if submission.is_unavailable() {
            return Ok(match submission.command() {
                GoalCommand::Help => GoalResponse::Help,
                _ => GoalResponse::Disabled,
            });
        }
        match submission.command() {
            GoalCommand::Help => Ok(GoalResponse::Help),
            GoalCommand::Start { budget, objective } => {
                self.start(settings, submission.ingress(), *budget, objective)
                    .await
            }
            GoalCommand::Status => self.status(submission.ingress(), false).await,
            GoalCommand::Budget => self.status(submission.ingress(), true).await,
            GoalCommand::SetBudget(selection) => {
                self.set_budget(submission.ingress(), *selection).await
            }
            GoalCommand::Pause { .. } => self.pause(submission.ingress()).await,
            GoalCommand::Resume { .. } => self.resume(settings, submission.ingress()).await,
            GoalCommand::Cancel => self.cancel(submission.ingress()).await,
        }
    }

    async fn start(
        &self,
        settings: &GoalHostSettings<'_>,
        ingress: &GoalIngressContext,
        selection: GoalBudgetSelection,
        objective: &str,
    ) -> Result<GoalResponse> {
        settings.require_execution_admissible()?;
        validate_goal_objective(objective).map_err(|error| Error::msg(format!("{error:?}")))?;
        let Some(limits) = select_limits(|| settings.default_limits(), selection)? else {
            return Ok(GoalResponse::BudgetRequired);
        };
        let session_id = ingress.session_key().durable_id();
        let task_id = Uuid::new_v4().to_string();
        let task = TaskRecord {
            id: task_id.clone(),
            kind: TaskKind::Goal,
            agent: ingress.agent().to_owned(),
            status: TaskStatus::Running,
            owner_pid: settings.owner_pid,
            owner_boot_id: settings.owner_boot_id.clone(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: Some(ingress.route().to_owned()),
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: ingress.durable_principal_id().map(ToOwned::to_owned),
            session_id: Some(session_id.clone()),
            execution_epoch: 1,
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
        };
        let goal = GoalTaskRecord {
            task_id: task_id.clone(),
            objective: objective.to_owned(),
            effective_token_limit: limits.token_limit,
            effective_cost_limit_usd: limits.cost_limit_usd,
            ..GoalTaskRecord::default()
        };
        match self
            .registry
            .create_or_replace_session_goal(task, goal)
            .await?
        {
            GoalTransitionResult::Applied => {
                self.projection_response(ingress, &session_id, &task_id, GoalResponse::Started)
                    .await
            }
            GoalTransitionResult::Stale => self.start_stale_response(ingress, &session_id).await,
            GoalTransitionResult::Missing => Ok(GoalResponse::Stale),
        }
    }

    /// A failed start CAS has more than one durable cause. Reload before
    /// presenting it: only a currently non-terminal Goal is actually active.
    async fn start_stale_response(
        &self,
        ingress: &GoalIngressContext,
        session_id: &str,
    ) -> Result<GoalResponse> {
        let Some(task) = self.registry.current_goal_for_session(session_id).await? else {
            return Ok(GoalResponse::Stale);
        };
        if !ingress_owns_task(ingress, &task) {
            // A Matrix principal that does not own this persisted Goal must
            // neither learn its state nor receive a retry-shaped response for
            // an authorization conflict.
            return Ok(GoalResponse::NoCurrentGoal);
        }
        if !task.status.is_terminal() {
            return Ok(GoalResponse::AlreadyActive);
        }
        self.projection_response_for_current(ingress, session_id, &task, GoalResponse::Terminal)
            .await
    }

    async fn status(&self, ingress: &GoalIngressContext, budget: bool) -> Result<GoalResponse> {
        let session_id = ingress.session_key().durable_id();
        let Some(task) = self.current(ingress, &session_id).await? else {
            return Ok(GoalResponse::NoCurrentGoal);
        };
        let Some(projection) = self.project_current(ingress, &session_id, &task).await? else {
            return Ok(GoalResponse::Stale);
        };
        if task.status.is_terminal() {
            Ok(GoalResponse::Terminal(projection))
        } else if budget {
            Ok(GoalResponse::Budget(projection))
        } else {
            Ok(GoalResponse::Status(projection))
        }
    }

    async fn set_budget(
        &self,
        ingress: &GoalIngressContext,
        selection: GoalBudgetSelection,
    ) -> Result<GoalResponse> {
        let session_id = ingress.session_key().durable_id();
        let Some(task) = self.current(ingress, &session_id).await? else {
            return Ok(GoalResponse::NoCurrentGoal);
        };
        if task.status.is_terminal() {
            return self
                .projection_response_for_current(
                    ingress,
                    &session_id,
                    &task,
                    GoalResponse::Terminal,
                )
                .await;
        }
        let limits = select_budget_update_limits(selection)?;
        match self
            .registry
            .update_session_goal_limits(
                &task.id,
                &session_id,
                task.execution_epoch,
                limits.token_limit,
                limits.cost_limit_usd,
            )
            .await?
        {
            GoalTransitionResult::Applied => {
                self.projection_response(
                    ingress,
                    &session_id,
                    &task.id,
                    GoalResponse::BudgetUpdated,
                )
                .await
            }
            GoalTransitionResult::Stale | GoalTransitionResult::Missing => Ok(GoalResponse::Stale),
        }
    }

    async fn pause(&self, ingress: &GoalIngressContext) -> Result<GoalResponse> {
        let session_id = ingress.session_key().durable_id();
        let Some(task) = self.current(ingress, &session_id).await? else {
            return Ok(GoalResponse::NoCurrentGoal);
        };
        if task.status == TaskStatus::Paused {
            return self
                .projection_response_for_current(
                    ingress,
                    &session_id,
                    &task,
                    GoalResponse::AlreadyPaused,
                )
                .await;
        }
        if task.status.is_terminal() {
            return self
                .projection_response_for_current(
                    ingress,
                    &session_id,
                    &task,
                    GoalResponse::Terminal,
                )
                .await;
        }
        match self
            .registry
            .pause_session_goal(
                &task.id,
                &session_id,
                task.execution_epoch,
                GoalPauseState {
                    reason: GoalPauseReason::OperatorPaused,
                    description: None,
                    blockers: Vec::new(),
                },
            )
            .await?
        {
            GoalTransitionResult::Applied => {
                self.projection_response(ingress, &session_id, &task.id, GoalResponse::Paused)
                    .await
            }
            GoalTransitionResult::Stale | GoalTransitionResult::Missing => Ok(GoalResponse::Stale),
        }
    }

    async fn resume(
        &self,
        settings: &GoalHostSettings<'_>,
        ingress: &GoalIngressContext,
    ) -> Result<GoalResponse> {
        settings.require_execution_admissible()?;
        let session_id = ingress.session_key().durable_id();
        let Some(task) = self.current(ingress, &session_id).await? else {
            return Ok(GoalResponse::NoCurrentGoal);
        };
        if task.status.is_terminal() {
            return self
                .projection_response_for_current(
                    ingress,
                    &session_id,
                    &task,
                    GoalResponse::Terminal,
                )
                .await;
        }
        if task.status != TaskStatus::Paused {
            return Ok(GoalResponse::AlreadyActive);
        }
        match self
            .registry
            .resume_session_goal(
                &task.id,
                &session_id,
                task.execution_epoch,
                settings.owner_pid,
                &settings.owner_boot_id,
            )
            .await?
        {
            GoalTransitionResult::Applied => {
                self.projection_response(ingress, &session_id, &task.id, GoalResponse::Resumed)
                    .await
            }
            GoalTransitionResult::Stale => self.resume_stale_response(ingress, &session_id).await,
            GoalTransitionResult::Missing => Ok(GoalResponse::Stale),
        }
    }

    /// Resume has additional durable accounting predicates beyond `Paused`.
    /// If one changes between the pre-check and the CAS, return the canonical
    /// state rather than asking the operator to retry an unchanged condition.
    async fn resume_stale_response(
        &self,
        ingress: &GoalIngressContext,
        session_id: &str,
    ) -> Result<GoalResponse> {
        let Some(task) = self.current(ingress, session_id).await? else {
            return Ok(GoalResponse::Stale);
        };
        if task.status.is_terminal() {
            return self
                .projection_response_for_current(ingress, session_id, &task, GoalResponse::Terminal)
                .await;
        }
        self.projection_response_for_current(ingress, session_id, &task, GoalResponse::Status)
            .await
    }

    async fn cancel(&self, ingress: &GoalIngressContext) -> Result<GoalResponse> {
        let session_id = ingress.session_key().durable_id();
        let Some(task) = self.current(ingress, &session_id).await? else {
            return Ok(GoalResponse::NoCurrentGoal);
        };
        if task.status == TaskStatus::Cancelled {
            return self
                .projection_response_for_current(
                    ingress,
                    &session_id,
                    &task,
                    GoalResponse::AlreadyCancelled,
                )
                .await;
        }
        if task.status.is_terminal() {
            return self
                .projection_response_for_current(
                    ingress,
                    &session_id,
                    &task,
                    GoalResponse::Terminal,
                )
                .await;
        }
        match self
            .registry
            .finish_session_goal(
                &task.id,
                &session_id,
                task.execution_epoch,
                TaskStatus::Cancelled,
                None,
            )
            .await?
        {
            GoalTransitionResult::Applied => {
                self.projection_response(ingress, &session_id, &task.id, GoalResponse::Cancelled)
                    .await
            }
            GoalTransitionResult::Stale | GoalTransitionResult::Missing => Ok(GoalResponse::Stale),
        }
    }

    async fn current(
        &self,
        ingress: &GoalIngressContext,
        session_id: &str,
    ) -> Result<Option<TaskRecord>> {
        let Some(task) = self.registry.current_goal_for_session(session_id).await? else {
            return Ok(None);
        };
        // Matrix history keys are filesystem-safe and therefore lossy. The
        // exact authenticated MXID is the durable authority for an existing
        // Matrix Goal, while a ZeroCode tui_id remains transient continuity.
        if !ingress_owns_task(ingress, &task) {
            return Ok(None);
        }
        Ok(Some(task))
    }

    async fn projection_for(
        &self,
        ingress: &GoalIngressContext,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<GoalStatusProjection>> {
        let Some(task) = self.current(ingress, session_id).await? else {
            return Ok(None);
        };
        if task.id != task_id {
            return Ok(None);
        }
        self.project_current(ingress, session_id, &task).await
    }

    async fn project_current(
        &self,
        ingress: &GoalIngressContext,
        session_id: &str,
        task: &TaskRecord,
    ) -> Result<Option<GoalStatusProjection>> {
        match self.registry.get_goal_task(&task.id).await? {
            Some(goal) => {
                // The extension read can overlap a lifecycle transition. Fence
                // the projection with a fresh canonical task snapshot so its
                // status and epoch cannot come from different moments.
                let current = self.current(ingress, session_id).await?;
                if current
                    .as_ref()
                    .is_none_or(|current| !same_task_snapshot(current, task))
                {
                    return Ok(None);
                }
                Ok(Some(GoalStatusProjection::from_parts(task, &goal)))
            }
            None => {
                // A terminal predecessor can be atomically replaced between
                // the current-task read and extension read. Keep reporting a
                // genuine missing-extension error only while the same task
                // snapshot remains current.
                let current = self.current(ingress, session_id).await?;
                if current
                    .as_ref()
                    .is_none_or(|current| !same_task_snapshot(current, task))
                {
                    Ok(None)
                } else {
                    bail!("Goal task extension is missing")
                }
            }
        }
    }

    async fn projection_response(
        &self,
        ingress: &GoalIngressContext,
        session_id: &str,
        task_id: &str,
        response: impl FnOnce(GoalStatusProjection) -> GoalResponse,
    ) -> Result<GoalResponse> {
        let projection = self.projection_for(ingress, session_id, task_id).await?;
        Ok(projection.map_or(GoalResponse::Stale, response))
    }

    async fn projection_response_for_current(
        &self,
        ingress: &GoalIngressContext,
        session_id: &str,
        task: &TaskRecord,
        response: impl FnOnce(GoalStatusProjection) -> GoalResponse,
    ) -> Result<GoalResponse> {
        Ok(self
            .project_current(ingress, session_id, task)
            .await?
            .map_or(GoalResponse::Stale, response))
    }
}

fn same_task_snapshot(current: &TaskRecord, original: &TaskRecord) -> bool {
    current.id == original.id
        && current.status == original.status
        && current.execution_epoch == original.execution_epoch
}

fn ingress_owns_task(ingress: &GoalIngressContext, task: &TaskRecord) -> bool {
    ingress.surface() != GoalSurface::Matrix
        || task.principal_id.as_deref() == ingress.durable_principal_id()
}

fn select_limits(
    defaults: impl FnOnce() -> Result<Option<GoalBudgetLimits>>,
    selection: GoalBudgetSelection,
) -> Result<Option<GoalBudgetLimits>> {
    match selection {
        GoalBudgetSelection::Defaults => defaults(),
        GoalBudgetSelection::Limits(limits) => validate_command_limits(limits).map(Some),
        GoalBudgetSelection::Unlimited => Ok(Some(GoalBudgetLimits {
            token_limit: None,
            cost_limit_usd: None,
        })),
    }
}

fn select_budget_update_limits(selection: GoalBudgetSelection) -> Result<GoalBudgetLimits> {
    match selection {
        GoalBudgetSelection::Defaults => bail!("Goal budget set cannot use configured defaults"),
        GoalBudgetSelection::Limits(limits) => validate_command_limits(limits),
        GoalBudgetSelection::Unlimited => Ok(GoalBudgetLimits {
            token_limit: None,
            cost_limit_usd: None,
        }),
    }
}

fn validate_command_limits(limits: GoalBudgetLimits) -> Result<GoalBudgetLimits> {
    if limits
        .token_limit
        .is_some_and(|limit| !is_valid_finite_goal_token_limit(limit))
        || limits
            .cost_limit_usd
            .is_some_and(|cost| !is_valid_finite_goal_cost_limit(cost))
        || (limits.token_limit.is_none() && limits.cost_limit_usd.is_none())
    {
        bail!("Goal finite budget limits are invalid");
    }
    Ok(limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_budget_update_cannot_smuggle_configured_defaults() {
        assert!(select_budget_update_limits(GoalBudgetSelection::Defaults).is_err());
    }

    #[test]
    fn typed_finite_limits_cannot_bypass_semantic_validation() {
        for limits in [
            GoalBudgetLimits {
                token_limit: None,
                cost_limit_usd: None,
            },
            GoalBudgetLimits {
                token_limit: Some(0),
                cost_limit_usd: None,
            },
            GoalBudgetLimits {
                token_limit: Some(i64::MAX as u64 + 1),
                cost_limit_usd: None,
            },
            GoalBudgetLimits {
                token_limit: None,
                cost_limit_usd: Some(0.0),
            },
            GoalBudgetLimits {
                token_limit: None,
                cost_limit_usd: Some(f64::NAN),
            },
        ] {
            assert!(
                select_limits(
                    || Ok(Some(GoalBudgetLimits {
                        token_limit: Some(1),
                        cost_limit_usd: None,
                    })),
                    GoalBudgetSelection::Limits(limits),
                )
                .is_err()
            );
            assert!(select_budget_update_limits(GoalBudgetSelection::Limits(limits)).is_err());
        }
    }

    #[test]
    fn typed_start_objective_cannot_bypass_semantic_validation() {
        assert!(validate_goal_objective(" \t\n").is_err());
        assert!(
            validate_goal_objective(
                &"a".repeat(zeroclaw_commands::goal::MAX_GOAL_OBJECTIVE_CHARS + 1)
            )
            .is_err()
        );
    }

    #[test]
    fn matrix_key_must_keep_the_existing_matrix_history_namespace() {
        assert!(GoalSessionKey::matrix("rpc_shared").is_err());
        assert!(GoalSessionKey::matrix("matrix_primary_room").is_ok());
    }

    #[test]
    fn zerocode_key_must_keep_the_existing_canonical_session_form() {
        assert!(GoalSessionKey::zero_code("same session").is_err());
        assert!(GoalSessionKey::zero_code("same-session").is_ok());
    }

    #[test]
    fn trusted_ingress_rejects_noncanonical_public_session_variants() {
        for session_key in [
            GoalSessionKey::Matrix {
                history_key: "rpc_shared".into(),
            },
            GoalSessionKey::Matrix {
                history_key: "".into(),
            },
            GoalSessionKey::Matrix {
                history_key: " matrix_room ".into(),
            },
            GoalSessionKey::Matrix {
                history_key: "matrix_room with spaces".into(),
            },
            GoalSessionKey::ZeroCode {
                raw_session_id: "same session".into(),
            },
            GoalSessionKey::ZeroCode {
                raw_session_id: " padded ".into(),
            },
            GoalSessionKey::ZeroCode {
                raw_session_id: "".into(),
            },
        ] {
            let principal = match session_key.surface() {
                GoalSurface::Matrix => GoalIngressPrincipal::Matrix {
                    raw_mxid: "@zeroclaw_user:example.org".into(),
                },
                GoalSurface::ZeroCode => GoalIngressPrincipal::ZeroCode {
                    tui_id: "client-1".into(),
                },
            };
            assert!(GoalIngressContext::trusted(session_key, "main", "direct", principal).is_err());
        }
    }

    #[test]
    fn trusted_ingress_preserves_valid_canonical_session_variants() {
        for (session_key, expected_durable_id) in [
            (
                GoalSessionKey::Matrix {
                    history_key: "matrix_room_primary".into(),
                },
                "matrix_room_primary",
            ),
            (
                GoalSessionKey::ZeroCode {
                    raw_session_id: "same-session".into(),
                },
                "rpc_same-session",
            ),
        ] {
            let expected_session_key = session_key.clone();
            let expected_surface = session_key.surface();
            let principal = match expected_surface {
                GoalSurface::Matrix => GoalIngressPrincipal::Matrix {
                    raw_mxid: "@zeroclaw_user:example.org".into(),
                },
                GoalSurface::ZeroCode => GoalIngressPrincipal::ZeroCode {
                    tui_id: "client-1".into(),
                },
            };
            let ingress =
                GoalIngressContext::trusted(session_key, "main", "direct", principal).unwrap();
            assert_eq!(ingress.surface(), expected_surface);
            assert_eq!(ingress.session_key(), &expected_session_key);
            assert_eq!(ingress.session_key().durable_id(), expected_durable_id);
        }
    }

    #[test]
    fn resumable_projection_includes_every_durable_resume_precondition() {
        let mut task = TaskRecord {
            id: "goal-1".into(),
            kind: TaskKind::Goal,
            agent: "main".into(),
            status: TaskStatus::Paused,
            owner_pid: 1,
            owner_boot_id: "boot".into(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: None,
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: None,
            session_id: Some("matrix_session".into()),
            execution_epoch: 1,
            started_at: "2026-01-01T00:00:00Z".into(),
            finished_at: None,
        };
        let mut goal = GoalTaskRecord {
            task_id: task.id.clone(),
            objective: "stop when complete".into(),
            ..GoalTaskRecord::default()
        };

        assert!(goal_is_resumable(&task, &goal));
        goal.pending_call_id = Some("operation-1".into());
        goal.pending_call_epoch = Some(1);
        assert!(!goal_is_resumable(&task, &goal));

        goal.pending_call_id = None;
        goal.pending_call_epoch = None;
        task.execution_epoch = i64::MAX;
        assert!(!goal_is_resumable(&task, &goal));
    }
}
