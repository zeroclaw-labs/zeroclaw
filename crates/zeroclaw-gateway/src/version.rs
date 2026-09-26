//! The dashboard's `/api/version/*` routes: an HTTP adapter over
//! [`zeroclaw_runtime::self_upgrade`], which owns the version check, restart
//! classification and the one in-flight upgrade the core's `system/*` methods
//! also drive.

use super::AppState;
use super::api::require_auth;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
pub use zeroclaw_runtime::self_upgrade::{
    CheckQuery, RestartInfo, RestartMode, UpgradeAcceptedResponse, UpgradeRequest,
    UpgradeStatusQuery, UpgradeStatusResponse, UpgradeStatusState, VersionCheckResponse,
    VersionErrorResponse, detect_restart,
};

fn json_error(code: StatusCode, msg: &str) -> axum::response::Response {
    (
        code,
        Json(VersionErrorResponse {
            error: msg.to_string(),
        }),
    )
        .into_response()
}

fn refusal_response(
    refusal: zeroclaw_runtime::self_upgrade::UpgradeRefusal,
) -> axum::response::Response {
    json_error(
        StatusCode::from_u16(refusal.http_status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        &refusal.message,
    )
}

/// `GET /api/version/check[?force=true][&version=X]`
///
/// Never fails the dashboard: on any error it returns 200 with
/// `{ is_newer: false, error }` so the version tag degrades gracefully.
pub async fn handle_version_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CheckQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    Json(zeroclaw_runtime::self_upgrade::version_check(&q).await).into_response()
}

/// POST /api/version/upgrade — apply an upgrade via `zeroclaw update`.
///
/// Returns 202 with a `handoff_id`; the work runs on a detached task and the
/// client polls `GET /api/version/upgrade/status`.
pub async fn handle_version_upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let req: UpgradeRequest = if body.is_empty() {
        UpgradeRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("invalid body: {e}")),
        }
    };
    let allow_self_upgrade = state.config.read().gateway.allow_self_upgrade;
    // Without a daemon wrapper (`zeroclaw gateway start`) nobody listens for
    // the daemon's shutdown signal, so a self-respawn signals the gateway's
    // own shutdown watch instead.
    let standalone_shutdown_tx = state.reload_tx.is_none().then(|| state.shutdown_tx.clone());
    match zeroclaw_runtime::self_upgrade::start_upgrade(
        allow_self_upgrade,
        req,
        standalone_shutdown_tx,
    ) {
        Ok(accepted) => (StatusCode::ACCEPTED, Json(accepted)).into_response(),
        Err(refusal) => refusal_response(refusal),
    }
}

/// GET /api/version/upgrade/status[?handoff_id=X]
pub async fn handle_version_upgrade_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UpgradeStatusQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    match zeroclaw_runtime::self_upgrade::upgrade_status(q.handoff_id.as_deref()) {
        Ok(status) => Json(status).into_response(),
        Err(refusal) => refusal_response(refusal),
    }
}
