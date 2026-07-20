use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use super::scope::StepToolScope;
use super::step_contract::{StepFailure, StepRouting};

// ── Priority ────────────────────────────────────────────────────

/// SOP priority level, used for execution mode resolution and scheduling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum SopPriority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

impl fmt::Display for SopPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Normal => write!(f, "normal"),
            Self::High => write!(f, "high"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

// ── Execution Mode ──────────────────────────────────────────────

/// How much autonomy the agent has when executing an SOP.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SopExecutionMode {
    /// Execute all steps without human approval.
    Auto,
    /// Request approval before starting, then execute all steps.
    #[default]
    Supervised,
    /// Request approval before each step.
    StepByStep,
    /// Critical/High → Auto, Normal/Low → Supervised.
    PriorityBased,
    /// Execute steps sequentially without LLM round-trips.
    /// Step outputs are piped as inputs to the next step.
    /// Checkpoint steps pause for human approval.
    Deterministic,
}

impl fmt::Display for SopExecutionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Supervised => write!(f, "supervised"),
            Self::StepByStep => write!(f, "step_by_step"),
            Self::PriorityBased => write!(f, "priority_based"),
            Self::Deterministic => write!(f, "deterministic"),
        }
    }
}

// ── Filesystem event kind ───────────────────────────────────────

/// A normalized filesystem change kind reported by the watcher.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum_macros::EnumIter,
    strum_macros::IntoStaticStr,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum FilesystemEventKind {
    Created,
    Modified,
    Deleted,
    Renamed,
}

impl fmt::Display for FilesystemEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Modified => write!(f, "modified"),
            Self::Deleted => write!(f, "deleted"),
            Self::Renamed => write!(f, "renamed"),
        }
    }
}

impl std::str::FromStr for FilesystemEventKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_value(serde_json::Value::String(s.to_ascii_lowercase())).map_err(|_| ())
    }
}

// ── Trigger ─────────────────────────────────────────────────────

/// What event can activate an SOP.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum_macros::EnumDiscriminants,
    zeroclaw_macros::TriggerFields,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "lowercase")]
#[strum_discriminants(
    name(SopTriggerSource),
    derive(
        Hash,
        Serialize,
        Deserialize,
        strum_macros::EnumIter,
        strum_macros::IntoStaticStr,
        strum_macros::Display
    ),
    serde(rename_all = "lowercase"),
    strum(serialize_all = "lowercase"),
    doc = "The source type of an incoming event that may trigger an SOP. \
           Derived from `SopTrigger`; one discriminant per trigger variant."
)]
pub enum SopTrigger {
    /// MQTT message arrival. Live: delivered by the MQTT listener.
    #[trigger(display = "topic")]
    Mqtt {
        /// Topic filter. `+` matches one level, `#` matches the remaining levels.
        topic: String,
        /// Optional expression evaluated against the message payload; the run
        /// starts only when it holds.
        #[serde(default)]
        condition: Option<String>,
    },
    /// Inbound HTTP request. Defined and matched, but no live route feeds it.
    #[trigger(display = "path")]
    Webhook {
        /// Request path matched exactly against the event path.
        path: String,
    },
    /// Time-based firing. Live: dispatched by the SOP maintenance tick (daemon / channel-start paths).
    #[trigger(display = "expression")]
    Cron {
        /// Cron expression evaluated over the run window.
        expression: String,
    },
    /// Hardware signal. Defined and matched, but no peripheral listener feeds it.
    #[trigger(display = "board/signal")]
    Peripheral {
        /// Board identifier the signal originates from.
        board: String,
        /// Signal name on the board; matched as `board/signal`.
        signal: String,
        /// Optional expression evaluated against the signal payload.
        #[serde(default)]
        condition: Option<String>,
    },
    /// Filesystem change. Live: delivered by the filesystem watcher.
    #[trigger(display = "path")]
    Filesystem {
        /// Path glob (`*`, `**`, `?`); a bare directory matches anything under it.
        path: String,
        /// Change kinds to match; empty matches every kind.
        #[serde(default)]
        events: Vec<FilesystemEventKind>,
        /// Optional expression evaluated against the change payload.
        #[serde(default)]
        condition: Option<String>,
    },
    /// Calendar event state. Defined and matched, but no poller feeds it live.
    #[trigger(display = "calendar_source")]
    Calendar {
        /// Calendar source identifier the event originates from.
        calendar_source: String,
        /// Calendar IDs to scope to; empty matches all of the source's calendars.
        #[serde(default)]
        calendar_ids: Vec<String>,
        /// Optional expression evaluated against the calendar event payload.
        #[serde(default)]
        condition: Option<String>,
    },
    /// Inbound message or forge/platform event on a configured channel
    /// (telegram, discord, slack, git, ...). Live: delivered by the channel
    /// orchestrator when the channel's SOP dispatch is enabled. The Git forge
    /// producer sets an event topic of the form `<channel>.<alias>:<event_type>`
    /// and puts `event_type` in the payload, so an authored `condition` filters
    /// forge events by type without a second trigger shape.
    #[trigger(config_derived, display = "channel", opt = "alias")]
    Channel {
        /// `ChannelKind` snake_case value naming the channel type.
        channel: String,
        /// Optional configured-instance alias; unset matches every instance.
        #[serde(default)]
        alias: Option<String>,
        /// Optional expression evaluated against the message payload.
        #[serde(default)]
        condition: Option<String>,
    },
    /// Agent-initiated run via the `sop_execute` tool. Not an external fan-in.
    Manual,
    /// AMQP delivery. Live: delivered by the AMQP consumer in a SOP dispatch mode.
    #[trigger(display = "routing_key")]
    Amqp {
        /// Routing-key filter (topic-exchange semantics): `.`-delimited words,
        /// `*` matches one word, `#` matches zero or more words.
        routing_key: String,
        /// Optional expression evaluated against the delivery body.
        #[serde(default)]
        condition: Option<String>,
    },
}

