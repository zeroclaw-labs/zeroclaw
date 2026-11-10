//! First-contact device bootstrap for a verified OIDC human. No pairing code,
//! client roster entry, or dedicated enrollment listener is involved. The
//! gateway HTTPS connection is the initial server trust anchor. The resulting
//! certificate admits only a device; WSS initialize still verifies the bearer.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use zeroclaw_api::grants::{Resource, Verb};
use zeroclaw_api::principal::{ActorKind, AuthMethod};
use zeroclaw_runtime::security::{
    audit::AuditLogger,
    cert_ledger::{CertLedger, CertStatus, LedgerEntry, effective_revoked_list_path},
};

use crate::AppState;
use crate::api_oidc::{
    OidcEnrollmentState, auth_gate, budget_gate, client_key, error_json, outbound_permit,
    relay_busy,
};
use crate::principal_gate::GatewayInboundAuth;

#[derive(Clone)]
struct BootstrapAuthority {
    inbound: Arc<GatewayInboundAuth>,
    audit: Option<Arc<AuditLogger>>,
}

// No Debug: a CSR is public but has no reason to enter diagnostics.
#[derive(Deserialize)]
struct CertificateRequest {
    csr_pem: String,
}

/// Kept outside the open device-grant routes. Even an unpaired gateway must
/// verify the explicit OIDC bearer: native pairing and dashboard cookies never
/// authorize this operation. A standalone gateway has no shared certificate
/// audit authority and refuses issuance rather than constructing a rival one.
pub(crate) fn routes(
    inbound: Arc<GatewayInboundAuth>,
    audit: Option<Arc<AuditLogger>>,
) -> Router<AppState> {
    Router::new()
        .route("/api/oidc/{alias}/certificate", post(handle_certificate))
        .layer(axum::middleware::from_fn(
            crate::relay_ingress::no_enrollment_through_a_relay,
        ))
        .layer(axum::middleware::from_fn(crate::api_oidc::no_store))
        .layer(Extension(Arc::new(OidcEnrollmentState::default())))
        .layer(Extension(BootstrapAuthority { inbound, audit }))
}

