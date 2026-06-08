//! Compatibility shim for the doctor command's log-reading utilities.
//!
//! The legacy positional-arg `record_event` shim was retired in favor of
//! direct `zeroclaw_log::record!` invocations carrying typed attribution
//! via `attribution_span!`. This module survives only as the doctor
//! command's path-resolution + load surface; new emission code goes
//! directly to `zeroclaw_log::record!`.

use std::path::Path;

use zeroclaw_log::LogEvent;

pub use zeroclaw_log::{LogEvent as RuntimeTraceEvent, LogFilter, LogPage};

fn to_log_config(config: &zeroclaw_config::schema::ObservabilityConfig) -> zeroclaw_log::LogConfig {
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