impl SopTrigger {
    pub fn source(&self) -> SopTriggerSource {
        SopTriggerSource::from(self)
    }
}

// ── Step kind ────────────────────────────────────────────────────

/// The kind of a workflow step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SopStepKind {
    /// Normal step — executed by the agent (or deterministic handler).
    #[default]
    Execute,
    /// Checkpoint step — pauses execution and waits for human approval.
    Checkpoint,
    /// Deterministic capability step - executed by the SOP capability registry.
    Capability,
}

impl fmt::Display for SopStepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Execute => write!(f, "execute"),
            Self::Checkpoint => write!(f, "checkpoint"),
            Self::Capability => write!(f, "capability"),
        }
    }
}

// ── Typed step parameters ────────────────────────────────────────

/// JSON Schema fragment for validating step input/output data.
/// Stored as a raw `serde_json::Value` so callers can validate without
/// pulling in a full JSON Schema library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct StepSchema {
    /// JSON Schema object describing expected input shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// JSON Schema object describing expected output shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
}

// ── Step ────────────────────────────────────────────────────────

/// An authored tool invocation planned for a step. Args may embed
/// `{{steps.N.path}}` / `{{calls.K.path}}` bindings (see `sop::binding`)
/// that resolve against captured run data before dispatch. `pinned`
/// carries a sample output (typically lifted from a real run's
/// `StepToolCall.output_data`) so downstream bindings can be authored
/// and previewed without re-executing the tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PlannedToolCall {
    pub tool: String,
    /// Argument template; string leaves may contain bindings.
    #[serde(default)]
    pub args: serde_json::Value,
    /// Pinned sample output for authoring/preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<serde_json::Value>,
}

/// Persisted canvas coordinate for a step node. Written by the Blueprint
/// editor when a node is dragged; never edited in the step-definition form.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct StepPos {
    pub x: f64,
    pub y: f64,
}

/// A single step in an SOP procedure, parsed from SOP.md.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SopStep {
    /// Ordinal position of this step within the procedure. Steps run in
    /// number order unless routing overrides the sequence; the daemon
    /// renumbers on save so gaps and reorders normalize to 1..N.
    #[serde(default)]
    pub number: u32,
    /// Short human label for the step, shown on the canvas node and in the
    /// step list. Leave blank and the surface falls back to "untitled".
    #[serde(default)]
    pub title: String,
    /// The step's instruction body in Markdown. This is the prompt the
    /// running agent executes for the step; bindings like `{{steps.N}}`
    /// resolve against prior step outputs at run time.
    #[serde(default)]
    pub body: String,
    /// Advisory tool names surfaced to the agent for this step. Legacy alias
    /// for `scope.allow`; when `step_scope_enforce` is off these are hints,
    /// not a hard restriction.
    #[serde(default)]
    pub suggested_tools: Vec<String>,
    /// Pause for human confirmation before this step runs. Independent of
    /// `checkpoint` kind and the SOP-level approval gate; both still apply.
    #[serde(default)]
    pub requires_confirmation: bool,
    /// Step kind: `execute` (default) or `checkpoint`.
    #[serde(default)]
    pub kind: SopStepKind,
    /// Typed input/output schemas for deterministic data flow validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<StepSchema>,
    /// Tool scope for this step. `suggested_tools` remains the legacy alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<StepToolScope>,
    /// Conditional routing metadata. Default preserves linear execution.
    #[serde(default, skip_serializing_if = "StepRouting::is_default")]
    pub routing: StepRouting,
    /// Failure handling metadata. Default preserves fail-the-run behavior.
    #[serde(default, skip_serializing_if = "StepFailure::is_fail")]
    pub on_failure: StepFailure,
    /// Optional per-step execution mode override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SopExecutionMode>,
    /// Ordered tool calls planned for this step. Args may carry
    /// `{{steps.N}}` / `{{calls.K}}` bindings validated at save time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<PlannedToolCall>,
    /// Persisted canvas coordinate, set by dragging the node in the Blueprint
    /// editor. Absent until the node is moved; not surfaced in the step form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<StepPos>,
    /// Agent alias that runs this step. Overrides the SOP's parent agent when
    /// set; unset inherits the parent. Authored, persisted, and resolved into
    /// the executing action via `effective_agent` (step override then parent).
    /// Spawning a distinct per-step agent's own session at execution time is
    /// staged follow-on work; today the resolved alias is stamped on the action
    /// and the active agent loop executes the step body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Capability identifier used when `kind = "capability"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    /// Capability arguments, serialized as `with` in TOML/JSON definitions.
    #[serde(default, rename = "with", skip_serializing_if = "Option::is_none")]
    pub capability_input: Option<serde_json::Value>,
    /// Approval policy name (a key in `[sop.approval].policies`) the approval broker
    /// enforces for this step's gate: required approver group + quorum. `None` keeps
    /// today's behavior (`approval_mode` alone governs, no membership/quorum).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
}

