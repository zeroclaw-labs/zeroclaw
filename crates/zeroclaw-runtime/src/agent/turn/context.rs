//! Shared read-only context for the per-iteration turn step functions.

use super::events::DraftEvent;
use crate::agent::tool_receipts::ReceiptGenerator;
use crate::approval::ApprovalManager;
use crate::hooks::HookRunner;
use crate::observability::Observer;
use crate::tools::{ActivatedToolSet, Tool};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::channel::Channel;
use zeroclaw_config::schema::{MultimodalConfig, PacingConfig};
use zeroclaw_providers::ModelProvider;

/// Shared references threaded through the turn step functions.
///
/// Carries shared refs ONLY — every `&mut` the loop owns (history, loop
/// detector, counters, accumulated text, retry counters) stays a loop local
/// passed as an explicit individual argument. Never add a `&mut` field:
/// it creates overlapping-borrow errors across step calls (RUN_SHEET
/// `turn.context.TurnCtx`).
// Some fields are not yet read through `ctx` — the orchestrator still passes
// them as explicit step arguments per the RUN_SHEET contracts. G2 (event_tx
// wiring, knobs) consumes the remainder; drop the allow then.
#[allow(dead_code)]
pub(crate) struct TurnCtx<'a> {
    pub(crate) model_provider: &'a dyn ModelProvider,
    pub(crate) tools_registry: &'a [Box<dyn Tool>],
    pub(crate) observer: &'a dyn Observer,
    pub(crate) provider_name: &'a str,
    pub(crate) model: &'a str,
    pub(crate) temperature: Option<f64>,
    pub(crate) silent: bool,
    pub(crate) approval: Option<&'a ApprovalManager>,
    pub(crate) channel_name: &'a str,
    pub(crate) channel_reply_target: Option<&'a str>,
    pub(crate) multimodal_config: &'a MultimodalConfig,
    pub(crate) cancellation_token: Option<&'a CancellationToken>,
    pub(crate) on_delta: Option<&'a Sender<DraftEvent>>,
    pub(crate) event_tx: Option<&'a Sender<TurnEvent>>,
    pub(crate) hooks: Option<&'a HookRunner>,
    pub(crate) excluded_tools: &'a [String],
    pub(crate) dedup_exempt_tools: &'a [String],
    pub(crate) activated_tools: Option<&'a Arc<Mutex<ActivatedToolSet>>>,
    pub(crate) pacing: &'a PacingConfig,
    pub(crate) strict_tool_parsing: bool,
    pub(crate) parallel_tools: bool,
    pub(crate) max_tool_result_chars: usize,
    pub(crate) channel: Option<&'a dyn Channel>,
    pub(crate) receipt_generator: Option<&'a ReceiptGenerator>,
    pub(crate) collected_receipts: Option<&'a Mutex<Vec<String>>>,
    pub(crate) turn_id: &'a str,
}
