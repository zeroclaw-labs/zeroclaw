//! The agent turn engine, decomposed into single-purpose step modules.
//!
//! Public paths are unchanged: external code keeps importing via
//! `crate::agent::loop_::*` (re-export block there). See the run sheet in
//! `/opt/notes/work/zeroclaw/unification_modular/RUN_SHEET.md` during the
//! #7415 migration.

pub(crate) mod approval_gate;
pub(crate) mod call_prep;
pub(crate) mod context;
pub(crate) mod context_recovery;
pub(crate) mod delivery_defaults;
pub(crate) mod events;
pub(crate) mod history_append;
pub(crate) mod history_window;
pub(crate) mod max_iter;
pub(crate) mod outcome;
pub(crate) mod parse_response;
pub(crate) mod post_exec;
pub(crate) mod protocol_detect;
pub(crate) mod provider_call;
pub(crate) mod redact;
pub(crate) mod results_collect;
pub(crate) mod stream_consume;
pub(crate) mod stream_guard;
pub(crate) mod tool_specs;
pub(crate) mod vision_route;

pub(crate) use call_prep::{PreparedToolCalls, prepare_tool_calls};
pub(crate) use context::TurnCtx;
pub(crate) use context_recovery::{record_llm_failure, try_recover_context_overflow};
#[cfg(test)]
pub(crate) use delivery_defaults::maybe_inject_channel_delivery_defaults;
pub(crate) use events::STREAM_CHUNK_MIN_CHARS;
pub use events::{DraftEvent, PROGRESS_MIN_INTERVAL_MS, StreamDelta};
pub(crate) use history_append::append_tool_round_to_history;
pub(crate) use history_window::preflight_history_maintenance;
pub(crate) use max_iter::finish_after_max_iterations;
pub use outcome::{
    ModelSwitchCallback, ModelSwitchRequested, ToolLoopCancelled, is_model_switch_requested,
    is_tool_loop_cancelled,
};
#[cfg(test)]
pub(crate) use parse_response::build_native_assistant_history;
pub(crate) use parse_response::{interpret_chat_response, resolve_display_text};
pub(crate) use post_exec::record_executed_outcomes;
pub(crate) use provider_call::{
    ProviderCallOutcome, announce_llm_request, call_provider, enforce_tool_loop_budget,
};
pub use redact::scrub_credentials;
pub(crate) use results_collect::{
    CollectedResults, check_identical_output_abort, collect_tool_results,
};
#[cfg(test)]
pub(crate) use stream_consume::StreamedChatOutcome;
#[cfg(test)]
pub(crate) use stream_consume::consume_provider_streaming_response;
pub(crate) use tool_specs::{IterationToolSpecs, build_iteration_tool_specs};
pub(crate) use vision_route::{prepare_messages_for_iteration, resolve_vision_provider};
