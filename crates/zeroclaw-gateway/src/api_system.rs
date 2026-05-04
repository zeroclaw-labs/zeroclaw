//! `/api/system/*` — version check + self-update endpoints.
//!
//! Backed by `zeroclaw_runtime::updater`, which is the same pipeline `zeroclaw
//! update` runs on the CLI. The gateway adds a small bit of state to track
//! whether an update is currently running (so we can return 409 instead of
//! kicking off a second one) and an SSE stream for live phase progress.

use super::AppState;
use super::api::require_auth;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use parking_lot::Mutex;
use serde::Deserialize;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use zeroclaw_runtime::updater::{self, UpdateEvent, UpdatePhase};

/// Capacity of the recent-events ring buffer surfaced by `/status`.
const RECENT_EVENTS_CAPACITY: usize = 64;

/// Capacity of the broadcast channel for live SSE consumers.
const SSE_BROADCAST_CAPACITY: usize = 128;

/// Tracks an in-progress (or just-completed) update run.
///
/// Clones of `UpdateState` (held inside `AppState`) all observe the same
/// snapshot, so handlers can inspect/mutate the run from any axum task.
pub struct UpdateState {
    inner: Mutex<UpdateStateInner>,
    events_tx: broadcast::Sender<UpdateEvent>,
}

struct UpdateStateInner {
    current: Option<UpdateRun>,
    recent: VecDeque<UpdateEvent>,
}

/// Snapshot of an update run.
#[derive(Clone)]
struct UpdateRun {
    task_id: String,
    phase: UpdatePhase,
    started_at: chrono::DateTime<chrono::Utc>,
    /// `Some(true)` on success, `Some(false)` on failure, `None` while running.
    success: Option<bool>,
}

impl UpdateState {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(SSE_BROADCAST_CAPACITY);
        Self {
            inner: Mutex::new(UpdateStateInner {
                current: None,
                recent: VecDeque::with_capacity(RECENT_EVENTS_CAPACITY),
            }),
            events_tx: tx,
        }
    }

    /// Start tracking a new run. Returns `Err(())` if one is already active.
    pub(crate) fn try_start(&self, task_id: &str) -> Result<(), ()> {
        let mut g = self.inner.lock();
        if let Some(run) = &g.current
            && run.success.is_none()
        {
            return Err(());
        }
        g.current = Some(UpdateRun {
            task_id: task_id.to_string(),
            phase: UpdatePhase::Preflight,
            started_at: chrono::Utc::now(),
            success: None,
        });
        g.recent.clear();
        Ok(())
    }

    fn record_event(&self, event: &UpdateEvent) {
        let mut g = self.inner.lock();
        if let Some(run) = g.current.as_mut() {
            run.phase = event.phase;
            match event.phase {
                UpdatePhase::Done => run.success = Some(true),
                UpdatePhase::Failed => run.success = Some(false),
                UpdatePhase::RolledBack => run.success = Some(false),
                _ => {}
            }
        }
        if g.recent.len() == RECENT_EVENTS_CAPACITY {
            g.recent.pop_front();
        }
        g.recent.push_back(event.clone());
    }

    fn snapshot(&self) -> StatusSnapshot {
        let g = self.inner.lock();
        StatusSnapshot {
            run: g.current.clone(),
            recent: g.recent.iter().cloned().collect(),
        }
    }
}

impl Default for UpdateState {
    fn default() -> Self {
        Self::new()
    }
}

struct StatusSnapshot {
    run: Option<UpdateRun>,
    recent: Vec<UpdateEvent>,
}

// ── Handlers ─────────────────────────────────────────────────────

/// `GET /api/system/version` — current vs. latest release info.
pub async fn handle_get_version(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    match updater::check(None).await {
        Ok(info) => Json(serde_json::json!({
            "current": info.current_version,
            "latest": info.latest_version,
            "update_available": info.is_newer,
            "latest_published_at": info.latest_published_at,
            "download_url": info.download_url,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": format!("failed to check for updates: {e}"),
            })),
        )
            .into_response(),
    }
}

