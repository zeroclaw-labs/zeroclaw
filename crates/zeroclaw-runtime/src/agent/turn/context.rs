//! Shared context for the per-iteration turn step functions. Most fields are
//! immutable for the turn; `serving_provider_name` is mutated per iteration
//! when vision routing selects a different provider.

use super::events::DraftEvent;
use crate::approval::ApprovalManager;
use crate::hooks::HookRunner;
use crate::observability::Observer;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::channel::Channel;
use zeroclaw_config::schema::PacingConfig;

pub(crate) struct TurnCtx<'a> {
    pub(crate) observer: &'a dyn Observer,
    pub(crate) provider_name: &'a str,
    pub(crate) model: &'a str,
    pub(crate) temperature: Option<f64>,
    pub(crate) approval: Option<&'a ApprovalManager>,
    pub(crate) channel_name: &'a str,
    pub(crate) channel_reply_target: Option<&'a str>,
    pub(crate) cancellation_token: Option<&'a CancellationToken>,
    pub(crate) on_delta: Option<&'a Sender<DraftEvent>>,
    pub(crate) event_tx: Option<&'a Sender<TurnEvent>>,
    pub(crate) hooks: Option<&'a HookRunner>,
    pub(crate) dedup_exempt_tools: &'a [String],
    pub(crate) pacing: &'a PacingConfig,
    pub(crate) strict_tool_parsing: bool,
    pub(crate) channel: Option<&'a dyn Channel>,
    pub(crate) turn_id: &'a str,
    pub(crate) agent_alias: Option<&'a str>,
    /// The delegating agent's alias when this loop is a nested cross-agent
    /// execution (a live SOP step naming a different agent): `agent_alias` is
    /// the EFFECTIVE agent whose policy/tools execute; this keeps the parent
    /// correlation on every emitted record. `None` for ordinary turns.
    pub(crate) parent_agent_alias: Option<&'a str>,
    /// Per-iteration override for the provider that actually served the
    /// current LLM call. Set after vision routing resolves the active
    /// provider; `None` means "use `provider_name`". Owned `String`
    /// because the vision-resolved name's lifetime is the iteration scope,
    /// not the `'a` of the struct.
    pub(crate) serving_provider_name: Option<String>,
}

/// Lightweight metadata for turn-level event emission.
/// Derived on-demand from `TurnCtx` via `meta()` — not a cached duplicate.
#[derive(Clone, Copy)]
pub(crate) struct TurnMeta<'a> {
    pub(crate) agent_alias: Option<&'a str>,
    pub(crate) parent_agent_alias: Option<&'a str>,
    pub(crate) turn_id: &'a str,
    pub(crate) channel_name: &'a str,
}

impl<'a> TurnCtx<'a> {
    pub(crate) fn meta(&self) -> TurnMeta<'a> {
        TurnMeta {
            agent_alias: self.agent_alias,
            parent_agent_alias: self.parent_agent_alias,
            turn_id: self.turn_id,
            channel_name: self.channel_name,
        }
    }
}
