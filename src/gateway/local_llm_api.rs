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
//! * `POST /api/local-llm/switch-tier?tier=<e2b|e4b|26b|31b>`
//!                                  — run `setup::run_setup` with the
//!                                    override tier and stream stage +
//!                                    install + pull progress as
//!                                    Server-Sent Events

use std::convert::Infallible;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

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
        .route("/switch-tier", post(handle_switch_tier))
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

// ── /switch-tier (SSE stream of setup progress) ─────────────────────

#[derive(Debug, Deserialize)]
pub struct SwitchTierQuery {
    pub tier: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SwitchEvent {
    Stage {
        stage: local_llm::setup::SetupStage,
    },
    InstallProgress {
        stage: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        line: Option<String>,
    },
    PullProgress {
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        fraction: Option<f32>,
    },
    Done {
        model_tag: String,
        pull_attempts: u32,
        probe_succeeded: bool,
    },
    Error {
        message: String,
    },
}

/// POST /api/local-llm/switch-tier?tier=<e2b|e4b|26b|31b>
///
/// Streams setup::run_setup progress as Server-Sent Events. Cancels the
/// background work when the SSE client disconnects so we don't keep
/// downloading multi-GB blobs for a closed connection.
async fn handle_switch_tier(Query(q): Query<SwitchTierQuery>) -> axum::response::Response {
    let tier = match parse_tier(&q.tier) {
        Ok(t) => t,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(ErrorBody { error: e })).into_response();
        }
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<SwitchEvent>(64);
    let cb_stage_tx = tx.clone();
    let cb_install_tx = tx.clone();
    let cb_pull_tx = tx.clone();
    let terminal_tx = tx.clone();
    let disconnect_watcher = tx.clone();

    tokio::spawn(async move {
        let mut on_stage = move |stage: local_llm::setup::SetupStage| {
            let _ = cb_stage_tx.try_send(SwitchEvent::Stage { stage });
        };
        let mut on_install = move |p: local_llm::installer::InstallProgress| {
            let _ = cb_install_tx.try_send(SwitchEvent::InstallProgress {
                stage: p.stage,
                line: p.line,
            });
        };
        let mut on_pull = move |p: local_llm::PullProgress| {
            let _ = cb_pull_tx.try_send(SwitchEvent::PullProgress {
                status: p.status.clone(),
                digest: p.digest.clone(),
                fraction: p.fraction(),
            });
        };

        let mut callbacks = local_llm::setup::SetupCallbacks {
            on_stage: &mut on_stage,
            on_install_progress: &mut on_install,
            on_pull_progress: &mut on_pull,
        };
        let opts = local_llm::setup::SetupOptions {
            override_tier: Some(tier),
            ..local_llm::setup::SetupOptions::default()
        };

        let setup_fut = local_llm::setup::run_setup(opts, &mut callbacks);
        let cancel_fut = disconnect_watcher.closed();

        tokio::select! {
            biased;
            _ = cancel_fut => {
                tracing::info!(
                    "switch-tier client disconnected mid-setup; aborting run_setup"
                );
            }
            result = setup_fut => {
                match result {
                    Ok(report) => {
                        let _ = terminal_tx
                            .send(SwitchEvent::Done {
                                model_tag: report.model_tag,
                                pull_attempts: report.pull_attempts,
                                probe_succeeded: report.probe_succeeded,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = terminal_tx
                            .send(SwitchEvent::Error {
                                message: format!("{e:#}"),
                            })
                            .await;
                    }
                }
            }
        }
    });
    drop(tx);

    let stream = ReceiverStream::new(rx).map(|evt| {
        let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
        Ok::<_, Infallible>(Event::default().data(payload))
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn parse_tier(raw: &str) -> Result<Tier, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "e2b" | "t1" | "t1e2b" => Ok(Tier::T1E2B),
        "e4b" | "t2" | "t2e4b" => Ok(Tier::T2E4B),
        "26b" | "t3" | "t3moe26b" => Ok(Tier::T3MoE26B),
        "31b" | "t4" | "t4dense31b" => Ok(Tier::T4Dense31B),
        other => Err(format!(
            "unknown tier `{other}`; valid: e2b, e4b, 26b, 31b"
        )),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tier_accepts_all_four_tiers() {
        assert_eq!(parse_tier("e2b").unwrap(), Tier::T1E2B);
        assert_eq!(parse_tier("e4b").unwrap(), Tier::T2E4B);
        assert_eq!(parse_tier("26b").unwrap(), Tier::T3MoE26B);
        assert_eq!(parse_tier("31b").unwrap(), Tier::T4Dense31B);
    }

    #[test]
    fn parse_tier_rejects_unknown() {
        let err = parse_tier("bogus").expect_err("bogus must be rejected");
        assert!(err.contains("bogus"));
    }

    #[test]
    fn switch_event_kind_tags() {
        let done = SwitchEvent::Done {
            model_tag: "gemma4:e4b".into(),
            pull_attempts: 1,
            probe_succeeded: true,
        };
        let err = SwitchEvent::Error {
            message: "disk full".into(),
        };
        assert_eq!(serde_json::to_value(&done).unwrap()["kind"], "done");
        assert_eq!(serde_json::to_value(&err).unwrap()["kind"], "error");
    }

    #[tokio::test]
    async fn switch_tier_aborts_setup_on_client_disconnect() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let (tx, rx) = tokio::sync::mpsc::channel::<SwitchEvent>(64);
        let disconnect_watcher = tx.clone();
        let flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&flag);

        let handle = tokio::spawn(async move {
            let surrogate_setup = async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                flag_clone.store(true, Ordering::Relaxed);
            };
            let cancel_fut = disconnect_watcher.closed();
            tokio::select! {
                biased;
                _ = cancel_fut => {}
                () = surrogate_setup => {}
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(rx);

        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("task should finish within 1 s of disconnect")
            .expect("no panic");

        assert!(
            !flag.load(Ordering::Relaxed),
            "surrogate completed; cancellation did not fire"
        );
        drop(tx);
    }
}