#[derive(Deserialize, Default)]
pub struct UpdateRequestBody {
    /// Optional explicit version (e.g. "0.7.5"). Defaults to latest.
    pub version: Option<String>,
    /// If true, run even when the latest version is not newer than current.
    /// Reserved for future use; currently ignored.
    #[serde(default)]
    #[allow(dead_code)]
    pub force: bool,
}

/// `POST /api/system/update` — kick off an update run.
///
/// Returns 409 if a run is already in progress.
pub async fn handle_post_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<UpdateRequestBody>>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let body = body.map(|Json(b)| b).unwrap_or_default();
    let task_id = format!("upd_{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"));

    if state.update_state.try_start(&task_id).is_err() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "an update is already in progress",
            })),
        )
            .into_response();
    }

    spawn_update_task(state.update_state.clone(), task_id.clone(), body.version);

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "task_id": task_id })),
    )
        .into_response()
}

/// `GET /api/system/update/status` — current phase + recent log lines.
pub async fn handle_get_update_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let snap = state.update_state.snapshot();
    let status_str = match snap.run.as_ref().and_then(|r| r.success) {
        Some(true) => "succeeded",
        Some(false) => "failed",
        None if snap.run.is_some() => "running",
        None => "idle",
    };

    Json(serde_json::json!({
        "status": status_str,
        "task_id": snap.run.as_ref().map(|r| &r.task_id),
        "phase": snap.run.as_ref().map(|r| r.phase),
        "started_at": snap.run.as_ref().map(|r| r.started_at),
        "log_tail": snap.recent,
    }))
    .into_response()
}

/// `GET /api/system/update/stream` — SSE of phase transitions and log lines.
pub async fn handle_get_update_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization: Bearer <token>",
            )
                .into_response();
        }
    }

    let rx = state.update_state.events_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => serde_json::to_string(&event)
            .ok()
            .map(|s| Ok::<_, Infallible>(Event::default().data(s))),
        Err(_) => None,
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ── Task driver ─────────────────────────────────────────────────

fn spawn_update_task(state: Arc<UpdateState>, task_id: String, target_version: Option<String>) {
    tokio::spawn(async move {
        let (tx, mut rx) = mpsc::channel::<UpdateEvent>(64);

        // Forward each event into the shared snapshot + SSE broadcast.
        let forwarder_state = state.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                forwarder_state.record_event(&event);
                let _ = forwarder_state.events_tx.send(event);
            }
        });

        let _ = updater::run_with_progress(target_version.as_deref(), &task_id, tx).await;

        let _ = forwarder.await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn try_start_returns_err_when_run_active() {
        let state = UpdateState::new();
        assert!(state.try_start("a").is_ok());
        assert!(
            state.try_start("b").is_err(),
            "second concurrent start should be rejected"
        );
    }

    #[tokio::test]
    async fn try_start_succeeds_after_terminal_event() {
        let state = UpdateState::new();
        state.try_start("a").unwrap();
        state.record_event(&UpdateEvent {
            task_id: "a".into(),
            phase: UpdatePhase::Done,
            level: updater::UpdateLevel::Info,
            message: "ok".into(),
            timestamp: chrono::Utc::now(),
        });
        assert!(
            state.try_start("b").is_ok(),
            "new run should be allowed once previous is in a terminal state"
        );
    }

    #[tokio::test]
    async fn snapshot_includes_recent_events() {
        let state = UpdateState::new();
        state.try_start("a").unwrap();
        for i in 0..3 {
            state.record_event(&UpdateEvent {
                task_id: "a".into(),
                phase: UpdatePhase::Download,
                level: updater::UpdateLevel::Info,
                message: format!("chunk {i}"),
                timestamp: chrono::Utc::now(),
            });
        }
        let snap = state.snapshot();
        assert_eq!(snap.recent.len(), 3);
        assert!(snap.run.is_some());
    }
}
