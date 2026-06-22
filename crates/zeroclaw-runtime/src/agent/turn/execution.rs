//! The resolved per-agent execution context the turn engine requires.
//!
//! `ToolLoop` (the engine's input) carries two kinds of state: values that are
//! stable for every turn to a given agent (the model binding, the gated tool
//! registry, the approval policy, the resolved runtime knobs) and values that
//! change every message (history, streaming sinks, steering, the ingress
//! envelope). This module groups the *stable* half into one bundle so the
//! engine accepts it as a single required input.
//!
//! Two layers:
//! - [`ResolvedModelAccess`]: the bare model binding (provider + model +
//!   temperature). Any LLM call needs it; the agent bundle composes it.
//! - [`ResolvedAgentExecution`]: the full per-agent policy: the model access
//!   plus the tool registry, approval, observability, and the resolved runtime
//!   knobs.
//!
//! G0 is a behavior-neutral regrouping: the field names mirror the engine's
//! former flat `ToolLoop` fields one-for-one, so the loop body is unchanged
//! after it destructures the bundle. Later epics move the *resolution* of these
//! fields into a single `resolve()` constructor and seal the inputs so a turn
//! cannot run with a partially- or un-resolved policy.

use std::sync::{Arc, Mutex};

use zeroclaw_config::schema::{MultimodalConfig, PacingConfig};
use zeroclaw_providers::ModelProvider;

use super::{LoopKnobs, ModelSwitchCallback};
use crate::agent::tool_receipts::ReceiptGenerator;
use crate::approval::ApprovalManager;
use crate::hooks::HookRunner;
use crate::observability::Observer;
use crate::tools::{ActivatedToolSet, Tool};

/// The resolved model binding: which provider, model, and temperature a turn
/// uses. The base layer any LLM call needs; [`ResolvedAgentExecution`] composes
/// it. Field names mirror the engine's former flat fields so the loop body is
/// unchanged after destructuring.
pub struct ResolvedModelAccess<'a> {
    pub model_provider: &'a dyn ModelProvider,
    pub provider_name: &'a str,
    pub model: &'a str,
    pub temperature: Option<f64>,
}

/// The per-agent-stable execution context the turn engine requires: the model
/// binding plus the tool registry, policy, observability, and resolved runtime
/// knobs that do not change between messages to the same agent. The engine
/// takes this as one input; per-message state (history, streaming, steering,
/// ingress, cancellation) stays on `ToolLoop` alongside it.
pub struct ResolvedAgentExecution<'a> {
    /// Provider + model + temperature.
    pub model_access: ResolvedModelAccess<'a>,
    /// The tools available this turn (gated per the agent's policy upstream).
    pub tools_registry: &'a [Box<dyn Tool>],
    /// Telemetry/audit sink.
    pub observer: &'a dyn Observer,
    /// Suppress stderr output (subagents/reviews run silent).
    pub silent: bool,
    /// Approval policy + back-channel; `None` for paths that never prompt.
    pub approval: Option<&'a ApprovalManager>,
    /// Vision-model routing config.
    pub multimodal_config: &'a MultimodalConfig,
    /// Agentic loop iteration cap.
    pub max_tool_iterations: usize,
    /// Lifecycle hooks; `None` when unconfigured.
    pub hooks: Option<&'a HookRunner>,
    /// Tools the policy denies (never invoked).
    pub excluded_tools: &'a [String],
    /// Tools exempt from call-dedup.
    pub dedup_exempt_tools: &'a [String],
    /// Activation set for on-demand (tool_search) MCP tools; shared so activated
    /// tools persist across iterations.
    pub activated_tools: Option<&'a Arc<Mutex<ActivatedToolSet>>>,
    /// Back-channel for the `model_switch` tool.
    pub model_switch_callback: Option<ModelSwitchCallback>,
    /// Loop-detection / ignore-tools / timing policy.
    pub pacing: &'a PacingConfig,
    /// Reject malformed tool-call protocol.
    pub strict_tool_parsing: bool,
    /// Allow concurrent tool execution.
    pub parallel_tools: bool,
    /// Truncation limit for tool outputs.
    pub max_tool_result_chars: usize,
    /// History-pruning token threshold.
    pub context_token_budget: usize,
    /// Tool-receipt tracer; `None` when receipts are off.
    pub receipt_generator: Option<&'a ReceiptGenerator>,
    /// Fine-grained loop behavior flags.
    pub knobs: &'a LoopKnobs,
}
