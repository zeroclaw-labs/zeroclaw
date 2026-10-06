//! `GET /api/logs` — paginated query over the persisted JSONL log.

use std::collections::{BTreeMap, HashMap};

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use zeroclaw_log::{LogFilter, LogPage, is_attribution_field};

use super::AppState;
use super::api::require_auth;
use crate::core_rpc::{CoreAccess, CoreCall, CoreError};
use zeroclaw_api::jsonrpc::JsonRpcError;
use zeroclaw_api::jsonrpc::error_codes::{INTERNAL_ERROR, METHOD_NOT_FOUND};
use zeroclaw_rpc_client::Method;
use zeroclaw_rpc_proto::types::{LogsQueryParams, LogsQueryResult};

const TOP_LEVEL_PARAMS: &[&str] = &[
    "since_ts",
    "until_ts",
    "until_id",
    "until_line_offset",
    "until_segment_cursor",
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
    /// Composite segment-aware cursor for the oldest event on this page. Pass
    /// back as `?until_segment_cursor=` on the next request to paginate
    /// across segment boundaries (active file + rotated archives). Supersedes
    /// `next_cursor_line_offset` for `rotating`-mode deployments with
    /// multiple retained segments.
    pub next_segment_cursor: Option<String>,
    /// True when the file was fully scanned for this filter.
    pub at_end: bool,
    /// True when a retained segment could not be read and was left out of this
    /// page. `at_end` then means "no older events among the segments that could
    /// be read", which is weaker than "no older events exist", so a client that
    /// stops paging on `at_end` should present the history as partial.
    pub incomplete: bool,
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
    zeroclaw_log::attribution_keys()
}

/// Read one page from the canonical persisted log store. Gateway surfaces with
/// different authorization policies (the dashboard and the localhost admin CLI)
/// share this helper so filtering, pagination, and retention behavior cannot
/// drift between them.
///
/// The scope comes from the writer that is actually running, so a daemon with
/// persistence disabled answers `persistence_enabled: false` rather than
/// serving a stale file left at the configured path.
#[allow(deprecated)] // we still forward the legacy cursor for backwards compat
pub(crate) fn load_logs_response(
    filter: &LogFilter,
    limit: usize,
    segment_cursor: Option<&zeroclaw_log::SegmentCursor>,
) -> anyhow::Result<LogsResponse> {
    let Some((active, reads_archives)) = zeroclaw_log::active_log_query_scope() else {
        return Ok(LogsResponse {
            events: Vec::new(),
            next_cursor: None,
            next_cursor_line_offset: None,
            next_segment_cursor: None,
            at_end: true,
            incomplete: false,
            persistence_enabled: false,
            daemon_started_at: zeroclaw_runtime::health::daemon_started_at(),
            attribution_keys: attribution_keys_for_response(),
        });
    };

    let LogPage {
        events,
        next_cursor,
        next_cursor_line_offset,
        next_segment_cursor,
        at_end,
        incomplete,
    } = zeroclaw_log::query_log_page(&active, reads_archives, filter, limit, segment_cursor)?;

    let events = events
        .into_iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .collect();

    Ok(LogsResponse {
        events,
        next_cursor,
        next_cursor_line_offset,
        next_segment_cursor,
        at_end,
        incomplete,
        persistence_enabled: true,
        daemon_started_at: zeroclaw_runtime::health::daemon_started_at(),
        attribution_keys: attribution_keys_for_response(),
    })
}

