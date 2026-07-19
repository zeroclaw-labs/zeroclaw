//! Unified log emission surface for the ZeroClaw workspace.

pub mod broadcast;
pub mod chain;
pub mod config;
pub mod event;
pub mod layer;
pub mod migrate;
pub mod observer_bridge;
pub mod reader;
mod subscriber;
pub mod tool_io;
pub mod writer;

/// Private re-export root. The `record!` / `scope!` / `attribution_span!`
/// macros expand to paths under here so external crates can never
/// reach `tracing` types via `zeroclaw_log::*`. Do NOT use directly
/// from anywhere outside this crate.
#[doc(hidden)]
pub mod __private {
    pub use ::chrono;
    pub use ::serde_json;
    pub use ::tracing;
    pub use ::uuid;
}

pub use broadcast::{
    LogBroadcastSender, clear_broadcast_hook, current_broadcast_hook, set_broadcast_hook,
    subscribe, subscribe_or_install,
};
pub use chain::display_chain;
pub use config::{LlmRequestPayloadPolicy, LogConfig, ResolvedPolicy, StoragePolicy, ToolIoPolicy};
pub use event::{
    ATTRIBUTION_FIELDS, Action, COMPOSITE_PREFIXES, Event, EventCategory, EventOutcome, LogEvent,
    Severity, ZeroclawAttribution, is_attribution_field, severity_text_from_number,
    severity_text_from_tracing_level,
};
pub use layer::LogCaptureLayer;
pub use writer::{
    EPHEMERAL_BROADCAST_MARKER, frame_carries_ephemeral_credentials,
    strip_ephemeral_broadcast_marker,
};

/// Opaque span handle. Same wire format as `tracing::Span` (we re-export
/// the type) but the public path is `zeroclaw_log::Span` — no `tracing`
/// in any consumer's source.
pub use ::tracing::Span;

/// Future combinator that attaches a [`Span`] to the future. Use as
/// `future.instrument(span).await` at entry points.
pub use ::tracing::Instrument;

/// Ad-hoc span constructors. Prefer `attribution_span!(thing)` when
/// the field set comes from an `Attributable` impl; reach for these
/// only when the work doesn't tie to a role.
pub use ::tracing::{debug_span, error_span, info_span, trace_span, warn_span};

/// Span field helpers (e.g. [`field::Empty`] for fields that get
/// recorded later via `span.record(...)`).
pub mod field {
    pub use ::tracing::field::{Empty, FieldSet};
}

pub use migrate::migrate_legacy_jsonl_in_place;
pub use observer_bridge::{clear_observer_bridge, set_observer_bridge};
pub use reader::{LogFilter, LogPage, current_log_path, find_event_by_id, load_page};
pub use subscriber::{install_global_subscriber, try_install_capture_subscriber};
pub use tool_io::{ToolIoCapture, capture_llm_request, capture_tool_input, capture_tool_output};
pub use writer::{
    flush_for_test, init_from_config, llm_request_payload_policy, record_event, runtime_trace_path,
};

mod r#macro;

/// Returns whether ZeroClaw DEBUG log events are enabled for the current
/// subscriber. Use this before building expensive structured DEBUG attrs.
pub fn debug_enabled() -> bool {
    ::tracing::enabled!(
        target: "zeroclaw_log_event",
        ::tracing::Level::DEBUG
    )
}

#[doc(hidden)]
#[must_use]
pub fn __private_test_writer_lock() -> impl Drop {
    crate::writer::WRITER_TEST_LOCK.lock()
}

#[doc(hidden)]
#[must_use]
pub fn __private_test_hook_lock() -> impl Drop {
    crate::broadcast::HOOK_TEST_LOCK.lock()
}