impl Default for SopStep {
    fn default() -> Self {
        Self {
            number: 0,
            title: String::new(),
            body: String::new(),
            suggested_tools: Vec::new(),
            requires_confirmation: false,
            kind: SopStepKind::Execute,
            schema: None,
            scope: None,
            routing: StepRouting::default(),
            on_failure: StepFailure::default(),
            mode: None,
            calls: Vec::new(),
            pos: None,
            agent: None,
            capability: None,
            capability_input: None,
            policy: None,
        }
    }
}

impl SopStep {
    pub fn capability_id(&self) -> Option<&str> {
        self.capability.as_deref()
    }

    pub fn capability_call_input(&self, piped_input: serde_json::Value) -> serde_json::Value {
        let Some(mut configured) = self.capability_input.clone() else {
            return piped_input;
        };
        if let Some(object) = configured.as_object_mut() {
            object.entry("input").or_insert(piped_input);
        }
        configured
    }

    pub fn effective_tool_scope(&self) -> Option<StepToolScope> {
        let mut scope = self.scope.clone();
        if !self.suggested_tools.is_empty() {
            let scope = scope.get_or_insert_with(StepToolScope::default);
            if scope.allow.is_none() {
                scope.allow = Some(self.suggested_tools.clone());
            }
        }
        scope
    }

    /// The agent alias that runs this step: the step's own override when set,
    /// otherwise the SOP's parent agent.
    pub fn effective_agent<'a>(&'a self, parent: Option<&'a str>) -> Option<&'a str> {
        self.agent.as_deref().or(parent)
    }
}

// ── SOP ─────────────────────────────────────────────────────────

/// A complete Standard Operating Procedure definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct Sop {
    /// Unique procedure name. Doubles as the on-disk directory key, so a
    /// rename in the editor moves the SOP's folder. Must be non-empty.
    pub name: String,
    /// Free-text summary of what the procedure does and when it fires. Shown
    /// in the SOP list and header; purely descriptive, never executed.
    pub description: String,
    /// Semantic version string for the procedure definition (e.g. `1.0.0`).
    /// Bump it when you change step behavior so runs are traceable.
    pub version: String,
    /// Scheduling priority when concurrency limits force a choice between
    /// runnable procedures: `critical`, `high`, `normal` (default), `low`.
    pub priority: SopPriority,
    /// How steps are driven: `auto`, `supervised` (default), `step_by_step`,
    /// `priority_based`, or `deterministic`. `deterministic = true` forces
    /// the last regardless of this field.
    pub execution_mode: SopExecutionMode,
    /// Signals that start a run. A procedure may bind several triggers; any
    /// one firing (subject to its own condition) launches the SOP.
    pub triggers: Vec<SopTrigger>,
    /// Ordered step definitions. This is the body of the procedure; the
    /// daemon renumbers and remaps routing refs on save.
    pub steps: Vec<SopStep>,
    /// Minimum seconds between successive runs of this procedure. `0`
    /// (default) disables the cooldown and back-to-back runs are allowed.
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    /// Maximum simultaneous runs of this one procedure. Excess trigger
    /// firings queue or drop per the engine's concurrency policy. Default 1.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    #[serde(skip)]
    #[cfg_attr(feature = "schema-export", schemars(skip))]
    pub location: Option<PathBuf>,
    /// When true, sets execution_mode to Deterministic.
    /// Steps execute sequentially without LLM round-trips.
    #[serde(default)]
    pub deterministic: bool,
    /// How to handle a trigger that cannot be admitted immediately because this
    /// SOP's execution slots are full (A2). Default `parallel`.
    #[serde(default)]
    pub admission_policy: SopAdmissionPolicy,
    /// Upper bound on runs of this SOP parked at a HITL approval at once
    /// (`0` = unlimited). Once reached, further triggers are deferred (surfaced for
    /// backpressure/redelivery) rather than silently dropped. Default `0`.
    #[serde(default = "default_max_pending_approvals")]
    pub max_pending_approvals: u32,
    /// Parent agent alias that owns this procedure. Every `execute` step runs
    /// as this agent unless the step names its own `agent` override. Required
    /// for headless triggers (mqtt, webhook, cron, amqp), which have no
    /// ambient agent loop to borrow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

fn default_cooldown_secs() -> u64 {
    0
}

fn default_max_concurrent() -> u32 {
    1
}

fn default_max_pending_approvals() -> u32 {
    0
}

