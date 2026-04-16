//! WebAuthn gateway API handlers for hardware key registration and authentication.
//!
//! All endpoints require bearer token authentication (PairingGuard) and the
//! `webauthn` feature flag.

use super::AppState;
use crate::api::require_auth;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use parking_lot::Mutex;
use serde::Deserialize;
use std::collections::HashMap;
use zeroclaw_runtime::security::webauthn::{
    AuthenticateCredentialResponse, AuthenticationState, RegisterCredentialResponse,
    RegistrationState, WebAuthnManager,
};

/// Shared WebAuthn state for the gateway.
pub struct WebAuthnState {
    pub manager: WebAuthnManager,
    /// Pending registration states keyed by challenge.
    pub pending_registrations: Mutex<HashMap<String, RegistrationState>>,
    /// Pending authentication states keyed by challenge.
    pub pending_authentications: Mutex<HashMap<String, AuthenticationState>>,
}

// ── Request bodies ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct StartRegistrationBody {
    pub user_id: String,
    pub user_name: String,
}

#[derive(Deserialize)]
pub struct FinishRegistrationBody {
    pub challenge: String,
    #[serde(flatten)]
    pub response: RegisterCredentialResponse,
}

#[derive(Deserialize)]
pub struct StartAuthenticationBody {
    pub user_id: String,
}

#[derive(Deserialize)]
pub struct FinishAuthenticationBody {
    pub challenge: String,
    #[serde(flatten)]
    pub response: AuthenticateCredentialResponse,
}

#[derive(Deserialize)]
pub struct CredentialsQuery {
    pub user_id: String,
}

// ── Handlers ────────────────────────────────────────────────────

/// POST /api/webauthn/register/start
pub async fn handle_register_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<StartRegistrationBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let webauthn = match &state.webauthn {
        Some(w) => w,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "WebAuthn is not enabled"})),
            )
                .into_response();
        }
    };

    match webauthn
        .manager
        .start_registration(&body.user_id, &body.user_name)
    {
        Ok((creation, reg_state)) => {
            webauthn
                .pending_registrations
                .lock()
                .insert(reg_state.challenge.clone(), reg_state);
            Json(serde_json::json!(creation)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /api/webauthn/register/finish
pub async fn handle_register_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<FinishRegistrationBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let webauthn = match &state.webauthn {
        Some(w) => w,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "WebAuthn is not enabled"})),
            )
                .into_response();
        }
    };

    let reg_state = match webauthn
        .pending_registrations
        .lock()
        .remove(&body.challenge)
    {
        Some(s) => s,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "No pending registration for this challenge"})),
            )
                .into_response();
        }
    };

    match webauthn
        .manager
        .finish_registration(&reg_state, &body.response)
    {
        Ok(credential) => Json(serde_json::json!({
            "credential_id": credential.credential_id,
            "label": credential.label,
            "registered_at": credential.registered_at,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /api/webauthn/auth/start
pub async fn handle_auth_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<StartAuthenticationBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let webauthn = match &state.webauthn {
        Some(w) => w,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "WebAuthn is not enabled"})),
            )
                .into_response();
        }
    };

    match webauthn.manager.start_authentication(&body.user_id) {
        Ok((request, auth_state)) => {
            webauthn
                .pending_authentications
                .lock()
                .insert(auth_state.challenge.clone(), auth_state);
            Json(serde_json::json!(request)).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /api/webauthn/auth/finish
pub async fn handle_auth_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<FinishAuthenticationBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let webauthn = match &state.webauthn {
        Some(w) => w,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "WebAuthn is not enabled"})),
            )
                .into_response();
        }
    };

    let auth_state = match webauthn
        .pending_authentications
        .lock()
        .remove(&body.challenge)
    {
        Some(s) => s,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "No pending authentication for this challenge"})),
            )
                .into_response();
        }
    };

    match webauthn
        .manager
        .finish_authentication(&auth_state, &body.response)
    {
        Ok(()) => Json(serde_json::json!({"status": "authenticated"})).into_response(),
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /api/webauthn/credentials?user_id=...
pub async fn handle_list_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<CredentialsQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let webauthn = match &state.webauthn {
        Some(w) => w,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "WebAuthn is not enabled"})),
            )
                .into_response();
        }
    };

    match webauthn.manager.list_credentials(&query.user_id) {
        Ok(creds) => {
            let items: Vec<serde_json::Value> = creds
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "credential_id": c.credential_id,
                        "label": c.label,
                        "registered_at": c.registered_at,
                        "sign_count": c.sign_count,
                    })
                })
                .collect();
            Json(serde_json::json!({"credentials": items})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /api/webauthn/credentials/:id?user_id=...
pub async fn handle_delete_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(credential_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<CredentialsQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let webauthn = match &state.webauthn {
        Some(w) => w,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "WebAuthn is not enabled"})),
            )
                .into_response();
        }
    };

    match webauthn
        .manager
        .remove_credential(&query.user_id, &credential_id)
    {
        Ok(()) => Json(serde_json::json!({"status": "deleted"})).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
