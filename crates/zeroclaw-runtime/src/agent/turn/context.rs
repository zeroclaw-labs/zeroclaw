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
    pub(crate) agent_alias: Option<&'a str>,
}

/// Lightweight metadata for turn-level event emission.
/// Derived on-demand from `TurnCtx` via `meta()` — not a cached duplicate.
#[derive(Clone, Copy)]
pub(crate) struct TurnMeta<'a> {
    pub(crate) agent_alias: Option<&'a str>,
    pub(crate) turn_id: &'a str,
    pub(crate) channel_name: &'a str,
}

impl<'a> TurnCtx<'a> {
    pub(crate) fn meta(&self) -> TurnMeta<'a> {
        TurnMeta {
            agent_alias: self.agent_alias,
            turn_id: self.turn_id,
            channel_name: self.channel_name,
        }
    }
}
