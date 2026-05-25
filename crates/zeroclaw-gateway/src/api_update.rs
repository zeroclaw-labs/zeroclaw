//! Gateway API handlers for the self-update flow.
//!
//! Two pairing-auth-gated endpoints:
//! - `GET  /api/update/check` — check for available updates
//! - `POST /api/update/run`   — start the update pipeline (returns immediately)

use super::AppState;
use axum::Json;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};

// ── GET /api/update/check ──────────────────────────────────────────

pub async fn handle_api_update_check(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    super::api::require_auth(&state, &headers)?;

    match zeroclaw_runtime::updater::check(None).await {
        Ok(info) => Ok(Json(serde_json::json!({
            "current_version": info.current_version,
            "latest_version": info.latest_version,
            "is_newer": info.is_newer,
            "download_url": info.download_url,
        }))),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

// ── POST /api/update/run ───────────────────────────────────────────

pub async fn handle_api_update_run(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    super::api::require_auth(&state, &headers)?;

    // Single-flight guard — only one update at a time.
    if state
        .update_in_progress
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_err()
    {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "status": "already_in_progress" })),
        ));
    }

    let event_tx = state.event_tx.clone();
    let shutdown_tx = state.shutdown_tx.clone();
    let update_in_progress = state.update_in_progress.clone();

    tokio::spawn(async move {
        let result = zeroclaw_runtime::updater::run(None, Some(&event_tx)).await;

        match result {
            Ok(_new_version) => {
                // Brief pause so the SSE client receives the update_complete event.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                // Spawn a new process with the updated binary, then exit.
                let exe = match std::env::current_exe() {
                    Ok(e) => e,
                    Err(_) => {
                        update_in_progress.store(false, std::sync::atomic::Ordering::SeqCst);
                        return;
                    }
                };
                let args: Vec<String> = std::env::args().skip(1).collect();
                let _ = std::process::Command::new(exe).args(&args).spawn();

                let _ = shutdown_tx.send(true);
                std::process::exit(0);
            }
            Err(e) => {
                let _ = event_tx.send(serde_json::json!({
                    "type": "update_failed",
                    "error": e.to_string(),
                }));
                update_in_progress.store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
    });

    Ok(Json(serde_json::json!({ "status": "started" })))
}