pub async fn handle_api_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    access: CoreAccess,
) -> Response {
    if let CoreAccess::Core(core) = access {
        return api_logs_through_core(&core, &params)
            .await
            .unwrap_or_else(IntoResponse::into_response);
    }
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let request = match LogsRequest::parse(&params) {
        Ok(request) => request,
        Err(refused) => return refused,
    };
    match load_logs_response(
        &request.filter,
        request.limit,
        request.segment_cursor.as_ref(),
    ) {
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

/// `GET /api/logs` through the core, the body every router serves for it.
/// The query is read and refused exactly as the in-process route reads it,
/// then the core filters its own log.
#[allow(deprecated)] // we still forward the legacy cursor for backwards compat
pub(crate) async fn api_logs_through_core(
    core: &CoreCall,
    params: &HashMap<String, String>,
) -> Result<Response, CoreError> {
    let request = match LogsRequest::parse(params) {
        Ok(request) => request,
        Err(refused) => return Ok(refused),
    };
    let LogFilter {
        since_ts,
        until_ts,
        until_id,
        until_line_offset,
        action,
        category,
        outcome,
        severity_min,
        trace_id,
        q,
        hide_internal,
        field_eq,
    } = request.filter;
    let query = LogsQueryParams {
        since_ts,
        until_ts,
        until_id,
        until_line_offset,
        until_segment_cursor: request.segment_cursor.map(|cursor| cursor.to_wire()),
        severity_min,
        q,
        category,
        action,
        outcome,
        trace_id,
        sop_run_id: None,
        hide_internal,
        limit: Some(request.limit),
        field_eq,
        report_disabled: true,
    };
    crate::api::require_core_feature(core, zeroclaw_rpc_proto::feature::LOGS_REPORT_DISABLED)?;
    crate::api::require_core_feature(core, zeroclaw_rpc_proto::feature::LOGS_QUERY_METADATA)?;
    if !query.field_eq.is_empty() {
        crate::api::require_core_feature(core, zeroclaw_rpc_proto::feature::LOGS_FIELD_EQ)?;
    }
    let params = serde_json::to_value(&query).map_err(|error| {
        CoreError::Rpc(JsonRpcError {
            code: INTERNAL_ERROR,
            message: format!("unencodable logs/query params: {error}"),
            data: None,
        })
    })?;
    let page: LogsQueryResult = core.call(Method::LogsQuery, params).await?;
    // A core that predates the attribution filters would ignore them and
    // answer unfiltered; it also omits the start time it now always reports.
    let Some(daemon_started_at) = page.daemon_started_at else {
        return Err(CoreError::Rpc(JsonRpcError {
            code: METHOD_NOT_FOUND,
            message: "the core did not return dashboard log metadata; install a core that \
                      supports this route"
                .into(),
            data: None,
        }));
    };
    Ok(Json(LogsResponse {
        events: page.events,
        next_cursor: page.next_cursor,
        next_cursor_line_offset: page.next_cursor_line_offset,
        next_segment_cursor: page.next_segment_cursor,
        at_end: page.at_end,
        incomplete: page.incomplete,
        persistence_enabled: page.persistence_enabled,
        daemon_started_at,
        attribution_keys: page.attribution_keys,
    })
    .into_response())
}

/// One `GET /api/logs` query, read as the route reads it: an unparsable
/// number is ignored, an unknown parameter or a malformed segment cursor is
/// refused with `400`.
struct LogsRequest {
    filter: LogFilter,
    limit: usize,
    segment_cursor: Option<zeroclaw_log::SegmentCursor>,
}

impl LogsRequest {
    #[allow(clippy::result_large_err)] // the refusal is the route's response
    fn parse(params: &HashMap<String, String>) -> Result<Self, Response> {
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
        let segment_cursor: Option<zeroclaw_log::SegmentCursor> = match params
            .get("until_segment_cursor")
            .map(|s| s.as_str())
        {
            None | Some("") => None,
            Some(raw) => match zeroclaw_log::SegmentCursor::from_wire(raw) {
                Some(c) => Some(c),
                None => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": "invalid until_segment_cursor: value is not a valid segment cursor",
                        })),
                    )
                        .into_response());
                }
            },
        };

        let mut field_eq: BTreeMap<String, String> = BTreeMap::new();
        for (key, value) in params {
            if TOP_LEVEL_PARAMS.contains(&key.as_str()) {
                continue;
            }
            if !is_attribution_field(key) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("unknown query parameter: {key}"),
                    })),
                )
                    .into_response());
            }
            if value.is_empty() {
                continue;
            }
            field_eq.insert(key.clone(), value.clone());
        }

        Ok(Self {
            filter: LogFilter {
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
            },
            limit,
            segment_cursor,
        })
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
