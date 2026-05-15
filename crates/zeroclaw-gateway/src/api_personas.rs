//! REST handlers for `/api/personas/*` — persona preset CRUD.
//!
//! Personas are user-authored bundles of `(provider, model,
//! personality, mode)` saved as TOML files under
//! `<workspace_dir>/personas/`. The slot settings drawer's Quick tab
//! reads this list to render its preset dropdown; selecting one
//! stamps all four fields onto the slot via `PATCH /api/slots/:id`.
//!
//! Behaviour:
//!   * `GET /api/personas` lists every persona, alphabetical by name.
//!     On first call against an empty/missing personas dir the four
//!     defaults from [`crate::persona::default_presets`] are seeded so
//!     the dashboard's first request gets a populated dropdown without
//!     onboarding ceremony.
//!   * `GET /api/personas/:name` reads a single persona; 404 when
//!     absent.
//!   * `POST /api/personas` upserts (creates or overwrites) a persona;
//!     name validation is enforced.
//!   * `DELETE /api/personas/:name` removes the file; 404 when
//!     absent.
//!
//! Auth: every handler gates on `require_auth` to match the rest of
//! the `/api/*` surface.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};

use super::AppState;
use super::api::require_auth;
use super::persona::{
    PersonaError, PersonaListResponse, PersonaPreset, delete_one, load_all, load_one, save_one,
    seed_defaults_if_empty, validate_name,
};

fn err_response(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (status, Json(PersonaError::new(code, message))).into_response()
}

fn io_err(e: std::io::Error) -> Response {
    err_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "persona_store_error",
        format!("persona store failure: {e}"),
    )
}

fn workspace_dir(state: &AppState) -> std::path::PathBuf {
    state.config.lock().workspace_dir.clone()
}

/// `GET /api/personas` — list every persona under
/// `<workspace_dir>/personas/`, sorted alphabetically.
///
/// Seeds the four defaults on first call against an empty dir so the
/// frontend always has at least the bundled presets to render.
pub async fn handle_api_personas_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let ws = workspace_dir(&state);

    if let Err(e) = seed_defaults_if_empty(&ws) {
        // Seeding failure is logged but does not block list; the user
        // still gets an honest empty (or partial) list rather than 500.
        tracing::warn!(error = %e, "persona default seeding failed");
    }

    match load_all(&ws) {
        Ok(personas) => Json(PersonaListResponse { personas }).into_response(),
        Err(e) => io_err(e),
    }
}

/// `GET /api/personas/:name` — read one persona by name.
pub async fn handle_api_personas_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let validated = match validate_name(&name) {
        Ok(n) => n,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(e)).into_response();
        }
    };
    let ws = workspace_dir(&state);
    match load_one(&ws, validated) {
        Ok(Some(preset)) => Json(preset).into_response(),
        Ok(None) => err_response(
            StatusCode::NOT_FOUND,
            "persona_not_found",
            format!("Persona {name:?} does not exist"),
        ),
        Err(e) => io_err(e),
    }
}

/// `POST /api/personas` — create or overwrite a persona.
///
/// The request body's `name` field is the canonical id; the path is
/// not used for create so an early-bound URL doesn't fight the body.
/// Round-trips through `validate_name` so traversal-style names are
/// rejected before any disk write.
pub async fn handle_api_personas_upsert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(preset): Json<PersonaPreset>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    if let Err(e) = validate_name(&preset.name) {
        return (StatusCode::BAD_REQUEST, Json(e)).into_response();
    }
    let ws = workspace_dir(&state);
    if let Err(e) = save_one(&ws, &preset) {
        return io_err(e);
    }
    Json(preset).into_response()
}

