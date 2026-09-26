//! Authenticated HTTP fan-in for SOP webhook triggers.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;

use super::{
    AppState, WebhookJsonResponse, authorize_webhook_request, check_webhook_idempotency,
    require_sop_dispatch_credentials,
};
use zeroclaw_runtime::sop::{SopEvent, SopTriggerSource, WebhookDispatch};

pub(super) enum SopWebhookOutcome {
    NoMatch,
    Handled(WebhookJsonResponse),
}

fn unavailable() -> WebhookJsonResponse {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": "sop_dispatch_unavailable" })),
    )
}

/// Check the current engine on demand. The gateway does not retain a second
/// trigger index, so reloads are visible here without refreshing cached state.
pub(super) fn has_matching_webhook_sop(
    state: &AppState,
    path: &str,
) -> Result<bool, WebhookJsonResponse> {
    let (Some(engine), Some(_audit)) = (&state.sop_engine, &state.sop_audit) else {
        if state.sop_engine.is_none() && state.sop_audit.is_none() {
            return Ok(false);
        }
        return Err(unavailable());
    };

    let event = SopEvent {
        source: SopTriggerSource::Webhook,
        topic: Some(path.to_string()),
        payload: None,
        timestamp: zeroclaw_runtime::sop::engine::now_iso8601(),
    };
    engine
        .lock()
        .map(|engine| !engine.match_trigger(&event).is_empty())
        .map_err(|_| unavailable())
}

pub(super) async fn dispatch_webhook_sop(
    state: &AppState,
    path: &str,
    payload: Option<&str>,
) -> SopWebhookOutcome {
    let (Some(engine), Some(audit)) = (&state.sop_engine, &state.sop_audit) else {
        return SopWebhookOutcome::Handled(unavailable());
    };

    let config = state.config.read().clone();
    let (blocked_only, results) = match zeroclaw_runtime::sop::dispatch_webhook_event(
        engine,
        audit,
        state.sop_driver_handles.as_ref(),
        &config,
        path,
        payload,
    )
    .await
    {
        WebhookDispatch::Unavailable => return SopWebhookOutcome::Handled(unavailable()),
        WebhookDispatch::NoMatch => return SopWebhookOutcome::NoMatch,
        WebhookDispatch::Dispatched { blocked, results } => (blocked, results),
    };
    let status = if blocked_only {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    SopWebhookOutcome::Handled((
        status,
        Json(serde_json::json!({
            "status": if blocked_only { "blocked" } else { "accepted" },
            "source": "webhook",
            "path": path,
            "results": results,
        })),
    ))
}

/// `POST /sop/{*rest}` — SOP-only webhook fan-in. A request that does not
/// match a loaded SOP returns 404 and never enters the chat/LLM path.
pub async fn handle_sop_webhook(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    Path(rest): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let auth_verdict = match authorize_webhook_request(&state, peer_addr, &headers) {
        Ok(verdict) => verdict,
        Err(response) => return response.into_response(),
    };
    // `/sop/*` is side-effecting and has no open chat fallback. After the
    // shared rate/auth checks above, fail closed before parsing the body or
    // consulting daemon-owned SOP state so anonymous callers cannot probe it.
    // The decision comes from the request-scoped verdict captured above, never
    // from a second read of the live config.
    if let Err(response) = require_sop_dispatch_credentials(auth_verdict) {
        return response.into_response();
    }

    let payload = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(value) => Some(value.to_string()),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "invalid_json" })),
                )
                    .into_response();
            }
        }
    };
    let path = format!("/sop/{rest}");

    match has_matching_webhook_sop(&state, &path) {
        Ok(true) => {}
        Ok(false) if state.sop_engine.is_none() && state.sop_audit.is_none() => {
            return unavailable().into_response();
        }
        Ok(false) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "no_matching_sop",
                    "path": path,
                })),
            )
                .into_response();
        }
        Err(response) => return response.into_response(),
    }

    // Namespace idempotency by the specific SOP path, not just the shared
    // `/sop/*` family — otherwise the same caller key sent to two different
    // SOP paths (e.g. `/sop/deploy` then `/sop/rollback`) would wrongly
    // suppress the second one as a duplicate of the first.
    let idempotency_namespace = format!("sop:{path}");
    if let Some(response) =
        check_webhook_idempotency(&state, &headers, Some(&idempotency_namespace))
    {
        return response.into_response();
    }

    match dispatch_webhook_sop(&state, &path, payload.as_deref()).await {
        SopWebhookOutcome::Handled(response) => response.into_response(),
        SopWebhookOutcome::NoMatch => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "no_matching_sop",
                "path": path,
            })),
        )
            .into_response(),
    }
}