/// How concurrent triggers are handled when a SOP's execution slots are full. A
/// run parked at a HITL approval releases its slot (A1), so this governs the
/// remaining case: too many runs actively *executing* at once. No variant ever
/// silently drops a trigger except `Drop`, which is explicit opt-in.
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SopAdmissionPolicy {
    /// Admit up to `max_concurrent` concurrent runs; a trigger that cannot admit
    /// now is DEFERRED (surfaced for backpressure/redelivery), never silently
    /// dropped. Best fit for independent work like PR-request approvals.
    #[default]
    Parallel,
    /// Serialize: admit only when no run of this SOP is active or parked; other
    /// triggers are deferred. For pipelines whose pre-approval steps must not overlap.
    Hold,
    /// Collapse concurrent triggers: when a run is already in flight for this SOP a
    /// new trigger is coalesced (dropped as redundant - the in-flight run already
    /// covers the latest state). For "only current state matters" SOPs.
    Coalesce,
    /// Legacy fire-and-forget: a trigger that cannot admit now is dropped. Explicit
    /// opt-in only; never the default.
    Drop,
}

impl fmt::Display for SopAdmissionPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parallel => write!(f, "parallel"),
            Self::Hold => write!(f, "hold"),
            Self::Coalesce => write!(f, "coalesce"),
            Self::Drop => write!(f, "drop"),
        }
    }
}

/// A2: the outcome of evaluating a matched trigger against a SOP's
/// `SopAdmissionPolicy`. Advisory - `Admit` still passes through the authoritative
/// CAS `start_run`; the non-admit variants are surfaced by the dispatch layer
/// (logged + carried on `DispatchResult`) so a trigger is never silently lost.
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SopAdmission {
    /// A slot is available - proceed to start the run.
    Admit,
    /// Cannot admit now (execution slots or the pending-approval pool are full).
    /// Apply backpressure / redelivery rather than dropping.
    Defer { reason: String },
    /// A run is already in flight for this SOP; collapse this trigger into it.
    Coalesce { existing_run_id: String },
    /// Drop this trigger: either the `drop` policy with no free slot, or a cooldown
    /// / unknown SOP (which drop regardless of policy).
    Drop { reason: String },
}

// ── TOML manifest (internal parse target) ───────────────────────

/// Top-level SOP.toml structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopManifest {
    pub sop: SopMeta,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<SopTrigger>,
    /// Persisted canvas coordinates per step. Written by the Blueprint editor,
    /// kept out of SOP.md so step prose stays position-free. Merged back onto
    /// `SopStep::pos` at load time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<StepPosition>,
    #[serde(default)]
    pub steps: Vec<SopStep>,
}

/// One step's persisted canvas coordinate in SOP.toml.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StepPosition {
    pub step: u32,
    pub x: f64,
    pub y: f64,
}

/// The `[sop]` table in SOP.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopMeta {
    pub name: String,
    pub description: String,
    #[serde(default = "default_sop_version")]
    pub version: String,
    #[serde(default)]
    pub priority: SopPriority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<SopExecutionMode>,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    /// Opt-in deterministic execution (no LLM round-trips between steps).
    #[serde(default)]
    pub deterministic: bool,
    /// Concurrent-trigger admission policy (`parallel` | `hold` | `coalesce` | `drop`).
    #[serde(default)]
    pub admission_policy: SopAdmissionPolicy,
    /// Max runs parked at a HITL approval at once (`0` = unlimited).
    #[serde(default = "default_max_pending_approvals")]
    pub max_pending_approvals: u32,
    /// Parent agent alias that owns the procedure. Steps run as this agent
    /// unless a step overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl SopManifest {
    pub fn from_sop(sop: &Sop) -> Self {
        Self {
            sop: SopMeta {
                name: sop.name.clone(),
                description: sop.description.clone(),
                version: sop.version.clone(),
                priority: sop.priority,
                execution_mode: Some(sop.execution_mode),
                cooldown_secs: sop.cooldown_secs,
                max_concurrent: sop.max_concurrent,
                deterministic: sop.deterministic,
                admission_policy: sop.admission_policy,
                max_pending_approvals: sop.max_pending_approvals,
                agent: sop.agent.clone(),
            },
            triggers: sop.triggers.clone(),
            positions: sop
                .steps
                .iter()
                .filter_map(|s| {
                    s.pos.map(|p| StepPosition {
                        step: s.number,
                        x: p.x,
                        y: p.y,
                    })
                })
                .collect(),
            steps: sop.steps.clone(),
        }
    }
}

fn default_sop_version() -> String {
    "0.1.0".to_string()
}

// ── Event ────────────────────────────────────────────────────────

/// An incoming event that may trigger one or more SOPs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopEvent {
    pub source: SopTriggerSource,
    /// Topic, path, or signal identifier (depends on source type).
    #[serde(default)]
    pub topic: Option<String>,
    /// Raw payload (JSON string, sensor reading, etc.).
    #[serde(default)]
    pub payload: Option<String>,
    /// When the event occurred (ISO-8601).
    pub timestamp: String,
}

// ── Run state ────────────────────────────────────────────────────

/// Status of an SOP execution run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SopRunStatus {
    Pending,
    Running,
    WaitingApproval,
    /// Paused at a checkpoint in a deterministic workflow.
    PausedCheckpoint,
    Completed,
    Failed,
    Cancelled,
}

