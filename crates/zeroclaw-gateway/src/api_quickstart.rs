//! HTTP routes for the Quickstart flow.
//!
//! Thin wrapper over `zeroclaw_runtime::quickstart::{validate_only, apply}`.
//! Routes:
//!
//! - `GET  /api/quickstart/state`     — current Quickstart state (completed flag + live-config slices for each step's "Use existing" section).
//! - `POST /api/quickstart/validate`  — run `validate_only` against the submitted `BuilderSubmission`; returns `{ ok: true }` or `{ ok: false, errors: [...] }`.
//! - `POST /api/quickstart/apply`     — atomically apply the submission, then signal an in-place daemon reload through the existing `reload_tx` watch channel (same mechanism `/admin/reload` uses); returns the `AppliedAgent` summary or a structured error list.
//!
//! All business logic lives in `zeroclaw-runtime`; this module is route
//! plumbing only.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use zeroclaw_config::presets::BuilderSubmission;
use zeroclaw_runtime::quickstart::{
    AppliedAgent, QuickstartError, QuickstartStep, Surface, apply_with_surface, record_dismissed,
    validate_only_with_surface,
};

use super::AppState;
use super::api::require_auth;

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValidateResult {
    Ok,
    Errors { errors: Vec<QuickstartError> },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApplyResult {
    Applied {
        agent: AppliedAgent,
        /// `true` when the in-place daemon reload was signalled (the
        /// supervisor will drain and re-init subsystems). `false` means
        /// apply succeeded but no daemon supervisor is attached (e.g.
        /// `zeroclaw gateway start` standalone) — the caller must
        /// restart the process to pick up the change.
        daemon_restarted: bool,
    },
    Errors {
        errors: Vec<QuickstartError>,
    },
}

/// `GET /api/quickstart/state` — minimal payload the Quickstart UI
/// needs to render every step's "Use existing" section without
/// pulling the entire config. Response shape is owned by
/// `zeroclaw_runtime::quickstart::QuickstartState`; both transports
/// build the body via [`snapshot_state`] so they cannot drift.
pub async fn handle_state(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.read().clone();
    let body = zeroclaw_runtime::quickstart::snapshot_state(&cfg);
    (StatusCode::OK, Json(body)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct FieldsRequest {
    pub section: zeroclaw_runtime::quickstart::FieldSection,
    pub type_key: String,
}

#[derive(Debug, Serialize)]
pub struct FieldsResult {
    pub fields: Vec<zeroclaw_runtime::quickstart::FieldDescriptor>,
}

pub async fn handle_fields(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<FieldsRequest>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let body = FieldsResult {
        fields: zeroclaw_runtime::quickstart::field_shape(req.section, &req.type_key),
    };
    (StatusCode::OK, Json(body)).into_response()
}

pub async fn handle_validate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(submission): Json<BuilderSubmission>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.read().clone();
    let body = match validate_only_with_surface(&submission, &cfg, Surface::Web) {
        Ok(()) => ValidateResult::Ok,
        Err(errors) => ValidateResult::Errors { errors },
    };
    (StatusCode::OK, Json(body)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct DismissRequest {
    pub run_id: String,
    /// Surface name as emitted in earlier events for this run. Echoed
    /// into the dismiss event so the SSE stream can correlate the
    /// dismissal back to the same `(run_id, surface)` pair. Deserialised
    /// straight into the typed enum (snake_case wire form) — no
    /// string-literal `match` at the route boundary.
    pub surface: Surface,
    /// Furthest step the user reached. `None` = didn't progress past
    /// the first selector.
    #[serde(default)]
    pub last_step: Option<QuickstartStep>,
}

pub async fn handle_dismiss(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DismissRequest>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    record_dismissed(&req.run_id, req.surface, req.last_step);
    (StatusCode::NO_CONTENT, ()).into_response()
}

pub async fn handle_apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(submission): Json<BuilderSubmission>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let mut working = state.config.read().clone();
    let result = apply_with_surface(submission, &mut working, Surface::Web).await;
    let body = match result {
        Ok(agent) => {
            *state.config.write() = working;
            state
                .pending_reload
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let reload_signalled = signal_daemon_reload(&state);
            ApplyResult::Applied {
                agent,
                daemon_restarted: reload_signalled,
            }
        }
        Err(errors) => ApplyResult::Errors { errors },
    };
    (StatusCode::OK, Json(body)).into_response()
}

/// Signal the in-place daemon reload using the same `reload_tx` watch
/// channel `/admin/reload` uses. The daemon supervisor reacts by
/// draining the current gateway/channels/scheduler and bringing them
/// back up against the new in-memory config — no process kill, no
/// PID respawn, no service-manager dependency.
fn signal_daemon_reload(state: &AppState) -> bool {
    let Some(reload_tx) = state.reload_tx.clone() else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "reason": "no_supervisor",
                })),
            "quickstart: daemon reload not available (standalone gateway)"
        );
        return false;
    };
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start),
        "quickstart: daemon reload signalled"
    );
    let shutdown_tx = state.shutdown_tx.clone();
    state
        .pending_reload
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let started = std::time::Instant::now();
    zeroclaw_spawn::spawn!(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let _ = shutdown_tx.send(true);
        let _ = reload_tx.send(true);
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                })),
            "quickstart: daemon reload dispatched"
        );
    });
    true
}

// Per-family alias collection lives in
// `zeroclaw_runtime::quickstart::snapshot_state` so both transports
// share one implementation.
