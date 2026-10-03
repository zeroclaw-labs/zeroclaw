//! HTTP routes for the Quickstart flow.

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use zeroclaw_config::presets::BuilderSubmission;
use zeroclaw_runtime::quickstart::{
    AppliedAgent, QuickstartError, QuickstartStep, Surface, apply_with_surface_checked,
    record_dismissed, validate_only_with_surface,
};

use super::AppState;

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

pub async fn handle_state(State(state): State<AppState>) -> impl IntoResponse {
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
    State(_state): State<AppState>,
    Json(req): Json<FieldsRequest>,
) -> impl IntoResponse {
    let body = FieldsResult {
        fields: zeroclaw_runtime::quickstart::field_shape(req.section, &req.type_key),
    };
    (StatusCode::OK, Json(body)).into_response()
}

pub async fn handle_validate(
    State(state): State<AppState>,
    Json(submission): Json<BuilderSubmission>,
) -> impl IntoResponse {
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
    State(_state): State<AppState>,
    Json(req): Json<DismissRequest>,
) -> impl IntoResponse {
    record_dismissed(&req.run_id, req.surface, req.last_step);
    (StatusCode::NO_CONTENT, ()).into_response()
}

pub async fn handle_apply(
    State(state): State<AppState>,
    principal: crate::principal_gate::RequestPrincipal,
    Json(submission): Json<BuilderSubmission>,
) -> axum::response::Response {
    let cfg_guard = std::sync::Arc::clone(&state.config_write_lock)
        .lock_owned()
        .await;
    // Quickstart can write an open-ended set of config paths. Refuse a
    // principal without whole-config authority before reserving the alias.
    let authorization = match crate::principal_gate::authorize_whole_config_write(
        &principal,
        &[
            zeroclaw_api::grants::Verb::Create,
            zeroclaw_api::grants::Verb::Update,
        ],
        &cfg_guard,
    ) {
        Ok(authorization) => authorization,
        Err(denied) => return denied.into_response(),
    };
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
            apply_reserved(state, submission, authorization, cfg_guard).await
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
    authorization: crate::principal_gate::ConfigWriteAuthorization,
    _cfg_guard: crate::ConfigWriteGuard,
) -> axum::response::Response {
    // Held through the swap below (and across `apply_with_surface`'s own
    // save, which runs while this guard is held) so a concurrent config
    // writer can't land between this read and the swap.
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
            authorization.publish_persisted(&working);
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

// ── Through the core (the standalone gateway) ───────────────────────
//
// Each route asks the core's `quickstart/*` method and answers with its
// result, which is the route's body. The core validates and applies a
// submission as the terminal surface, which only labels its telemetry: the
// answer is the same as the web surface's.

/// `GET /api/quickstart/state` through the core.
pub(crate) async fn state_through_core(
    core: &crate::core_rpc::CoreCall,
) -> Result<axum::response::Response, crate::core_rpc::CoreError> {
    let body = core
        .request(
            zeroclaw_rpc_client::Method::QuickstartState,
            serde_json::json!({}),
        )
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/quickstart/fields` through the core.
pub(crate) async fn fields_through_core(
    core: &crate::core_rpc::CoreCall,
    req: FieldsRequest,
) -> Result<axum::response::Response, crate::core_rpc::CoreError> {
    let body = core
        .request(
            zeroclaw_rpc_client::Method::QuickstartFields,
            serde_json::json!({ "section": req.section, "type_key": req.type_key }),
        )
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/quickstart/validate` through the core.
pub(crate) async fn validate_through_core(
    core: &crate::core_rpc::CoreCall,
    submission: BuilderSubmission,
) -> Result<axum::response::Response, crate::core_rpc::CoreError> {
    let body = core
        .request(
            zeroclaw_rpc_client::Method::QuickstartValidate,
            serde_json::json!({ "submission": submission }),
        )
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/quickstart/apply` through the core. The core reloads after a
/// successful apply, which ends this gateway's core connections; the next
/// request dials again.
pub(crate) async fn apply_through_core(
    core: &crate::core_rpc::CoreCall,
    submission: BuilderSubmission,
) -> Result<axum::response::Response, crate::core_rpc::CoreError> {
    let body = core
        .request(
            zeroclaw_rpc_client::Method::QuickstartApply,
            serde_json::json!({ "submission": submission }),
        )
        .await?;
    Ok((StatusCode::OK, Json(body)).into_response())
}

/// `POST /api/quickstart/dismiss` through the core.
pub(crate) async fn dismiss_through_core(
    core: &crate::core_rpc::CoreCall,
    req: DismissRequest,
) -> Result<axum::response::Response, crate::core_rpc::CoreError> {
    core.request(
        zeroclaw_rpc_client::Method::QuickstartDismiss,
        serde_json::json!({
            "run_id": req.run_id,
            "surface": req.surface,
            "last_step": req.last_step,
        }),
    )
    .await?;
    Ok((StatusCode::NO_CONTENT, ()).into_response())
}

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
        let response = handle_apply(State(state.clone()), None, Json(submission()))
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
        let response = handle_apply(State(state.clone()), None, Json(submission()))
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