impl fmt::Display for SopRunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::WaitingApproval => write!(f, "waiting_approval"),
            Self::PausedCheckpoint => write!(f, "paused_checkpoint"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Result status of a single step execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SopStepStatus {
    Completed,
    Failed,
    Skipped,
}

impl fmt::Display for SopStepStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

/// One tool invocation captured during a step's execution. A step may
/// make any number of calls (including the same tool repeatedly);
/// `index` preserves invocation order so authoring surfaces can replay
/// the sequence and map data between calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct StepToolCall {
    /// Zero-based invocation order within the step.
    pub index: u32,
    pub tool: String,
    /// Arguments the tool actually received.
    pub args: serde_json::Value,
    pub success: bool,
    /// Display text of the tool output.
    pub output: String,
    /// Structured output when the tool declared one (`ToolOutput::data`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Wall-clock duration in milliseconds.
    #[serde(default)]
    pub duration_ms: u64,
}

/// Result of executing a single SOP step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopStepResult {
    pub step_number: u32,
    pub status: SopStepStatus,
    pub output: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    /// Ordered tool invocations made while executing this step. Empty
    /// for checkpoint steps and legacy records persisted before capture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<StepToolCall>,
}

/// A full SOP execution run (from trigger to completion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopRun {
    pub run_id: String,
    pub sop_name: String,
    pub trigger_event: SopEvent,
    /// Stable per-run boundary marker for untrusted trigger framing.
    #[serde(default)]
    pub frame_marker_id: String,
    pub status: SopRunStatus,
    pub current_step: u32,
    pub total_steps: u32,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub step_results: Vec<SopStepResult>,
    /// ISO-8601 timestamp when the run entered WaitingApproval (for timeout tracking).
    #[serde(default)]
    pub waiting_since: Option<String>,
    /// Number of LLM calls saved by deterministic execution in this run.
    #[serde(default)]
    pub llm_calls_saved: u64,
}

impl ::zeroclaw_api::attribution::Attributable for SopRun {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Sop
    }
    fn alias(&self) -> &str {
        &self.sop_name
    }
}

/// Lightweight projection of a run for list surfaces (Runs page). Carries
/// just enough to render a row and open the per-run overlay, without the
/// full step-result payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopRunSummary {
    pub run_id: String,
    pub sop_name: String,
    pub status: SopRunStatus,
    pub current_step: u32,
    pub total_steps: u32,
    pub started_at: String,
    pub completed_at: Option<String>,
    /// Where the run's trigger came from (manual, a channel, cron, ...).
    pub trigger_source: String,
    /// True while the run is live (in the engine's active set) rather than
    /// a retained terminal record.
    pub active: bool,
}

impl SopRunSummary {
    pub fn from_run(run: &SopRun, active: bool) -> Self {
        Self {
            run_id: run.run_id.clone(),
            sop_name: run.sop_name.clone(),
            status: run.status,
            current_step: run.current_step,
            total_steps: run.total_steps,
            started_at: run.started_at.clone(),
            completed_at: run.completed_at.clone(),
            trigger_source: run.trigger_event.source.to_string(),
            active,
        }
    }
}

// ── Deterministic workflow state (persistence + resume) ──────────

/// Persisted state for a deterministic workflow run, enabling resume
/// after interruption. Serialized to a JSON file alongside the SOP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeterministicRunState {
    /// Identifier of this run.
    pub run_id: String,
    /// SOP name this state belongs to.
    pub sop_name: String,
    /// Last successfully completed step number (0 = none completed).
    pub last_completed_step: u32,
    /// Total steps in the workflow.
    pub total_steps: u32,
    /// Output of each completed step, keyed by step number.
    pub step_outputs: HashMap<u32, serde_json::Value>,
    /// ISO-8601 timestamp when this state was last persisted.
    pub persisted_at: String,
    /// Number of LLM calls that were saved by deterministic execution.
    pub llm_calls_saved: u64,
    /// Whether the run is paused at a checkpoint awaiting approval.
    pub paused_at_checkpoint: bool,
}

// ── Cost savings metric ──────────────────────────────────────────

/// Tracks how many LLM round-trips were saved by deterministic execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeterministicSavings {
    /// Total LLM calls saved across all deterministic runs.
    pub total_llm_calls_saved: u64,
    /// Total deterministic runs completed.
    pub total_runs: u64,
}

/// What the engine instructs the caller to do next after a state transition.
#[derive(Debug, Clone)]
pub enum SopRunAction {
    /// Inject this step into the agent for execution. `step.agent` is the
    /// resolved effective agent (step override then parent), not the raw
    /// persisted override; consumers must not re-resolve it.
    ExecuteStep {
        run_id: String,
        step: SopStep,
        context: String,
    },
    /// Pause and wait for operator approval before executing this step.
    WaitApproval {
        run_id: String,
        step: SopStep,
        context: String,
    },
    /// Execute a step deterministically (no LLM). The `input` is the piped
    /// output from the previous step (or trigger payload for step 1).
    DeterministicStep {
        run_id: String,
        step: SopStep,
        input: serde_json::Value,
    },
    /// Deterministic workflow hit a checkpoint — pause for human approval.
    /// Workflow state has been persisted so it can resume after approval.
    CheckpointWait {
        run_id: String,
        step: SopStep,
        state_file: PathBuf,
    },
    /// Routing selected a step whose dependencies are not yet satisfied.
    Pending {
        run_id: String,
        sop_name: String,
        step: u32,
        reason: String,
    },
    /// The SOP run completed successfully.
    Completed { run_id: String, sop_name: String },
    /// The SOP run failed.
    Failed {
        run_id: String,
        sop_name: String,
        reason: String,
    },
}

