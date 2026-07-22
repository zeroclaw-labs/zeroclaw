//! Out-of-band SOP approval surface (EPIC C, C6; EPIC G broker).
//!
//! `GET /admin/sop/pending`, `POST /admin/sop/approve`, `POST /admin/sop/deny`.
//! Auth reuses the vetted `/admin/reload` gate: loopback is always allowed and
//! attributed as `cli` UNLESS pairing is required and it also presents a valid
//! bearer token, in which case it is attributed as `http` (the authenticated
//! subject); a non-loopback caller needs `gateway.allow_remote_admin` + pairing
//! and passes `require_auth`, attributed as an `http` principal. The principal
//! is ALWAYS derived from the transport here, never from the request body.
//! Resolution funnels through `resolve_via_broker` (EPIC G's authorization/
//! quorum layer over the shared engine's `resolve_gate` chokepoint - a no-op
//! pass-through when no `[sop.approval]` policy applies); `sop_engine = None`
//! yields 503.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::{AdminReloadGate, AppState, admin_reload_gate};
use zeroclaw_runtime::sop::approval::{
    ApprovalDecision, ApprovalPrincipal, BrokerOutcome, ResolveOutcome,
};
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
        AdminReloadGate::Allow => {
            // A loopback caller is always allowed, but if pairing is actually
            // REQUIRED and it ALSO presents a valid paired bearer token, capture
            // that authenticated identity instead of discarding it as anonymous
            // CLI - otherwise a local dashboard/HTTP client with a valid token and
            // `http:<hash>` group membership would be rejected as an anonymous
            // `cli(None)` (no identity, so it can never satisfy a required-group
            // policy), while the SAME token from a non-loopback peer (the
            // `RequireAuth` arm below) resolves correctly.
            //
            // Gated on `require_pairing`: when pairing is OFF,
            // `authenticate_and_hash` treats EVERY token as valid (a no-op
            // pass-through for that mode - see `PairingGuard::is_authenticated`),
            // so deriving an identity from an unauthenticated header in that mode
            // would let any caller-supplied bearer value fabricate an approval
            // identity. WS already only derives a subject when auth is actually
            // meaningful; this matches that.
            let subject = require_pairing
                .then(|| crate::api::extract_bearer_token(headers))
                .flatten()
                .and_then(|t| state.pairing.authenticate_and_hash(t));
            Ok(match subject {
                Some(hash) => ApprovalPrincipal::http(Some(hash)),
                None => ApprovalPrincipal::cli(None),
            })
        }
        AdminReloadGate::RequireAuth => {
            crate::api::require_auth(state, headers)?;
            // Auth passed: derive a STABLE transport-authenticated subject (the
            // paired-token hash) so a required-group approval policy can be satisfied
            // by an authenticated gateway caller. An operator grants approval rights
            // to this paired device via an `http:<token-hash>` group member. Without
            // this the gateway would carry no identity and every policied gate would
            // be unsatisfiable from HTTP.
            let subject = crate::api::extract_bearer_token(headers)
                .and_then(|t| state.pairing.authenticate_and_hash(t));
            Ok(ApprovalPrincipal::http(subject))
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
        ResolveOutcome::Revised => (StatusCode::OK, "revised"),
        ResolveOutcome::AlreadyResolved => (StatusCode::OK, "already_resolved"),
        ResolveOutcome::NotWaiting => (StatusCode::NOT_FOUND, "not_waiting"),
        ResolveOutcome::RejectedSelfApproval => (StatusCode::FORBIDDEN, "rejected_self_approval"),
        // Approved, but re-admitting would exceed the concurrency caps: temporary
        // backpressure, retry once a slot frees (the gate stays waiting).
        ResolveOutcome::DeferredAtCapacity => {
            (StatusCode::SERVICE_UNAVAILABLE, "deferred_at_capacity")
        }
    }
}

/// GET /admin/sop/pending - list the runs parked on a human: `WaitingApproval`
/// gates AND deterministic `PausedCheckpoint` runs (both are resolved by the same
/// approve/deny chokepoint), distinguished by the `kind` field.
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
        .filter(|r| {
            matches!(
                r.status,
                SopRunStatus::WaitingApproval | SopRunStatus::PausedCheckpoint
            )
        })
        .map(|r| {
            serde_json::json!({
                "run_id": r.run_id,
                "sop_name": r.sop_name,
                "step": r.current_step,
                "total_steps": r.total_steps,
                "waiting_since": r.waiting_since,
                "kind": if r.status == SopRunStatus::PausedCheckpoint {
                    "checkpoint"
                } else {
                    "approval"
                },
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
        // EPIC G: route through the broker (membership + quorum). With no
        // `[sop.approval]` policy this is exactly `resolve_gate`.
        guard
            .resolve_via_broker(run_id, decision, principal)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": format!("resolve failed: {e}") })),
                )
            })?
    };
    let config = state.config.read();
    zeroclaw_runtime::sop::drive_resumed_broker_action(
        &config,
        std::sync::Arc::clone(engine),
        state.sop_audit.clone(),
        &outcome,
    );
    let (code, label) = broker_outcome_response(&outcome);
    Ok((
        code,
        Json(serde_json::json!({ "outcome": label, "run_id": run_id })),
    ))
}

