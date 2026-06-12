//! The agent turn engine, decomposed into single-purpose step modules.
//!
//! Public paths are unchanged: external code keeps importing via
//! `crate::agent::loop_::*` (re-export block there). See the run sheet in
//! `/opt/notes/work/zeroclaw/unification_modular/RUN_SHEET.md` during the
//! #7415 migration.

pub(crate) mod delivery_defaults;
pub(crate) mod events;
pub(crate) mod outcome;
pub(crate) mod protocol_detect;
pub(crate) mod redact;
pub(crate) mod stream_guard;

pub(crate) use delivery_defaults::maybe_inject_channel_delivery_defaults;
pub(crate) use protocol_detect::{
    detect_internal_protocol_without_tools, detect_tool_call_parse_issue_for_known_tools,
};
pub use events::{DraftEvent, PROGRESS_MIN_INTERVAL_MS, StreamDelta};
pub(crate) use events::STREAM_CHUNK_MIN_CHARS;
pub use outcome::{
    ModelSwitchCallback, ModelSwitchRequested, ToolLoopCancelled, is_model_switch_requested,
    is_tool_loop_cancelled,
};
pub use redact::scrub_credentials;
pub(crate) use stream_guard::{StreamTextGuard, StreamThinkTagStripper};
