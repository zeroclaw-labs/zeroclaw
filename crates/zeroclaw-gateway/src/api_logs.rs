//! `GET /api/logs` — paginated query over the persisted JSONL log.

use std::collections::{BTreeMap, HashMap};

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use zeroclaw_log::{
    ATTRIBUTION_FIELDS, COMPOSITE_PREFIXES, LogFilter, LogPage, is_attribution_field,
};

use super::AppState;
use super::api::require_auth;

const TOP_LEVEL_PARAMS: &[&str] = &[
    "since_ts",
    "until_ts",
    "until_id",
    "until_line_offset",
    "action",
    "category",
    "outcome",
    "severity_min",
    "trace_id",
    "q",
    "hide_internal",
    "limit",
];

#[derive(Debug, Serialize)]
pub struct LogsResponse {
    pub events: Vec<serde_json::Value>,
    #[deprecated(
        since = "0.8.0",
        note = "tie-breaks by lexicographic id and can silently drop events; \
                use `next_cursor_line_offset` / `until_line_offset` instead. \
                Removal tracked in zeroclaw-labs/zeroclaw#8012."
    )]
    pub next_cursor: Option<(String, String)>,
    /// Byte offset past the last event on this page. Pass back as
    /// `?until_line_offset=` on the next request to resume without
    /// re-scanning already-read bytes.
    pub next_cursor_line_offset: Option<u64>,
    /// True when the file was fully scanned for this filter.
    pub at_end: bool,
    /// Whether this daemon is persisting the runtime trace. An empty event list
    /// is otherwise ambiguous between "no matches" and "logging disabled".
    pub persistence_enabled: bool,
    /// Daemon start time so callers can implement "since daemon start"
    /// without an extra `/api/status` round-trip.
    pub daemon_started_at: String,
    /// Canonical attribution-field names — `ATTRIBUTION_FIELDS` plus, for
    /// each entry in `COMPOSITE_PREFIXES`, the bare prefix and its
    /// `<prefix>_type` / `<prefix>_alias` decomposed keys. The dashboard
    /// reads this instead of enumerating schema fields client-side.
    pub attribution_keys: Vec<String>,
}

fn attribution_keys_for_response() -> Vec<String> {
    let mut keys: Vec<String> = ATTRIBUTION_FIELDS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    for prefix in COMPOSITE_PREFIXES {
        keys.push((*prefix).to_string());
        keys.push(format!("{prefix}_type"));
        keys.push(format!("{prefix}_alias"));
    }
    keys
}

/// Read one page from the canonical persisted log store. Gateway surfaces with
/// different authorization policies (the dashboard and localhost admin CLI)
/// share this helper so filtering, pagination, and retention behavior cannot
/// drift between them.
#[allow(deprecated)] // we still forward the legacy cursor for backwards compat
pub(crate) fn load_logs_response(filter: &LogFilter, limit: usize) -> anyhow::Result<LogsResponse> {
    // The storage-aware accessor, not the configured one. `current_log_path`
    // reports the path the writer was configured with whatever the storage
    // mode, so a daemon running `log_persistence = "none"` would answer
    // `persistence_enabled: true` and then serve whatever stale file happened
    // to sit at that path. The RPC twin already reads `active_log_path`; this
    // is the same question, so it reads the same source of truth.
    let Some(path) = zeroclaw_log::active_log_path() else {
        return Ok(LogsResponse {
            events: Vec::new(),
            next_cursor: None,
            next_cursor_line_offset: None,
            at_end: true,
            persistence_enabled: false,
            daemon_started_at: zeroclaw_runtime::health::daemon_started_at(),
            attribution_keys: attribution_keys_for_response(),
        });
    };

    let LogPage {
        events,
        next_cursor,
        next_cursor_line_offset,
        at_end,
    } = zeroclaw_log::load_page(&path, filter, limit)?;

    let events = events
        .into_iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .collect();

    Ok(LogsResponse {
        events,
        next_cursor,
        next_cursor_line_offset,
        at_end,
        persistence_enabled: true,
        daemon_started_at: zeroclaw_runtime::health::daemon_started_at(),
        attribution_keys: attribution_keys_for_response(),
    })
}

#[allow(deprecated)] // we still forward the legacy cursor for backwards compat
pub async fn handle_api_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let take = |key: &str| -> Option<String> {
        params.get(key).map(String::from).filter(|s| !s.is_empty())
    };

    let severity_min = params
        .get("severity_min")
        .and_then(|raw| raw.parse::<u8>().ok());
    let hide_internal = params
        .get("hide_internal")
        .map(|raw| matches!(raw.as_str(), "true" | "1" | "yes"))
        .unwrap_or(false);
    let limit = params
        .get("limit")
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(200);
    let until_line_offset = params
        .get("until_line_offset")
        .and_then(|raw| raw.parse::<u64>().ok());

    let mut field_eq: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in &params {
        if TOP_LEVEL_PARAMS.contains(&key.as_str()) {
            continue;
        }
        if !is_attribution_field(key) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("unknown query parameter: {key}"),
                })),
            )
                .into_response();
        }
        if value.is_empty() {
            continue;
        }
        field_eq.insert(key.clone(), value.clone());
    }

    let filter = LogFilter {
        since_ts: take("since_ts"),
        until_ts: take("until_ts"),
        until_id: take("until_id"),
        until_line_offset,
        action: take("action"),
        category: take("category"),
        outcome: take("outcome"),
        severity_min,
        trace_id: take("trace_id"),
        q: take("q"),
        hide_internal,
        field_eq,
    };

    match load_logs_response(&filter, limit) {
        Ok(response) => Json(response).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("log read failed: {err:#}"),
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::attribution_keys_for_response;

    #[test]
    fn attribution_keys_expose_sop_run_id_to_dynamic_clients() {
        assert!(
            attribution_keys_for_response()
                .iter()
                .any(|key| key == "sop_run_id")
        );
    }
}