/// Map a broker outcome to an HTTP status + label. `Resolved` delegates to the
/// existing `resolve_gate` mapping; the broker-specific outcomes get their own.
fn broker_outcome_response(outcome: &BrokerOutcome) -> (StatusCode, String) {
    match outcome {
        BrokerOutcome::Resolved(o) => {
            let (code, label) = outcome_response(o);
            (code, label.to_string())
        }
        BrokerOutcome::NotWaiting => (StatusCode::NOT_FOUND, "not_waiting".to_string()),
        BrokerOutcome::PendingQuorum { have, need } => (
            StatusCode::ACCEPTED,
            format!("pending_quorum ({have}/{need})"),
        ),
        BrokerOutcome::NotAuthorized { required_group } => (
            StatusCode::FORBIDDEN,
            format!("not_authorized (requires group '{required_group}')"),
        ),
        // A step naming an absent policy is a server-side config defect: fail closed
        // (the gate is left waiting) with a non-success status, never a silent clear.
        BrokerOutcome::PolicyMissing { name } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("policy_missing ('{name}')"),
        ),
        BrokerOutcome::PolicyUnavailable { reason } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("policy_unavailable ({reason})"),
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
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
        assert_eq!(
            broker_outcome_response(&BrokerOutcome::PolicyMissing {
                name: "prod".into()
            }),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "policy_missing ('prod')".to_string()
            )
        );
    }

    /// Build a gateway `AppState` whose SOP engine holds a run parked at a
    /// `policy = "prod"` approval gate, with group `release` = [`http:<token-hash>`]
    /// so the paired `token` (and only it) satisfies the policy over HTTP.
    pub(crate) fn state_with_policied_gate(token: &str) -> (AppState, String) {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};
        use zeroclaw_config::schema::{
            ApprovalGroupConfig, ApprovalPolicyConfig, SopApprovalConfig, SopConfig,
        };
        use zeroclaw_runtime::security::pairing::PairingGuard;
        use zeroclaw_runtime::sop::approval::ApprovalBroker;
        use zeroclaw_runtime::sop::engine::{SopEngine, now_iso8601};
        use zeroclaw_runtime::sop::types::{
            Sop, SopAdmissionPolicy, SopEvent, SopExecutionMode, SopPriority, SopRunAction,
            SopStep, SopStepKind, SopTrigger, SopTriggerSource,
        };

        let hash = PairingGuard::token_hash(token);
        let mut groups = HashMap::new();
        groups.insert(
            "release".to_string(),
            ApprovalGroupConfig {
                // Bare subject (any source) so the same paired-token hash satisfies the
                // policy over HTTP and over WS - both handler smokes reuse this helper.
                members: vec![hash.clone()],
            },
        );
        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: Some("release".into()),
                quorum: 1,
                request_route: None,
                escalation_route: None,
            },
        );
        let approval = SopApprovalConfig { groups, policies };

        let sop = Sop {
            name: "deploy".into(),
            description: "t".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "gate".into(),
                requires_confirmation: true,
                kind: SopStepKind::Execute,
                policy: Some("prod".into()),
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
        };
        let mut engine = SopEngine::new(SopConfig {
            approval,
            ..SopConfig::default()
        })
        .with_approval_broker(Arc::new(ApprovalBroker::disabled()));
        engine.set_sops_for_test(vec![sop]);
        let action = engine
            .start_run(
                "deploy",
                SopEvent {
                    source: SopTriggerSource::Manual,
                    topic: None,
                    payload: None,
                    timestamp: now_iso8601(),
                },
            )
            .unwrap();
        let run_id = match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        };

        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.allow_remote_admin = true;
        let mut state = crate::api::test_state(config);
        state.sop_engine = Some(Arc::new(Mutex::new(engine)));
        state.pairing = Arc::new(PairingGuard::new(true, &[token.to_string()]));
        (state, run_id)
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    #[tokio::test]
    async fn http_surface_enforces_policy_membership_via_authenticated_subject() {
        let run_status = |state: &AppState, id: &str| {
            state
                .sop_engine
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .get_run(id)
                .map(|r| r.status)
        };
        let token = "pair-token-abc";
        let (state, run_id) = state_with_policied_gate(token);
        // A remote (non-loopback) caller forces the authenticated `http` path.
        let peer: SocketAddr = "203.0.113.7:5000".parse().unwrap();

        // An authenticated caller whose paired-token hash is NOT in `release` is
        // rejected (membership is enforced on the derived gateway identity), and the
        // gate is left waiting.
        let other = "some-other-paired-token";
        let mut state_other = state.clone();
        state_other.pairing =
            std::sync::Arc::new(zeroclaw_runtime::security::pairing::PairingGuard::new(
                true,
                &[token.to_string(), other.to_string()],
            ));
        let resp = handle_sop_approve(
            State(state_other),
            ConnectInfo(peer),
            bearer(other),
            Json(SopResolveBody {
                run_id: run_id.clone(),
                reason: None,
            }),
        )
        .await
        .expect("handler returns a response")
        .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "an authenticated non-member must NOT clear a policied gate"
        );
        assert_eq!(
            run_status(&state, &run_id),
            Some(SopRunStatus::WaitingApproval),
            "the gate stays waiting after a non-member attempt"
        );

        // The authenticated member (its `http:<hash>` is in `release`) clears it.
        let resp = handle_sop_approve(
            State(state.clone()),
            ConnectInfo(peer),
            bearer(token),
            Json(SopResolveBody {
                run_id: run_id.clone(),
                reason: None,
            }),
        )
        .await
        .expect("handler returns a response")
        .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "an authenticated member clears the policied gate over HTTP"
        );
        assert_ne!(
            run_status(&state, &run_id),
            Some(SopRunStatus::WaitingApproval),
            "the gate is cleared (run resumes) once an authorized member approves"
        );
    }

    #[tokio::test]
    async fn loopback_caller_with_a_valid_token_keeps_its_authenticated_identity() {
        // Regression: a loopback caller is always allowed
        // (`AdminReloadGate::Allow`), but `authorize()` used to map that straight
        // to `ApprovalPrincipal::cli(None)` regardless of whether a valid paired
        // bearer token was also presented, discarding the authenticated identity.
        // A local dashboard/HTTP client with a valid token whose hash is a policy's
        // `required_group` member would then be rejected as an anonymous CLI (no
        // identity, cannot satisfy any required-group membership), even though the
        // same token from a non-loopback peer resolves correctly (see
        // `http_surface_enforces_policy_membership_via_authenticated_subject`).
        let run_status = |state: &AppState, id: &str| {
            state
                .sop_engine
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .get_run(id)
                .map(|r| r.status)
        };
        let token = "pair-token-abc";
        let (state, run_id) = state_with_policied_gate(token);
        let loopback: SocketAddr = "127.0.0.1:9".parse().unwrap();

        let resp = handle_sop_approve(
            State(state.clone()),
            ConnectInfo(loopback),
            bearer(token),
            Json(SopResolveBody {
                run_id: run_id.clone(),
                reason: None,
            }),
        )
        .await
        .expect("handler returns a response")
        .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a loopback caller presenting its member token must clear the policied \
             gate, not be treated as anonymous CLI"
        );
        assert_ne!(
            run_status(&state, &run_id),
            Some(SopRunStatus::WaitingApproval),
            "the gate is cleared once the loopback caller's authenticated identity \
             is recognized as a member"
        );
    }

    #[tokio::test]
    async fn loopback_bearer_identity_is_not_derived_when_pairing_is_disabled() {
        // Regression: a loopback caller's identity must only be derived from a
        // bearer token when pairing is required and the token is a real
        // authentication credential. `PairingGuard::is_authenticated` treats every
        // token as valid when `require_pairing` is false (a pass-through for that
        // mode, not real authentication). Without gating on `require_pairing`, a
        // loopback caller with pairing disabled could present an arbitrary bearer
        // value and be attributed the derived hash as its approval identity,
        // fabricating membership in a required group from an unauthenticated
        // header. The gate must not clear from an unauthenticated loopback request
        // even if its pairing-off token hash happens to match a real group member.
        let run_status = |state: &AppState, id: &str| {
            state
                .sop_engine
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .get_run(id)
                .map(|r| r.status)
        };
        let token = "pair-token-abc";
        let (mut state, run_id) = state_with_policied_gate(token);
        // Pairing is now OFF - `is_authenticated` accepts any token, so the SAME
        // token string that legitimately satisfied membership when paired must NOT
        // grant an authenticated identity anymore.
        state.pairing = std::sync::Arc::new(
            zeroclaw_runtime::security::pairing::PairingGuard::new(false, &[]),
        );
        let loopback: SocketAddr = "127.0.0.1:9".parse().unwrap();

        let resp = handle_sop_approve(
            State(state.clone()),
            ConnectInfo(loopback),
            bearer(token),
            Json(SopResolveBody {
                run_id: run_id.clone(),
                reason: None,
            }),
        )
        .await
        .expect("handler returns a response")
        .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "a loopback caller must NOT get an authenticated identity from an \
             unauthenticated bearer header when pairing is disabled"
        );
        assert_eq!(
            run_status(&state, &run_id),
            Some(SopRunStatus::WaitingApproval),
            "the gate stays waiting - no fabricated identity may clear it"
        );
    }
}
