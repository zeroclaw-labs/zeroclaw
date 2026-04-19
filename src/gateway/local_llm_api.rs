//! Local-LLM administration REST endpoints.
//!
//! Backs the `SettingsLocalModel.tsx` React component (spec §6). All
//! handlers are cheap wrappers over the existing building blocks in
//! `src/host_probe/` and `src/local_llm/setup.rs` — this module only
//! bridges HTTP to those functions and serializes the results.
//!
//! ## Endpoints
//!
//! * `GET  /api/local-llm/status`   — current model + daemon health +
//!                                    last probe
//! * `POST /api/local-llm/reprobe`  — re-run host hardware probe and
//!                                    persist the fresh profile
//! * `POST /api/local-llm/uninstall`— remove the configured model from
//!                                    the Ollama daemon (does NOT remove
//!                                    Ollama itself)
//! * `POST /api/local-llm/offline-only`
//!                                  — toggle `reliability.offline_force_local`
//!
//! Tier switching is handled by the existing `setup local-llm --tier X`
//! CLI + `setup::run_setup` — the switch UI triggers a background
//! `run_setup` call and streams progress via the same channel as
//! `maybe_auto_bootstrap_local_llm`, so there's no new endpoint here.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::AppState;
use crate::host_probe::{self, HardwareProfile, Tier};
use crate::local_llm::{self, InstalledModel, LocalLlmConfig};

/// Mount point. Callers wire this into the top-level gateway Router via
/// `.nest("/api/local-llm", local_llm_api::router())`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/status", get(handle_status))
        .route("/reprobe", post(handle_reprobe))
        .route("/uninstall", post(handle_uninstall))
        .route("/offline-only", post(handle_offline_only))
}

// ── /status ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    /// Tag MoA will route local requests to, from `~/.moa/local_llm.toml`.
    /// `None` if setup has never run.
    pub default_model: Option<String>,
    /// Whether the Ollama daemon is answering at the configured base URL.
    pub daemon_running: bool,
    /// Whether the configured model tag is actually installed in the
    /// daemon (runs `/api/tags` and looks for a match).
    pub model_installed: bool,
    /// Last persisted hardware probe, if any. `None` if setup never ran
    /// or the file is missing.
    pub hardware: Option<HardwareProfile>,
    /// Full inventory of installed models (for the "change tier"
    /// dropdown: tiers already installed should be highlighted).
    pub installed_models: Vec<InstalledModel>,
    /// Whether strict offline-only mode is on (reliability.offline_force_local).
    pub offline_force_local: bool,
    /// MoA's active reasoning model tag (legacy "primary cloud" display).
    pub primary_cloud: Option<String>,
}

async fn handle_status(State(state): State<AppState>) -> impl IntoResponse {
    let base_url = local_llm::DEFAULT_OLLAMA_URL;
    let daemon_running = local_llm::is_ollama_running(base_url).await;
    let default_model = match LocalLlmConfig::default_path() {
        Ok(path) if path.exists() => LocalLlmConfig::load(&path).await.ok().map(|c| c.default_model),
        _ => None,
    };
    let model_installed = match &default_model {
        Some(tag) if daemon_running => {
            local_llm::is_installed(base_url, tag).await.unwrap_or(false)
        }
        _ => false,
    };
    let hardware = match HardwareProfile::default_path() {
        Ok(path) if path.exists() => HardwareProfile::load(&path).await.ok(),
        _ => None,
    };
    let installed_models = if daemon_running {
        local_llm::list_installed(base_url).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    let (offline_force_local, primary_cloud) = {
        let guard = state.config.lock();
        (
            guard.reliability.offline_force_local,
            guard.default_provider.clone(),
        )
    };

    Json(StatusResponse {
        default_model,
        daemon_running,
        model_installed,
        hardware,
        installed_models,
        offline_force_local,
        primary_cloud,
    })
}

// ── /reprobe ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ReprobeResponse {
    pub profile: HardwareProfile,
    /// Whether the result was more conservative than the raw probe (the
    /// §2.2 one-step downgrade when near a tier boundary).
    pub downgraded: bool,
    /// `true` if the persisted profile at `~/.moa/hardware_profile.json`
    /// was overwritten. `false` on write failure (e.g. read-only home).
    pub persisted: bool,
}

async fn handle_reprobe() -> impl IntoResponse {
    match host_probe::probe(true).await {
        Ok(profile) => {
            let persisted = if let Ok(path) = HardwareProfile::default_path() {
                profile.save(&path).await.is_ok()
            } else {
                false
            };
            Json(ReprobeResponse {
                downgraded: profile.downgraded,
                profile,
                persisted,
            })
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("hardware probe failed: {e:#}"),
            }),
        )
            .into_response(),
    }
}

// ── /uninstall ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UninstallRequest {
    /// Ollama tag to remove (e.g. `gemma4:e4b`). Defaults to the tag in
    /// `~/.moa/local_llm.toml` when omitted.
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UninstallResponse {
    pub removed_tag: String,
}

async fn handle_uninstall(Json(req): Json<UninstallRequest>) -> impl IntoResponse {
    let base_url = local_llm::DEFAULT_OLLAMA_URL;
    let tag = match req.tag {
        Some(t) => t,
        None => match LocalLlmConfig::default_path() {
            Ok(p) if p.exists() => match LocalLlmConfig::load(&p).await {
                Ok(c) => c.default_model,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ErrorBody {
                            error: format!("no tag supplied and local_llm.toml unreadable: {e:#}"),
                        }),
                    )
                        .into_response();
                }
            },
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorBody {
                        error: "no tag supplied and no local_llm.toml to default from".into(),
                    }),
                )
                    .into_response();
            }
        },
    };

    // Call Ollama's DELETE /api/delete. We don't have a helper for this
    // in local_llm yet, so build the request inline.
    let client = reqwest::Client::new();
    let resp = match client
        .delete(format!("{base_url}/api/delete"))
        .json(&serde_json::json!({ "model": tag.clone() }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(ErrorBody {
                    error: format!("Ollama delete failed: {e}"),
                }),
            )
                .into_response();
        }
    };
    if !resp.status().is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(ErrorBody {
                error: format!("Ollama delete returned status {}", resp.status()),
            }),
        )
            .into_response();
    }
    Json(UninstallResponse { removed_tag: tag }).into_response()
}

// ── /offline-only ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct OfflineOnlyRequest {
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct OfflineOnlyResponse {
    pub offline_force_local: bool,
}

async fn handle_offline_only(
    State(state): State<AppState>,
    Json(req): Json<OfflineOnlyRequest>,
) -> impl IntoResponse {
    // Take a snapshot under the lock, mutate, then drop the lock before
    // the async save so we don't hold a std::sync::Mutex across await.
    let snapshot = {
        let mut guard = state.config.lock();
        guard.reliability.offline_force_local = req.enabled;
        guard.clone()
    };
    if let Err(e) = snapshot.save().await {
        tracing::warn!(
            error = %format!("{e:#}"),
            "failed to persist offline_force_local toggle — in-memory value still applied"
        );
    }
    Json(OfflineOnlyResponse {
        offline_force_local: req.enabled,
    })
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Friendly tier labels matching the §1 matrix. Kept here next to the API
/// so the React side can just use the human-readable strings.
pub fn tier_display_name(tier: Tier) -> &'static str {
    tier.display_name()
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}
