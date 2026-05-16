//! Compatibility shim: the actual log surface lives in `zeroclaw-log`.
//! This module is retained only so existing in-tree callers
//! (`agent::loop_`, the gateway's `ws.rs`, the channel orchestrator, and
//! the doctor command) can keep importing `runtime_trace::...` while the
//! workspace transitions to direct `zeroclaw_log::...` use.
//!
//! Everything here forwards to `zeroclaw_log` with no behavior change.
//! New call sites should reach for `zeroclaw_log::record!` directly.

use std::path::Path;

use zeroclaw_log::{EventCategory, EventOutcome, LogEvent, Severity};

pub use zeroclaw_log::{LogEvent as RuntimeTraceEvent, LogFilter, LogPage};

fn to_log_config(
    config: &zeroclaw_config::schema::ObservabilityConfig,
) -> zeroclaw_log::LogConfig {
    zeroclaw_log::LogConfig {
        log_persistence: config.log_persistence.clone(),
        log_persistence_path: config.log_persistence_path.clone(),
        log_persistence_max_entries: config.log_persistence_max_entries,
        log_tool_io: config.log_tool_io.clone(),
        log_tool_io_truncate_bytes: config.log_tool_io_truncate_bytes,
        log_tool_io_denylist: config.log_tool_io_denylist.clone(),
    }
}

/// Initialize log persistence from the observability config.
pub fn init_from_config(
    config: &zeroclaw_config::schema::ObservabilityConfig,
    workspace_dir: &Path,
) {
    zeroclaw_log::init_from_config(&to_log_config(config), workspace_dir);
}

/// Resolve the configured log path (used by the doctor command).
pub fn resolve_trace_path(
    config: &zeroclaw_config::schema::ObservabilityConfig,
    workspace_dir: &Path,
) -> std::path::PathBuf {
    let policy = zeroclaw_log::ResolvedPolicy::from_config(&to_log_config(config), workspace_dir);
    policy.path
}

/// Legacy entry point. Bridges the pre-v0.8.0 positional-arg interface to
/// the new structured [`LogEvent`]. Prefer `zeroclaw_log::record!` at new
/// call sites — it handles alias-bound splitting automatically and lets
/// `tracing::event!` carry the correct call-site source info.
#[allow(clippy::too_many_arguments)]
pub fn record_event(
    event_type: &str,
    channel: Option<&str>,
    model_provider: Option<&str>,
    model: Option<&str>,
    turn_id: Option<&str>,
    success: Option<bool>,
    message: Option<&str>,
    payload: serde_json::Value,
) {
    record_event_with_agent(
        event_type,
        channel,
        model_provider,
        model,
        turn_id,
        success,
        message,
        None,
        payload,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn record_event_with_agent(
    event_type: &str,
    channel: Option<&str>,
    model_provider: Option<&str>,
    model: Option<&str>,
    turn_id: Option<&str>,
    success: Option<bool>,
    message: Option<&str>,
    agent_alias: Option<&str>,
    payload: serde_json::Value,
) {
    let severity = if matches!(success, Some(false)) {
        Severity::Warn
    } else {
        Severity::Info
    };
    let category = category_for_action(event_type);
    let mut event = LogEvent::new(severity, event_type, category);
    let outcome = match success {
        Some(true) => EventOutcome::Success,
        Some(false) => EventOutcome::Failure,
        None => EventOutcome::Unknown,
    };
    event.set_outcome(outcome);
    if let Some(channel) = channel {
        event.zeroclaw.set_composite("channel", channel);
    }
    if let Some(provider_composite) = model_provider {
        event
            .zeroclaw
            .set_composite("model_provider", provider_composite);
    }
    if let Some(model) = model {
        event.zeroclaw.set("model", model);
    }
    if let Some(turn) = turn_id {
        event.trace_id = Some(turn.to_string());
    }
    if let Some(agent) = agent_alias {
        event.zeroclaw.set("agent_alias", agent);
    }
    if let Some(msg) = message {
        event.message = Some(msg.to_string());
    }
    event.attributes = payload;
    zeroclaw_log::record_event(event);
}

/// Load a page of events. Replaces the old `load_events` shape with a
/// thin wrapper around the new paginated reader. The legacy
/// `event_filter` (single action match) and `contains` (substring) args
/// map straight onto the new [`LogFilter`] fields.
pub fn load_events(
    path: &Path,
    limit: usize,
    event_filter: Option<&str>,
    contains: Option<&str>,
) -> anyhow::Result<Vec<LogEvent>> {
    let filter = LogFilter {
        action: event_filter.map(str::to_string),
        q: contains.map(str::to_string),
        ..LogFilter::default()
    };
    let page = zeroclaw_log::load_page(path, &filter, limit)?;
    Ok(page.events)
}

/// Lookup a single event by id.
pub fn find_event_by_id(path: &Path, id: &str) -> anyhow::Result<Option<LogEvent>> {
    zeroclaw_log::find_event_by_id(path, id)
}

fn category_for_action(action: &str) -> EventCategory {
    match action {
        "llm_request" | "agent_start" | "agent_end" => EventCategory::Agent,
        "tool_call" | "tool_call_start" | "tool_call_result" => EventCategory::Tool,
        "channel_message_inbound" | "channel_send" => EventCategory::Channel,
        "cron_run" => EventCategory::Cron,
        "memory_store" | "memory_recall" | "memory_forget" => EventCategory::Memory,
        "session_open" | "session_close" | "gateway_ws_turn" => EventCategory::Session,
        "error" => EventCategory::System,
        _ => EventCategory::System,
    }
}
