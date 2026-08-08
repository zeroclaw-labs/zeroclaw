//! Shared read-only context for the per-iteration turn step functions.

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
    /// Caller-owned cross-turn conversation attribution. Threaded verbatim
    /// from the turn's mint site (`Agent`/`process_message`) through the
    /// `ToolLoop` -> `TurnCtx` -> `TurnMeta` chain so every turn-level
    /// observer event carries one stable id across turns. `None` for nested
    /// sub-loops and paths without a conversation owner. Deliberately
    /// independent of `memory_session_id` (which selects the memory
    /// partition only) - never a fallback between the two.
    pub(crate) conversation_id: Option<&'a str>,
    pub(crate) agent_alias: Option<&'a str>,
    /// The delegating agent's alias when this loop is a nested cross-agent
    /// execution (a live SOP step naming a different agent): `agent_alias` is
    /// the EFFECTIVE agent whose policy/tools execute; this keeps the parent
    /// correlation on every emitted record. `None` for ordinary turns.
    pub(crate) parent_agent_alias: Option<&'a str>,
}

/// Lightweight metadata for turn-level event emission.
/// Built at emission call sites from the turn's borrows — not a cached duplicate.
/// The values are borrows from the turn mint site; the mint site stays the
/// single source of truth for the `(channel, agent_alias, turn_id)` triple.
#[derive(Clone, Copy, Debug)]
pub struct TurnMeta<'a> {
    pub agent_alias: Option<&'a str>,
    pub parent_agent_alias: Option<&'a str>,
    pub turn_id: &'a str,
    pub conversation_id: Option<&'a str>,
    pub channel_name: &'a str,
}

impl<'a> TurnCtx<'a> {
    pub(crate) fn meta(&self) -> TurnMeta<'a> {
        TurnMeta {
            agent_alias: self.agent_alias,
            parent_agent_alias: self.parent_agent_alias,
            turn_id: self.turn_id,
            conversation_id: self.conversation_id,
            channel_name: self.channel_name,
        }
    }
}