async fn handle_certificate(
    State(state): State<AppState>,
    Extension(authority): Extension<BootstrapAuthority>,
    Extension(limits): Extension<Arc<OidcEnrollmentState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(alias): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CertificateRequest>,
) -> Response {
    // Never bootstrap trust through a cleartext remote listener. Forwarded
    // HTTPS counts only behind an operator-trusted proxy, as for sign-in.
    if !crate::api_oidc::request_is_https(&state, &headers, None)
        && !crate::api_oidc::is_loopback_request(peer, &headers)
    {
        return error_json(
            StatusCode::BAD_REQUEST,
            "OIDC device bootstrap requires HTTPS",
        );
    }
    let key = client_key(&state, peer, &headers);
    if let Err(response) = auth_gate(&limits, &key, false) {
        return *response;
    }
    if let Err(response) = budget_gate(&limits, &key) {
        return *response;
    }
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|t| !t.trim().is_empty())
    else {
        return error_json(StatusCode::UNAUTHORIZED, "an OIDC access token is required");
    };
    if body.csr_pem.is_empty() || body.csr_pem.len() > 16 * 1024 {
        return error_json(StatusCode::BAD_REQUEST, "CSR is empty or too large");
    }
    let Some(_permit) = outbound_permit(&limits) else {
        return relay_busy();
    };
    let binding = {
        let config = state.config.read();
        let Some(entry) = config.oidc.get(&alias) else {
            return error_json(StatusCode::UNAUTHORIZED, "unknown OIDC provider");
        };
        zeroclaw_runtime::rpc::auth::OidcBinding::of(entry)
    };
    let auth = match authority
        .inbound
        .authenticate_bound(token, &format!("oidc.{alias}"), &binding)
        .await
    {
        Ok(auth) => auth,
        Err(_) => {
            limits.attempts.record_attempt(&key);
            return error_json(StatusCode::UNAUTHORIZED, "OIDC authentication refused");
        }
    };
    if auth.principal.auth_method != AuthMethod::Oidc || auth.principal.actor != ActorKind::Human {
        return error_json(
            StatusCode::FORBIDDEN,
            "interactive OIDC sign-in is required",
        );
    }
    // Serialize against configuration publication, then hold accepted policy
    // through signing/ledger commit. A revocation while authentication awaited
    // the IdP, the request body, or this lock therefore wins before the effect.
    let _commit = match state.begin_config_commit().await {
        Ok(commit) => commit,
        Err(_) => {
            return error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "daemon generation is closing",
            );
        }
    };
    let lease = authority.inbound.hold_authority();
    let grants = match lease.current_grants(&auth) {
        Ok(grants) => grants,
        Err(_) => {
            return error_json(
                StatusCode::UNAUTHORIZED,
                "OIDC authentication is no longer current",
            );
        }
    };
    // A human already allowed to create agent sessions may bootstrap the
    // transport needed to do so. This certificate grants no RPC permissions:
    // every initialize still verifies the live OIDC bearer, and agent selectors
    // still limit all session operations. Keep system:execute for deployments
    // that explicitly authorize bootstrap independently of session creation.
    let may_create_session =
        grants.permits(Resource::Sessions, Verb::Create) && !grants.allowed_agents.is_empty();
    if !grants.permits(Resource::System, Verb::Execute) && !may_create_session {
        return error_json(
            StatusCode::FORBIDDEN,
            "OIDC device bootstrap requires sessions:create with an allowed agent, or system:execute",
        );
    }
    let config = state.config.read();
    if !config.wss.enabled {
        return error_json(StatusCode::CONFLICT, "remote WSS is not enabled");
    }
    if config
        .wss
        .client_auth
        .as_ref()
        .is_some_and(|a| !a.pinned_certs.is_empty())
    {
        return error_json(
            StatusCode::CONFLICT,
            "static client certificate pins require out-of-band provisioning",
        );
    }
    let Some(audit) = authority.audit.as_ref() else {
        return error_json(
            StatusCode::SERVICE_UNAVAILABLE,
            "shared certificate audit authority is unavailable",
        );
    };
    let result = (|| -> anyhow::Result<serde_json::Value> {
        let tls_dir = config.data_dir.join("tls");
        // Only use existing daemon materials. Never create a parallel CA or
        // silently issue against a CA different from the WSS verifier's.
        let ca = std::fs::read_to_string(tls_dir.join("ca.crt"))?;
        if let Some(explicit) = config
            .wss
            .client_auth
            .as_ref()
            .filter(|a| a.enabled && !a.ca_cert_path.is_empty())
        {
            let expected = std::fs::read_to_string(&explicit.ca_cert_path)?;
            anyhow::ensure!(
                zeroclaw_tls::single_cert_pem_sha256_fingerprint(&expected)?
                    == zeroclaw_tls::single_cert_pem_sha256_fingerprint(&ca)?,
                "WSS CA differs from signing CA"
            );
        }
        let signing_key = zeroclaw_tls::load_ca_key_pem(
            &tls_dir.join("ca.key"),
            &zeroclaw_tls::CaKeyProtection::from_env(),
        )?;
        let device_id = format!("dev_{}", uuid::Uuid::new_v4().simple());
        let issued = zeroclaw_tls::sign_csr(&ca, &signing_key, &device_id, &body.csr_pem)?;
        let ledger = CertLedger::open_at(
            &config.data_dir,
            Some(Arc::clone(audit)),
            effective_revoked_list_path(
                &config.data_dir,
                config.wss.client_auth.as_ref().map(|a| a.crl_path.as_str()),
            ),
        )?;
        ledger.record_issued(
            &LedgerEntry {
                device_id: device_id.clone(),
                fingerprint: issued.fingerprint.clone(),
                not_before: issued.not_before,
                not_after: issued.not_after,
                status: CertStatus::Active,
                token_hash: String::new(),
                actor: format!("oidc:{}", auth.principal.id.as_str()),
                issued_at: chrono::Utc::now().timestamp(),
            },
            false,
        )?;
        // Like RPC certificate renewal, delivery here means a response payload
        // has been constructed, not receipt by the client. No credential or
        // private key is logged; the device's key never left that device.
        let response = serde_json::json!({
            "cert_pem": issued.cert_pem, "ca_chain_pem": ca,
            "device_id": device_id, "not_after": issued.not_after,
        });
        anyhow::ensure!(
            ledger.mark_delivered(&issued.fingerprint)?,
            "issued certificate not found in ledger"
        );
        Ok(response)
    })();
    match result {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(serde_json::json!({"error":format!("{error:#}")})),
                "OIDC device certificate issuance failed"
            );
            error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                "device certificate issuance failed; check daemon certificate configuration",
            )
        }
    }
}

#[cfg(test)]
#[path = "api_oidc_enroll_tests.rs"]
mod tests;