/// Exhaustive sample builder: one representative `SopTrigger` per source.
/// The match has no wildcard, so adding a source fails to compile here until a
/// sample is supplied, keeping every drift walk that consumes it exhaustive.
#[cfg(test)]
pub(crate) fn sample_trigger(source: SopTriggerSource) -> SopTrigger {
    match source {
        SopTriggerSource::Mqtt => SopTrigger::Mqtt {
            topic: "t".into(),
            condition: None,
        },
        SopTriggerSource::Webhook => SopTrigger::Webhook {
            path: "/hook".into(),
        },
        SopTriggerSource::Cron => SopTrigger::Cron {
            expression: "* * * * *".into(),
        },
        SopTriggerSource::Peripheral => SopTrigger::Peripheral {
            board: "b".into(),
            signal: "s".into(),
            condition: None,
        },
        SopTriggerSource::Filesystem => SopTrigger::Filesystem {
            path: "/p".into(),
            events: vec![],
            condition: None,
        },
        SopTriggerSource::Calendar => SopTrigger::Calendar {
            calendar_source: "src".into(),
            calendar_ids: vec![],
            condition: None,
        },
        SopTriggerSource::Channel => SopTrigger::Channel {
            channel: "telegram".into(),
            alias: None,
            condition: None,
        },
        SopTriggerSource::Manual => SopTrigger::Manual,
        SopTriggerSource::Amqp => SopTrigger::Amqp {
            routing_key: "k".into(),
            condition: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn priority_display() {
        assert_eq!(SopPriority::Critical.to_string(), "critical");
        assert_eq!(SopPriority::Low.to_string(), "low");
    }

    #[test]
    fn execution_mode_display() {
        assert_eq!(SopExecutionMode::Auto.to_string(), "auto");
        assert_eq!(
            SopExecutionMode::PriorityBased.to_string(),
            "priority_based"
        );
    }

    #[test]
    fn trigger_display() {
        let mqtt = SopTrigger::Mqtt {
            topic: "sensors/temp".into(),
            condition: Some("$.value > 85".into()),
        };
        assert_eq!(mqtt.to_string(), "mqtt:sensors/temp");

        let calendar = SopTrigger::Calendar {
            calendar_source: "microsoft365".into(),
            calendar_ids: vec!["primary".into()],
            condition: None,
        };
        assert_eq!(calendar.to_string(), "calendar:microsoft365");

        let manual = SopTrigger::Manual;
        assert_eq!(manual.to_string(), "manual");
    }

    #[test]
    fn trigger_display_covers_every_variant() {
        // Walks every source through the exhaustive sample builder and checks
        // structural Display invariants that hold for all sources, with no
        // per-source string table to drift. A variant missing its
        // `#[trigger(display=...)]` attribute renders as the bare source and
        // fails the payload-suffix check; a new source is forced through
        // `sample_trigger` by the compiler.
        for source in SopTriggerSource::iter() {
            let trigger = sample_trigger(source);
            let rendered = trigger.to_string();
            let source_str = source.to_string();

            assert!(
                rendered == source_str || rendered.starts_with(&format!("{source_str}:")),
                "source {source} Display '{rendered}' must be the bare source or \
                 '{source_str}:<value>'"
            );

            // Whether the variant carries authoring fields is derived from its
            // serde form (keys beyond the `type` tag), not a hardcoded list.
            let json = serde_json::to_value(&trigger).unwrap();
            let payload_fields = json
                .as_object()
                .unwrap()
                .keys()
                .filter(|k| k.as_str() != "type")
                .count();

            if payload_fields == 0 {
                assert_eq!(
                    rendered, source_str,
                    "fieldless source {source} must render as the bare source"
                );
            } else {
                assert_ne!(
                    rendered, source_str,
                    "source {source} has {payload_fields} field(s) but renders as the \
                     bare source; its `#[trigger(display=...)]` attribute is missing"
                );
                let suffix = &rendered[source_str.len() + 1..];
                assert!(
                    !suffix.is_empty(),
                    "source {source} renders an empty value suffix"
                );
            }
        }
        // The Channel alias branch is a within-variant option, not a source, so
        // it is exercised explicitly: alias present must append `/{alias}`.
        let bare = SopTrigger::Channel {
            channel: "telegram".into(),
            alias: None,
            condition: None,
        };
        let aliased = SopTrigger::Channel {
            channel: "telegram".into(),
            alias: Some("prod".into()),
            condition: None,
        };
        assert_eq!(aliased.to_string(), format!("{}/prod", bare));
    }

    #[test]
    fn priority_serde_roundtrip() {
        let json = serde_json::to_string(&SopPriority::Critical).unwrap();
        assert_eq!(json, "\"critical\"");
        let parsed: SopPriority = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SopPriority::Critical);
    }

    #[test]
    fn execution_mode_serde_roundtrip() {
        let json = serde_json::to_string(&SopExecutionMode::PriorityBased).unwrap();
        assert_eq!(json, "\"priority_based\"");
        let parsed: SopExecutionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SopExecutionMode::PriorityBased);
    }

    #[test]
    fn calendar_trigger_serde_roundtrip() {
        let trigger = SopTrigger::Calendar {
            calendar_source: "microsoft365".into(),
            calendar_ids: vec!["primary".into()],
            condition: None,
        };

        let json = serde_json::to_string(&trigger).unwrap();
        let parsed: SopTrigger = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed, trigger);
        assert_eq!(SopTriggerSource::Calendar.to_string(), "calendar");
        assert_eq!(
            serde_json::to_string(&SopTriggerSource::Calendar).unwrap(),
            "\"calendar\""
        );
    }

    #[test]
    fn calendar_trigger_toml_roundtrip() {
        let toml_str = r#"
type = "calendar"
calendar_source = "microsoft365"
calendar_ids = ["primary", "team"]
"#;
        let trigger: SopTrigger = toml::from_str(toml_str).unwrap();

        assert!(
            matches!(trigger, SopTrigger::Calendar { ref calendar_source, ref calendar_ids, .. }
                if calendar_source == "microsoft365"
                    && calendar_ids.as_slice() == ["primary", "team"])
        );
    }

    #[test]
    fn trigger_toml_roundtrip() {
        let toml_str = r#"
type = "mqtt"
topic = "facility/pump/pressure"
condition = "$.value > 85"
"#;
        let trigger: SopTrigger = toml::from_str(toml_str).unwrap();
        assert!(
            matches!(trigger, SopTrigger::Mqtt { ref topic, .. } if topic == "facility/pump/pressure")
        );
    }

    #[test]
    fn trigger_manual_toml() {
        let toml_str = r#"type = "manual""#;
        let trigger: SopTrigger = toml::from_str(toml_str).unwrap();
        assert_eq!(trigger, SopTrigger::Manual);
    }

    #[test]
    fn trigger_filesystem_toml_roundtrip() {
        let toml_str = r#"
type = "filesystem"
path = "/var/inbox/**/*.json"
events = ["created", "modified"]
condition = "$.extension == \"json\""
"#;
        let trigger: SopTrigger = toml::from_str(toml_str).unwrap();
        match trigger {
            SopTrigger::Filesystem {
                path,
                events,
                condition,
            } => {
                assert_eq!(path, "/var/inbox/**/*.json");
                assert_eq!(
                    events,
                    vec![FilesystemEventKind::Created, FilesystemEventKind::Modified]
                );
                assert_eq!(condition.as_deref(), Some(r#"$.extension == "json""#));
            }
            other => panic!("expected Filesystem trigger, got {other:?}"),
        }
    }

    #[test]
    fn trigger_filesystem_defaults_events_empty() {
        let toml_str = r#"
type = "filesystem"
path = "/var/inbox"
"#;
        let trigger: SopTrigger = toml::from_str(toml_str).unwrap();
        assert!(
            matches!(trigger, SopTrigger::Filesystem { ref events, ref condition, .. } if events.is_empty() && condition.is_none())
        );
    }

    #[test]
    fn trigger_channel_toml() {
        let toml_str = r#"
type = "channel"
channel = "git"
alias = "main"
condition = "$.event_type == \"pull_request.opened\""
"#;
        let trigger: SopTrigger = toml::from_str(toml_str).unwrap();
        assert!(matches!(
            trigger,
            SopTrigger::Channel { ref channel, ref alias, .. }
                if channel == "git" && alias.as_deref() == Some("main")
        ));
    }

    #[test]
    fn filesystem_event_kind_display_and_serde() {
        assert_eq!(FilesystemEventKind::Created.to_string(), "created");
        assert_eq!(FilesystemEventKind::Renamed.to_string(), "renamed");
        let json = serde_json::to_string(&FilesystemEventKind::Deleted).unwrap();
        assert_eq!(json, "\"deleted\"");
        let parsed: FilesystemEventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, FilesystemEventKind::Deleted);
    }

    #[test]
    fn trigger_filesystem_display() {
        let trigger = SopTrigger::Filesystem {
            path: "/var/inbox/*.json".into(),
            events: vec![FilesystemEventKind::Created],
            condition: None,
        };
        assert_eq!(trigger.to_string(), "filesystem:/var/inbox/*.json");
    }

    #[test]
    fn trigger_source_filesystem_display() {
        assert_eq!(SopTriggerSource::Filesystem.to_string(), "filesystem");
    }

    #[test]
    fn run_status_display() {
        assert_eq!(
            SopRunStatus::WaitingApproval.to_string(),
            "waiting_approval"
        );
    }

    #[test]
    fn step_kind_display() {
        assert_eq!(SopStepKind::Execute.to_string(), "execute");
        assert_eq!(SopStepKind::Checkpoint.to_string(), "checkpoint");
        assert_eq!(SopStepKind::Capability.to_string(), "capability");
    }

    #[test]
    fn step_kind_serde_roundtrip() {
        let json = serde_json::to_string(&SopStepKind::Checkpoint).unwrap();
        assert_eq!(json, "\"checkpoint\"");
        let parsed: SopStepKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SopStepKind::Checkpoint);
    }

    #[test]
    fn execution_mode_deterministic_roundtrip() {
        let json = serde_json::to_string(&SopExecutionMode::Deterministic).unwrap();
        assert_eq!(json, "\"deterministic\"");
        let parsed: SopExecutionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SopExecutionMode::Deterministic);
    }

    #[test]
    fn deterministic_run_state_serde() {
        let state = DeterministicRunState {
            run_id: "det-001".into(),
            sop_name: "test-sop".into(),
            last_completed_step: 2,
            total_steps: 5,
            step_outputs: {
                let mut m = std::collections::HashMap::new();
                m.insert(1, serde_json::json!({"result": "ok"}));
                m.insert(2, serde_json::json!("step2_done"));
                m
            },
            persisted_at: "2026-03-01T00:00:00Z".into(),
            llm_calls_saved: 2,
            paused_at_checkpoint: true,
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: DeterministicRunState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.run_id, "det-001");
        assert_eq!(parsed.last_completed_step, 2);
        assert_eq!(parsed.llm_calls_saved, 2);
        assert!(parsed.paused_at_checkpoint);
        assert_eq!(parsed.step_outputs.len(), 2);
    }

    #[test]
    fn run_status_paused_checkpoint_display() {
        assert_eq!(
            SopRunStatus::PausedCheckpoint.to_string(),
            "paused_checkpoint"
        );
    }

    #[test]
    fn step_defaults() {
        let step: SopStep =
            serde_json::from_str(r#"{"number": 1, "title": "Check", "body": "Verify readings"}"#)
                .unwrap();
        assert!(step.suggested_tools.is_empty());
        assert!(!step.requires_confirmation);
        assert!(step.capability.is_none());
        assert!(step.capability_input.is_none());
    }

    #[test]
    fn default_step_contract_fields_do_not_serialize() {
        let step = SopStep {
            number: 1,
            title: "Check".into(),
            body: "Verify readings".into(),
            ..SopStep::default()
        };
        let value = serde_json::to_value(step).unwrap();

        assert!(value.get("scope").is_none());
        assert!(value.get("routing").is_none());
        assert!(value.get("on_failure").is_none());
        assert!(value.get("mode").is_none());
        assert!(value.get("capability").is_none());
        assert!(value.get("with").is_none());
    }

    #[test]
    fn manifest_parse() {
        let toml_str = r#"
[sop]
name = "test-sop"
description = "A test SOP"

[[triggers]]
type = "manual"

[[triggers]]
type = "webhook"
path = "/sop/test"
"#;
        let manifest: SopManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.sop.name, "test-sop");
        assert_eq!(manifest.triggers.len(), 2);
        assert_eq!(manifest.sop.priority, SopPriority::Normal);
        assert_eq!(manifest.sop.execution_mode, None);
    }

    #[test]
    fn trigger_source_display() {
        assert_eq!(SopTriggerSource::Mqtt.to_string(), "mqtt");
        assert_eq!(SopTriggerSource::Channel.to_string(), "channel");
        assert_eq!(SopTriggerSource::Manual.to_string(), "manual");
    }

    #[test]
    fn step_status_display() {
        assert_eq!(SopStepStatus::Completed.to_string(), "completed");
        assert_eq!(SopStepStatus::Failed.to_string(), "failed");
        assert_eq!(SopStepStatus::Skipped.to_string(), "skipped");
    }

    #[test]
    fn sop_event_serde_roundtrip() {
        let event = SopEvent {
            source: SopTriggerSource::Mqtt,
            topic: Some("sensors/pressure".into()),
            payload: Some(r#"{"value": 87.3}"#.into()),
            timestamp: "2026-02-19T12:00:00Z".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: SopEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.source, SopTriggerSource::Mqtt);
        assert_eq!(parsed.topic.as_deref(), Some("sensors/pressure"));
    }

    #[test]
    fn sop_run_serde_roundtrip() {
        let run = SopRun {
            run_id: "run-001".into(),
            sop_name: "test-sop".into(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: "2026-02-19T12:00:00Z".into(),
            },
            frame_marker_id: "marker-run-001".into(),
            status: SopRunStatus::Running,
            current_step: 2,
            total_steps: 5,
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: None,
            step_results: vec![SopStepResult {
                step_number: 1,
                status: SopStepStatus::Completed,
                output: "Step 1 done".into(),
                started_at: "2026-02-19T12:00:00Z".into(),
                completed_at: Some("2026-02-19T12:00:05Z".into()),
                tool_calls: Vec::new(),
            }],
            waiting_since: None,
            llm_calls_saved: 0,
        };
        let json = serde_json::to_string(&run).unwrap();
        let parsed: SopRun = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.run_id, "run-001");
        assert_eq!(parsed.status, SopRunStatus::Running);
        assert_eq!(parsed.step_results.len(), 1);
        assert_eq!(parsed.step_results[0].status, SopStepStatus::Completed);
    }
}