/// `DELETE /api/personas/:name` — remove a persona.
pub async fn handle_api_personas_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let validated = match validate_name(&name) {
        Ok(n) => n,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(e)).into_response(),
    };
    let ws = workspace_dir(&state);
    match delete_one(&ws, validated) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => err_response(
            StatusCode::NOT_FOUND,
            "persona_not_found",
            format!("Persona {name:?} does not exist"),
        ),
        Err(e) => io_err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_queue::{SessionActorQueue, SlotActorQueue};
    use axum::body::to_bytes;
    use std::sync::Arc;
    use zeroclaw_config::schema::Config;

    fn personas_test_state(workspace_dir: std::path::PathBuf) -> AppState {
        let config = Config {
            workspace_dir,
            ..Config::default()
        };
        AppState {
            config: Arc::new(parking_lot::Mutex::new(config)),
            provider: Arc::new(StubProvider),
            model: "stub-model".into(),
            temperature: 0.0,
            mem: Arc::new(StubMemory),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(zeroclaw_runtime::security::pairing::PairingGuard::new(
                false,
                &[],
            )),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(crate::GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(crate::auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(crate::IdempotencyStore::new(
                std::time::Duration::from_secs(300),
                1000,
            )),
            whatsapp: None,
            whatsapp_app_secret: None,
            linq: None,
            linq_signing_secret: None,
            nextcloud_talk: None,
            nextcloud_talk_webhook_secret: None,
            wati: None,
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(crate::sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(crate::nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            web_dashboard_dist_dir: None,
            session_backend: None,
            session_queue: Arc::new(SessionActorQueue::new(8, 30, 600)),
            slot_queue: Arc::new(SlotActorQueue::new(8, 30, 600)),
            slot_store: None,
            slot_cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            mcp_registry: None,
            slot_registry: crate::slot_registry::SlotRegistry::new(600),
            device_registry: None,
            pending_pairings: None,
            canvas_store: zeroclaw_runtime::tools::CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            #[cfg(feature = "webauthn")]
            webauthn: None,
        }
    }

    struct StubProvider;
    #[async_trait::async_trait]
    impl zeroclaw_providers::Provider for StubProvider {
        async fn chat_with_system(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".into())
        }
    }

    struct StubMemory;
    #[async_trait::async_trait]
    impl zeroclaw_memory::Memory for StubMemory {
        fn name(&self) -> &str {
            "stub"
        }
        async fn store(
            &self,
            _: &str,
            _: &str,
            _: zeroclaw_memory::MemoryCategory,
            _: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recall(
            &self,
            _: &str,
            _: usize,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn get(&self, _: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _: Option<&zeroclaw_memory::MemoryCategory>,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn forget(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn health_check(&self) -> bool {
            true
        }
    }

    async fn body_to_json(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_default();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn personas_list_seeds_defaults_on_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = personas_test_state(tmp.path().to_path_buf());

        let resp = handle_api_personas_list(State(state.clone()), HeaderMap::new()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_to_json(resp).await;
        let personas = json["personas"].as_array().unwrap();
        assert_eq!(personas.len(), 4);
        let names: Vec<&str> = personas
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"claude-code-default"));
        assert!(names.contains(&"codex-researcher"));
        assert!(names.contains(&"gemini-cli-coder"));
        assert!(names.contains(&"bedrock-claude"));
    }

    #[tokio::test]
    async fn personas_get_returns_404_for_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = personas_test_state(tmp.path().to_path_buf());
        let resp =
            handle_api_personas_get(State(state), HeaderMap::new(), Path("ghost".into())).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_to_json(resp).await;
        assert_eq!(json["code"], "persona_not_found");
    }

    #[tokio::test]
    async fn personas_get_rejects_traversal_with_400() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = personas_test_state(tmp.path().to_path_buf());
        let resp =
            handle_api_personas_get(State(state), HeaderMap::new(), Path("../etc".into())).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_to_json(resp).await;
        assert_eq!(json["code"], "invalid_persona_name");
    }

    #[tokio::test]
    async fn personas_upsert_then_get_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = personas_test_state(tmp.path().to_path_buf());
        let preset = PersonaPreset {
            name: "my-preset".into(),
            provider: "anthropic".into(),
            model: Some("claude-sonnet-4".into()),
            personality: Some("SOUL.md".into()),
            mode: crate::slot::SlotMode::Normal,
            description: Some("desc".into()),
        };

        let upsert = handle_api_personas_upsert(
            State(state.clone()),
            HeaderMap::new(),
            Json(preset.clone()),
        )
        .await;
        assert_eq!(upsert.status(), StatusCode::OK);

        let get =
            handle_api_personas_get(State(state), HeaderMap::new(), Path("my-preset".into())).await;
        assert_eq!(get.status(), StatusCode::OK);
        let json = body_to_json(get).await;
        assert_eq!(json["name"], "my-preset");
        assert_eq!(json["provider"], "anthropic");
        assert_eq!(json["model"], "claude-sonnet-4");
        assert_eq!(json["personality"], "SOUL.md");
    }

    #[tokio::test]
    async fn personas_upsert_rejects_invalid_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = personas_test_state(tmp.path().to_path_buf());
        let preset = PersonaPreset {
            name: "../escape".into(),
            provider: "anthropic".into(),
            model: None,
            personality: None,
            mode: crate::slot::SlotMode::Normal,
            description: None,
        };
        let resp = handle_api_personas_upsert(State(state), HeaderMap::new(), Json(preset)).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn personas_delete_returns_204_then_404() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = personas_test_state(tmp.path().to_path_buf());
        // Seed via list endpoint.
        let _ = handle_api_personas_list(State(state.clone()), HeaderMap::new()).await;

        let del = handle_api_personas_delete(
            State(state.clone()),
            HeaderMap::new(),
            Path("codex-researcher".into()),
        )
        .await;
        assert_eq!(del.status(), StatusCode::NO_CONTENT);

        let del_again = handle_api_personas_delete(
            State(state),
            HeaderMap::new(),
            Path("codex-researcher".into()),
        )
        .await;
        assert_eq!(del_again.status(), StatusCode::NOT_FOUND);
    }
}
