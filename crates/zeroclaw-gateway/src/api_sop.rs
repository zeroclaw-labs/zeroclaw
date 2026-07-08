//! Out-of-band SOP approval surface (EPIC C, C6).
//!
//! `GET /admin/sop/pending`, `POST /admin/sop/approve`, `POST /admin/sop/deny`.
//! Auth reuses the vetted `/admin/reload` gate verbatim: loopback (the CLI) is
//! allowed and attributed as a `cli` principal; a non-loopback caller needs
//! `gateway.allow_remote_admin` + pairing and passes `require_auth`, attributed
//! as an `http` principal. The principal is ALWAYS derived from the transport
//! here, never from the request body. Resolution funnels through the shared
//! engine's `resolve_gate` chokepoint; `sop_engine = None` yields 503.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::{AdminReloadGate, AppState, admin_reload_gate};
use zeroclaw_runtime::sop::approval::{ApprovalDecision, ApprovalPrincipal, ResolveOutcome};
use zeroclaw_runtime::sop::types::SopRunStatus;

type JsonErr = (StatusCode, Json<serde_json::Value>);

/// Body for approve/deny.
#[derive(Deserialize)]
pub struct SopResolveBody {
    run_id: String,
    #[serde(default)]
    reason: Option<String>,
}

fn sop_disabled() -> JsonErr {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": "SOP subsystem not enabled" })),
    )
}

fn lock_poisoned() -> JsonErr {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "SOP engine lock poisoned" })),
    )
}

/// Authorize an admin SOP call and derive the transport-bound principal. Mirrors
/// `handle_admin_reload`'s gate so the two never diverge.
fn authorize(
    state: &AppState,
    peer: &SocketAddr,
    headers: &HeaderMap,
) -> Result<ApprovalPrincipal, JsonErr> {
    let allow_remote = state.config.read().gateway.allow_remote_admin;
    let require_pairing = state.pairing.require_pairing();
    match admin_reload_gate(peer.ip().is_loopback(), allow_remote, require_pairing) {
        AdminReloadGate::Allow => Ok(ApprovalPrincipal::cli(None)),
        AdminReloadGate::RequireAuth => {
            crate::api::require_auth(state, headers)?;
            Ok(ApprovalPrincipal::http(None))
        }
        AdminReloadGate::Forbidden | AdminReloadGate::ForbiddenNoPairing => Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "Remote SOP approval is disabled. Call from localhost, or set \
                          gateway.allow_remote_admin = true with pairing enabled, then pair."
            })),
        )),
    }
}

/// Map a `ResolveOutcome` to its HTTP status + wire label. Pure (unit-tested).
fn outcome_response(outcome: &ResolveOutcome) -> (StatusCode, &'static str) {
    match outcome {
        ResolveOutcome::Resumed(_) => (StatusCode::OK, "resumed"),
        ResolveOutcome::Denied => (StatusCode::OK, "denied"),
        ResolveOutcome::AlreadyResolved => (StatusCode::OK, "already_resolved"),
        ResolveOutcome::NotWaiting => (StatusCode::NOT_FOUND, "not_waiting"),
        ResolveOutcome::RejectedSelfApproval => (StatusCode::FORBIDDEN, "rejected_self_approval"),
    }
}

/// GET /admin/sop/pending - list the runs currently `WaitingApproval`.
pub async fn handle_sop_pending(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, JsonErr> {
    // Pending is a read, gated the same as the resolve endpoints.
    authorize(&state, &peer, &headers)?;
    let engine = state.sop_engine.as_ref().ok_or_else(sop_disabled)?;
    let guard = engine.lock().map_err(|_| lock_poisoned())?;
    let pending: Vec<serde_json::Value> = guard
        .active_runs()
        .values()
        .filter(|r| r.status == SopRunStatus::WaitingApproval)
        .map(|r| {
            serde_json::json!({
                "run_id": r.run_id,
                "sop_name": r.sop_name,
                "step": r.current_step,
                "total_steps": r.total_steps,
                "waiting_since": r.waiting_since,
            })
        })
        .collect();
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "pending": pending })),
    ))
}

/// POST /admin/sop/approve - clear a waiting gate out-of-band.
pub async fn handle_sop_approve(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<SopResolveBody>,
) -> Result<impl IntoResponse, JsonErr> {
    let principal = authorize(&state, &peer, &headers)?;
    resolve(&state, &body.run_id, ApprovalDecision::Approve, principal)
}

/// POST /admin/sop/deny - deny (cancel) a waiting run out-of-band.
pub async fn handle_sop_deny(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<SopResolveBody>,
) -> Result<impl IntoResponse, JsonErr> {
    let principal = authorize(&state, &peer, &headers)?;
    resolve(
        &state,
        &body.run_id,
        ApprovalDecision::Deny {
            reason: body.reason,
        },
        principal,
    )
}

fn resolve(
    state: &AppState,
    run_id: &str,
    decision: ApprovalDecision,
    principal: ApprovalPrincipal,
) -> Result<(StatusCode, Json<serde_json::Value>), JsonErr> {
    let engine = state.sop_engine.as_ref().ok_or_else(sop_disabled)?;
    let outcome = {
        let mut guard = engine.lock().map_err(|_| lock_poisoned())?;
        guard
            .resolve_gate(run_id, decision, principal)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": format!("resolve failed: {e}") })),
                )
            })?
    };
    let (code, label) = outcome_response(&outcome);
    Ok((
        code,
        Json(serde_json::json!({ "outcome": label, "run_id": run_id })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_response_maps_status_codes() {
        assert_eq!(
            outcome_response(&ResolveOutcome::Denied),
            (StatusCode::OK, "denied")
        );
        assert_eq!(
            outcome_response(&ResolveOutcome::AlreadyResolved),
            (StatusCode::OK, "already_resolved")
        );
        assert_eq!(
            outcome_response(&ResolveOutcome::NotWaiting),
            (StatusCode::NOT_FOUND, "not_waiting")
        );
        assert_eq!(
            outcome_response(&ResolveOutcome::RejectedSelfApproval),
            (StatusCode::FORBIDDEN, "rejected_self_approval")
        );
    }
}
