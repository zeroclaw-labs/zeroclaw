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

/// Shared references threaded through the turn step functions.
///
/// Carries shared refs ONLY — every `&mut` the loop owns (history, loop
/// detector, counters, accumulated text, retry counters) stays a loop local
/// passed as an explicit individual argument. Never add a `&mut` field:
/// it creates overlapping-borrow errors across step calls (RUN_SHEET
/// `turn.context.TurnCtx`).
///
/// Fields the orchestrator threads to steps as explicit arguments
/// (`model_provider`, `tools_registry`, vision/tool config, receipts, …)
/// are NOT carried here — only the values steps actually read off `ctx`.
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
}
