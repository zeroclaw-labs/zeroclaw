//! HTTP routes for the Quickstart flow.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use zeroclaw_config::presets::BuilderSubmission;
use zeroclaw_runtime::quickstart::{
    AppliedAgent, QuickstartError, QuickstartStep, Surface, record_dismissed, stage_apply,
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
        daemon_restarted: bool,
    },
    Errors {
        errors: Vec<QuickstartError>,
    },
}

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
    // Admit one serialized config commit for the whole
    // stage-persist-publish sequence, so the install can't race a
    // concurrent config write.
    let commit = match state.begin_config_commit().await {
        Ok(commit) => commit,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApplyResult::Errors {
                    errors: vec![QuickstartError {
                        step: QuickstartStep::Agent,
                        field: String::new(),
                        message: format!(
                            "daemon generation is closing; config write refused without any change: {e}"
                        ),
                    }],
                }),
            )
                .into_response();
        }
    };
    let mut working = commit.current_config();
    // Stage the submission (validation, mutation, personality tempfiles) —
    // cancellable preparation with nothing on disk. A rejected submission
    // returns with the published pair untouched.
    let staged = match stage_apply(submission, &mut working, Surface::Web) {
        Ok(staged) => staged,
        Err(errors) => {
            return (StatusCode::OK, Json(ApplyResult::Errors { errors })).into_response();
        }
    };
    // Allocate the checked revision before the irreversible save.
    let revision = match commit.next_revision() {
        Ok(revision) => revision,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApplyResult::Errors {
                    errors: vec![QuickstartError {
                        step: QuickstartStep::Agent,
                        field: String::new(),
                        message: format!("config revision unavailable: {e}"),
                    }],
                }),
            )
                .into_response();
        }
    };
    // The completion (save, publish, personality install) runs retained:
    // the task owns the commit, so a cancelled requester cannot strand a
    // committed config without its publication, and a post-commit
    // personality failure still publishes the committed config while the
    // truthful errors reach the caller below.
    let task =
        zeroclaw_runtime::live_config_authority::spawn_agent_lifecycle_job(Box::pin(async move {
            zeroclaw_runtime::quickstart::complete_staged_apply_as_commit(staged, &commit, revision)
                .await
        }));
    let result = match task.await {
        Ok(result) => result,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApplyResult::Errors {
                    errors: vec![QuickstartError {
                        step: QuickstartStep::Agent,
                        field: String::new(),
                        message: format!("quickstart commit task failed: {e}"),
                    }],
                }),
            )
                .into_response();
        }
    };
    let body = match result {
        Ok(zeroclaw_runtime::quickstart::QuickstartApplyOutcome::Applied(agent)) => {
            state
                .pending_reload
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let reload_signalled = signal_daemon_reload(&state);
            ApplyResult::Applied {
                agent,
                daemon_restarted: reload_signalled,
            }
        }
        Ok(
            zeroclaw_runtime::quickstart::QuickstartApplyOutcome::CommittedWithSideEffectErrors {
                errors,
                ..
            },
        ) => {
            // The config is committed AND published; mark the reload
            // pending and signal it exactly like a fully successful apply,
            // and return the truthful personality errors.
            state
                .pending_reload
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = signal_daemon_reload(&state);
            ApplyResult::Errors { errors }
        }
        Err(errors) => ApplyResult::Errors { errors },
    };
    (StatusCode::OK, Json(body)).into_response()
}

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
