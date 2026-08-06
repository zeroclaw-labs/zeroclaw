//! HTTP routes for the Quickstart flow.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zeroclaw_config::presets::BuilderSubmission;
use zeroclaw_runtime::quickstart::{
    AppliedAgent, QuickstartError, QuickstartStep, QuickstartWarning, Surface, record_dismissed,
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
        warnings: Vec<QuickstartWarning>,
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
    let quickstart_config = zeroclaw_runtime::quickstart::QuickstartConfigState::from_parts(
        Arc::clone(&state.config),
        Arc::clone(&state.config_write_lock),
        Arc::clone(&state.quickstart_reload_admission),
    );
    let result = quickstart_config
        .apply_and_admit_reload(submission, Surface::Web)
        .await;
    let body = match result {
        Ok(outcome) => {
            state
                .pending_reload
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let reload_signalled = signal_daemon_reload(&state);
            if !reload_signalled {
                quickstart_config.cancel_reload_admission();
            }
            ApplyResult::Applied {
                agent: outcome.agent,
                daemon_restarted: reload_signalled,
                warnings: outcome.warnings,
            }
        }
        Err(errors) => ApplyResult::Errors { errors },
    };
    (StatusCode::OK, Json(body)).into_response()
}

fn signal_daemon_reload(state: &AppState) -> bool {
    let Some(reload_tx) = state.reload_tx.clone() else {
        state
            .pending_reload
            .store(false, std::sync::atomic::Ordering::Relaxed);
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
    use axum::{Router, body::Body, http::Request, routing::post};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use zeroclaw_config::presets::{
        AgentIdentity, BuilderSubmission, MemoryChoice, ModelProviderChoice, SelectorChoice,
    };

    #[tokio::test]
    async fn http_quickstart_apply_persists_anthropic_setup_token() {
        let tmp = tempfile::tempdir().unwrap();
        let config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("workspace"),
            ..Default::default()
        };
        config.save().await.unwrap();
        let state = crate::api::test_state(config);
        let router = Router::new()
            .route("/api/quickstart/apply", post(handle_apply))
            .with_state(state.clone());
        let submission = BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".into(),
                alias: "subscription".into(),
                model: "claude-sonnet-4-5".into(),
                fields: std::collections::HashMap::from([
                    ("auth_mode".to_string(), "setup_token".to_string()),
                    ("api_key".to_string(), "synthetic-setup-token".to_string()),
                ]),
            }),
            risk_profile: SelectorChoice::Fresh("balanced".into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: vec![],
            peer_groups: vec![],
            agent: AgentIdentity {
                name: "quickstart_bot".into(),
                system_prompt: "You are helpful.".into(),
                personality_file: None,
                personality_files: vec![],
            },
        };
        let mut second_submission = submission.clone();
        let SelectorChoice::Fresh(second_provider) = &mut second_submission.model_provider else {
            panic!("test submission must create a provider");
        };
        second_provider.alias = "subscription_two".into();
        second_provider
            .fields
            .insert("api_key".into(), "synthetic-setup-token-two".into());
        second_submission.agent.name = "quickstart_bot_two".into();

        // Hold a prior transaction open to prove the HTTP handler does not
        // clone config until it owns the cross-await Quickstart transaction.
        // With the historical clone-before-lock ordering, both requests would
        // take the same stale snapshot here and the later swap would erase the
        // first agent/alias after this guard is released.
        let held_transaction = Arc::clone(&state.config_write_lock).lock_owned().await;
        let first_router = router.clone();
        let mut first_apply = zeroclaw_spawn::spawn!(async move {
            let request = Request::builder()
                .method("POST")
                .uri("/api/quickstart/apply")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&submission).unwrap()))
                .unwrap();
            first_router.oneshot(request).await.unwrap()
        });
        let second_router = router.clone();
        let mut second_apply = zeroclaw_spawn::spawn!(async move {
            let request = Request::builder()
                .method("POST")
                .uri("/api/quickstart/apply")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&second_submission).unwrap()))
                .unwrap();
            second_router.oneshot(request).await.unwrap()
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut first_apply)
                .await
                .is_err(),
            "the first concurrent Quickstart request must wait for the current transaction"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut second_apply)
                .await
                .is_err(),
            "the second concurrent Quickstart request must wait before cloning config"
        );
        drop(held_transaction);
        for response in [first_apply.await.unwrap(), second_apply.await.unwrap()] {
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                body["kind"], "applied",
                "concurrent apply response must be successful: {body}"
            );
            assert_eq!(
                body["warnings"],
                serde_json::json!([]),
                "successful HTTP Quickstart responses preserve the warnings contract"
            );
        }

        let config = state.config.read().clone();
        let entry = config
            .providers
            .models
            .find("anthropic", "subscription")
            .expect("HTTP apply must persist the requested alias");
        assert!(entry.api_key.is_none(), "setup token must not enter config");
        assert!(
            config.agents.contains_key("quickstart_bot"),
            "the later request must not erase the first request's agent"
        );
        assert!(
            config
                .providers
                .models
                .find("anthropic", "subscription_two")
                .is_some(),
            "the later request must start from the first request's committed config"
        );
        assert!(
            config.agents.contains_key("quickstart_bot_two"),
            "the second concurrent request must persist its agent"
        );
        let profile = zeroclaw_providers::auth::AuthService::from_config(&config)
            .get_profile("anthropic", Some("subscription"))
            .await
            .unwrap()
            .expect("HTTP apply must store the same-alias profile");
        assert_eq!(profile.token.as_deref(), Some("synthetic-setup-token"));
        assert_eq!(
            zeroclaw_providers::auth::AuthService::from_config(&config)
                .get_profile("anthropic", Some("subscription_two"))
                .await
                .unwrap()
                .expect("HTTP apply must store the second same-alias profile")
                .token
                .as_deref(),
            Some("synthetic-setup-token-two")
        );
        assert!(
            !state
                .pending_reload
                .load(std::sync::atomic::Ordering::Relaxed),
            "standalone gateway must not retain a reload request it cannot dispatch"
        );

        // Reconstruct from the persisted file, rather than the gateway's
        // in-memory working clone, and build the selected OAuth alias. OAuth
        // aliases deliberately cannot use a local HTTP mock: their setup
        // token is restricted to Anthropic's official endpoint.
        let mut reloaded: zeroclaw_config::schema::Config =
            toml::from_str(&std::fs::read_to_string(tmp.path().join("config.toml")).unwrap())
                .unwrap();
        reloaded.config_path = tmp.path().join("config.toml");
        assert!(
            reloaded
                .providers
                .models
                .find("anthropic", "subscription_two")
                .is_some(),
            "persisted config must retain both concurrent applies"
        );
        zeroclaw_providers::create_model_provider_from_ref(&reloaded, "anthropic.subscription")
            .expect("gateway bootstrap must build the persisted OAuth alias");

        // Use the exact resilient-construction call made by gateway boot.
        // This makes the provider-selection contract explicit instead of only
        // proving the direct provider factory path.
        let entry = reloaded
            .providers
            .models
            .find("anthropic", "subscription")
            .unwrap();
        zeroclaw_providers::create_resilient_model_provider_from_ref(
            &reloaded,
            "anthropic.subscription",
            entry.api_key.as_deref(),
            entry.uri.as_deref(),
            &reloaded.reliability,
            &zeroclaw_providers::provider_runtime_options_for_alias(
                &reloaded,
                "anthropic",
                "subscription",
            ),
        )
        .expect("gateway bootstrap must resolve the persisted OAuth alias");
    }

    #[tokio::test]
    async fn http_quickstart_apply_failure_does_not_swap_or_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("workspace"),
            ..Default::default()
        };
        config.save().await.unwrap();
        std::fs::remove_file(&config.config_path).unwrap();
        std::fs::create_dir(&config.config_path).unwrap();
        let state = crate::api::test_state(config);
        let router = Router::new()
            .route("/api/quickstart/apply", post(handle_apply))
            .with_state(state.clone());
        let submission = BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".into(),
                alias: "subscription".into(),
                model: "claude-sonnet-4-5".into(),
                fields: std::collections::HashMap::from([
                    ("auth_mode".to_string(), "setup_token".to_string()),
                    ("api_key".to_string(), "synthetic-setup-token".to_string()),
                ]),
            }),
            risk_profile: SelectorChoice::Fresh("balanced".into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: vec![],
            peer_groups: vec![],
            agent: AgentIdentity {
                name: "quickstart_bot".into(),
                system_prompt: "You are helpful.".into(),
                personality_file: None,
                personality_files: vec![],
            },
        };
        let request = Request::builder()
            .method("POST")
            .uri("/api/quickstart/apply")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&submission).unwrap()))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["kind"],
            "errors"
        );
        assert!(
            state
                .config
                .read()
                .providers
                .models
                .find("anthropic", "subscription")
                .is_none(),
            "failed HTTP apply must not swap the live configuration"
        );
        assert!(
            !state
                .pending_reload
                .load(std::sync::atomic::Ordering::Relaxed),
            "failed HTTP apply must not request reload"
        );
    }

    #[tokio::test]
    async fn http_quickstart_rejects_another_apply_while_supervised_reload_is_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("workspace"),
            ..Default::default()
        };
        config.save().await.unwrap();
        let mut state = crate::api::test_state(config);
        let (reload_tx, _reload_rx) = tokio::sync::watch::channel(false);
        state.reload_tx = Some(reload_tx);
        let router = Router::new()
            .route("/api/quickstart/apply", post(handle_apply))
            .with_state(state.clone());
        let submission = BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".into(),
                alias: "subscription".into(),
                model: "claude-sonnet-4-5".into(),
                fields: std::collections::HashMap::from([
                    ("auth_mode".to_string(), "setup_token".to_string()),
                    ("api_key".to_string(), "synthetic-setup-token".to_string()),
                ]),
            }),
            risk_profile: SelectorChoice::Fresh("balanced".into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: vec![],
            peer_groups: vec![],
            agent: AgentIdentity {
                name: "quickstart_bot".into(),
                system_prompt: "You are helpful.".into(),
                personality_file: None,
                personality_files: vec![],
            },
        };
        let mut second_submission = submission.clone();
        second_submission.agent.name = "quickstart_bot_two".into();

        let first = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/quickstart/apply")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&submission).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first: serde_json::Value =
            serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(first["kind"], "applied");

        let second = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/quickstart/apply")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&second_submission).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let second: serde_json::Value =
            serde_json::from_slice(&second.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(second["kind"], "errors");
        assert_eq!(second["errors"][0]["field"], "reload");
        assert!(
            !state
                .config
                .read()
                .agents
                .contains_key("quickstart_bot_two"),
            "rejected queued HTTP apply must not modify the outgoing daemon config"
        );
    }

    #[tokio::test]
    async fn http_quickstart_personality_failure_returns_applied_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("workspace"),
            ..Default::default()
        };
        config.save().await.unwrap();
        std::fs::create_dir_all(config.agent_workspace_dir("quickstart_bot").join("SOUL.md"))
            .unwrap();
        let state = crate::api::test_state(config);
        let router = Router::new()
            .route("/api/quickstart/apply", post(handle_apply))
            .with_state(state.clone());
        let submission = BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".into(),
                alias: "subscription".into(),
                model: "claude-sonnet-4-5".into(),
                fields: std::collections::HashMap::from([
                    ("auth_mode".to_string(), "setup_token".to_string()),
                    ("api_key".to_string(), "synthetic-setup-token".to_string()),
                ]),
            }),
            risk_profile: SelectorChoice::Fresh("balanced".into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: vec![],
            peer_groups: vec![],
            agent: AgentIdentity {
                name: "quickstart_bot".into(),
                system_prompt: "You are helpful.".into(),
                personality_file: None,
                personality_files: vec![zeroclaw_config::presets::QuickstartPersonalityFile {
                    filename: "SOUL.md".into(),
                    content: "synthetic personality".into(),
                }],
            },
        };
        let request = Request::builder()
            .method("POST")
            .uri("/api/quickstart/apply")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&submission).unwrap()))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["kind"], "applied");
        assert_eq!(body["warnings"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["warnings"][0]["field"], "personality_files");
        assert!(
            state.config.read().agents.contains_key("quickstart_bot"),
            "the live gateway configuration must be swapped after durable setup"
        );
    }
}
