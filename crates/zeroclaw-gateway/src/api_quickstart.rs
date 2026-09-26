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
    AppliedAgent, QuickstartError, QuickstartStep, Surface, apply_with_surface_checked,
    record_dismissed, validate_only_with_surface,
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
    let reservation = match state
        .agent_lifecycle
        .reserve_config_mutation(&submission.agent.name)
    {
        Ok(reservation) => reservation,
        Err(error) => {
            return (
                StatusCode::OK,
                Json(ApplyResult::Errors {
                    errors: vec![QuickstartError {
                        step: QuickstartStep::Agent,
                        field: "agent.name".into(),
                        message: error.to_string(),
                    }],
                }),
            )
                .into_response();
        }
    };
    // Keep admission and save/publication together if the request disconnects.
    let task =
        zeroclaw_runtime::live_config_authority::spawn_agent_lifecycle_job(Box::pin(async move {
            let _reservation = reservation;
            apply_reserved(state, submission).await
        }));
    match task.await {
        Ok(response) => response,
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Quickstart completion failed: {error}")
            })),
        )
            .into_response(),
    }
}

async fn apply_reserved(
    state: AppState,
    submission: BuilderSubmission,
) -> axum::response::Response {
    // Held through the swap below (and across `apply_with_surface`'s own
    // save, which runs while this guard is held) so a concurrent config
    // writer can't land between this read and the swap.
    let _cfg_guard = std::sync::Arc::clone(&state.config_write_lock)
        .lock_owned()
        .await;
    let mut working = state.config.read().clone();
    // The staged policy is compiled BEFORE Quickstart's first write, so a
    // rejected one cannot reach disk and then be reported as not saved.
    let result = apply_with_surface_checked(submission, &mut working, Surface::Web, &|staged| {
        zeroclaw_runtime::rpc::auth::validate_accepted_auth_config(staged)
            .map_err(|e| e.to_string())
    })
    .await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::presets::{
        AgentIdentity, MemoryChoice, ModelProviderChoice, SelectorChoice,
    };

    #[tokio::test]
    async fn quickstart_creation_waits_for_destructive_cleanup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config.save().await.unwrap();
        let disk_before = std::fs::read(&config.config_path).unwrap();
        let workspace = config.agent_workspace_dir("recreated");
        let state = crate::api::tests::test_state(config);
        let mut cleanup = state.agent_lifecycle.begin_delete("recreated").unwrap();
        cleanup.commit_destructive_mutation();
        let submission = || BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".into(),
                alias: "anthropic".into(),
                model: "claude-sonnet-4-5".into(),
                fields: std::collections::HashMap::from([("api_key".into(), "sk-test".into())]),
            }),
            risk_profile: SelectorChoice::Fresh("balanced".into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: vec![],
            peer_groups: vec![],
            agent: AgentIdentity {
                name: "recreated".into(),
                system_prompt: "You are helpful.".into(),
                personality_file: None,
                personality_files: vec![],
            },
        };
        let response = handle_apply(State(state.clone()), HeaderMap::new(), Json(submission()))
            .await
            .into_response();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["kind"], "errors");
        assert!(
            result["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("recreated")
        );
        assert!(!state.config.read().agents.contains_key("recreated"));
        assert_eq!(
            std::fs::read(&state.config.read().config_path).unwrap(),
            disk_before
        );
        assert!(!workspace.exists());

        drop(cleanup);
        let response = handle_apply(State(state.clone()), HeaderMap::new(), Json(submission()))
            .await
            .into_response();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["kind"], "applied", "{result}");
        assert!(state.config.read().agents.contains_key("recreated"));
    }
}
