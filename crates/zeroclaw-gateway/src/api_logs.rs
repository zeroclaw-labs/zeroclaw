//! `GET /api/logs` — paginated query over the persisted JSONL log.
//!
//! Thin HTTP adapter over [`zeroclaw_log::load_page`]. Pagination is
//! cursor-based: responses include `next_cursor: (timestamp, id)` which
//! callers pass back as `until_ts` / `until_id` to fetch older events.
//!
//! Top-level query params: `since_ts`, `until_ts`, `until_id`, `action`,
//! `category`, `outcome`, `severity_min`, `trace_id`, `q`,
//! `hide_internal`, `limit`. Every other `?key=value` is treated as a
//! per-attribution exact-match (`zeroclaw.<key> == value`), driven by
//! [`zeroclaw_log::is_attribution_field`]. Adding a new attribution
//! field anywhere in the schema requires no changes here.

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
    /// `Some((timestamp, id))` when more older events may exist.
    pub next_cursor: Option<(String, String)>,
    /// True when the file was fully scanned for this filter.
    pub at_end: bool,
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

pub async fn handle_api_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(path) = zeroclaw_log::current_log_path() else {
        return Json(LogsResponse {
            events: Vec::new(),
            next_cursor: None,
            at_end: true,
            daemon_started_at: zeroclaw_runtime::health::daemon_started_at(),
            attribution_keys: attribution_keys_for_response(),
        })
        .into_response();
    };

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
        action: take("action"),
        category: take("category"),
        outcome: take("outcome"),
        severity_min,
        trace_id: take("trace_id"),
        q: take("q"),
        hide_internal,
        field_eq,
    };

    let LogPage {
        events,
        next_cursor,
        at_end,
    } = match zeroclaw_log::load_page(&path, &filter, limit) {
        Ok(page) => page,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("log read failed: {err:#}"),
                })),
            )
                .into_response();
        }
    };

    let events_json: Vec<serde_json::Value> = events
        .into_iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .collect();

    Json(LogsResponse {
        events: events_json,
        next_cursor,
        at_end,
        daemon_started_at: zeroclaw_runtime::health::daemon_started_at(),
        attribution_keys: attribution_keys_for_response(),
    })
    .into_response()
}
